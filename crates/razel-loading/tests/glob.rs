//! glob() package-boundary behavior.

use razel_loading::{GlobalFlags, analyze_workspace_with};

#[test]
fn glob_does_not_cross_into_subpackages() {
    let root = std::env::temp_dir().join(format!("razel-glob-subpkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    std::fs::create_dir_all(root.join("p/sub")).unwrap();
    std::fs::write(
        root.join("p/BUILD"),
        r#"filegroup(name = "g", srcs = glob(["**/*.txt"]))"#,
    )
    .unwrap();
    std::fs::write(root.join("p/a.txt"), "").unwrap();
    std::fs::write(root.join("p/sub/BUILD"), "filegroup(name = \"nested\")\n").unwrap();
    std::fs::write(root.join("p/sub/b.txt"), "").unwrap();

    let res = analyze_workspace_with(&root, "//p:g", GlobalFlags::default());
    let _ = std::fs::remove_dir_all(&root);

    let targets = res.unwrap();
    let g = targets.iter().find(|t| t.name == "//p:g").unwrap();
    assert_eq!(
        g.default_info,
        ["p/a.txt"],
        "Bazel glob() does not include files from nested packages"
    );
}
