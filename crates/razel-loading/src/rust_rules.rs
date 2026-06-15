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

use crate::state::{AnalyzedAction, AnalyzedTarget, canon_label, native_decl, qualify, session};
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

/// The crate name a dependent uses for `--extern` / `use`: the target segment of a
/// canonical label (`//lib:greet` → `greet`, bare `greet` → `greet`).
fn crate_name_of(canon: &str) -> String {
    canon
        .rsplit_once(':')
        .map(|(_, n)| n)
        .unwrap_or(canon)
        .to_string()
}

/// Resolve `deps` to `(--extern crate=rlib args, dep rlib inputs, dep canon names)`.
fn extern_args(
    eval: &mut Evaluator<'_, '_, '_>,
    deps: Vec<String>,
) -> anyhow::Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let (mut args, mut inputs, mut names) = (Vec::new(), Vec::new(), Vec::new());
    for d in &deps {
        let dep = resolve_dep(eval, d)?;
        let crate_name = crate_name_of(&dep.canon);
        // A rust_library exports exactly one rlib in default_info → dep.libs.
        for rlib in &dep.libs {
            args.push("--extern".into());
            args.push(format!("{crate_name}={rlib}"));
            inputs.push(rlib.clone());
        }
        names.push(dep.canon);
    }
    Ok((args, inputs, names))
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

/// P3.6 (§5.2): validate a `cargo_build_script`'s extra `**kwargs` against the build-script attr
/// surface (loud-error on unknown) and extract the compile-phase argv attrs (`crate_name`/
/// `crate_root`/`crate_features`/`rustc_flags`) into the shared [`CompileAttrs`]. Run-phase and
/// deferred-compile attrs are accepted but left inert here — their action effects land in the
/// named later step (accept ≠ implement, P2#3); a regression test pins the argv-inertness.
fn bs_compile_attrs<'v>(
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
        if verdict == BsVerdict::CompileArgv {
            match key.as_str() {
                "crate_name" => out.crate_name = val.unpack_str().map(str::to_owned),
                "crate_root" => out.crate_root = val.unpack_str().map(str::to_owned),
                "crate_features" => {
                    out.crate_features = crate::values::str_attr_parts(eval, Some(*val))?
                }
                "rustc_flags" => {
                    out.rustc_flags = crate::values::str_attr_parts(eval, Some(*val))?
                }
                _ => unreachable!("CompileArgv keys are exactly the four matched above"),
            }
        }
        // `Compile` (deferred) / `Run` / `Ignored` → accepted no-ops here; never extracted.
    }
    Ok(out)
}

/// Resolve the P3.2a compile-argv tail at ANALYSIS time: `--cfg=feature="x"` per `crate_features`
/// (rules_rust's form), then `rustc_flags` verbatim. (`crate_name`/`crate_root` overrides are
/// applied inline by each rule, since they replace existing argv tokens.)
fn compile_tail<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    compile: &CompileAttrs,
) -> anyhow::Result<Vec<String>> {
    let mut tail = Vec::new();
    for f in crate::values::resolve_str_parts(eval, &compile.crate_features)? {
        tail.push(format!("--cfg=feature=\"{f}\""));
    }
    tail.extend(crate::values::resolve_str_parts(eval, &compile.rustc_flags)?);
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
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let tail = compile_tail(eval, &compile)?;
        let data = data_inputs(eval, &compile)?; // P3.2b: compile_data → inputs
        let sess = session(eval);

        let rlib = qualify(sess, &format!("lib{name}.rlib"));
        let mut argv = vec![
            rustc(),
            "--edition".into(),
            edition,
            "--crate-type".into(),
            "lib".into(),
            "--crate-name".into(),
            crate_name,
            crate_root,
            "-o".into(),
            rlib.clone(),
        ];
        argv.extend(extern_flags);
        argv.extend(tail);

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
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
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let tail = compile_tail(eval, &compile)?;
        let data = data_inputs(eval, &compile)?; // P3.2b: compile_data → inputs
        let sess = session(eval);

        let out = qualify(sess, &name);
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

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
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
        let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
        let sess = session(eval);
        let dylib = qualify(sess, &format!("lib{name}.dylib"));
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
        let (_, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
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

    /// `cargo_build_script(name, srcs, deps=[], edition="2021", **attrs)` — P3.6 (§5.2) compiles
    /// the build-script (action 1): `crate_root`/`srcs[0]` → a HOST `rust_binary` (`<name>_`, the
    /// §12 `:_bs_` bin) with the single toolchain, linking `deps` as `--extern` (build-deps, NOT
    /// run inputs). The compile-phase attr split (`crate_name`/`crate_root`/`crate_features`/
    /// `rustc_flags`) shapes the argv; the §5.2 run-phase attrs are accepted but inert until P3.8.
    /// `default_info` is EMPTY (§4.3: a build-script target exposes no libs — the bin is consumed
    /// intra-target by the run action P3.8, never `--extern`'d by a dependent crate).
    fn native_cargo_build_script<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        let compile = bs_compile_attrs(eval, &name, &kw)?; // §5.2 verdict + compile-attr split
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
            let (extern_flags, dep_rlibs, dep_names) = extern_args(eval, deps.clone())?;
            let tail = compile_tail(eval, &compile)?;
            let sess = session(eval);

            // The host build-script bin (`<name>_`, the §12 `:_bs_`); P3.8's run action consumes
            // this exact path. An intermediate output — kept OUT of `default_info` (§4.3).
            let bin = qualify(sess, &format!("{name}_"));
            let mut argv = vec![
                rustc(),
                "--edition".into(),
                edition,
                "--crate-name".into(),
                crate_name,
                crate_root,
                "-o".into(),
                bin.clone(),
            ];
            argv.extend(extern_flags);
            argv.extend(tail);

            let mut inputs = srcs;
            inputs.extend(dep_rlibs);
            record_target(sess, AnalyzedTarget {
                name: canon_label(sess, &name),
                deps: dep_names,
                actions: vec![AnalyzedAction {
                    mnemonic: "Rustc".into(),
                    argv,
                    inputs,
                    outputs: vec![bin],
                }],
                default_info: Vec::new(),
                providers: Default::default(),
            });
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
            let out = qualify(sess, &name);
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
        // crate_name overrides the default (`t`).
        let i = argv.iter().position(|a| a == "--crate-name").unwrap();
        assert_eq!(argv[i + 1], "custom", "crate_name overrides --crate-name: {argv:?}");
        // crate_root picks root.rs as the positional (not srcs[0] = lib.rs).
        assert!(argv.contains(&"app/root.rs".to_string()), "crate_root is the positional: {argv:?}");
        assert!(!argv.contains(&"app/lib.rs".to_string()), "lib.rs is not the root: {argv:?}");
        // crate_features → one `--cfg=feature="x"` per feature (rules_rust's form).
        assert!(argv.contains(&"--cfg=feature=\"alpha\"".to_string()), "{argv:?}");
        assert!(argv.contains(&"--cfg=feature=\"beta\"".to_string()), "{argv:?}");
        // rustc_flags appended verbatim.
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
        assert!(t.actions[0].argv.iter().any(|a| a == "--crate-name"), "{:?}", t.actions[0].argv);
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
        // Compiles the build-script root → a HOST bin (`<name>_`, the §12 `:_bs_`) via rustc.
        assert!(argv.first().is_some_and(|a| a.ends_with("rustc")), "rustc compile: {argv:?}");
        let o = argv.iter().position(|a| a == "-o").expect("an -o flag");
        assert_eq!(argv[o + 1], "app/t_", "host build-script bin output: {argv:?}");
        // `deps` → `--extern` (build-deps link the host bin), NOT run inputs.
        assert!(
            argv.windows(2).any(|w| w[0] == "--extern" && w[1] == "dep=app/libdep.rlib"),
            "build-dep is an --extern: {argv:?}"
        );
        // `crate_root` picks root.rs as the positional; default crate_name = the target name.
        assert!(argv.contains(&"app/root.rs".to_string()), "crate_root positional: {argv:?}");
        let cn = argv.iter().position(|a| a == "--crate-name").unwrap();
        assert_eq!(argv[cn + 1], "t", "default crate_name = name: {argv:?}");
        // `crate_features` → `--cfg feature` (compile phase); `rustc_flags` verbatim.
        assert!(argv.contains(&"--cfg=feature=\"std\"".to_string()), "{argv:?}");
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
}
