//! S5 burn-down: `cc_test` — a cc executable with test semantics (the protocol is the
//! `test` verb's; analysis-wise it is cc_binary). Gates configurations/cc_test +
//! rules_cc trees in the survey.

use razel_loading::{GlobalFlags, analyze_workspace_with};

fn write(path: &std::path::Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[test]
fn cc_test_analyzes_like_a_binary() {
    let ws = std::env::temp_dir().join(format!("razel-cctest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    write(
        &ws.join("t/BUILD"),
        "load(\"@rules_cc//cc:cc_test.bzl\", \"cc_test\")\n\
         cc_test(name = \"sm\", srcs = [\"sm.cc\"])\n",
    );
    write(&ws.join("t/sm.cc"), "int main() { return 0; }\n");
    let targets = analyze_workspace_with(&ws, "//t:sm", GlobalFlags::default()).unwrap();
    let t = targets.iter().find(|t| t.name == "//t:sm").expect("target");
    assert_eq!(t.default_info, vec!["t/sm".to_string()], "the test executable");
    assert!(t.actions.iter().any(|a| a.mnemonic == "CppLink"));
    let _ = std::fs::remove_dir_all(&ws);
}
