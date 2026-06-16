//! Rust rules — shared toolchain, dep-closure, and build-script-edge helpers (split from
//! `rust_rules.rs`); used by both the core rust natives and the cargo build-script rules.

use crate::state::{canon_label, session};
use crate::deps::resolve_dep;
use starlark::eval::Evaluator;

pub(crate) const RUSTC: &str = "/usr/bin/rustc";


/// The `rustc` to invoke, resolved to an **absolute** path. Prefer the fixed
/// `/usr/bin/rustc` (matching cc's pinned toolchain paths); when absent — e.g. a
/// rustup install under `~/.cargo/bin` — scan `PATH` for it. The executor runs
/// actions with a cleared env (no `PATH`), so a bare name wouldn't resolve; an
/// absolute path also lets rustc locate its sysroot relative to its own binary.
/// Falls back to the bare name if nothing is found (the test guards on rustc).
pub(crate) fn rustc() -> String {
    if std::path::Path::new(RUSTC).exists() {
        return RUSTC.into();
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let cand = std::path::Path::new(dir).join("rustc");
            if cand.exists() {
                return cand.to_string_lossy().into_owned();
            }
        }
    }
    "rustc".into()
}


/// RazelRustParityPlan A5: a deterministic per-crate metadata hash for `--codegen=metadata`/
/// `extra-filename` + the `lib<name>-<hash>.rlib` output name (rules_rust's hashed-output model).
/// razel mints its OWN hash — the VALUE is a content hash Bazel computes that razel can't reproduce,
/// so it's normalized to `-<hash>` in the parity diff; only the SHAPE (`-` + ≥6 digits) matters here.
/// `DefaultHasher::new()` has a fixed seed → deterministic across runs.
pub(crate) fn metadata_hash(canon: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut h);
    format!("{:010}", h.finish() % 10_000_000_000) // 10 decimal digits (≥6 → normalizes to -<hash>)
}


/// P3.8: the razel process wrapper bin (the build-script runner / the P3.9 rustc wrapper). Resolved
/// like [`rustc`]: an explicit `RAZEL_PROCESS_WRAPPER` override, else the bare crate name (the
/// executor / toolchain resolves it on the exec path). Parity normalizes the wrapper prefix
/// (P3.11), so the exact path is not parity-gated.
pub(crate) fn process_wrapper() -> String {
    std::env::var("RAZEL_PROCESS_WRAPPER").unwrap_or_else(|_| "razel-process-wrapper".into())
}


/// P3.10 (§4.3): if a crate has a build-script edge, route its rustc `argv` through the process
/// wrapper — `[wrapper, rustc, --rustc=<rustc>, --flags-file=<flags>, --env=OUT_DIR=<dir>, --,
/// <original rustc argv…>]` — so the crate's compile consumes the build script's flags-file + the
/// `OUT_DIR` tree. Returns the (possibly rewritten) argv + the extra inputs to stage. No edge → argv
/// unchanged. Slice-1 supports ≤1 build script per crate (blake3 has one); >1 is a loud error until a
/// crate in scope needs it (the P3.9 `rustc` subcommand takes a single `--flags-file`).
pub(crate) fn apply_build_script_edge(
    rule: &str,
    name: &str,
    argv: Vec<String>,
    build_scripts: &[crate::deps::BuildScriptRunInfo],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    match build_scripts {
        [] => Ok((argv, Vec::new())),
        [bs] => {
            let mut it = argv.into_iter();
            let rustc_path =
                it.next().ok_or_else(|| anyhow::anyhow!("{rule} `{name}`: empty rustc argv"))?;
            let mut wrapped = vec![
                process_wrapper(),
                "rustc".into(),
                format!("--rustc={rustc_path}"),
                format!("--flags-file={}", bs.flags_file),
                format!("--env=OUT_DIR={}", bs.out_dir),
                "--".into(),
            ];
            wrapped.extend(it);
            Ok((wrapped, vec![bs.flags_file.clone(), bs.out_dir.clone()]))
        }
        _ => anyhow::bail!(
            "{rule} `{name}`: multiple build-script deps are not supported yet (slice-1 — the rustc \
             wrapper takes a single --flags-file)"
        ),
    }
}


/// The crate name a dependent uses for `--extern` / `use`: the target segment of a
/// canonical label (`//lib:greet` → `greet`, bare `greet` → `greet`).
pub(crate) fn crate_name_of(canon: &str) -> String {
    canon
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(canon)
        .to_string()
}


/// RazelRustParityPlan B3/B4: the TRANSITIVE rlib closure (the rlib FILE paths). rules_rust passes a
/// `-Ldependency=<dir>` for EVERY crate in the transitive closure AND stages each transitive rlib as an
/// action input — a direct rlib references its own deps by name+hash, so rustc loads their `.rmeta`
/// from a `-Ldependency` dir, which the per-action sandbox must contain. Walk the ALREADY-analyzed dep
/// graph (`results[canon].deps`), collecting each rust target's rlib output FILE; deduped + SORTED
/// (deterministic — rustc treats search-path ORDER as irrelevant, so the parity diff compares the SET).
/// Only CRATES (rlib outputs) are collected/recursed: a `cargo_build_script` (`_bs`, a bin output) is
/// the intra-target build-script EDGE, not a crate dep — recursing it would wrongly pull in BUILD-only
/// deps (e.g. `version_check`) Bazel's crate-compile `-Ldependency` excludes.
pub(crate) fn transitive_rlibs(sess: &crate::state::Session, roots: &[String]) -> Vec<String> {
    let results = sess.results.borrow();
    let mut seen = std::collections::HashSet::new();
    let mut rlibs: Vec<String> = Vec::new();
    let mut stack: Vec<String> = roots.to_vec();
    while let Some(canon) = stack.pop() {
        if !seen.insert(canon.clone()) {
            continue;
        }
        let Some(t) = results.get(&canon) else { continue };
        let Some(rlib) = t.default_info.iter().find(|l| l.ends_with(".rlib")) else {
            continue; // not a crate (build-script bin / env file) → skip + don't recurse
        };
        if !rlibs.iter().any(|r| r == rlib) {
            rlibs.push(rlib.clone());
        }
        stack.extend(t.deps.iter().cloned());
    }
    rlibs.sort();
    rlibs
}


/// Resolve `deps` to `(--extern crate=rlib args, dep rlib inputs, dep canon names, build-script
/// edges)`. P3.10 (§4.3): a `cargo_build_script` dep is the intra-target build-script edge — it is
/// NEVER passed as `--extern` (it's not an rlib); it's collected for the rustc-wrapper routing.
pub(crate) fn extern_args(
    eval: &mut Evaluator<'_, '_, '_>,
    deps: Vec<String>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<crate::deps::BuildScriptRunInfo>)> {
    let (mut args, mut inputs, mut names, mut build_scripts) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut rlib_deps: Vec<String> = Vec::new(); // the `--extern`'d deps → roots for the -Ldependency fold
    for d in &deps {
        let dep = resolve_dep(eval, d)?;
        if let Some(bs) = dep.build_script {
            build_scripts.push(bs); // the §4.3 edge — captured, never `--extern`'d
            names.push(dep.canon);
            continue;
        }
        // P4.1 (§5.3): a `rust_proc_macro` dep is `--extern`'d as a HOST dylib, but is NOT a target
        // rlib — keep it OUT of `rlib_deps` so it never enters the transitive `-Ldependency` closure.
        if dep.proc_macro {
            let crate_name = crate_name_of(&dep.canon);
            for dylib in &dep.libs {
                args.push(format!("--extern={crate_name}={dylib}"));
            }
            names.push(dep.canon);
            continue;
        }
        let crate_name = crate_name_of(&dep.canon);
        // A rust_library exports exactly one rlib in default_info → dep.libs. rules_rust's faithful
        // form is `--extern=<name>=<rlib>` (joined) for the DIRECT dep.
        for rlib in &dep.libs {
            args.push(format!("--extern={crate_name}={rlib}"));
        }
        rlib_deps.push(dep.canon.clone());
        names.push(dep.canon);
    }
    // RazelRustParityPlan B3/B4: rustc must FIND the transitive rlib closure (a direct rlib references
    // its deps by name+hash → rustc loads their `.rmeta` from a `-Ldependency` dir). rules_rust emits a
    // `-Ldependency=<dir>` per transitive crate AND stages every transitive rlib as an input (the
    // per-action sandbox materializes only DECLARED inputs). Stage the rlibs + emit the dirs; the direct
    // `--extern` rlibs are a subset. The parity diff drops `external/<repo>/` inputs, so this is
    // analysis-parity-neutral; it's what makes B4 execution find e.g. `cc`'s `shlex`.
    let transitive = transitive_rlibs(session(eval), &rlib_deps);
    inputs.extend(transitive.iter().cloned());
    let mut dirs: Vec<String> =
        transitive.iter().filter_map(|r| r.rsplit_once('/').map(|(d, _)| d.to_string())).collect();
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        args.push(format!("-Ldependency={dir}"));
    }
    Ok((args, inputs, names, build_scripts))
}


/// P3.4 (§5.4): is the target INCOMPATIBLE with the configured platform? Compatible iff EVERY
/// `target_compatible_with` constraint holds (`condition_matches` == `Some(true)`); an empty list
/// is compatible. `@platforms//:incompatible` never holds (so it forces incompatibility), and any
/// unsatisfied/unresolved constraint is incompatible — never a silent pass.
pub(crate) fn is_incompatible(sess: &crate::state::Session, constraints: &[String]) -> anyhow::Result<bool> {
    for c in constraints {
        let canon = canon_label(sess, c);
        if crate::selects::condition_matches(sess, &canon, true, 32)? != Some(true) {
            return Ok(true);
        }
    }
    Ok(false)
}


