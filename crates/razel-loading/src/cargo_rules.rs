//! cargo rules — `cargo_build_script` + `cargo_toml_env_vars` natives (split from
//! `rust_rules.rs`); registered alongside the core rust rules in `rust_rules::module()`.

use crate::state::{AnalyzedAction, AnalyzedTarget, canon_label, native_decl, out_dir, out_path, qualify, session};
use crate::deps::{record_target, resolve_dep};
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::Value;
use starlark::values::none::NoneType;
use crate::{rust_attrs::*, rust_common::*, cargo_support::*};

#[starlark::starlark_module]
pub(crate) fn cargo_rules(b: &mut GlobalsBuilder) {
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
        // P5.1 (§11.2): capture the loading-phase query nodes (runner + `:_bs_`/`:_bs-` macro
        // children) at DECLARE time, before `srcs`/`deps` are rebound from `Value` to parts.
        crate::loaded::capture_cargo_build_script(eval, &label, &[("srcs", srcs), ("deps", deps)], &kw);
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
            let (extern_flags, dep_rlibs, mut dep_names, _bs) = extern_args(eval, deps.clone(), &[])?;
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
            // their output file(s); passed `--env-file` (and staged as run inputs). B4: each is a
            // dep of the build-script run — add its canon to `deps` so `collect_order` BUILDS the
            // env-file before the run (else the `--env-file` is missing → the run ENOENTs at exec).
            let mut env_files = Vec::new();
            for entry in crate::values::resolve_str_parts(eval, &compile.rustc_env_files)? {
                let dep = resolve_dep(eval, &entry)?;
                env_files.extend(dep.libs);
                dep_names.push(dep.canon);
            }
            // P4.5 (§5.5/§6): `link_deps` — resolve each `links` crate to its (links-name, flags-file)
            // so this run inherits its `DEP_<LINKS>_*` metadata (the cross-build-script channel). The
            // producing build script joins `deps` (it must RUN before this one); its flags-file is a
            // run input. A link_dep that isn't a `links` crate contributes nothing.
            let mut dep_metadata: Vec<(String, String)> = Vec::new();
            for entry in crate::values::resolve_str_parts(eval, &compile.link_deps)? {
                let dep = resolve_dep(eval, &entry)?;
                if let Some(bs) = &dep.build_script
                    && let Some(links) = &bs.links
                {
                    dep_metadata.push((links.clone(), bs.flags_file.clone()));
                    dep_names.push(dep.canon.clone());
                }
            }
            let sess = session(eval);

            // --- action 1: compile the host build-script bin (`<name>_`, §12 `:_bs_`) ---
            // RazelRustParityPlan A3: rules_rust's faithful bin compile (exec config) — `bin`
            // crate-type, `opt-level=3`/`strip=debuginfo` (exec), `--emit=link=<bin>`+`--emit=dep-info`
            // (no metadata/extra-filename → unhashed bin name), in Bazel's argv order. The cc-toolchain
            // link flags + `--sysroot`/`-L` are deviations razel doesn't emit (the diff filters them).
            let bin = out_path(sess, &format!("{name}_"));
            let dsym = out_path(sess, &format!("{name}_.dSYM")); // macOS debug-symbols tree output
            // The bare rustc args (everything after the rustc binary).
            let mut bare_argv = vec![
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
            bare_argv.extend(feature_cfgs); // B3: feature cfgs after --target (Bazel's order)
            bare_argv.push(format!("--edition={edition}"));
            bare_argv.push("-Cembed-bitcode=no".into());
            bare_argv.extend(extern_flags);
            bare_argv.extend(rustc_flags); // rustc_flags (e.g. --cap-lints) at the end
            let mut compile_inputs = srcs.clone();
            compile_inputs.extend(dep_rlibs);
            // B4: a build script's `build.rs` can read the Cargo env at COMPILE time (e.g.
            // crossbeam-utils' `env!("CARGO_PKG_NAME")`). When the crate declares any Cargo env,
            // route this compile through the wrapper's `rustc` subcommand with the SAME env-file +
            // literal `version`/`pkg_name` as the run, so `env!()` resolves. Parity-neutral:
            // `canonicalize_rust_argv` strips the wrapper prefix, and the env-file lives under
            // `external/<repo>/…` so `source_inputs` drops it.
            let compile_argv = if !env_files.is_empty()
                || compile.version.is_some()
                || compile.pkg_name.is_some()
            {
                let mut w = process_wrapper_prefix();
                w.push("rustc".into());
                w.push(format!("--rustc={}", rustc()));
                for ef in &env_files {
                    w.push(format!("--env-file={ef}"));
                }
                if let Some(v) = &compile.version {
                    w.push(format!("--env=CARGO_PKG_VERSION={v}"));
                }
                if let Some(p) = &compile.pkg_name {
                    w.push(format!("--env=CARGO_PKG_NAME={p}"));
                }
                w.push("--".into());
                w.extend(bare_argv);
                compile_inputs.extend(env_files.clone());
                w
            } else {
                let mut a = vec![rustc()];
                a.extend(bare_argv);
                a
            };

            // --- action 2: run the bin via the wrapper → §6.1 flags file + OUT_DIR tree ---
            let flags_out = out_path(sess, &format!("{name}.out")); // §6.1 `<name>.out`
            let out_dir = out_path(sess, &format!("{name}.out_dir")); // P2.4 tree output
            let triple = crate::state::host_triple();
            // B4: cargo runs a build script with cwd = the crate's manifest dir, so cc-rs's
            // `build.file("c/blake3_neon.c")` (relative to `CARGO_MANIFEST_DIR`) resolves. `--rundir`
            // = the crate SOURCE dir (`external/<repo>` for a vendored crate, the package dir
            // otherwise); the wrapper chdir's the bin there + sets `CARGO_MANIFEST_DIR`. Absolute
            // OUT_DIR/program are the wrapper's job (cwd-independent, like cargo).
            let crate_dir = qualify(sess, "").trim_end_matches('/').to_string();
            let mut run_argv = process_wrapper_prefix();
            run_argv.extend([
                "build-script".into(),
                "--flags-out".into(),
                flags_out.clone(),
                "--out-dir".into(),
                out_dir.clone(),
                "--rundir".into(),
                crate_dir,
            ]);
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
            // Cargo always sets RUSTC for build scripts; some probe `rustc --version` (e.g.
            // allocative's nightly check). The wrapper `env_clear`s, so pass the ABSOLUTE rustc path
            // (`rustc()` resolves it) — it must run with no inherited PATH.
            run_argv.push("--env".into());
            run_argv.push(format!("RUSTC={}", rustc()));
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
            // P4.5 (§5.5/§6): the cross-build-script `DEP_<LINKS>_*` channel — one `--dep-metadata`
            // per link_dep; the wrapper reads each links crate's flags-file + injects DEP_<LINKS>_<K>.
            for (links, flags_file) in &dep_metadata {
                run_argv.push("--dep-metadata".into());
                run_argv.push(format!("{links}={flags_file}"));
            }
            run_argv.push("--".into());
            run_argv.push(bin.clone());
            let mut run_inputs = vec![bin.clone()];
            run_inputs.extend(srcs);
            run_inputs.extend(data_files);
            run_inputs.extend(env_files);
            run_inputs.extend(dep_metadata.iter().map(|(_, f)| f.clone())); // P4.5: link_deps flags-files

            // The bar's per-action label: `<crate> <version>` — the version names the source repo
            // (`crates__<crate>-<version>`), so it doubles as "which crate dir". Empty for a path /
            // workspace crate with no recorded version → the bar falls back to the output name.
            let crate_desc = compile.pkg_name.as_deref().map_or(String::new(), |p| crate_label(p, &compile.version));
            let mut t = AnalyzedTarget {
                name: canon_label(sess, &name),
                deps: dep_names,
                actions: vec![
                    AnalyzedAction {
                        mnemonic: "Rustc".into(),
                        argv: compile_argv,
                        inputs: compile_inputs,
                        outputs: vec![bin.clone(), dsym],
                        description: crate_desc.clone(),
                    },
                    AnalyzedAction {
                        mnemonic: "CargoBuildScriptRun".into(),
                        argv: run_argv,
                        inputs: run_inputs,
                        outputs: vec![flags_out.clone(), out_dir.clone()],
                        description: crate_desc.clone(),
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
            // P4.5 (§5.5/§6): publish this build script's `links` name so a dependent's `link_deps`
            // can form `--dep-metadata <links>=<flags-file>` (own-only — named, not folded).
            if let Some(links) = &compile.links {
                t.set_set("BuildScriptRun", "links", vec![links.clone()]);
            }
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
                    description: String::new(),
                }],
                default_info: vec![out],
                ..Default::default()
            });
            Ok(())
        }))?;
        Ok(NoneType)
    }
}
