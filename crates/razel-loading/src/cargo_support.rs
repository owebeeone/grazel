//! cargo build-script support — the run-env (`CARGO_CFG_*`), the §5.2 build-script attr
//! verdict/parse, and the `CARGO_PKG_*` env-file content (split from `rust_rules.rs`).

use starlark::collections::SmallMap;
use starlark::eval::Evaluator;
use starlark::values::Value;
use crate::rust_attrs::*;

/// P3.8d (§5.2): the `CARGO_CFG_*` build-script env cargo derives from the target, synthesized from
/// the configured (host==target) triple `<arch>-<vendor>-<sys>[-<env>]`. Covers the cfgs blake3's
/// `build.rs` keys SIMD off (`TARGET_ARCH`/`OS`/`ENV`/`FEATURE`) plus the standard set.
/// `TARGET_FEATURE` is a slice-1 per-arch BASELINE (the always-on features) — refined against the
/// P3.12 execution-parity golden, where the exact feature set is observable.
pub(crate) fn cargo_cfg_env(triple: &str) -> Vec<(String, String)> {
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


/// §5.2 verdict for a `cargo_build_script` attribute — the build-script attr surface, **distinct
/// from `rust_library`'s §5.5** and split by phase (compile = action 1, run = action 2). Same
/// accept-vs-loud-error discipline: an attr absent here is a loud error, never a silent pass.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BsVerdict {
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
pub(crate) fn build_script_attr_verdict(attr: &str) -> Option<BsVerdict> {
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
pub(crate) fn bs_attrs<'v>(
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
                // P4.5 (§5.5/§6): `links` (this crate's `DEP_<LINKS>_*` prefix) + `link_deps` (the
                // links crates whose metadata this build script inherits — the cross-build-script
                // channel). The remaining run attrs (`build_script_env`/`tools`/`rundir`) stay inert.
                "links" => out.links = val.unpack_str().map(str::to_owned),
                "link_deps" => out.link_deps = crate::values::str_attr_parts(eval, Some(*val))?,
                _ => {}
            },
            // `Compile` (deferred) / `Ignored` → accepted no-ops here; never extracted.
            BsVerdict::Compile | BsVerdict::Ignored => {}
        }
    }
    Ok(out)
}


/// P3.5: the absolute disk path of `<target's package>/<src>`, resolved like `resolve_dep`'s file
/// path — `@repo//pkg:name` → the vendored external dir (`external_repo_dirs`), `//pkg:name` → the
/// workspace root. (Handles the `@@` canonical form via `trim_start_matches('@')`.)
pub(crate) fn pkg_file_abs(
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
pub(crate) fn cargo_pkg_env_content(toml: &str) -> String {
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


