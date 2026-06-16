//! @rules_rust → razel native rules: rust_library / rust_binary (rustc).
//!
//! `load("@rules_rust//rust:defs.bzl", "rust_binary"|"rust_library")` resolves to
//! these. Like `rules::cc_rules`, razel provides the rules *natively* (one `rustc`
//! action per target) instead of executing rules_rust's Starlark.
//!
//! A `rust_library` compiles its crate root (`srcs[0]`) to `lib<name>.rlib` and
//! exports it via `default_info`; a dependent reads that rlib through
//! `resolve_dep().libs` and wires it in as `--extern <crate>=<rlib>`. The dep crate
//! name is the dep's canonical-label target segment (`//lib:greet` → `greet`), so
//! the consumer can `use greet::...`. Paths are workspace-root-relative (exec_root =
//! workspace root), matching how cc uses `-iquote .`.

use crate::state::{
    AnalyzedAction, AnalyzedTarget, canon_label, native_decl, out_dir, out_path, qualify, session,
};
use crate::deps::{record_target, resolve_dep};
use crate::values::{unpack, unpack_strs};
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

const RUSTC: &str = "/usr/bin/rustc";

/// The `rustc` to invoke, resolved to an **absolute** path. Prefer the fixed
/// `/usr/bin/rustc` (matching cc's pinned toolchain paths); when absent — e.g. a
/// rustup install under `~/.cargo/bin` — scan `PATH` for it. The executor runs
/// actions with a cleared env (no `PATH`), so a bare name wouldn't resolve; an
/// absolute path also lets rustc locate its sysroot relative to its own binary.
/// Falls back to the bare name if nothing is found (the test guards on rustc).
fn rustc() -> String {
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
fn metadata_hash(canon: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    canon.hash(&mut h);
    format!("{:010}", h.finish() % 10_000_000_000) // 10 decimal digits (≥6 → normalizes to -<hash>)
}

/// P3.8: the razel process wrapper bin (the build-script runner / the P3.9 rustc wrapper). Resolved
/// like [`rustc`]: an explicit `RAZEL_PROCESS_WRAPPER` override, else the bare crate name (the
/// executor / toolchain resolves it on the exec path). Parity normalizes the wrapper prefix
/// (P3.11), so the exact path is not parity-gated.
fn process_wrapper() -> String {
    std::env::var("RAZEL_PROCESS_WRAPPER").unwrap_or_else(|_| "razel-process-wrapper".into())
}

/// P3.8d (§5.2): the `CARGO_CFG_*` build-script env cargo derives from the target, synthesized from
/// the configured (host==target) triple `<arch>-<vendor>-<sys>[-<env>]`. Covers the cfgs blake3's
/// `build.rs` keys SIMD off (`TARGET_ARCH`/`OS`/`ENV`/`FEATURE`) plus the standard set.
/// `TARGET_FEATURE` is a slice-1 per-arch BASELINE (the always-on features) — refined against the
/// P3.12 execution-parity golden, where the exact feature set is observable.
fn cargo_cfg_env(triple: &str) -> Vec<(String, String)> {
    let mut p = triple.split('-');
    let arch = p.next().unwrap_or("");
    let vendor = p.next().unwrap_or(""); // "apple" | "unknown"
    let sys = p.next().unwrap_or(""); // "darwin" | "linux" | …
    let abi = p.next().unwrap_or(""); // "gnu" | "" …
    let (os, env) = match sys {
        "darwin" => ("macos", ""),
        "linux" => ("linux", if abi.is_empty() { "gnu" } else { abi }),
        other => (other, abi),
    };
    // The always-on features rustc reports for the base target (no `-C target-feature` tuning).
    let feature = match arch {
        "x86_64" => "fxsr,sse,sse2",
        "aarch64" => "neon",
        _ => "",
    };
    [
        ("CARGO_CFG_TARGET_ARCH", arch),
        ("CARGO_CFG_TARGET_OS", os),
        ("CARGO_CFG_TARGET_FAMILY", "unix"),
        ("CARGO_CFG_TARGET_VENDOR", vendor),
        ("CARGO_CFG_TARGET_ENV", env),
        ("CARGO_CFG_TARGET_POINTER_WIDTH", "64"),
        ("CARGO_CFG_TARGET_ENDIAN", "little"),
        ("CARGO_CFG_TARGET_FEATURE", feature),
        ("CARGO_CFG_UNIX", ""), // a boolean cfg → present with an empty value (cargo's form)
        ("CARGO_CFG_PANIC", "unwind"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// P3.10 (§4.3): if a crate has a build-script edge, route its rustc `argv` through the process
/// wrapper — `[wrapper, rustc, --rustc=<rustc>, --flags-file=<flags>, --env=OUT_DIR=<dir>, --,
/// <original rustc argv…>]` — so the crate's compile consumes the build script's flags-file + the
/// `OUT_DIR` tree. Returns the (possibly rewritten) argv + the extra inputs to stage. No edge → argv
/// unchanged. Slice-1 supports ≤1 build script per crate (blake3 has one); >1 is a loud error until a
/// crate in scope needs it (the P3.9 `rustc` subcommand takes a single `--flags-file`).
fn apply_build_script_edge(
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
fn crate_name_of(canon: &str) -> String {
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
fn transitive_rlibs(sess: &crate::state::Session, roots: &[String]) -> Vec<String> {
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
fn extern_args(
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

/// §5.5 verdict for an attribute of the rust compile rules (`rust_library`/`rust_binary`): the
/// single source of truth for **accept vs loud-error**, kept separate from "implement semantics"
/// (P2#3). An attr absent from [`rust_attr_verdict`] is a loud error — "accept" is a deliberate,
/// enumerated choice, never a silent fall-through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// Compile-affecting and shapes the `rustc` argv **now** (P3.2a).
    CompileArgv,
    /// Compile-affecting via env vars / extra inputs / extern-aliasing — accepted + captured now,
    /// its argv/action effect lands in **P3.2b** (argv-inert here, by design).
    CompileEnv,
    /// Accepted + captured into the `LoadedTarget`; semantics DELEGATED to a later named step
    /// (`proc_macro_deps`→P4.1, `target_compatible_with`→P3.4, `link_deps` `DEP_*`→P4.5).
    /// Argv-inert until then — a regression test pins that so it can't silently stay a no-op.
    Delegated,
    /// Recorded parity deviation: accepted, never affects the action graph (a Bazel-only concern).
    Ignored,
}

/// §5.5 verdict table. `name`/`srcs`/`deps`/`edition` are bound as named params (they never reach
/// `**kwargs`), so they're handled by the signature, not here.
fn rust_attr_verdict(attr: &str) -> Option<Verdict> {
    Some(match attr {
        // compile-affecting NOW — shape the rustc argv (P3.2a)
        "crate_name" | "crate_root" | "crate_features" | "rustc_flags" => Verdict::CompileArgv,
        // compile-affecting via env/inputs/aliasing — accepted now, argv effect in P3.2b
        "rustc_env" | "rustc_env_files" | "rustc_env_file" | "compile_data" | "version"
        | "pkg_name" | "aliases" => Verdict::CompileEnv,
        // accepted + recorded, semantics delegated to the named step (argv-inert)
        "proc_macro_deps" => Verdict::Delegated, // → P4.1
        "target_compatible_with" => Verdict::Delegated, // → P3.4
        "link_deps" => Verdict::Delegated,       // → P4.5
        // ignored — recorded parity deviation
        "data" | "tags" | "visibility" => Verdict::Ignored,
        _ => return None,
    })
}

/// The compile-affecting attrs we extract so far, captured at DECLARE time (list attrs resolve at
/// analysis time — `crate_features`/`rustc_flags`/`compile_data` may be `select()`s).
/// `crate_name`/`crate_root` are scalars; a `select()` on either is not modeled (eager
/// `unpack_str`, `None` → the default). Grows per step: P3.2a = argv set; P3.2b = `compile_data`
/// (→ inputs); the env family (`rustc_env`/`version`/`pkg_name`/`rustc_env_files`) lands in P3.5.
#[derive(Default)]
struct CompileAttrs {
    crate_name: Option<String>,
    crate_root: Option<String>,
    crate_features: Vec<crate::values::StrAttrPart>,
    rustc_flags: Vec<crate::values::StrAttrPart>,
    compile_data: Vec<crate::values::StrAttrPart>,
    target_compatible_with: Vec<crate::values::StrAttrPart>,
    /// P3.8b: `data` — build-script RUN inputs (§5.2 action 2); only `cargo_build_script` fills it.
    data: Vec<crate::values::StrAttrPart>,
    /// P3.8c: build-script RUN env (§5.2/§6.2). `rustc_env_files` are env-file targets → `--env-file`
    /// (the `CARGO_PKG_*` from `cargo_toml_env_vars`, P3.5a); literal `version`/`pkg_name` → `--env
    /// CARGO_PKG_VERSION`/`NAME` which OVERRIDE the env-file (§6.2). `cargo_build_script` only.
    rustc_env_files: Vec<crate::values::StrAttrPart>,
    version: Option<String>,
    pkg_name: Option<String>,
}

/// Apply the §5.5 verdict table to a compile rule's extra `**kwargs`: **loud-error** on any attr
/// not in the table, then extract the compile-affecting set (`CompileArgv`/`CompileEnv`) that's
/// implemented so far. `Delegated`/`Ignored` are accepted no-ops — recorded via `capture_rule`,
/// never extracted. Accept (the verdict) is decoupled from implement (extraction grows per step —
/// P2#3); an accepted-but-not-yet-extracted attr (e.g. `rustc_env` → P3.5) is simply inert here.
/// `rule` names the rule for the error.
fn compile_attrs<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    rule: &str,
    name: &str,
    kw: &SmallMap<String, Value<'v>>,
) -> anyhow::Result<CompileAttrs> {
    let mut out = CompileAttrs::default();
    for (key, val) in kw.iter() {
        let Some(verdict) = rust_attr_verdict(key) else {
            anyhow::bail!(
                "{rule} `{name}`: unknown attribute `{key}` — not in the rust rule attr surface \
                 (§5.5). Add it to the verdict table with an explicit verdict (it must not be \
                 silently accepted)."
            );
        };
        match verdict {
            // Compile-affecting → extract the ones implemented so far; the rest stay inert.
            Verdict::CompileArgv | Verdict::CompileEnv => match key.as_str() {
                "crate_name" => out.crate_name = val.unpack_str().map(str::to_owned),
                "crate_root" => out.crate_root = val.unpack_str().map(str::to_owned),
                "crate_features" => {
                    out.crate_features = crate::values::str_attr_parts(eval, Some(*val))?
                }
                "rustc_flags" => {
                    out.rustc_flags = crate::values::str_attr_parts(eval, Some(*val))?
                }
                "compile_data" => {
                    out.compile_data = crate::values::str_attr_parts(eval, Some(*val))? // P3.2b → inputs
                }
                _ => {} // env family — accepted, extraction in P3.5 (argv/env-inert here)
            },
            // `target_compatible_with` semantics land HERE (P3.4); the other delegated attrs
            // (`proc_macro_deps` → P4.1, `link_deps` → P4.5) stay accepted + inert.
            Verdict::Delegated => {
                if key == "target_compatible_with" {
                    out.target_compatible_with = crate::values::str_attr_parts(eval, Some(*val))?;
                }
            }
            // Accepted + recorded (via `capture_rule`); never an action effect.
            Verdict::Ignored => {}
        }
    }
    Ok(out)
}

/// §5.2 verdict for a `cargo_build_script` attribute — the build-script attr surface, **distinct
/// from `rust_library`'s §5.5** and split by phase (compile = action 1, run = action 2). Same
/// accept-vs-loud-error discipline: an attr absent here is a loud error, never a silent pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BsVerdict {
    /// Compile (action 1) — shapes the host build-script bin's `rustc` argv NOW (P3.6).
    CompileArgv,
    /// Compile (action 1) — accepted + captured, effect deferred (`proc_macro_deps`→P4.1,
    /// `rustc_env`→env P3.x, `aliases`→extern rename). Argv-inert here.
    Compile,
    /// Run (action 2) — the build-script RUN env / inputs / links edge; semantics land in P3.8
    /// (`version`/`pkg_name`/`rustc_env_files`/`build_script_env`/`data`/`compile_data`/`tools`/
    /// `links`/`rundir`/`link_deps`). Argv-inert in the compile phase.
    Run,
    /// Bazel-only (`tags`/`visibility`) — never an action effect.
    Ignored,
}

/// §5.2 build-script verdict table. `name`/`srcs`/`deps`/`edition` are named params (they never
/// reach `**kwargs`), so they're handled by the signature, not here.
fn build_script_attr_verdict(attr: &str) -> Option<BsVerdict> {
    Some(match attr {
        // compile (action 1) — shapes the bin argv now (P3.6); `crate_features` is BOTH phases
        // (its run-env `CARGO_FEATURE_<F>` duty lands in P3.8).
        "crate_name" | "crate_root" | "crate_features" | "rustc_flags" => BsVerdict::CompileArgv,
        "proc_macro_deps" | "rustc_env" | "aliases" => BsVerdict::Compile,
        "version" | "pkg_name" | "rustc_env_files" | "build_script_env" | "data" | "compile_data"
        | "tools" | "links" | "rundir" | "link_deps" => BsVerdict::Run,
        "tags" | "visibility" => BsVerdict::Ignored,
        _ => return None,
    })
}

/// §5.2: validate a `cargo_build_script`'s extra `**kwargs` against the build-script attr surface
/// (loud-error on unknown) and extract the implemented attrs into the shared [`CompileAttrs`]:
/// compile-phase argv (`crate_name`/`crate_root`/`crate_features`/`rustc_flags`, P3.6) + run-phase
/// inputs (`data`/`compile_data`, P3.8b — staged into the run action). The remaining run-phase env
/// attrs (`version`/`pkg_name`/`rustc_env_files` → P3.8c) and the deferred-compile attrs are
/// accepted but inert here (accept ≠ implement, P2#3).
fn bs_attrs<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    name: &str,
    kw: &SmallMap<String, Value<'v>>,
) -> anyhow::Result<CompileAttrs> {
    let mut out = CompileAttrs::default();
    for (key, val) in kw.iter() {
        let Some(verdict) = build_script_attr_verdict(key) else {
            anyhow::bail!(
                "cargo_build_script `{name}`: unknown attribute `{key}` — not in the build-script \
                 attr surface (§5.2). Add it to the verdict table with an explicit phase verdict \
                 (it must not be silently accepted)."
            );
        };
        match verdict {
            BsVerdict::CompileArgv => match key.as_str() {
                "crate_name" => out.crate_name = val.unpack_str().map(str::to_owned),
                "crate_root" => out.crate_root = val.unpack_str().map(str::to_owned),
                "crate_features" => {
                    out.crate_features = crate::values::str_attr_parts(eval, Some(*val))?
                }
                "rustc_flags" => {
                    out.rustc_flags = crate::values::str_attr_parts(eval, Some(*val))?
                }
                _ => unreachable!("CompileArgv keys are exactly the four matched above"),
            },
            // P3.8b: `data`/`compile_data` → the run action's INPUTS (§5.2 action 2). P3.8c:
            // `rustc_env_files`/`version`/`pkg_name` → the run env (§6.2). The rest
            // (`links`/`build_script_env`/`tools`/`rundir`) stay accepted-but-inert.
            BsVerdict::Run => match key.as_str() {
                "data" => out.data = crate::values::str_attr_parts(eval, Some(*val))?,
                "compile_data" => out.compile_data = crate::values::str_attr_parts(eval, Some(*val))?,
                "rustc_env_files" => {
                    out.rustc_env_files = crate::values::str_attr_parts(eval, Some(*val))?
                }
                "version" => out.version = val.unpack_str().map(str::to_owned),
                "pkg_name" => out.pkg_name = val.unpack_str().map(str::to_owned),
                _ => {}
            },
            // `Compile` (deferred) / `Ignored` → accepted no-ops here; never extracted.
            BsVerdict::Compile | BsVerdict::Ignored => {}
        }
    }
    Ok(out)
}

/// Resolve the compile-argv extras at ANALYSIS time, returned SEPARATELY so each rule can POSITION
/// them in Bazel's order (RazelRustParityPlan B3): the feature cfgs go right after `--target` (before
/// `--edition`); `rustc_flags` go at the very end (after the externs). rules_rust emits each feature
/// cfg as TWO tokens — `--cfg` then `feature="x"` (NOT joined `--cfg=feature="x"`) — so the argv
/// matches token-for-token. (`crate_name`/`crate_root` overrides are applied inline by each rule.)
fn compile_extras<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    compile: &CompileAttrs,
) -> anyhow::Result<(Vec<String>, Vec<String>)> {
    let mut cfgs = Vec::new();
    for f in crate::values::resolve_str_parts(eval, &compile.crate_features)? {
        cfgs.push("--cfg".into());
        cfgs.push(format!("feature=\"{f}\""));
    }
    let flags = crate::values::resolve_str_parts(eval, &compile.rustc_flags)?;
    Ok((cfgs, flags))
}

/// Back-compat flat tail (feature cfgs then `rustc_flags`) for rules whose faithful argv ORDER is not
/// yet gated (e.g. the lean `rust_binary`). Faithful rules (`rust_library`, `cargo_build_script`) use
/// [`compile_extras`] directly to position the parts per Bazel.
fn compile_tail<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    compile: &CompileAttrs,
) -> anyhow::Result<Vec<String>> {
    let (mut tail, flags) = compile_extras(eval, compile)?;
    tail.extend(flags);
    Ok(tail)
}

/// P3.2b: resolve `compile_data` to the rustc action's extra INPUTS at analysis time. Each entry
/// resolves through [`resolve_dep`] — a source file → its path, a target (e.g. a generated file or
/// a build-script output) → its outputs — so data files are staged in the sandbox at compile time.
fn data_inputs<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    compile: &CompileAttrs,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in crate::values::resolve_str_parts(eval, &compile.compile_data)? {
        out.extend(resolve_dep(eval, &entry)?.libs);
    }
    Ok(out)
}

/// P3.4 (§5.4): is the target INCOMPATIBLE with the configured platform? Compatible iff EVERY
/// `target_compatible_with` constraint holds (`condition_matches` == `Some(true)`); an empty list
/// is compatible. `@platforms//:incompatible` never holds (so it forces incompatibility), and any
/// unsatisfied/unresolved constraint is incompatible — never a silent pass.
fn is_incompatible(sess: &crate::state::Session, constraints: &[String]) -> anyhow::Result<bool> {
    for c in constraints {
        let canon = canon_label(sess, c);
        if crate::selects::condition_matches(sess, &canon, true, 32)? != Some(true) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// P3.5: the absolute disk path of `<target's package>/<src>`, resolved like `resolve_dep`'s file
/// path — `@repo//pkg:name` → the vendored external dir (`external_repo_dirs`), `//pkg:name` → the
/// workspace root. (Handles the `@@` canonical form via `trim_start_matches('@')`.)
fn pkg_file_abs(
    sess: &crate::state::Session,
    target_canon: &str,
    src: &str,
) -> Option<std::path::PathBuf> {
    let trimmed = target_canon.trim_start_matches('@');
    let (repo, rest) = match trimmed.split_once("//") {
        Some((r, rest)) if !r.is_empty() => (Some(r), rest),
        _ => (None, trimmed.trim_start_matches("//")),
    };
    let pkg = rest.split_once(':').map(|(p, _)| p).unwrap_or(rest);
    let join_pkg = |root: &std::path::Path| {
        if pkg.is_empty() { root.join(src) } else { root.join(pkg).join(src) }
    };
    match repo {
        Some(repo) => sess
            .global
            .external_repo_dirs(repo)
            .into_iter()
            .map(|d| join_pkg(&d))
            .find(|p| crate::state::path_is_file(sess, p)),
        None => {
            let p = join_pkg(sess.workspace.as_ref()?);
            crate::state::path_is_file(sess, &p).then_some(p)
        }
    }
}

/// P3.5 (§6.2): parse a `Cargo.toml`'s `[package]` table (a minimal flat `key = value` scan — no
/// `toml` dep) into the `CARGO_PKG_*` env-file body (newline `KEY=VALUE`). Always emits NAME,
/// VERSION, and the four VERSION parts (Cargo's contract); the optional scalar fields only when
/// present. Array values (`authors`) join with `:` (Cargo's `CARGO_PKG_AUTHORS` separator).
fn cargo_pkg_env_content(toml: &str) -> String {
    let mut pkg = std::collections::BTreeMap::new();
    let mut in_pkg = false;
    for raw in toml.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_pkg = line == "[package]";
            continue;
        }
        if !in_pkg || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim();
            let val = if let Some(inner) = v.strip_prefix('[') {
                inner
                    .trim_end_matches(']')
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(":")
            } else {
                v.trim_matches('"').to_string()
            };
            pkg.insert(k.trim().to_string(), val);
        }
    }
    let get = |k: &str| pkg.get(k).cloned().unwrap_or_default();
    let version = get("version");
    let (core, pre) = version.split_once('-').unwrap_or((version.as_str(), ""));
    let mut parts = core.split('.');
    let mut lines = vec![
        format!("CARGO_PKG_NAME={}", get("name")),
        format!("CARGO_PKG_VERSION={version}"),
        format!("CARGO_PKG_VERSION_MAJOR={}", parts.next().unwrap_or("")),
        format!("CARGO_PKG_VERSION_MINOR={}", parts.next().unwrap_or("")),
        format!("CARGO_PKG_VERSION_PATCH={}", parts.next().unwrap_or("")),
        format!("CARGO_PKG_VERSION_PRE={pre}"),
    ];
    for (key, var) in [
        ("authors", "CARGO_PKG_AUTHORS"),
        ("description", "CARGO_PKG_DESCRIPTION"),
        ("homepage", "CARGO_PKG_HOMEPAGE"),
        ("repository", "CARGO_PKG_REPOSITORY"),
        ("license", "CARGO_PKG_LICENSE"),
        ("license-file", "CARGO_PKG_LICENSE_FILE"),
        ("rust-version", "CARGO_PKG_RUST_VERSION"),
        ("readme", "CARGO_PKG_README"),
    ] {
        if let Some(v) = pkg.get(key) {
            lines.push(format!("{var}={v}"));
        }
    }
    lines.join("\n") + "\n"
}

#[starlark::starlark_module]
fn rust_rules(b: &mut GlobalsBuilder) {
    /// `rust_library(name, srcs, deps=[], edition="2021")` → one `rustc` action
    /// compiling `srcs[0]` to `lib<name>.rlib`, exported to dependents.
    fn native_rust_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // E0c: record now, analyze in the demand-driven pass (forward refs resolve).
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_library", &[("srcs", srcs), ("deps", deps)], &_kw);
        let compile = compile_attrs(eval, "rust_library", &name, &_kw)?; // P3.2a §5.5 verdict
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        // P3.4: target_compatible_with — incompatible on this platform → NO actions, flagged.
        let tcw = crate::values::resolve_str_parts(eval, &compile.target_compatible_with)?;
        {
            let sess = session(eval);
            if is_incompatible(sess, &tcw)? {
                let nm = canon_label(sess, &name);
                sess.incompatible_targets.borrow_mut().insert(nm.clone());
                record_target(sess, AnalyzedTarget { name: nm, ..Default::default() });
                return Ok(());
            }
        }
        let srcs = crate::values::resolve_str_parts(eval, &srcs)?;
        let deps = crate::values::resolve_str_parts(eval, &deps)?;
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        // P3.2a: `crate_root` attr (if set) picks the root src; else srcs[0].
        let crate_root = match &compile.crate_root {
            Some(cr) => qualify(sess, cr),
            None => srcs
                .first()
                .ok_or_else(|| anyhow::anyhow!("rust_library `{name}` needs at least one src"))?
                .clone(),
        };
        let edition = edition.unwrap_or_else(|| "2021".into());
        let crate_name = compile.crate_name.clone().unwrap_or_else(|| name.clone());
        let (extern_flags, dep_rlibs, dep_names, build_scripts) = extern_args(eval, deps.clone())?;
        let (feature_cfgs, rustc_flags) = compile_extras(eval, &compile)?;
        let data = data_inputs(eval, &compile)?; // P3.2b: compile_data → inputs
        let sess = session(eval);

        // RazelRustParityPlan A3/A5: rules_rust's faithful rustc invocation — `--flag=value` syntax,
        // the `--out-dir`+`--codegen=extra-filename/metadata` hashed-output model (→
        // `lib<name>-<hash>.rlib`), in Bazel's argv ORDER. The toolchain/link deviations
        // (`--sysroot`/`-L`/`--remap-path-prefix`) are NOT emitted (razel's system rustc) — the
        // parity diff filters them on both sides.
        let hash = metadata_hash(&canon_label(sess, &name));
        let rlib = out_path(sess, &format!("lib{crate_name}-{hash}.rlib"));
        let mut argv = vec![
            rustc(),
            crate_root,
            format!("--crate-name={crate_name}"),
            "--crate-type=rlib".into(),
            "--error-format=human".into(),
            format!("--codegen=metadata=-{hash}"),
            format!("--codegen=extra-filename=-{hash}"),
            format!("--out-dir={}", out_dir(sess)),
            "--codegen=opt-level=0".into(),
            "--codegen=debuginfo=0".into(),
            "--codegen=strip=none".into(),
            "--emit=dep-info,link".into(),
            "--color=always".into(),
            format!("--target={}", crate::state::host_triple()),
        ];
        argv.extend(feature_cfgs); // B3: `--cfg feature="x"` right after --target (Bazel's order)
        argv.push(format!("--edition={edition}"));
        argv.push("-Cembed-bitcode=no".into());
        argv.extend(extern_flags); // build-script dep is the edge, not an --extern
        argv.extend(rustc_flags); // `rustc_flags` (e.g. --cap-lints) at the end (Bazel's order)
        // P3.10 (§4.3): a build-script dep routes this rustc through the process wrapper.
        let (argv, bs_inputs) = apply_build_script_edge("rust_library", &name, argv, &build_scripts)?;

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
        inputs.extend(bs_inputs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![rlib.clone()],
            }],
            default_info: vec![rlib],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_binary(name, srcs, deps=[], edition="2021")` → one `rustc` action
    /// compiling `srcs[0]` to the `<name>` executable, linking dep rlibs.
    fn native_rust_binary<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // E0c: record now, analyze in the demand-driven pass (forward refs resolve).
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_binary", &[("srcs", srcs), ("deps", deps)], &_kw);
        let compile = compile_attrs(eval, "rust_binary", &name, &_kw)?; // P3.2a §5.5 verdict
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        // P3.4: target_compatible_with — incompatible on this platform → NO actions, flagged.
        let tcw = crate::values::resolve_str_parts(eval, &compile.target_compatible_with)?;
        {
            let sess = session(eval);
            if is_incompatible(sess, &tcw)? {
                let nm = canon_label(sess, &name);
                sess.incompatible_targets.borrow_mut().insert(nm.clone());
                record_target(sess, AnalyzedTarget { name: nm, ..Default::default() });
                return Ok(());
            }
        }
        let srcs = crate::values::resolve_str_parts(eval, &srcs)?;
        let deps = crate::values::resolve_str_parts(eval, &deps)?;
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        // P3.2a: `crate_root` attr (if set) picks the root src; else srcs[0].
        let crate_root = match &compile.crate_root {
            Some(cr) => qualify(sess, cr),
            None => srcs
                .first()
                .ok_or_else(|| anyhow::anyhow!("rust_binary `{name}` needs at least one src"))?
                .clone(),
        };
        let edition = edition.unwrap_or_else(|| "2021".into());
        let crate_name = compile.crate_name.clone().unwrap_or_else(|| name.clone());
        let (extern_flags, dep_rlibs, dep_names, build_scripts) = extern_args(eval, deps.clone())?;
        let tail = compile_tail(eval, &compile)?;
        let data = data_inputs(eval, &compile)?; // P3.2b: compile_data → inputs
        let sess = session(eval);

        let out = out_path(sess, &name);
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-name".into(),
            crate_name,
            crate_root,
            "-o".into(),
            out.clone(),
        ];
        argv.extend(extern_flags);
        argv.extend(tail);
        // P3.10 (§4.3): a build-script dep routes this rustc through the process wrapper.
        let (argv, bs_inputs) = apply_build_script_edge("rust_binary", &name, argv, &build_scripts)?;

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
        inputs.extend(bs_inputs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![out.clone()],
            }],
            default_info: vec![out],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_shared_library(name, srcs, deps=[], edition=…)` → one rustc cdylib
    /// action producing `lib<name>.dylib` (host posture: macOS suffix).
    fn native_rust_shared_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let (srcs, deps) = (unpack_strs(srcs), unpack_strs(deps));
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let sess = session(eval);
        let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
        let crate_root = srcs
            .first()
            .ok_or_else(|| anyhow::anyhow!("rust_shared_library `{name}` needs at least one src"))?
            .clone();
        let edition = edition.unwrap_or_else(|| "2021".into());
        // rust_shared_library: build-script edge unused for slice-1 (blake3 is a rust_library).
        let (extern_flags, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone())?;
        let sess = session(eval);
        let dylib = out_path(sess, &format!("lib{name}.dylib"));
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-type".into(),
            "cdylib".into(),
            "--crate-name".into(),
            name.clone(),
            crate_root,
            "-o".into(),
            dylib.clone(),
        ];
        argv.extend(extern_flags);
        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![dylib.clone()],
            }],
            default_info: vec![dylib],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_library_group(name, deps)` — faithful GROUPING rule: no actions,
    /// DefaultInfo = the deps' rlibs (rules_rust's lib-collection shape).
    fn native_rust_library_group<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] deps: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let deps = unpack_strs(deps);
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
        let (_, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone())?;
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: Vec::new(),
            default_info: dep_rlibs,
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_doc(name, crate, …)` — ANALYSIS-ONLY today (named hole: the rustdoc
    /// action lands when a consumer demands the docs, not just the load).
    fn native_rust_doc<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: Vec::new(),
            actions: Vec::new(),
            default_info: Vec::new(),
            providers: Default::default(),
        });
        Ok(NoneType)
    }

    /// `cargo_build_script(name, srcs, deps=[], edition="2021", **attrs)` — the build-script's TWO
    /// actions (§5.2): **(1) compile** `crate_root`/`srcs[0]` → a HOST `rust_binary` (`<name>_`, the
    /// §12 `:_bs_` bin) with the single toolchain, linking `deps` as `--extern` (build-deps, P3.6);
    /// **(2) run** the bin via the process wrapper → the §6.1 `<name>.out` flags file + `OUT_DIR`
    /// tree, under a default-deny Cargo env the wrapper assembles (P3.8). The run env POLICY is
    /// staged: P3.8b emits `TARGET`/`HOST`/`OPT_LEVEL`/`CARGO_FEATURE_<F>`; the env-file
    /// (`CARGO_PKG_*`) + `CARGO_CFG_*`/cc/`DEP_*` follow. `default_info` is EMPTY (§4.3: a
    /// build-script target exposes no libs — the bin is intra-target, the flags-file/`OUT_DIR` are
    /// consumed via the P3.10 edge).
    fn native_cargo_build_script<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let compile = bs_attrs(eval, &name, &kw)?; // §5.2 verdict + compile/run attr split
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            let srcs = crate::values::resolve_str_parts(eval, &srcs)?;
            let deps = crate::values::resolve_str_parts(eval, &deps)?;
            let sess = session(eval);
            let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
            // `crate_root` (if set) is the build.rs root; else srcs[0].
            let crate_root = match &compile.crate_root {
                Some(cr) => qualify(sess, cr),
                None => srcs
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("cargo_build_script `{name}` needs a build-script src"))?
                    .clone(),
            };
            let edition = edition.unwrap_or_else(|| "2021".into());
            let crate_name = compile.crate_name.clone().unwrap_or_else(|| name.clone());
            // All the `&mut eval` resolutions up front (compile argv + run env/inputs), before
            // re-taking `sess` for the path `qualify`s + `record_target`. (Build-deps of the bs bin
            // are normal crates, not build scripts → `_bs` edge unused here.)
            let (extern_flags, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone())?;
            let (feature_cfgs, rustc_flags) = compile_extras(eval, &compile)?;
            let features = crate::values::resolve_str_parts(eval, &compile.crate_features)?;
            // §5.2 slice-1 run inputs: declared `data` + `compile_data`, resolved to files.
            let mut data_files = Vec::new();
            for parts in [&compile.data, &compile.compile_data] {
                for entry in crate::values::resolve_str_parts(eval, parts)? {
                    data_files.extend(resolve_dep(eval, &entry)?.libs);
                }
            }
            // P3.8c: `rustc_env_files` are env-file TARGETS (e.g. `cargo_toml_env_vars`, P3.5a) →
            // their output file(s); passed `--env-file` (and staged as run inputs).
            let mut env_files = Vec::new();
            for entry in crate::values::resolve_str_parts(eval, &compile.rustc_env_files)? {
                env_files.extend(resolve_dep(eval, &entry)?.libs);
            }
            let sess = session(eval);

            // --- action 1: compile the host build-script bin (`<name>_`, §12 `:_bs_`) ---
            // RazelRustParityPlan A3: rules_rust's faithful bin compile (exec config) — `bin`
            // crate-type, `opt-level=3`/`strip=debuginfo` (exec), `--emit=link=<bin>`+`--emit=dep-info`
            // (no metadata/extra-filename → unhashed bin name), in Bazel's argv order. The cc-toolchain
            // link flags + `--sysroot`/`-L` are deviations razel doesn't emit (the diff filters them).
            let bin = out_path(sess, &format!("{name}_"));
            let dsym = out_path(sess, &format!("{name}_.dSYM")); // macOS debug-symbols tree output
            let mut compile_argv = vec![
                rustc(),
                crate_root,
                format!("--crate-name={crate_name}"),
                "--crate-type=bin".into(),
                "--error-format=human".into(),
                format!("--out-dir={}", out_dir(sess)),
                "--codegen=opt-level=3".into(),
                "--codegen=debuginfo=0".into(),
                "--codegen=strip=debuginfo".into(),
                format!("--emit=link={bin}"),
                "--emit=dep-info".into(),
                "--color=always".into(),
                format!("--target={}", crate::state::host_triple()),
            ];
            compile_argv.extend(feature_cfgs); // B3: feature cfgs after --target (Bazel's order)
            compile_argv.push(format!("--edition={edition}"));
            compile_argv.push("-Cembed-bitcode=no".into());
            compile_argv.extend(extern_flags);
            compile_argv.extend(rustc_flags); // rustc_flags (e.g. --cap-lints) at the end
            let mut compile_inputs = srcs.clone();
            compile_inputs.extend(dep_rlibs);

            // --- action 2: run the bin via the wrapper → §6.1 flags file + OUT_DIR tree ---
            let flags_out = out_path(sess, &format!("{name}.out")); // §6.1 `<name>.out`
            let out_dir = out_path(sess, &format!("{name}.out_dir")); // P2.4 tree output
            let triple = crate::state::host_triple();
            let mut run_argv = vec![
                process_wrapper(),
                "build-script".into(),
                "--flags-out".into(),
                flags_out.clone(),
                "--out-dir".into(),
                out_dir.clone(),
            ];
            // P3.8c: env-files FIRST (lower precedence than the literal `--env` below, §6.2).
            for ef in &env_files {
                run_argv.push("--env-file".into());
                run_argv.push(ef.clone());
            }
            // Cargo env POLICY (P3.8b): host==target triple, a default OPT_LEVEL, one
            // `CARGO_FEATURE_<F>` per feature (uppercased, non-alnum → `_`). P3.8c: literal
            // `version`/`pkg_name` → `CARGO_PKG_*` (OVERRIDE the env-file). `CARGO_CFG_*`/cc/`DEP_*`
            // are later. (`--env` is applied AFTER `--env-file` by the wrapper, hence the override.)
            for (k, v) in [("TARGET", triple), ("HOST", triple), ("OPT_LEVEL", "0")] {
                run_argv.push("--env".into());
                run_argv.push(format!("{k}={v}"));
            }
            for f in &features {
                let var = f.to_uppercase().replace(|c: char| !c.is_ascii_alphanumeric(), "_");
                run_argv.push("--env".into());
                run_argv.push(format!("CARGO_FEATURE_{var}=1"));
            }
            // P3.8d: the `CARGO_CFG_*` set cargo derives from the (host==target) triple.
            for (k, v) in cargo_cfg_env(triple) {
                run_argv.push("--env".into());
                run_argv.push(format!("{k}={v}"));
            }
            if let Some(v) = &compile.version {
                run_argv.push("--env".into());
                run_argv.push(format!("CARGO_PKG_VERSION={v}"));
            }
            if let Some(p) = &compile.pkg_name {
                run_argv.push("--env".into());
                run_argv.push(format!("CARGO_PKG_NAME={p}"));
            }
            run_argv.push("--".into());
            run_argv.push(bin.clone());
            let mut run_inputs = vec![bin.clone()];
            run_inputs.extend(srcs);
            run_inputs.extend(data_files);
            run_inputs.extend(env_files);

            let mut t = AnalyzedTarget {
                name: canon_label(sess, &name),
                deps: dep_names,
                actions: vec![
                    AnalyzedAction {
                        mnemonic: "Rustc".into(),
                        argv: compile_argv,
                        inputs: compile_inputs,
                        outputs: vec![bin.clone(), dsym],
                    },
                    AnalyzedAction {
                        mnemonic: "CargoBuildScriptRun".into(),
                        argv: run_argv,
                        inputs: run_inputs,
                        outputs: vec![flags_out.clone(), out_dir.clone()],
                    },
                ],
                default_info: Vec::new(),
                providers: Default::default(),
            };
            // P3.10 (§4.3): expose the run action's flags-file + OUT_DIR as the OWN-only
            // `BuildScriptRun` edge (no `dep_fold` — never propagates); the crate's `rust_library`
            // reads it via `resolve_dep(...).build_script` and routes its rustc through the wrapper.
            t.set_set("BuildScriptRun", "flags_file", vec![flags_out]);
            t.set_set("BuildScriptRun", "out_dir", vec![out_dir]);
            record_target(sess, t);
            Ok(())
        }))?;
        Ok(NoneType)
    }

    /// P3.1: `cargo_toml_env_vars` load surface — a STUB target for now; the `CARGO_PKG_*` env-file
    /// it emits lands in P3.5.
    /// P3.5 (§6.2): `cargo_toml_env_vars(name, src="Cargo.toml")` emits a `CARGO_PKG_*` env-file
    /// from the crate's `Cargo.toml` via a `FileWrite` action (content baked at analysis — so it's
    /// the action's cache key; no `Cargo.toml` runtime input). The rustc wrapper consumes it through
    /// `--env-file=` (P3.9); precedence over literal `rustc_env`/`version`/`pkg_name` lands there.
    fn native_cargo_toml_env_vars<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] src: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let src = src.unwrap_or_else(|| "Cargo.toml".into());
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            let sess = session(eval);
            let canon = canon_label(sess, &name);
            let toml = pkg_file_abs(sess, &canon, &src)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .ok_or_else(|| anyhow::anyhow!("cargo_toml_env_vars `{name}`: cannot read `{src}`"))?;
            let content = cargo_pkg_env_content(&toml);
            let out = out_path(sess, &name);
            let script = format!(
                "printf '%s' {} > {}",
                crate::values::shquote(&content),
                crate::values::shquote(&out)
            );
            record_target(sess, AnalyzedTarget {
                name: canon,
                actions: vec![AnalyzedAction {
                    mnemonic: "FileWrite".into(),
                    argv: vec!["/bin/sh".into(), "-c".into(), script],
                    inputs: Vec::new(),
                    outputs: vec![out.clone()],
                }],
                default_info: vec![out],
                ..Default::default()
            });
            Ok(())
        }))?;
        Ok(NoneType)
    }
}

/// The synthetic `@rules_rust` module: re-exports the native rules under the names real BUILD
/// files `load()` (`rust_binary`, `rust_library`, the `cargo:defs.bzl` natives), plus the
/// `crate_universe/private:selects.bzl` `selects` namespace. Every `@rules_rust//…` load routes
/// here (`ruleset_modules` prefix), so one module serves `rust:defs.bzl`, `cargo:defs.bzl`, and
/// `selects.bzl` alike.
///
/// `selects` is a faithful pure-Starlark port of rules_rust's vendored skylib `selects.bzl`:
/// `with_or`/`with_or_dict` fan tuple keys out to one `select()` arm each. `config_setting_group`
/// (P3.1b) creates `config_setting` targets — deferred with a loud error until a crate in scope
/// needs it (cf. the skylib lib-helper policy in `shims.rs`); blake3 uses bare `select()`.
const RUST_RULES_BZL: &str = r#"
rust_binary = native_rust_binary
rust_library = native_rust_library
rust_shared_library = native_rust_shared_library
rust_library_group = native_rust_library_group
rust_doc = native_rust_doc
rust_doc_test = native_rust_doc
cargo_build_script = native_cargo_build_script
cargo_toml_env_vars = native_cargo_toml_env_vars

def _with_or_dict(input_dict, no_match_error = ""):
    output_dict = {}
    for (key_set, value) in input_dict.items():
        if type(key_set) == type(()):
            for key in key_set:
                if key in output_dict:
                    fail("key " + str(key) + " is used multiple times in " + str(input_dict))
                output_dict[key] = value
        else:
            if key_set in output_dict:
                fail("key " + str(key_set) + " is used multiple times in " + str(input_dict))
            output_dict[key_set] = value
    return output_dict

def _with_or(input_dict, no_match_error = ""):
    return select(_with_or_dict(input_dict, no_match_error), no_match_error = no_match_error)

def _config_setting_group(**kwargs):
    fail("selects.config_setting_group is not yet modeled in razel (no crate in scope needs it; lands when one does)")

selects = struct(
    with_or = _with_or,
    with_or_dict = _with_or_dict,
    config_setting_group = _config_setting_group,
)
"#;

pub(crate) fn module() -> Result<FrozenModule, String> {
    // StructType for `selects = struct(...)`; `rule_globals` for `select()` (used by `with_or`).
    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(crate::dialect::rule_globals)
        .with(rust_rules)
        .build();
    Module::with_temp_heap(|module| {
        let ast = AstModule::parse("@rules_rust", RUST_RULES_BZL.to_owned(), &Dialect::Extended)
            .map_err(|e| format!("{e}"))?;
        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{e:?}"))
    })
}

#[cfg(test)]
mod tests {
    //! P3.2a §5.5 verdict-table gate. Analysis-only (no rustc): asserts on the captured Rustc
    //! argv, never executes — so it runs without a rust toolchain.
    use super::cargo_cfg_env;
    use crate::rules::analyze_workspace_with;
    use crate::state::{AnalyzedTarget, GlobalFlags};

    const LOAD: &str = "load(\"@rules_rust//rust:defs.bzl\", \"rust_library\")\n";

    /// Analyze a one-package workspace whose `app/BUILD` is `build`; `tag` keeps the temp dir
    /// unique across tests sharing this process id.
    fn analyze(tag: &str, build: &str) -> Result<Vec<AnalyzedTarget>, String> {
        let tmp = std::env::temp_dir().join(format!("razel-p32-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pkg = tmp.join("app");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(pkg.join("lib.rs"), "pub fn x() {}\n").unwrap();
        std::fs::write(pkg.join("root.rs"), "pub fn y() {}\n").unwrap();
        std::fs::write(pkg.join("table.bin"), "data\n").unwrap();
        std::fs::write(pkg.join("host.txt"), "h\n").unwrap();
        std::fs::write(pkg.join("default.txt"), "d\n").unwrap();
        // P3.8c: a `Cargo.toml` so a `cargo_toml_env_vars` target (the `rustc_env_files` source)
        // can resolve; inert for tests that don't declare one.
        std::fs::write(pkg.join("Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(pkg.join("BUILD"), build).unwrap();
        let r = analyze_workspace_with(&tmp, "//app:t", GlobalFlags::default());
        let _ = std::fs::remove_dir_all(&tmp);
        r
    }

    /// The Rustc action of `//app:t`.
    fn action_of(targets: &[AnalyzedTarget]) -> &crate::state::AnalyzedAction {
        targets
            .iter()
            .find(|t| t.name == "//app:t")
            .expect("//app:t analyzed")
            .actions
            .first()
            .expect("a Rustc action")
    }
    fn argv_of(targets: &[AnalyzedTarget]) -> Vec<String> {
        action_of(targets).argv.clone()
    }
    fn inputs_of(targets: &[AnalyzedTarget]) -> Vec<String> {
        action_of(targets).inputs.clone()
    }

    #[test]
    fn p32_compile_affecting_attrs_shape_the_argv() {
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\", \"root.rs\"], \
             crate_name = \"custom\", crate_root = \"root.rs\", \
             crate_features = [\"alpha\", \"beta\"], rustc_flags = [\"-Cdebuginfo=0\"])\n"
        );
        let argv = argv_of(&analyze("compile", &build).unwrap());
        // crate_name overrides the default (`t`) — rules_rust's `--crate-name=<n>` joined form (A3).
        assert!(argv.contains(&"--crate-name=custom".to_string()), "crate_name override: {argv:?}");
        // crate_root picks root.rs as the positional (not srcs[0] = lib.rs).
        assert!(argv.contains(&"app/root.rs".to_string()), "crate_root is the positional: {argv:?}");
        assert!(!argv.contains(&"app/lib.rs".to_string()), "lib.rs is not the root: {argv:?}");
        // crate_features → `--cfg` `feature="x"` (TWO tokens, rules_rust's form — B3), right after --target.
        assert!(argv.windows(2).any(|w| w == ["--cfg", "feature=\"alpha\""]), "{argv:?}");
        assert!(argv.windows(2).any(|w| w == ["--cfg", "feature=\"beta\""]), "{argv:?}");
        // rustc_flags appended verbatim (at the end).
        assert!(argv.contains(&"-Cdebuginfo=0".to_string()), "{argv:?}");
    }

    #[test]
    fn p32_delegated_and_ignored_attrs_are_accepted_but_argv_inert() {
        // The regression pin: every delegated/ignored attr is accepted (no error) AND must leave
        // the argv byte-identical — so a delegation can't silently turn into a no-op effect.
        let base = format!("{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"])\n");
        let with_extra = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], \
             proc_macro_deps = [], target_compatible_with = [], link_deps = [], \
             data = [], tags = [\"manual\"], visibility = [\"//visibility:public\"])\n"
        );
        let base_argv = argv_of(&analyze("inert_base", &base).unwrap());
        let extra_argv = argv_of(&analyze("inert_extra", &with_extra).unwrap());
        assert_eq!(base_argv, extra_argv, "delegated/ignored attrs must be argv-inert in P3.2a");
    }

    #[test]
    fn p32b_compile_data_is_a_compile_input_not_argv() {
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], compile_data = [\"table.bin\"])\n"
        );
        let targets = analyze("compile_data", &build).unwrap();
        // compile_data → a compile-time INPUT (staged in the sandbox).
        assert!(
            inputs_of(&targets).contains(&"app/table.bin".to_string()),
            "compile_data is a compile input: {:?}",
            inputs_of(&targets)
        );
        // …and it shapes inputs only — never an argv token (not a flag, not the crate root).
        assert!(
            !argv_of(&targets).contains(&"app/table.bin".to_string()),
            "compile_data is not an argv token: {:?}",
            argv_of(&targets)
        );
    }

    #[test]
    fn p33_host_platform_triple_resolves_the_select_arm() {
        // P3.3: a `select()` keyed on the HOST `@rules_rust//rust/platform:<triple>` picks that arm
        // (resolved from the host, no @rules_rust vendoring). Observed through P3.2b's compile_data.
        let host = crate::state::host_triple();
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], compile_data = select({{\
             \"@rules_rust//rust/platform:{host}\": [\"host.txt\"], \
             \"//conditions:default\": [\"default.txt\"]}}))\n"
        );
        let inputs = inputs_of(&analyze("p33host", &build).unwrap());
        assert!(inputs.contains(&"app/host.txt".to_string()), "host-triple arm wins: {inputs:?}");
        assert!(!inputs.contains(&"app/default.txt".to_string()), "default not taken: {inputs:?}");
    }

    #[test]
    fn p33_non_host_platform_triple_falls_to_default() {
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], compile_data = select({{\
             \"@rules_rust//rust/platform:some-other-triple\": [\"host.txt\"], \
             \"//conditions:default\": [\"default.txt\"]}}))\n"
        );
        let inputs = inputs_of(&analyze("p33other", &build).unwrap());
        assert!(inputs.contains(&"app/default.txt".to_string()), "non-host → default: {inputs:?}");
        assert!(!inputs.contains(&"app/host.txt".to_string()), "non-host arm not taken: {inputs:?}");
    }

    #[test]
    fn p33_platforms_os_constraint_resolves_from_host() {
        // `@platforms//os:<host-os>` matches; a foreign os → default. (host_constraint_matches.)
        let host_os = match std::env::consts::OS {
            "macos" => "osx",
            os => os,
        };
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], compile_data = select({{\
             \"@platforms//os:{host_os}\": [\"host.txt\"], \
             \"//conditions:default\": [\"default.txt\"]}}))\n"
        );
        let inputs = inputs_of(&analyze("p33os", &build).unwrap());
        assert!(inputs.contains(&"app/host.txt".to_string()), "host-os arm wins: {inputs:?}");
    }

    #[test]
    fn p34b_named_incompatible_target_is_a_loud_error() {
        // §5.4: an explicitly-named incompatible target is a loud error (not a silent no-op).
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], \
             target_compatible_with = [\"@platforms//:incompatible\"])\n"
        );
        let err = analyze("p34b_named", &build).unwrap_err();
        assert!(err.contains("incompatible"), "named incompatible → loud error: {err}");
    }

    #[test]
    fn p34b_incompatible_dep_of_compatible_target_errors() {
        // §5.4: a compatible target that pulls in an incompatible dep is a loud error (never a
        // silent drop). `t` (no constraints) deps on `lib` (@platforms//:incompatible).
        let build = format!(
            "{LOAD}rust_library(name = \"lib\", srcs = [\"lib.rs\"], \
             target_compatible_with = [\"@platforms//:incompatible\"])\n\
             rust_library(name = \"t\", srcs = [\"root.rs\"], deps = [\":lib\"])\n"
        );
        let err = analyze("p34b_dep", &build).unwrap_err();
        assert!(err.contains("incompatible"), "incompatible dep → loud error: {err}");
    }

    #[test]
    fn p34a_host_compatible_target_builds() {
        // blake3's pattern: select on the host triple → [] (compatible) → the Rustc action is present.
        let host = crate::state::host_triple();
        let build = format!(
            "{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], target_compatible_with = select({{\
             \"@rules_rust//rust/platform:{host}\": [], \
             \"//conditions:default\": [\"@platforms//:incompatible\"]}}))\n"
        );
        let targets = analyze("p34a_compat", &build).unwrap();
        let t = targets.iter().find(|t| t.name == "//app:t").expect("//app:t");
        assert!(!t.actions.is_empty(), "compatible target builds a Rustc action");
        assert!(t.actions[0].argv.iter().any(|a| a.starts_with("--crate-name=")), "{:?}", t.actions[0].argv);
    }

    #[test]
    fn p35a_cargo_toml_env_vars_emits_the_cargo_pkg_env_file() {
        let tmp = std::env::temp_dir().join(format!("razel-p35a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let pkg = tmp.join("c");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(
            pkg.join("Cargo.toml"),
            "[package]\nname = \"blake3\"\nversion = \"1.8.2\"\nlicense = \"CC0-1.0\"\n\
             edition = \"2021\"\n\n[dependencies]\narrayref = \"0.3\"\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "load(\"@rules_rust//cargo:defs.bzl\", \"cargo_toml_env_vars\")\n\
             cargo_toml_env_vars(name = \"env\", src = \"Cargo.toml\")\n",
        )
        .unwrap();
        let targets = analyze_workspace_with(&tmp, "//c:env", GlobalFlags::default()).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        let t = targets.iter().find(|t| t.name == "//c:env").expect("//c:env analyzed");
        let script = &t.actions.first().expect("a FileWrite action").argv[2];
        for want in [
            "CARGO_PKG_NAME=blake3",
            "CARGO_PKG_VERSION=1.8.2",
            "CARGO_PKG_VERSION_MAJOR=1",
            "CARGO_PKG_VERSION_MINOR=8",
            "CARGO_PKG_VERSION_PATCH=2",
            "CARGO_PKG_LICENSE=CC0-1.0",
        ] {
            assert!(script.contains(want), "env-file missing `{want}`: {script}");
        }
        // The `[dependencies]` table is not `[package]` — it must not leak into the env-file.
        assert!(!script.contains("arrayref"), "only the [package] table: {script}");
    }

    #[test]
    fn p32_unknown_attr_is_a_loud_error() {
        let build = format!("{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"], bogus_attr = 1)\n");
        let err = analyze("unknown", &build).unwrap_err();
        assert!(err.contains("unknown attribute"), "loud error: {err}");
        assert!(err.contains("bogus_attr"), "names the attr: {err}");
    }

    const BS_LOAD: &str = "load(\"@rules_rust//cargo:defs.bzl\", \"cargo_build_script\")\n";

    #[test]
    fn p36_build_script_compiles_to_a_host_bin_with_externs() {
        // The build-script target is named `t` so the `analyze`/`action_of` helpers (which key on
        // `//app:t`) observe ITS compile action; `:dep` is its sole build-dependency.
        let build = format!(
            "{LOAD}{BS_LOAD}\
             rust_library(name = \"dep\", srcs = [\"lib.rs\"])\n\
             cargo_build_script(name = \"t\", srcs = [\"root.rs\"], deps = [\":dep\"], \
                 crate_features = [\"std\"], rustc_flags = [\"-Cdebuginfo=0\"], \
                 version = \"1.2.3\", links = \"z\", data = [\"table.bin\"], tags = [\"manual\"])\n"
        );
        let targets = analyze("p36_bs", &build).unwrap();
        let argv = argv_of(&targets);
        // Compiles the build-script root → a HOST bin (`<name>_`, the §12 `:_bs_`) via rustc, with
        // rules_rust's faithful argv (A3): `--crate-type=bin`, `--emit=link=<bin>` (not `-o`).
        assert!(argv.first().is_some_and(|a| a.ends_with("rustc")), "rustc compile: {argv:?}");
        assert!(argv.contains(&"--crate-type=bin".to_string()), "bin crate-type: {argv:?}");
        assert!(argv.contains(&"--emit=link=app/t_".to_string()), "host build-script bin output: {argv:?}");
        // `deps` → `--extern=` (joined, build-deps link the host bin), NOT run inputs; rlib hashed.
        assert!(
            argv.iter().any(|a| a.starts_with("--extern=dep=app/libdep-")),
            "build-dep is an --extern (joined, hashed rlib): {argv:?}"
        );
        // `crate_root` picks root.rs as the positional; default crate_name = the target name.
        assert!(argv.contains(&"app/root.rs".to_string()), "crate_root positional: {argv:?}");
        assert!(argv.contains(&"--crate-name=t".to_string()), "default crate_name = name: {argv:?}");
        // `crate_features` → `--cfg` `feature="x"` (two tokens, compile phase, B3); `rustc_flags` verbatim.
        assert!(argv.windows(2).any(|w| w == ["--cfg", "feature=\"std\""]), "{argv:?}");
        assert!(argv.contains(&"-Cdebuginfo=0".to_string()), "{argv:?}");
        // Run-phase attrs (`version`/`links`/`data`) + ignored (`tags`) are ACCEPTED but argv-inert.
        assert!(
            !argv.iter().any(|a| a.contains("1.2.3") || a.contains("table.bin") || a == "z" || a == "manual"),
            "run-phase/ignored attrs are not compile argv: {argv:?}"
        );
        // §4.3: the build-script target exposes NO libs — a crate dep on it gets no `--extern`.
        let bs = targets.iter().find(|t| t.name == "//app:t").unwrap();
        assert!(bs.default_info.is_empty(), "build-script target has no default-info libs: {:?}", bs.default_info);
    }

    #[test]
    fn p36_build_script_unknown_attr_is_a_loud_error() {
        // The build-script surface is its OWN table (§5.2) — an attr outside it is a loud error.
        let build = format!(
            "{BS_LOAD}cargo_build_script(name = \"t\", srcs = [\"root.rs\"], not_a_bs_attr = 1)\n"
        );
        let err = analyze("p36_unknown", &build).unwrap_err();
        assert!(err.contains("unknown attribute") && err.contains("not_a_bs_attr"), "loud error: {err}");
        assert!(err.contains("build-script attr surface"), "names the surface: {err}");
    }

    #[test]
    fn p38b_build_script_run_action_invokes_the_wrapper_with_cargo_env() {
        let build = format!(
            "{LOAD}{BS_LOAD}\
             rust_library(name = \"dep\", srcs = [\"lib.rs\"])\n\
             cargo_build_script(name = \"t\", srcs = [\"root.rs\"], deps = [\":dep\"], \
                 crate_features = [\"std\", \"simd-asm\"], data = [\"table.bin\"])\n"
        );
        let targets = analyze("p38b_run", &build).unwrap();
        let t = targets.iter().find(|t| t.name == "//app:t").expect("bs target analyzed");
        // The target now carries BOTH §5.2 actions: compile (1) then run (2).
        assert_eq!(t.actions.len(), 2, "compile + run: {:?}",
            t.actions.iter().map(|a| a.mnemonic.clone()).collect::<Vec<_>>());
        assert_eq!(t.actions[0].mnemonic, "Rustc", "action 1 is the compile");
        let run = &t.actions[1];
        assert_eq!(run.mnemonic, "CargoBuildScriptRun", "action 2 is the run");
        let a = &run.argv;
        // argv = [wrapper, build-script, --flags-out F, --out-dir D, --env…, --, bin]
        assert!(a[0].contains("razel-process-wrapper"), "the wrapper bin: {a:?}");
        assert_eq!(a[1], "build-script", "the runner subcommand: {a:?}");
        let fo = a.iter().position(|x| x == "--flags-out").expect("--flags-out");
        assert_eq!(a[fo + 1], "app/t.out", "§6.1 <name>.out flags file: {a:?}");
        let od = a.iter().position(|x| x == "--out-dir").expect("--out-dir");
        assert_eq!(a[od + 1], "app/t.out_dir", "the OUT_DIR tree: {a:?}");
        // env POLICY slice (P3.8b): TARGET/HOST = host triple, OPT_LEVEL, CARGO_FEATURE_* / feature.
        let env_vals: Vec<String> =
            a.windows(2).filter(|w| w[0] == "--env").map(|w| w[1].clone()).collect();
        let triple = crate::state::host_triple();
        assert!(env_vals.contains(&format!("TARGET={triple}")), "TARGET=host triple: {env_vals:?}");
        assert!(env_vals.contains(&format!("HOST={triple}")), "HOST=host triple: {env_vals:?}");
        assert!(env_vals.iter().any(|e| e.starts_with("OPT_LEVEL=")), "{env_vals:?}");
        assert!(env_vals.contains(&"CARGO_FEATURE_STD=1".to_string()), "feature→CARGO_FEATURE_: {env_vals:?}");
        assert!(env_vals.contains(&"CARGO_FEATURE_SIMD_ASM=1".to_string()), "non-alnum→_: {env_vals:?}");
        // the compiled bs bin is the program after `--`.
        let sep = a.iter().position(|x| x == "--").expect("the `--` separator");
        assert_eq!(a[sep + 1], "app/t_", "the bs bin runs after --: {a:?}");
        // run inputs: bin + the build-script srcs + declared data (§5.2 slice-1 static keying).
        assert!(run.inputs.contains(&"app/t_".to_string()), "bin is a run input: {:?}", run.inputs);
        assert!(run.inputs.contains(&"app/root.rs".to_string()), "src is a run input: {:?}", run.inputs);
        assert!(run.inputs.contains(&"app/table.bin".to_string()), "data is a run input: {:?}", run.inputs);
        // outputs: flags file + OUT_DIR tree; default_info stays empty (§4.3 no libs).
        assert_eq!(run.outputs, ["app/t.out", "app/t.out_dir"], "run outputs: {:?}", run.outputs);
        assert!(t.default_info.is_empty(), "no default-info libs: {:?}", t.default_info);
    }

    #[test]
    fn p38c_run_env_wires_the_env_file_and_cargo_pkg_overrides() {
        // `rustc_env_files` → a `cargo_toml_env_vars` env-file target (P3.5a); literal version/
        // pkg_name → CARGO_PKG_* that OVERRIDE the env-file (§6.2).
        let build = format!(
            "{LOAD}{BS_LOAD}load(\"@rules_rust//cargo:defs.bzl\", \"cargo_toml_env_vars\")\n\
             cargo_toml_env_vars(name = \"cenv\", src = \"Cargo.toml\")\n\
             cargo_build_script(name = \"t\", srcs = [\"root.rs\"], \
                 rustc_env_files = [\":cenv\"], version = \"9.9.9\", pkg_name = \"pkgx\")\n"
        );
        let targets = analyze("p38c_env", &build).unwrap();
        let t = targets.iter().find(|t| t.name == "//app:t").expect("bs target analyzed");
        let run = &t.actions[1];
        let a = &run.argv;
        // the env-file is passed `--env-file <cargo_toml_env_vars output>` and staged as a run input.
        let ef = a.iter().position(|x| x == "--env-file").expect("--env-file");
        assert_eq!(a[ef + 1], "app/cenv", "the cargo_toml_env_vars env-file: {a:?}");
        assert!(run.inputs.contains(&"app/cenv".to_string()), "env-file staged as input: {:?}", run.inputs);
        // literal version/pkg_name → CARGO_PKG_* (override).
        let env_vals: Vec<String> =
            a.windows(2).filter(|w| w[0] == "--env").map(|w| w[1].clone()).collect();
        assert!(env_vals.contains(&"CARGO_PKG_VERSION=9.9.9".to_string()), "version override: {env_vals:?}");
        assert!(env_vals.contains(&"CARGO_PKG_NAME=pkgx".to_string()), "pkg_name override: {env_vals:?}");
        // §6.2 PRECEDENCE: the `--env-file` precedes the literal `--env` overrides in argv (the
        // wrapper applies files first, then --env), so the literal wins.
        let first_env = a.iter().position(|x| x == "--env").unwrap();
        assert!(ef < first_env, "env-file comes before the literal --env overrides: {a:?}");
    }

    #[test]
    fn p38d_cargo_cfg_env_derives_from_the_triple() {
        let linux: std::collections::BTreeMap<String, String> =
            cargo_cfg_env("x86_64-unknown-linux-gnu").into_iter().collect();
        assert_eq!(linux["CARGO_CFG_TARGET_ARCH"], "x86_64");
        assert_eq!(linux["CARGO_CFG_TARGET_OS"], "linux");
        assert_eq!(linux["CARGO_CFG_TARGET_VENDOR"], "unknown");
        assert_eq!(linux["CARGO_CFG_TARGET_ENV"], "gnu");
        assert_eq!(linux["CARGO_CFG_TARGET_FEATURE"], "fxsr,sse,sse2", "x86_64 baseline features");
        assert_eq!(linux["CARGO_CFG_UNIX"], "", "boolean cfg → present, empty value");

        let mac: std::collections::BTreeMap<String, String> =
            cargo_cfg_env("aarch64-apple-darwin").into_iter().collect();
        assert_eq!(mac["CARGO_CFG_TARGET_ARCH"], "aarch64");
        assert_eq!(mac["CARGO_CFG_TARGET_OS"], "macos", "darwin → macos");
        assert_eq!(mac["CARGO_CFG_TARGET_VENDOR"], "apple");
        assert_eq!(mac["CARGO_CFG_TARGET_ENV"], "", "darwin has no target_env");
        assert_eq!(mac["CARGO_CFG_TARGET_FEATURE"], "neon", "aarch64 baseline feature");
    }

    #[test]
    fn p38d_run_action_carries_the_cargo_cfg_env() {
        let build = format!(
            "{BS_LOAD}cargo_build_script(name = \"t\", srcs = [\"root.rs\"])\n"
        );
        let targets = analyze("p38d_cfg", &build).unwrap();
        let run = &targets.iter().find(|t| t.name == "//app:t").unwrap().actions[1];
        let env_vals: Vec<String> =
            run.argv.windows(2).filter(|w| w[0] == "--env").map(|w| w[1].clone()).collect();
        // The run action emits the CARGO_CFG_* set for the host (host==target).
        let triple = crate::state::host_triple();
        let want: std::collections::BTreeMap<String, String> = cargo_cfg_env(triple).into_iter().collect();
        assert!(
            env_vals.contains(&format!("CARGO_CFG_TARGET_ARCH={}", want["CARGO_CFG_TARGET_ARCH"])),
            "CARGO_CFG_TARGET_ARCH for the host triple: {env_vals:?}"
        );
        assert!(env_vals.iter().any(|e| e.starts_with("CARGO_CFG_TARGET_OS=")), "{env_vals:?}");
        assert!(env_vals.iter().any(|e| e.starts_with("CARGO_CFG_TARGET_FEATURE=")), "{env_vals:?}");
    }

    #[test]
    fn p310_build_script_dep_routes_the_crate_rustc_through_the_wrapper() {
        // blake3's shape: a crate deps on its own build script (via the alias), and razel must
        // route the crate's rustc through the wrapper (consuming the flags-file + OUT_DIR) WITHOUT
        // passing the build script as an --extern (§4.3).
        let build = format!(
            "{LOAD}{BS_LOAD}\
             cargo_build_script(name = \"bs\", srcs = [\"root.rs\"])\n\
             rust_library(name = \"t\", srcs = [\"lib.rs\"], deps = [\":bs\"])\n"
        );
        let targets = analyze("p310_edge", &build).unwrap();
        let t = targets.iter().find(|t| t.name == "//app:t").expect("crate analyzed");
        let argv = &t.actions[0].argv;
        // The crate's rustc is wrapped: [wrapper, rustc, --rustc=…, --flags-file=…, --env=OUT_DIR=…, --, <rustc argv>].
        assert!(argv[0].contains("razel-process-wrapper"), "routed through the wrapper: {argv:?}");
        assert_eq!(argv[1], "rustc", "the rustc subcommand: {argv:?}");
        assert!(argv.iter().any(|a| a.starts_with("--rustc=")), "carries the real rustc: {argv:?}");
        assert!(argv.contains(&"--flags-file=app/bs.out".to_string()), "consumes the flags file: {argv:?}");
        assert!(argv.contains(&"--env=OUT_DIR=app/bs.out_dir".to_string()), "points OUT_DIR at the tree: {argv:?}");
        // The real rustc argv follows `--` (the lib compile, A3's faithful `--crate-type=rlib`).
        let sep = argv.iter().position(|a| a == "--").expect("the `--` separator");
        assert!(argv[sep + 1..].contains(&"--crate-type=rlib".to_string()), "the crate compile is after --: {argv:?}");
        // §4.3: the build script is NEVER an --extern.
        assert!(!argv.iter().any(|a| a == "--extern"), "build script is not an --extern: {argv:?}");
        assert!(!argv.iter().any(|a| a.starts_with("bs=")), "no bs rlib extern: {argv:?}");
        // The flags-file + OUT_DIR tree are staged as inputs.
        let inputs = &t.actions[0].inputs;
        assert!(inputs.contains(&"app/bs.out".to_string()), "flags-file is an input: {inputs:?}");
        assert!(inputs.contains(&"app/bs.out_dir".to_string()), "OUT_DIR tree is an input: {inputs:?}");
        // …and `:bs` is still a recorded dep (the graph edge), just not an --extern.
        assert!(t.deps.iter().any(|d| d.ends_with(":bs")), "the build script stays a dep: {:?}", t.deps);
    }

    #[test]
    fn p310_plain_crate_is_not_wrapped() {
        // No build-script dep → the rustc argv is unchanged (additive: the edge only fires on a
        // build-script dep), so argv[0] is rustc, not the wrapper.
        let build = format!("{LOAD}rust_library(name = \"t\", srcs = [\"lib.rs\"])\n");
        let argv = argv_of(&analyze("p310_plain", &build).unwrap());
        assert!(argv[0].ends_with("rustc"), "plain crate runs rustc directly: {argv:?}");
        assert!(!argv[0].contains("razel-process-wrapper"), "not wrapped: {argv:?}");
    }
}
