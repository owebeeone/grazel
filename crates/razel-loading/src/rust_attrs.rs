//! Rust rules — the §5.5 compile-attr verdict + parse machinery (split from `rust_rules.rs`).

use crate::deps::resolve_dep;
use starlark::collections::SmallMap;
use starlark::eval::Evaluator;
use starlark::values::Value;

/// §5.5 verdict for an attribute of the rust compile rules (`rust_library`/`rust_binary`): the
/// single source of truth for **accept vs loud-error**, kept separate from "implement semantics"
/// (P2#3). An attr absent from [`rust_attr_verdict`] is a loud error — "accept" is a deliberate,
/// enumerated choice, never a silent fall-through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Verdict {
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
pub(crate) fn rust_attr_verdict(attr: &str) -> Option<Verdict> {
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
pub(crate) struct CompileAttrs {
    pub(crate) crate_name: Option<String>,
    pub(crate) crate_root: Option<String>,
    pub(crate) crate_features: Vec<crate::values::StrAttrPart>,
    pub(crate) rustc_flags: Vec<crate::values::StrAttrPart>,
    pub(crate) compile_data: Vec<crate::values::StrAttrPart>,
    pub(crate) target_compatible_with: Vec<crate::values::StrAttrPart>,
    /// P3.8b: `data` — build-script RUN inputs (§5.2 action 2); only `cargo_build_script` fills it.
    pub(crate) data: Vec<crate::values::StrAttrPart>,
    /// P3.8c: build-script RUN env (§5.2/§6.2). `rustc_env_files` are env-file targets → `--env-file`
    /// (the `CARGO_PKG_*` from `cargo_toml_env_vars`, P3.5a); literal `version`/`pkg_name` → `--env
    /// CARGO_PKG_VERSION`/`NAME` which OVERRIDE the env-file (§6.2). `cargo_build_script` only.
    pub(crate) rustc_env_files: Vec<crate::values::StrAttrPart>,
    pub(crate) version: Option<String>,
    pub(crate) pkg_name: Option<String>,
}


/// Apply the §5.5 verdict table to a compile rule's extra `**kwargs`: **loud-error** on any attr
/// not in the table, then extract the compile-affecting set (`CompileArgv`/`CompileEnv`) that's
/// implemented so far. `Delegated`/`Ignored` are accepted no-ops — recorded via `capture_rule`,
/// never extracted. Accept (the verdict) is decoupled from implement (extraction grows per step —
/// P2#3); an accepted-but-not-yet-extracted attr (e.g. `rustc_env` → P3.5) is simply inert here.
/// `rule` names the rule for the error.
pub(crate) fn compile_attrs<'v>(
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


/// Resolve the compile-argv extras at ANALYSIS time, returned SEPARATELY so each rule can POSITION
/// them in Bazel's order (RazelRustParityPlan B3): the feature cfgs go right after `--target` (before
/// `--edition`); `rustc_flags` go at the very end (after the externs). rules_rust emits each feature
/// cfg as TWO tokens — `--cfg` then `feature="x"` (NOT joined `--cfg=feature="x"`) — so the argv
/// matches token-for-token. (`crate_name`/`crate_root` overrides are applied inline by each rule.)
pub(crate) fn compile_extras<'v>(
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
pub(crate) fn compile_tail<'v>(
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
pub(crate) fn data_inputs<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    compile: &CompileAttrs,
) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in crate::values::resolve_str_parts(eval, &compile.compile_data)? {
        out.extend(resolve_dep(eval, &entry)?.libs);
    }
    Ok(out)
}


