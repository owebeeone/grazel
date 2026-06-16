#[cfg(test)]
mod tests {
    use crate::rules::*;
    // External types the tests reference directly (the inline `mod tests` saw these via the
    // parent's private imports; as a facade submodule it imports them explicitly).
    use crate::state::GlobalFlags;

    // ── fold_field (F3/F24): the LIVE transitive fold, tested directly (not only via the .bzl). ──

    #[test]
    fn tab_indented_source_parses_like_bazel() {
        // Bazel accepts tab indentation (a tab advances to the next multiple-of-8 column);
        // starlark-rust rejects tabs outright (rules_ml_toolchain's cuda_redist_versions
        // .bzl is tab-indented — the fetch R1 probe wall). Leading tabs expand; tabs
        // inside strings are untouched.
        let src = "def _impl(ctx):\n\treturn [DefaultInfo(files = [\"a\tb\"])]\n\nr = rule(implementation = _impl, attrs = {})\nr(name = \"x\")\n";
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].default_info,
            vec!["a\tb"],
            "string-internal tab preserved"
        );
    }

    // P0.5b: the central capture seam covers the mixed dialect rules, and finalize_edges resolves
    // every edge kind over the live session indexes (a hermetic bare-native corpus — no loads).
    #[test]
    fn p05b_mixed_rule_corpus_captures_the_loading_graph() {
        use crate::loaded::EdgeKind;
        let tmp = std::env::temp_dir().join(format!("razel-p05b-{}", std::process::id()));
        let pkg = tmp.join("q1");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(pkg.join("src.txt"), "").unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "filegroup(name = \"leaf\", srcs = [])\n\
             filegroup(name = \"lib\", srcs = [\":leaf\", \"src.txt\"])\n\
             alias(name = \"lib_alias\", actual = \":lib\")\n\
             config_setting(name = \"dbg\", values = {\"compilation_mode\": \"dbg\"})\n\
             genrule(name = \"gen\", srcs = [\":lib\"], outs = [\"out.txt\"], cmd = \"touch $@\")\n\
             filegroup(name = \"uses_alias\", srcs = [\":lib_alias\"])\n\
             filegroup(name = \"uses_gen_out\", srcs = [\"out.txt\"])\n",
        )
        .unwrap();

        let (session, _r, _l) =
            drive_tree(&tmp, GlobalFlags::default(), &["q1".to_string()], Vec::new(), 1);
        let g = session.loaded_targets.borrow();
        let _ = std::fs::remove_dir_all(&tmp);

        // The central seam captured every rule family with its query-facing rule_class.
        assert_eq!(g.get("//q1:lib").expect("lib").rule_class, "filegroup");
        assert_eq!(g.get("//q1:lib_alias").expect("alias").rule_class, "alias");
        assert_eq!(g.get("//q1:dbg").expect("config_setting").rule_class, "config_setting");
        assert_eq!(g.get("//q1:gen").expect("genrule").rule_class, "genrule");

        let has = |label: &str, kind: EdgeKind, to: &str| {
            g.get(label).unwrap_or_else(|| panic!("{label} not captured")).edges.iter().any(|e| e.kind == kind && e.to == to)
        };
        // R4 classification over the live indexes: Rule / SourceFile / Alias / GeneratedFile.
        assert!(has("//q1:lib", EdgeKind::Rule, "//q1:leaf"), "lib deps -> :leaf is a Rule edge");
        assert!(has("//q1:lib", EdgeKind::SourceFile, "//q1:src.txt"), "lib srcs -> src.txt is a SourceFile edge");
        assert!(has("//q1:uses_alias", EdgeKind::Alias, "//q1:lib_alias"), "ref to an alias is an Alias edge");
        assert!(has("//q1:uses_gen_out", EdgeKind::GeneratedFile, "//q1:out.txt"), "ref to a genrule out is a GeneratedFile edge");
    }

    // P3.1: the @rules_rust//cargo:defs.bzl load surface — a per-crate-style BUILD loading
    // cargo_build_script + cargo_toml_env_vars resolves and loads (stub targets).
    #[test]
    fn p31_cargo_defs_load_surface() {
        let tmp = std::env::temp_dir().join(format!("razel-p31-{}", std::process::id()));
        let pkg = tmp.join("c");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        // P3.5: cargo_toml_env_vars now really reads Cargo.toml (was a stub at P3.1a).
        std::fs::write(pkg.join("Cargo.toml"), "[package]\nname = \"c\"\nversion = \"0.1.0\"\n").unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "load(\"@rules_rust//cargo:defs.bzl\", \"cargo_build_script\", \"cargo_toml_env_vars\")\n\
             cargo_toml_env_vars(name = \"env\", src = \"Cargo.toml\")\n\
             cargo_build_script(name = \"bs\", srcs = [\"build.rs\"], deps = [])\n",
        )
        .unwrap();
        let (_session, report, _) =
            drive_tree(&tmp, GlobalFlags::default(), &["c".to_string()], Vec::new(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
        // the package evaluated: the cargo: loads resolved and the rules were defined.
        assert!(report.iter().all(|(_, r)| r.is_ok()), "cargo:defs.bzl load surface: {report:?}");
    }

    #[test]
    // crate-universe P3.1b: the FULL per-crate generated-BUILD load surface (blake3's three loads):
    // `cargo:defs.bzl`, `rust:defs.bzl`, and `crate_universe/private:selects.bzl` all resolve, and
    // a `rust_library` whose `target_compatible_with` is a bare `select(...)` loads (the select is
    // captured, not resolved at load). Mirrors the real BUILD.bazel in
    // bazel-razel/external/rules_rust++crate+crates__blake3-1.8.2/.
    fn p31b_per_crate_build_loads_with_selects() {
        let tmp = std::env::temp_dir().join(format!("razel-p31b-{}", std::process::id()));
        let pkg = tmp.join("c");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(pkg.join("Cargo.toml"), "").unwrap();
        std::fs::write(pkg.join("lib.rs"), "").unwrap();
        std::fs::write(pkg.join("build.rs"), "").unwrap();
        std::fs::write(
            pkg.join("BUILD"),
            "load(\"@rules_rust//cargo:defs.bzl\", \"cargo_build_script\", \"cargo_toml_env_vars\")\n\
             load(\"@rules_rust//rust:defs.bzl\", \"rust_library\")\n\
             load(\"@rules_rust//crate_universe/private:selects.bzl\", \"selects\")\n\
             cargo_toml_env_vars(name = \"cargo_toml_env_vars\", src = \"Cargo.toml\")\n\
             rust_library(\n\
                 name = \"blake3\",\n\
                 srcs = glob([\"**/*.rs\"], allow_empty = True),\n\
                 deps = [\":build_script_build\"],\n\
                 edition = \"2021\",\n\
                 target_compatible_with = select({\n\
                     \"@rules_rust//rust/platform:x86_64-apple-darwin\": [],\n\
                     \"//conditions:default\": [\"@platforms//:incompatible\"],\n\
                 }),\n\
             )\n\
             cargo_build_script(name = \"_bs\", srcs = glob([\"**/*.rs\"], allow_empty = True), crate_root = \"build.rs\", deps = [])\n\
             alias(name = \"build_script_build\", actual = \":_bs\")\n",
        )
        .unwrap();
        let (_session, report, _) =
            drive_tree(&tmp, GlobalFlags::default(), &["c".to_string()], Vec::new(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(report.iter().all(|(_, r)| r.is_ok()), "blake3 per-crate load surface: {report:?}");
    }

    #[test]
    // crate-universe P3.1b: `selects.with_or` is a real (faithful) body, not just a present symbol —
    // tuple keys fan out to one select arm each, and the result is a usable (deferred) select.
    fn p31b_selects_with_or_builds_a_select() {
        let tmp = std::env::temp_dir().join(format!("razel-p31bw-{}", std::process::id()));
        let pkg = tmp.join("c");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(pkg.join("a.txt"), "").unwrap();
        // Same-package config_settings keep the select resolvable (no external repo); the tuple
        // key `(":x", ":y")` is the point — `with_or` must fan it out to one arm each. Under the
        // default config neither matches, so srcs resolves to the default `["a.txt"]`.
        std::fs::write(
            pkg.join("BUILD"),
            "load(\"@rules_rust//crate_universe/private:selects.bzl\", \"selects\")\n\
             config_setting(name = \"x\", values = {\"compilation_mode\": \"dbg\"})\n\
             config_setting(name = \"y\", values = {\"compilation_mode\": \"opt\"})\n\
             filegroup(name = \"fg\", srcs = selects.with_or({\n\
                 (\":x\", \":y\"): [],\n\
                 \"//conditions:default\": [\"a.txt\"],\n\
             }))\n",
        )
        .unwrap();
        let (_session, report, _) =
            drive_tree(&tmp, GlobalFlags::default(), &["c".to_string()], Vec::new(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
        assert!(report.iter().all(|(_, r)| r.is_ok()), "selects.with_or: {report:?}");
    }

    #[test]
    // crate-universe P3.1d: a build of an ALIAS top-label follows it to its terminal `actual`,
    // loading the actual's package (Bazel builds the actual, not the alias node). Exercised on an
    // external `@crates//:blake3` → `@crates__blake3-1.8.2//:blake3` (apparent; the canonical
    // identity is P3.1e). Hand-materialized external dirs.
    fn p31d_build_follows_external_alias_top_label() {
        let tmp = std::env::temp_dir().join(format!("razel-p31d-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ext = tmp.join("ext");
        std::fs::create_dir_all(ext.join("crates")).unwrap();
        std::fs::create_dir_all(ext.join("crates__blake3-1.8.2")).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(tmp.join("BUILD"), "filegroup(name = \"ws\", srcs = [])\n").unwrap();
        std::fs::write(
            ext.join("crates/BUILD.bazel"),
            "alias(name = \"blake3\", actual = \"@crates__blake3-1.8.2//:blake3\")\n",
        )
        .unwrap();
        std::fs::write(
            ext.join("crates__blake3-1.8.2/BUILD.bazel"),
            "filegroup(name = \"blake3\", srcs = [])\n",
        )
        .unwrap();
        let flags = GlobalFlags { fetched_external_base: Some(ext.clone()), ..Default::default() };
        let targets = analyze_workspace_with(&tmp, "@crates//:blake3", flags).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"@crates__blake3-1.8.2//:blake3"),
            "alias top-label followed to its terminal actual; got {names:?}",
        );
    }

    #[test]
    // crate-universe P3.1e: with the lock seeded, `@crates//:blake3` resolves to its CANONICAL
    // `@@rules_rust++crate+...` identity (§11.3) — double-`@` everywhere. External dirs are
    // canonical-named (the real bazel `external/` layout); the root alias's apparent `actual`
    // (`@crates__blake3-1.8.2//:blake3`) is itself canonicalized on load.
    fn p31e_crates_identity_is_canonical() {
        let tmp = std::env::temp_dir().join(format!("razel-p31e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let ext = tmp.join("ext");
        std::fs::create_dir_all(ext.join("rules_rust++crate+crates")).unwrap();
        std::fs::create_dir_all(ext.join("rules_rust++crate+crates__blake3-1.8.2")).unwrap();
        std::fs::write(tmp.join("MODULE.bazel"), "").unwrap();
        std::fs::write(tmp.join("BUILD"), "filegroup(name = \"ws\", srcs = [])\n").unwrap();
        std::fs::write(
            ext.join("rules_rust++crate+crates/BUILD.bazel"),
            "alias(name = \"blake3\", actual = \"@crates__blake3-1.8.2//:blake3\")\n",
        )
        .unwrap();
        std::fs::write(
            ext.join("rules_rust++crate+crates__blake3-1.8.2/BUILD.bazel"),
            "filegroup(name = \"blake3\", srcs = [])\n",
        )
        .unwrap();
        let repo = crate::lock::CrateRepo {
            urls: vec![],
            sha256: String::new(),
            strip_prefix: None,
            build_file_content: String::new(),
            remote_patch_strip: None,
            archive_type: None,
        };
        let mut crates = std::collections::BTreeMap::new();
        crates.insert("crates__blake3-1.8.2".to_string(), repo);
        let lock = crate::lock::CrateLock {
            version: 26,
            root_contents: std::collections::BTreeMap::new(),
            crates,
            recorded_inputs: vec![],
            canonical_prefix: "rules_rust++crate+".to_string(),
        };
        let flags = GlobalFlags {
            fetched_external_base: Some(ext.clone()),
            crate_lock: Some(std::sync::Arc::new(lock)),
            ..Default::default()
        };
        let targets = analyze_workspace_with(&tmp, "@crates//:blake3", flags).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"@@rules_rust++crate+crates__blake3-1.8.2//:blake3"),
            "expected canonical @@ identity; got {names:?}",
        );
    }

    #[test]
    fn starlark_rule_analyzes_by_running_its_impl() {
        let src = r#"
def _impl(ctx):
    out = ctx.actions.declare_file(ctx.attr.name + ".o")
    ctx.actions.run(
        executable = "cc",
        outputs = [out],
        inputs = [ctx.attr.src],
        arguments = ["-c", ctx.attr.src],
    )
    return [DefaultInfo(files = [out])]

cc_thing = rule(implementation = _impl, attrs = {"src": 1})
cc_thing(name = "widget", src = "widget.c")
cc_thing(name = "gadget", src = "gadget.c")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert_eq!(targets.len(), 2);
        let w = &targets[0];
        assert_eq!(w.name, "widget");
        assert_eq!(w.actions.len(), 1);
        assert_eq!(w.actions[0].mnemonic, "cc");
        assert_eq!(w.actions[0].inputs, vec!["widget.c"]);
        assert_eq!(w.actions[0].outputs, vec!["widget.o"]);
        assert_eq!(w.default_info, vec!["widget.o"]);
        assert_eq!(targets[1].name, "gadget");
    }

    #[test]
    fn select_picks_default_branch() {
        // razelV3: conditions must be DECLARED config_settings (the stub tolerated unknowns);
        // under the default config (fastbuild) the non-matching :dbg falls through to default.
        // Round 40: select sits on the ATTR (Bazel's model — selects are attr values, never
        // impl-time expressions; the retired eager hybrid had let the impl-time form work).
        let src = r#"
config_setting(name = "dbg", values = {"compilation_mode": "dbg"})

def _impl(ctx):
    ctx.actions.run(executable = "cc", outputs = [ctx.attr.name], inputs = [], arguments = ctx.attr.flags)
    return [DefaultInfo(files = [ctx.attr.name])]

thing = rule(implementation = _impl, attrs = {"flags": attr.string_list()})
thing(name = "x", flags = select({"//conditions:default": ["-O2"], ":dbg": ["-g"]}))
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let x = targets.iter().find(|t| t.name.ends_with("x")).unwrap();
        assert_eq!(x.actions[0].mnemonic, "cc");
        assert!(
            x.actions[0].argv.contains(&"-O2".to_string()),
            "default branch picked"
        );
    }

    #[test]
    fn dependent_reads_dep_providers_two_phase() {
        // lib declared first; bin's deps=[":lib"] reads lib's analyzed DefaultInfo.
        let src = r#"
def _lib(ctx):
    out = "lib" + ctx.attr.name + ".a"
    ctx.actions.run(executable = "ar", outputs = [out], inputs = [], arguments = ["rcs", out])
    return [DefaultInfo(files = [out])]

def _bin(ctx):
    libs = []
    for d in ctx.attr.deps:
        libs = libs + d.files
    out = ctx.attr.name
    ctx.actions.run(executable = "cc", outputs = [out], inputs = libs, arguments = ["-o", out] + libs)
    return [DefaultInfo(files = [out])]

lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})

lib_rule(name = "math")
bin_rule(name = "app", deps = [":math"])
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        let app = targets.iter().find(|t| t.name == "app").unwrap();
        assert_eq!(app.deps, vec!["math"]);
        // app linked the dep's analyzed output — the provider flowed across targets.
        assert_eq!(app.actions[0].inputs, vec!["libmath.a"]);
        assert!(app.actions[0].argv.contains(&"libmath.a".to_string()));
    }

    #[test]
    fn forward_dep_reference_analyzes() {
        // E0: bin declared before its dep → the demand-driven pass analyzes :math first.
        // (Inverts the pre-E0 pin that forward refs must error — RazelV3Plan §2.)
        let src = r#"
def _lib(ctx):
    return [DefaultInfo(files = ["x"])]
def _bin(ctx):
    return [DefaultInfo(files = ctx.attr.deps[0].files)]
lib_rule = rule(implementation = _lib, attrs = {})
bin_rule = rule(implementation = _bin, attrs = {})
bin_rule(name = "app", deps = [":math"])
lib_rule(name = "math")
"#;
        let targets = analyze_starlark("BUILD", src).unwrap();
        assert!(targets.iter().any(|t| t.name.ends_with("app")));
    }
}

