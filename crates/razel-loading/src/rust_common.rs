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
    extra_env: &[(String, String)],
    argv: Vec<String>,
    build_scripts: &[crate::deps::BuildScriptRunInfo],
    env_files: &[String],
    link_flags_files: &[String],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    // The OWN intra-target edge (§4.3, ≤1 in slice-1) supplies `--flags-file` (cfg+env+link to THIS
    // crate's compile) + the `OUT_DIR`. P4.5 (§5.5/§6): `link_flags_files` are the TRANSITIVE
    // build-script flags-files of the dep closure — passed `--link-flags-file` (LINK directives only,
    // P4.5a) so a `rust_binary`'s final link inherits the closure's native libs. Only `rust_binary`
    // passes these (an rlib is not a final link); library/proc-macro pass `&[]`.
    let own = match build_scripts {
        [] => None,
        [bs] => Some(bs),
        _ => anyhow::bail!(
            "{rule} `{name}`: multiple build-script deps are not supported yet (slice-1 — the rustc \
             wrapper takes a single --flags-file)"
        ),
    };
    if own.is_none() && link_flags_files.is_empty() && env_files.is_empty() && extra_env.is_empty() {
        return Ok((argv, Vec::new())); // nothing to inject → bare rustc unchanged
    }
    let mut it = argv.into_iter();
    let rustc_path =
        it.next().ok_or_else(|| anyhow::anyhow!("{rule} `{name}`: empty rustc argv"))?;
    let mut wrapped = vec![process_wrapper(), "rustc".into(), format!("--rustc={rustc_path}")];
    let mut inputs = Vec::new();
    if let Some(bs) = own {
        wrapped.push(format!("--flags-file={}", bs.flags_file));
        wrapped.push(format!("--env=OUT_DIR={}", bs.out_dir));
        inputs.push(bs.flags_file.clone());
        inputs.push(bs.out_dir.clone());
    }
    // `rustc_env_files` (cargo_toml_env_vars-style env-file targets) apply to the crate's rustc even
    // with NO build script — the `CARGO_PKG_*` a crate may `env!()` at COMPILE time (e.g.
    // serde_derive's `CARGO_PKG_VERSION_PATCH`). rules_rust routes these through the wrapper's
    // `--env-file`; matching it is what makes a pure-proc-macro/library crate that reads Cargo env compile.
    for f in env_files {
        wrapped.push(format!("--env-file={f}"));
        inputs.push(f.clone());
    }
    for f in link_flags_files {
        wrapped.push(format!("--link-flags-file={f}"));
        inputs.push(f.clone());
    }
    // The compile's extra process env — `--env K=V` BEFORE `--` (stripped by `canonicalize_rust_argv`
    // → parity-neutral). CARGO_MANIFEST_DIR (a proc-macro may read it: schemafy's schema path; the
    // wrapper absolutizes it like OUT_DIR) + CARGO_PKG_VERSION/NAME for a crate WITHOUT a
    // cargo_toml_env_vars env-file (workspace crates that `env!()` them — rules_rust's `version` attr
    // default). A bare crate with no extra env stays bare (the guard above).
    for (k, v) in extra_env {
        wrapped.push(format!("--env={k}={v}"));
    }
    wrapped.push("--".into());
    wrapped.extend(it);
    Ok((wrapped, inputs))
}

/// The exec-root-relative package (manifest) dir of a target's canonical label, for
/// `CARGO_MANIFEST_DIR`. External `@repo//pkg:name` → `external/<repo>[/<pkg>]` (Bazel's exec-root
/// form, trimming all leading `@` — §11.3); workspace `//pkg:name` → `pkg`.
pub(crate) fn manifest_dir_rel(canon: &str) -> String {
    if let Some(rest) = canon.strip_prefix('@') {
        let rest = rest.trim_start_matches('@');
        if let Some((repo, pkgname)) = rest.split_once("//") {
            let pkg = pkgname.split_once(':').map(|(p, _)| p).unwrap_or("");
            return if pkg.is_empty() {
                format!("external/{repo}")
            } else {
                format!("external/{repo}/{pkg}")
            };
        }
    }
    canon
        .strip_prefix("//")
        .and_then(|r| r.split_once(':'))
        .map(|(p, _)| p.to_string())
        .unwrap_or_default()
}

/// The extra `--env` a rust target's COMPILE needs (passed to [`apply_build_script_edge`]):
/// `CARGO_MANIFEST_DIR` always (the per-crate pkg dir; the wrapper absolutizes it), plus — for a
/// crate WITHOUT a `cargo_toml_env_vars` env-file — `CARGO_PKG_VERSION`/`CARGO_PKG_NAME` from the
/// rule attrs (rules_rust's `version` default "0.0.0", pkg_name = crate name). A workspace crate
/// that `env!()`s these (e.g. razel-daemon's `CARGO_PKG_VERSION`) compiles; an `@crates` crate
/// carries the env-file (rustc_env_files), so we DON'T emit here (else `--env` would override it).
pub(crate) fn compile_env(compile: &crate::rust_attrs::CompileAttrs, canon: &str) -> Vec<(String, String)> {
    let mut env = vec![("CARGO_MANIFEST_DIR".to_string(), manifest_dir_rel(canon))];
    if compile.rustc_env_files.is_empty() {
        env.push((
            "CARGO_PKG_VERSION".into(),
            compile.version.clone().unwrap_or_else(|| "0.0.0".into()),
        ));
        env.push((
            "CARGO_PKG_NAME".into(),
            compile.pkg_name.clone().unwrap_or_else(|| crate_name_of(canon)),
        ));
    }
    env
}

/// Resolve `rustc_env_files` (cargo_toml_env_vars-style env-file TARGETS) → `(dep canon names, env-file
/// OUTPUT paths)`. Each target's `default_info` is the generated `CARGO_PKG_*` env-file. The paths
/// become `--env-file`s on the crate's compile (via [`apply_build_script_edge`]) + staged inputs; the
/// NAMES must join the crate's `deps` so `collect_order` BUILDS the env-file action FIRST (else the
/// `--env-file` is absent at compile time). Lets a crate's COMPILE see the Cargo env it may `env!()`.
pub(crate) fn rustc_env_file_deps(
    eval: &mut Evaluator<'_, '_, '_>,
    parts: &[crate::values::StrAttrPart],
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let labels = crate::values::resolve_str_parts(eval, parts)?;
    let (mut names, mut files) = (Vec::new(), Vec::new());
    for label in &labels {
        let dep = crate::deps::resolve_dep(eval, label)?;
        files.extend(dep.libs);
        names.push(dep.canon);
    }
    Ok((names, files))
}

/// P4.5 (§5.5/§6): the TRANSITIVE set of build-script flags-file paths in `deps`' closure. Each
/// `rust_library` publishes its OWN build script's flags-file via `RustLinkInfo.bs_flags` (folded
/// transitively), so a consuming FINAL link (`rust_binary`) inherits the whole closure's native-link
/// directives. Deduped + SORTED (link-search order is a SET to rustc — deterministic action key).
pub(crate) fn transitive_link_flags_files(
    eval: &mut Evaluator<'_, '_, '_>,
    deps: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut files: Vec<String> = Vec::new();
    for d in deps {
        for f in crate::deps::resolve_dep(eval, d)?.field("rust_link_bs_flags") {
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    files.sort();
    Ok(files)
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
        if let Some(rlib) = t.default_info.iter().find(|l| l.ends_with(".rlib")) {
            if !rlibs.iter().any(|r| r == rlib) {
                rlibs.push(rlib.clone());
            }
            stack.extend(t.deps.iter().cloned()); // a crate's rlib deps are transitive
        } else if let Some(dylib) = t.default_info.iter().find(|l| l.ends_with(".dylib")) {
            // A re-exported PROC-MACRO dep (serde → serde_derive, `MacrosOnly`): a TRANSITIVE consumer
            // must find it on `-Ldependency` to LOAD the re-exporting crate (else "can't find crate for
            // serde"). Include the host dylib + its dir, but DON'T recurse — a proc-macro's own deps
            // (syn/quote/proc-macro2) were linked into the dylib and are not the consumer's concern.
            if !rlibs.iter().any(|r| r == dylib) {
                rlibs.push(dylib.clone());
            }
        }
        // else: build-script bin / env file → skip + don't recurse (BUILD-only deps stay excluded).
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
    aliases: &[(String, String)],
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>, Vec<crate::deps::BuildScriptRunInfo>)> {
    let (mut args, mut inputs, mut names, mut build_scripts) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut rlib_deps: Vec<String> = Vec::new(); // the `--extern`'d deps → roots for the -Ldependency fold
    // P5.4: dep-rename map — the `aliases` keys are APPARENT labels, canonicalized here to match each
    // resolved `dep.canon`. A dep present in this map is `--extern`'d under its ALIAS (rustix's
    // `errno` → `libc_errno`); absent → the crate's own name (`crate_name_of`).
    let alias_map: std::collections::HashMap<String, String> =
        aliases.iter().map(|(k, v)| (canon_label(session(eval), k), v.clone())).collect();
    let extern_name = |dep: &crate::deps::DepInfo| {
        alias_map.get(&dep.canon).cloned().unwrap_or_else(|| crate_name_of(&dep.canon))
    };
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
            let crate_name = extern_name(&dep);
            for dylib in &dep.libs {
                args.push(format!("--extern={crate_name}={dylib}"));
                // The host dylib is a DECLARED INPUT — the per-action sandbox stages only declared
                // inputs, so without this rustc reports "extern location does not exist" (e.g. ctor's
                // `linktime_proc_macro`). Still kept OUT of `rlib_deps` (it's a host dylib, not a
                // target rlib → never in the `-Ldependency` fold). `external/<repo>/` inputs are
                // dropped by the parity diff → analysis-parity-neutral.
                inputs.push(dylib.clone());
            }
            names.push(dep.canon);
            continue;
        }
        let crate_name = extern_name(&dep);
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


