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
use crate::deps::record_target;
use crate::values::unpack_strs;
use starlark::collections::SmallMap;
use starlark::environment::{FrozenModule, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::list::UnpackList;
use starlark::values::none::NoneType;

use crate::cargo_rules::cargo_rules;
use crate::{rust_attrs::*, rust_common::*};

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
        // P4.1 (§5.3): `proc_macro_deps` resolve + `--extern` alongside `deps` (extern_args routes
        // each by its DepInfo — a proc-macro dep becomes a host-dylib extern, not an rlib).
        let mut all_deps = deps.clone();
        all_deps.extend(crate::values::resolve_str_parts(eval, &compile.proc_macro_deps)?);
        let (extern_flags, dep_rlibs, dep_names, build_scripts) = extern_args(eval, all_deps, &compile.aliases)?;
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
        // P3.10 (§4.3): a build-script dep routes this rustc through the process wrapper. P4.5: an
        // rlib is NOT a final link — it passes NO transitive link-flags-files (`&[]`); it only
        // PUBLISHES its own build script's flags-file (below) for a downstream binary to link.
        let (env_file_deps, env_files) = rustc_env_file_deps(eval, &compile.rustc_env_files)?;
        let extra_env = compile_env(&compile, &canon_label(session(eval), &name));
        let (argv, bs_inputs) = apply_build_script_edge("rust_library", &name, &extra_env, argv, &build_scripts, &env_files, &[])?;
        let mut dep_names = dep_names;
        dep_names.extend(env_file_deps); // build the env-file (cargo_toml_env_vars) target FIRST

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
        inputs.extend(bs_inputs);
        let mut t = AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![rlib.clone()],
                description: String::new(),
            }],
            default_info: vec![rlib],
            providers: Default::default(),
        };
        // P4.5 (§5.5/§6): publish this crate's OWN build-script flags-file into `RustLinkInfo` so its
        // native-link directives FOLD transitively to a consuming `rust_binary`'s final link.
        let bs_flags: Vec<String> = build_scripts.iter().map(|bs| bs.flags_file.clone()).collect();
        if !bs_flags.is_empty() {
            t.set_set("RustLinkInfo", "bs_flags", bs_flags);
        }
        record_target(sess, t);
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
        // P4.1 (§5.3): `proc_macro_deps` resolve + `--extern` alongside `deps` (extern_args routes
        // each by its DepInfo — a proc-macro dep becomes a host-dylib extern, not an rlib).
        let mut all_deps = deps.clone();
        all_deps.extend(crate::values::resolve_str_parts(eval, &compile.proc_macro_deps)?);
        let (extern_flags, dep_rlibs, dep_names, build_scripts) = extern_args(eval, all_deps, &compile.aliases)?;
        let tail = compile_tail(eval, &compile)?;
        let data = data_inputs(eval, &compile)?; // P3.2b: compile_data → inputs
        // P4.5 (§5.5/§6): a `rust_binary` is the FINAL link — it inherits the native-link directives
        // of EVERY build script in its (regular-)dep closure (NOT proc-macro deps: those are host
        // dylibs). The `RustLinkInfo` fold gives the flags-files; applied link-only (`--link-flags-file`).
        let link_flags_files = transitive_link_flags_files(eval, &deps)?;
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
        // P3.10 (§4.3): a build-script dep routes this rustc through the process wrapper. P4.5: the
        // transitive build-script link channel does too (final link inherits the closure's libs).
        let (env_file_deps, env_files) = rustc_env_file_deps(eval, &compile.rustc_env_files)?;
        let extra_env = compile_env(&compile, &canon_label(session(eval), &name));
        let (argv, bs_inputs) = apply_build_script_edge("rust_binary", &name, &extra_env, argv, &build_scripts, &env_files, &link_flags_files)?;
        let mut dep_names = dep_names;
        dep_names.extend(env_file_deps); // build the env-file (cargo_toml_env_vars) target FIRST

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
                description: String::new(),
            }],
            default_info: vec![out],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_test(name, srcs|crate, deps=[], edition=…)` — DECLARES + captures, then ANALYZES to a
    /// `rustc --test` action producing a runnable test binary (in `default_info`, so `razel test`
    /// executes it). Two forms: `srcs=` is a standalone test crate; `crate=:lib` recompiles the
    /// lib-under-test's OWN sources WITH `--test` (Bazel's unit-test form). Lean argv (mirrors
    /// `rust_shared_library`); the faithful rules_rust argv, the bazel test ENV
    /// (`TEST_TMPDIR`/`CARGO_MANIFEST_DIR`/runfiles), `--test_arg`/filter passthrough, build-script
    /// `OUT_DIR`, and `test_suite` expansion are the named follow-ons.
    fn native_rust_test<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_test", &[("srcs", srcs), ("deps", deps)], &_kw);
        // `crate=:lib` (the lib-under-test) + `crate_name`/`crate_root` arrive via kwargs (`crate` is
        // a Rust keyword, never bound as a param). Read at LOAD; the lib's sources resolve at analysis.
        let crate_under_test = _kw.get("crate").and_then(|v| v.unpack_str()).map(str::to_owned);
        let crate_name_attr = _kw.get("crate_name").and_then(|v| v.unpack_str()).map(str::to_owned);
        let crate_root_attr = _kw.get("crate_root").and_then(|v| v.unpack_str()).map(str::to_owned);
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
            // The string leaves of a captured attr (source paths / dep labels) — for reading the
            // `crate=:lib` lib-under-test's `srcs`/`deps`/… at analysis (loading twin of `attr_labels`).
            fn raw_strings(raw: &crate::loaded::RawAttr) -> Vec<String> {
                use crate::loaded::RawAttr::*;
                match raw {
                    Str(s) | Label(s) => vec![s.clone()],
                    List(xs) | Tuple(xs) | Concat(xs) => xs.iter().flat_map(raw_strings).collect(),
                    _ => vec![],
                }
            }
            let mut srcs = crate::values::resolve_str_parts(eval, &srcs)?;
            let mut deps = crate::values::resolve_str_parts(eval, &deps)?;
            let mut edition = edition.clone().unwrap_or_else(|| "2021".into());
            let mut crate_name = crate_name_attr.clone().unwrap_or_else(|| name.clone());
            let mut crate_root_raw = crate_root_attr.clone();
            // `crate=:lib` — recompile the lib-under-test's OWN sources WITH `--test` (Bazel's
            // unit-test form): pull its srcs/deps/edition/crate_root/crate_name from the loaded graph.
            if let Some(ref cut) = crate_under_test {
                let sess = session(eval);
                let cut_label = canon_label(sess, cut);
                let lib = sess.loaded_targets.borrow().get(&cut_label).cloned().ok_or_else(|| {
                    anyhow::anyhow!("rust_test `{name}`: crate `{cut}` not found in the loaded graph")
                })?;
                if let Some(a) = lib.attrs.get("srcs") {
                    srcs = raw_strings(a);
                }
                if let Some(a) = lib.attrs.get("deps") {
                    deps.extend(raw_strings(a));
                }
                if let Some(e) =
                    lib.attrs.get("edition").map(raw_strings).and_then(|v| v.into_iter().next())
                {
                    edition = e;
                }
                if let Some(n) =
                    lib.attrs.get("crate_name").map(raw_strings).and_then(|v| v.into_iter().next())
                {
                    crate_name = n;
                }
                if crate_root_raw.is_none() {
                    crate_root_raw =
                        lib.attrs.get("crate_root").map(raw_strings).and_then(|v| v.into_iter().next());
                }
            }
            let sess = session(eval);
            let srcs: Vec<String> = srcs.iter().map(|s| qualify(sess, s)).collect();
            let crate_root = match crate_root_raw {
                Some(cr) => qualify(sess, &cr),
                None => srcs
                    .first()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("rust_test `{name}` needs `srcs` or `crate`"))?,
            };
            let (extern_flags, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone(), &[])?;
            let sess = session(eval);
            // The `--test` harness: rustc compiles a runnable test-runner binary (the built-in
            // libtest `main`). Lean argv (like rust_shared_library); faithful rules_rust argv + the
            // bazel test ENV + `test_suite` expansion are the follow-ons. The bin lands in
            // `default_info`, so `razel test` (run_one_test) executes it.
            let testbin = out_path(sess, &name);
            let mut argv = vec![
                rustc(),
                "--edition".into(),
                edition,
                "--test".into(),
                "--crate-name".into(),
                crate_name,
                crate_root,
                "-o".into(),
                testbin.clone(),
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
                    outputs: vec![testbin.clone()],
                    description: String::new(),
                }],
                default_info: vec![testbin],
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
        let (extern_flags, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone(), &[])?;
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
                description: String::new(),
            }],
            default_info: vec![dylib],
            providers: Default::default(),
        });
        Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `rust_proc_macro(name, srcs, deps=[], edition="2021", **attrs)` (§5.3, P4.1/P4.2) → one rustc
    /// action `--crate-type=proc-macro` → `lib<name>-<hash>.{dylib,so}`, in rules_rust's faithful A3/A5
    /// form (joined flags, the hashed-output model, `--emit=dep-info,link`). A proc-macro is HOST-
    /// compiled — single toolchain, **no `--target`** (§5.3). A dependent links it via `proc_macro_deps`
    /// → `--extern <name>=<dylib>`; the own-only `RustProcMacro` marker tells `resolve_dep` it's a HOST
    /// dylib, not a target rlib (so it stays out of the transitive `-Ldependency` rlib closure).
    fn native_rust_proc_macro<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<Value<'v>>,
        #[starlark(require = named)] deps: Option<Value<'v>>,
        #[starlark(require = named)] edition: Option<String>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        crate::loaded::capture_rule(eval, &label, "rust_proc_macro", &[("srcs", srcs), ("deps", deps)], &_kw);
        let compile = compile_attrs(eval, "rust_proc_macro", &name, &_kw)?;
        let srcs = crate::values::str_attr_parts(eval, srcs)?;
        let deps = crate::values::str_attr_parts(eval, deps)?;
        crate::dialect::record_native(eval, label, native_decl(move |eval| {
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
        let crate_root = match &compile.crate_root {
            Some(cr) => qualify(sess, cr),
            None => srcs
                .first()
                .ok_or_else(|| anyhow::anyhow!("rust_proc_macro `{name}` needs at least one src"))?
                .clone(),
        };
        let edition = edition.unwrap_or_else(|| "2021".into());
        let crate_name = compile.crate_name.clone().unwrap_or_else(|| name.clone());
        let mut all_deps = deps.clone();
        all_deps.extend(crate::values::resolve_str_parts(eval, &compile.proc_macro_deps)?);
        let (extern_flags, dep_rlibs, dep_names, build_scripts) = extern_args(eval, all_deps, &compile.aliases)?;
        let (feature_cfgs, rustc_flags) = compile_extras(eval, &compile)?;
        let data = data_inputs(eval, &compile)?;
        let sess = session(eval);

        // §5.3/P4.2: faithful proc-macro argv (rules_rust A3/A5) — `--crate-type=proc-macro`, the
        // `--out-dir`+hashed `metadata`/`extra-filename` model → `lib<name>-<hash>.{dylib,so}`. HOST
        // compile: NO `--target` (vs the target crate's `--target=<triple>`).
        let hash = metadata_hash(&canon_label(sess, &name));
        let dylib = out_path(sess, &format!("lib{crate_name}-{hash}{}", std::env::consts::DLL_SUFFIX));
        let mut argv = vec![
            rustc(),
            crate_root,
            format!("--crate-name={crate_name}"),
            "--crate-type=proc-macro".into(),
            "--error-format=human".into(),
            format!("--codegen=metadata=-{hash}"),
            format!("--codegen=extra-filename=-{hash}"),
            format!("--out-dir={}", out_dir(sess)),
            "--codegen=opt-level=0".into(),
            "--codegen=debuginfo=0".into(),
            "--codegen=strip=none".into(),
            "--emit=dep-info,link".into(),
            "--color=always".into(),
            // §5.3: rules_rust passes `--target=<host triple>` even for the HOST proc-macro compile
            // (host==target here) — observed in the serde_derive golden.
            format!("--target={}", crate::state::host_triple()),
        ];
        argv.extend(feature_cfgs);
        argv.push(format!("--edition={edition}"));
        argv.push("-Cembed-bitcode=no".into());
        argv.extend(extern_flags);
        // §5.3: a proc-macro links the implicit `proc_macro` sysroot crate (rules_rust emits a bare
        // `--extern proc_macro`, no path — rustc resolves it from the toolchain). Only proc-macros.
        argv.push("--extern".into());
        argv.push("proc_macro".into());
        argv.extend(rustc_flags);
        // P4.5: a proc-macro is a HOST dylib, not a target final link — no transitive link channel.
        let (env_file_deps, env_files) = rustc_env_file_deps(eval, &compile.rustc_env_files)?;
        let extra_env = compile_env(&compile, &canon_label(session(eval), &name));
        let (argv, bs_inputs) = apply_build_script_edge("rust_proc_macro", &name, &extra_env, argv, &build_scripts, &env_files, &[])?;
        let mut dep_names = dep_names;
        dep_names.extend(env_file_deps); // build the env-file (cargo_toml_env_vars) target FIRST

        let mut inputs = srcs;
        inputs.extend(dep_rlibs);
        inputs.extend(data);
        inputs.extend(bs_inputs);
        let mut t = AnalyzedTarget {
            name: canon_label(sess, &name),
            deps: dep_names,
            actions: vec![AnalyzedAction {
                mnemonic: "Rustc".into(),
                argv,
                inputs,
                outputs: vec![dylib.clone()],
                description: String::new(),
            }],
            default_info: vec![dylib],
            providers: Default::default(),
        };
        // §5.3: own-only marker — `resolve_dep` reads it to `--extern` this as a host dylib.
        t.set_set("RustProcMacro", "marker", vec!["1".into()]);
        record_target(sess, t);
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
        let (_, dep_rlibs, dep_names, _bs) = extern_args(eval, deps.clone(), &[])?;
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
rust_proc_macro = native_rust_proc_macro
rust_test = native_rust_test
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

# crate_universe's generated `@crates//:defs.bzl` LOADs `local_crate_mirror` (a repo rule from
# `crate_universe/private:local_crate_mirror.bzl`) but only USES it inside `crate_repositories()` —
# the WORKSPACE/extension repo-declaration path. razel materializes `@crates` from the lock (P2.6),
# never via repo rules, so the SYMBOL must exist for defs.bzl to load; calling it (BUILD-mode) is a
# loud, named error.
def local_crate_mirror(**kwargs):
    fail("local_crate_mirror is a crate_universe WORKSPACE-mode repo rule; razel materializes @crates from the lock — not callable in BUILD analysis")

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
        .with(cargo_rules)
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


