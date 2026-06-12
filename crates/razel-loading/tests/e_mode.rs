//! S1 (V3sh1, ws-razel/RazelReleaseSpike §5): E-mode core — `BUILD.razel` as a package's
//! SOLE grammar (design E, spike §3c), the XOR rule, `--strict_bazel` invisibility, the
//! `MODULE.razel` boundary walk, and the `.bazelignore` boundary guard (warning at S1).
//! Test-first.

use razel_loading::{
    GlobalFlags, analyze_workspace_with, e_mode_guard, find_workspace_root,
};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("razel-emode-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

// ── package-file resolution ──────────────────────────────────────────────────

/// Ground truth (bazel, verified 2026-06-12: both files in one package, `query //p:all`
/// returns the BUILD.bazel target): **BUILD.bazel wins over BUILD**. razel's historical
/// probe order was backwards; pinned here.
#[test]
fn build_bazel_preferred_over_build() {
    let ws = fixture("prec");
    write(&ws.join("p/BUILD"), "filegroup(name = \"from_build\")\n");
    write(&ws.join("p/BUILD.bazel"), "filegroup(name = \"from_build_bazel\")\n");
    let targets = analyze_workspace_with(&ws, "//p:from_build_bazel", GlobalFlags::default())
        .expect("BUILD.bazel must be the package file when both exist");
    assert!(targets.iter().any(|t| t.name == "//p:from_build_bazel"));
    let _ = std::fs::remove_dir_all(&ws);
}

/// A `BUILD.razel` package loads and analyzes exactly like a BUILD package (E-mode:
/// same grammar, razel-only visibility).
#[test]
fn build_razel_package_loads_and_analyzes() {
    let ws = fixture("emode");
    write(&ws.join("a/BUILD.razel"), "filegroup(name = \"a\", srcs = [\"f.txt\"])\n");
    write(&ws.join("a/f.txt"), "");
    let targets = analyze_workspace_with(&ws, "//a:a", GlobalFlags::default()).unwrap();
    let a = targets.iter().find(|t| t.name == "//a:a").expect("E-package target");
    assert!(a.default_info.iter().any(|f| f == "a/f.txt"));
    let _ = std::fs::remove_dir_all(&ws);
}

/// The XOR rule: `BUILD.razel` next to `BUILD` (or `BUILD.bazel`) is a LOUD error —
/// coexistence is design A/B smuggled back in (spike §3c).
#[test]
fn coexistence_with_build_is_an_error() {
    let ws = fixture("xor1");
    write(&ws.join("a/BUILD.razel"), "filegroup(name = \"a\")\n");
    write(&ws.join("a/BUILD"), "filegroup(name = \"a\")\n");
    let err = analyze_workspace_with(&ws, "//a:a", GlobalFlags::default())
        .expect_err("coexistence must error");
    assert!(err.contains("BUILD.razel"), "error must name the conflict: {err}");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn coexistence_with_build_bazel_is_an_error() {
    let ws = fixture("xor2");
    write(&ws.join("a/BUILD.razel"), "filegroup(name = \"a\")\n");
    write(&ws.join("a/BUILD.bazel"), "filegroup(name = \"a\")\n");
    let err = analyze_workspace_with(&ws, "//a:a", GlobalFlags::default())
        .expect_err("coexistence must error");
    assert!(err.contains("BUILD.razel"), "error must name the conflict: {err}");
    let _ = std::fs::remove_dir_all(&ws);
}

// ── --strict_bazel (spike §3d: acts as if it IS bazel) ──────────────────────

/// Strict mode makes E-packages INVISIBLE: a BUILD.razel-only dir is not a package.
#[test]
fn strict_bazel_hides_e_packages() {
    let ws = fixture("strict1");
    write(&ws.join("a/BUILD.razel"), "filegroup(name = \"a\")\n");
    let flags = GlobalFlags { strict_bazel: true, ..GlobalFlags::default() };
    let err = analyze_workspace_with(&ws, "//a:a", flags)
        .expect_err("strict mode must not see the E-package");
    assert!(err.contains("no BUILD"), "must read as a missing package, not a razel error: {err}");
    let _ = std::fs::remove_dir_all(&ws);
}

/// Strict mode ignores `.razel` files ENTIRELY — coexistence is not an error (bazel
/// wouldn't error), the bazel-grammar file simply serves the package.
#[test]
fn strict_bazel_coexistence_serves_the_bazel_file() {
    let ws = fixture("strict2");
    write(&ws.join("a/BUILD.razel"), "filegroup(name = \"razel_only\")\n");
    write(&ws.join("a/BUILD"), "filegroup(name = \"from_bazel\")\n");
    let flags = GlobalFlags { strict_bazel: true, ..GlobalFlags::default() };
    let targets = analyze_workspace_with(&ws, "//a:from_bazel", flags).unwrap();
    assert!(targets.iter().any(|t| t.name == "//a:from_bazel"));
    let _ = std::fs::remove_dir_all(&ws);
}

// ── boundary walk-up (MODULE.razel joins the marker set) ─────────────────────

#[test]
fn module_razel_marks_a_workspace_boundary() {
    let root = fixture("walk1");
    write(&root.join("MODULE.razel"), "");
    let nested = root.join("a/b");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(find_workspace_root(&nested, false), Some(root.clone()));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn strict_bazel_walkup_ignores_module_razel() {
    let root = fixture("walk2");
    write(&root.join("MODULE.razel"), "");
    let nested = root.join("a/b");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(find_workspace_root(&nested, true), None, "strict: not a bazel boundary");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn bazel_markers_bound_in_both_modes() {
    for marker in ["MODULE.bazel", "REPO.bazel", "WORKSPACE", "WORKSPACE.bazel"] {
        let root = fixture(&format!("walk-{}", marker.to_lowercase().replace('.', "-")));
        write(&root.join(marker), "");
        let nested = root.join("x");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(find_workspace_root(&nested, false), Some(root.clone()), "{marker}");
        assert_eq!(find_workspace_root(&nested, true), Some(root.clone()), "{marker} strict");
        let _ = std::fs::remove_dir_all(&root);
    }
}

/// The NEAREST boundary wins (an inner razel-native module bounds before an outer
/// bazel workspace).
#[test]
fn nearest_boundary_wins() {
    let outer = fixture("walk3");
    write(&outer.join("WORKSPACE"), "");
    let inner = outer.join("mod");
    write(&inner.join("MODULE.razel"), "");
    let nested = inner.join("p");
    std::fs::create_dir_all(&nested).unwrap();
    assert_eq!(find_workspace_root(&nested, false), Some(inner.clone()));
    // strict walks PAST the razel module to the bazel boundary.
    assert_eq!(find_workspace_root(&nested, true), Some(outer.clone()));
    let _ = std::fs::remove_dir_all(&outer);
}

// ── the boundary guard (spike §3c rule 2; WARNING at S1, ERROR at S3) ────────

/// In a DUAL workspace (bazel boundary grammar at the root), an E-package must live
/// under a `.bazelignore`d subtree — otherwise bazel sees an unignored dir whose
/// package status the engines disagree on. S1: warn.
#[test]
fn dual_workspace_unignored_e_package_warns() {
    let w = e_mode_guard(true, Some("other/dir\n"), "srv/app");
    assert!(w.is_some(), "unignored E-package in a dual workspace must warn");
    assert!(w.unwrap().contains("srv/app"));
}

#[test]
fn bazelignored_subtree_is_quiet() {
    assert_eq!(e_mode_guard(true, Some("srv\nother\n"), "srv/app"), None);
    assert_eq!(e_mode_guard(true, Some("srv/app\n"), "srv/app"), None);
}

#[test]
fn razel_native_module_needs_no_guard() {
    // No bazel grammar at the root ⇒ bazel can't open the workspace at all ⇒ no
    // divergence to guard against.
    assert_eq!(e_mode_guard(false, None, "srv/app"), None);
}

#[test]
fn dual_workspace_without_bazelignore_warns() {
    assert!(e_mode_guard(true, None, "srv/app").is_some());
}
