#[cfg(test)]
mod tests {
    //! P3.2a §5.5 verdict-table gate. Analysis-only (no rustc): asserts on the captured Rustc
    //! argv, never executes — so it runs without a rust toolchain.
    use crate::cargo_support::cargo_cfg_env;
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
    fn p43_per_cfg_dep_select_externs_the_host_arm_and_drops_the_rest() {
        // P4.3 (§5.4): the libc/getrandom shape — `deps = select({<triple>: [...], default: [...]})`.
        // The HOST-triple arm's dep is compiled + `--extern`'d into the consumer; a NON-host arm
        // referencing a crate that was never fetched (`@crates__wasi`) is SELECTED AWAY — never
        // resolved, so its absent repo is no error — exactly as getrandom's wasm-only dep is inert on
        // a darwin host. (Beyond p33's compile_data: proves the per-cfg DEP edge + extern + the
        // select-away of an unfetched dep.)
        let host = crate::state::host_triple();
        let build = format!(
            "{LOAD}\
             rust_library(name = \"dep_host\", srcs = [\"lib.rs\"])\n\
             rust_library(name = \"t\", srcs = [\"root.rs\"], deps = select({{\
             \"@rules_rust//rust/platform:{host}\": [\":dep_host\"], \
             \"@rules_rust//rust/platform:wasm32-unknown-unknown\": \
             [\"@crates__wasi-0.11.0//:wasi\"], \
             \"//conditions:default\": []}}))\n"
        );
        let argv = argv_of(&analyze("p43dep", &build).unwrap());
        assert!(
            argv.iter().any(|a| a.starts_with("--extern=dep_host=app/libdep_host-")),
            "host-arm dep is compiled + extern'd: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.contains("wasi")),
            "the non-host (unfetched) arm is selected away, never resolved: {argv:?}"
        );
    }

    #[test]
    fn p43_selects_with_or_over_triples_picks_the_host_dep() {
        // P4.3: crate_universe groups triples that share a dep via `selects.with_or` (one cfg-class →
        // many triples → one dep). The tuple fans out to one arm each (p31b), and on the host the
        // host-triple arm resolves the shared dep into `--extern`; the non-host triple in the tuple
        // is inert.
        let host = crate::state::host_triple();
        let build = format!(
            "{LOAD}\
             load(\"@rules_rust//crate_universe/private:selects.bzl\", \"selects\")\n\
             rust_library(name = \"dep_host\", srcs = [\"lib.rs\"])\n\
             rust_library(name = \"t\", srcs = [\"root.rs\"], deps = selects.with_or({{\
             (\"@rules_rust//rust/platform:{host}\", \
             \"@rules_rust//rust/platform:wasm32-unknown-unknown\"): [\":dep_host\"], \
             \"//conditions:default\": []}}))\n"
        );
        let argv = argv_of(&analyze("p43wor", &build).unwrap());
        assert!(
            argv.iter().any(|a| a.starts_with("--extern=dep_host=app/libdep_host-")),
            "with_or host-triple arm resolves the shared dep: {argv:?}"
        );
    }

    #[test]
    fn p43_incompatible_dep_in_a_non_host_arm_is_selected_away() {
        // §5.4 guarantee: the CORRECT graph selects incompatible deps away. p34b proves a plain
        // `deps = [":bad"]` on an incompatible `:bad` is a loud error; here `:bad` lives only in the
        // NON-host (default) arm, so the select removes it before the incompatible-dep check ever
        // sees it — `t` builds cleanly. (Select-away precedes the §5.4 incompatible-dep error.)
        let host = crate::state::host_triple();
        let build = format!(
            "{LOAD}\
             rust_library(name = \"good\", srcs = [\"lib.rs\"])\n\
             rust_library(name = \"bad\", srcs = [\"lib.rs\"], \
             target_compatible_with = [\"@platforms//:incompatible\"])\n\
             rust_library(name = \"t\", srcs = [\"root.rs\"], deps = select({{\
             \"@rules_rust//rust/platform:{host}\": [\":good\"], \
             \"//conditions:default\": [\":bad\"]}}))\n"
        );
        let argv = argv_of(&analyze("p43incompat", &build).expect("select removes the incompatible arm"));
        assert!(
            argv.iter().any(|a| a.starts_with("--extern=good=app/libgood-")),
            "host arm's compatible dep is extern'd: {argv:?}"
        );
        assert!(!argv.iter().any(|a| a.contains("=bad=")), "the incompatible arm is gone: {argv:?}");
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
        // B4: `version` declares a COMPILE-time Cargo env (`env!(CARGO_PKG_VERSION)` in `build.rs`),
        // so the bin compile routes through the wrapper's `rustc` subcommand —
        // `[wrapper, rustc, --rustc=…, --env=CARGO_PKG_VERSION=…, --, <bare rustc args>]`.
        assert_eq!(argv.first().map(String::as_str), Some("razel-process-wrapper"), "wrapped compile: {argv:?}");
        assert_eq!(argv.get(1).map(String::as_str), Some("rustc"), "the rustc subcommand: {argv:?}");
        assert!(argv.iter().any(|a| a.starts_with("--rustc=") && a.ends_with("rustc")), "carries the rustc binary: {argv:?}");
        assert!(argv.contains(&"--env=CARGO_PKG_VERSION=1.2.3".to_string()), "version → compile-time CARGO_PKG_VERSION: {argv:?}");
        // The bare rustc args (after `--`) are rules_rust's faithful bin compile (A3):
        // `--crate-type=bin`, `--emit=link=<bin>` (not `-o`).
        let dd = argv.iter().position(|a| a == "--").expect("wrapper `--` separator");
        let bare = &argv[dd + 1..];
        assert!(bare.contains(&"--crate-type=bin".to_string()), "bin crate-type: {bare:?}");
        assert!(bare.contains(&"--emit=link=app/t_".to_string()), "host build-script bin output: {bare:?}");
        // `deps` → `--extern=` (joined, build-deps link the host bin), NOT run inputs; rlib hashed.
        assert!(
            bare.iter().any(|a| a.starts_with("--extern=dep=app/libdep-")),
            "build-dep is an --extern (joined, hashed rlib): {bare:?}"
        );
        // `crate_root` picks root.rs as the positional; default crate_name = the target name.
        assert!(bare.contains(&"app/root.rs".to_string()), "crate_root positional: {bare:?}");
        assert!(bare.contains(&"--crate-name=t".to_string()), "default crate_name = name: {bare:?}");
        // `crate_features` → `--cfg` `feature="x"` (two tokens, compile phase, B3); `rustc_flags` verbatim.
        assert!(bare.windows(2).any(|w| w == ["--cfg", "feature=\"std\""]), "{bare:?}");
        assert!(bare.contains(&"-Cdebuginfo=0".to_string()), "{bare:?}");
        // `links`/`data`/`tags` are ACCEPTED but argv-inert; `version` is the compile Cargo env above,
        // never a bare rustc arg.
        assert!(
            !bare.iter().any(|a| a.contains("1.2.3") || a.contains("table.bin") || a == "z" || a == "manual"),
            "run-phase/ignored attrs are not bare compile argv: {bare:?}"
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

    #[test]
    fn p41_rust_proc_macro_compiles_to_a_dylib_and_dependents_extern_it() {
        // §5.3/P4.1: a rust_proc_macro compiles `--crate-type proc-macro` → a HOST dylib; a
        // dependent's `proc_macro_deps` `--extern`s that dylib (NOT as a target rlib).
        let suffix = std::env::consts::DLL_SUFFIX; // ".dylib" / ".so"
        let build = "load(\"@rules_rust//rust:defs.bzl\", \"rust_library\", \"rust_proc_macro\")\n\
             rust_proc_macro(name = \"mac\", srcs = [\"mac.rs\"])\n\
             rust_library(name = \"t\", srcs = [\"lib.rs\"], proc_macro_deps = [\":mac\"])\n";
        let targets = analyze("p41", build).unwrap();
        // the proc-macro: `--crate-type proc-macro`, host-dylib DefaultInfo.
        let mac = targets.iter().find(|t| t.name == "//app:mac").expect("mac analyzed");
        let margv = &mac.actions[0].argv;
        assert!(margv.contains(&"--crate-type=proc-macro".to_string()), "crate-type: {margv:?}");
        // faithful (P4.2): hashed-output dylib name `lib<name>-<hash>.{dylib,so}`.
        assert!(margv.iter().any(|a| a.starts_with("--codegen=extra-filename=-")), "hashed: {margv:?}");
        assert!(
            mac.default_info.iter().any(|o| o.ends_with(suffix)),
            "proc-macro output is a host dylib ({suffix}): {:?}",
            mac.default_info
        );
        // the dependent `--extern`s the proc-macro DYLIB (via proc_macro_deps), not an rlib.
        let t = targets.iter().find(|t| t.name == "//app:t").expect("t analyzed");
        let targv = &t.actions[0].argv;
        assert!(
            targv.iter().any(|a| a.starts_with("--extern=mac=") && a.ends_with(suffix)),
            "proc-macro dylib --extern'd: {targv:?}"
        );
    }
}

