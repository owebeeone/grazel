//! Workspace-boundary + package-file resolution tests (formerly `e_mode.rs`).
//! `BUILD.razel` is gone; what remains is BUILD.bazel-over-BUILD precedence and the
//! (dormant) `MODULE.razel` boundary walk that `find_workspace_root` still supports.

use razel_loading::{GlobalFlags, analyze_workspace_with, find_workspace_root};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn fixture(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("razel-ws-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

// ── package-file resolution ──────────────────────────────────────────────────

/// Ground truth (bazel, verified 2026-06-12: both files in one package, `query //p:all`
/// returns the BUILD.bazel target): **BUILD.bazel wins over BUILD**.
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

// ── boundary walk-up (MODULE.razel = the dormant razel-native module marker) ──

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
