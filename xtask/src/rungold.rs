//! The L4 RUN-golden: analyze a real upstream cc target and EXECUTE its action set —
//! the analyzes→builds ratchet. Outputs must exist afterwards (loud on any failure).

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::Path;
use std::process::Command;

/// A fresh EXECROOT sandbox: workspace entries symlinked in, `external/` mapping every
/// vendored repo (both `_`/`-` dir forms). Actions run here; the vendored tree stays clean.
fn execroot(root: &Path, ws: &Path, name: &str) -> Result<std::path::PathBuf, String> {
    let er = std::env::temp_dir().join(format!("razel-rungold-{name}"));
    let _ = std::fs::remove_dir_all(&er);
    std::fs::create_dir_all(er.join("external")).map_err(|e| e.to_string())?;
    let link = |src: &Path, dst: &Path| -> Result<(), String> {
        std::os::unix::fs::symlink(src, dst).map_err(|e| format!("{}: {e}", dst.display()))
    };
    for e in std::fs::read_dir(ws).map_err(|e| e.to_string())?.flatten() {
        link(&e.path(), &er.join(e.file_name()))?;
    }
    let tp = root.join("../third-party");
    for e in std::fs::read_dir(&tp).map_err(|e| e.to_string())?.flatten() {
        if e.path().is_dir() {
            let n = e.file_name().to_string_lossy().to_string();
            for alias in [n.clone(), n.replace('-', "_")] {
                let dst = er.join("external").join(&alias);
                if !dst.exists() {
                    let _ = link(&e.path(), &dst);
                }
            }
        }
    }
    Ok(er)
}

pub(crate) fn rungold(root: &Path) -> Result<(), String> {
    // The abseil leaf: real upstream BUILD, real rules_cc impl, razel's host cc_common.compile.
    let ws = root.join("../third-party/com_google_absl");
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    let target = "//absl/base:log_severity";
    let targets = analyze_workspace_with(&ws, target, flags)
        .map_err(|e| format!("analysis failed: {e}"))?;
    let t = targets
        .iter()
        .find(|t| t.name.ends_with(":log_severity"))
        .ok_or("target not analyzed")?;
    if t.actions.is_empty() {
        let with_actions: Vec<String> = targets
            .iter()
            .filter(|x| !x.actions.is_empty())
            .map(|x| format!("{} ({})", x.name, x.actions.len()))
            .collect();
        return Err(format!(
            "no actions on {} — {} targets analyzed, with actions: {:?}",
            t.name,
            targets.len(),
            &with_actions[..with_actions.len().min(8)]
        ));
    }
    let er = execroot(root, &ws, "absl")?;
    let mut ran = 0;
    for a in &t.actions {
        for o in &a.outputs {
            if let Some(dir) = Path::new(o).parent() {
                std::fs::create_dir_all(er.join(dir)).map_err(|e| e.to_string())?;
            }
        }
        // Tool resolution at the RUN boundary: the Constrain config carries Bazel's
        // `external/<repo>/cc_wrapper.sh` (itself a clang wrapper) — the host executor
        // resolves it to the real driver. (Executor-level tool resolution; registered debt.)
        let exe = if a.argv[0].ends_with("cc_wrapper.sh") { "cc" } else { a.argv[0].as_str() };
        let st = Command::new(exe)
            .args(&a.argv[1..])
            .current_dir(&er)
            .output()
            .map_err(|e| format!("{} spawn failed: {e}", a.argv[0]))?;
        if !st.status.success() {
            return Err(format!(
                "action `{}` failed ({}):\nargv: {:?}\nstderr: {}",
                a.mnemonic,
                st.status,
                a.argv,
                String::from_utf8_lossy(&st.stderr)
            ));
        }
        for o in &a.outputs {
            if !er.join(o).exists() {
                return Err(format!("action `{}` did not produce `{o}`", a.mnemonic));
            }
        }
        ran += 1;
    }
    println!("xtask rungold: OK — {ran} real actions executed for {target} (outputs verified)");

    // The RUST golden: tinyjson (a real crates.io package) through real rules_rust.
    let ws = root.join("../third-party/rules_rust_tinyjson");
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    let target = "//:tinyjson";
    let targets = analyze_workspace_with(&ws, target, flags)
        .map_err(|e| format!("rust analysis failed: {e}"))?;
    let t = targets
        .iter()
        .find(|t| t.name.ends_with(":tinyjson"))
        .ok_or("tinyjson not analyzed")?;
    if t.actions.is_empty() {
        return Err("no rust actions".into());
    }
    let er = execroot(root, &ws, "rust")?;
    let mut ran = 0;
    for a in &t.actions {
        // Run-boundary tool resolution: a None bootstrap process_wrapper means DIRECT rustc
        // (`None -- rustc args…` → `rustc args…`).
        let argv: Vec<&str> = match a.argv.as_slice() {
            [n, sep, rest @ ..] if n == "None" && sep == "--" => {
                rest.iter().map(|s| s.as_str()).collect()
            }
            all => all.iter().map(|s| s.as_str()).collect(),
        };
        for o in &a.outputs {
            if let Some(dir) = Path::new(o).parent() {
                std::fs::create_dir_all(er.join(dir)).map_err(|e| e.to_string())?;
            }
        }
        let st = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(&er)
            .output()
            .map_err(|e| format!("{} spawn failed: {e}", argv[0]))?;
        if !st.status.success() {
            return Err(format!(
                "rust action `{}` failed:
argv: {:?}
stderr: {}",
                a.mnemonic,
                argv,
                String::from_utf8_lossy(&st.stderr)
            ));
        }
        ran += 1;
    }
    println!("xtask rungold: OK — {ran} real rust actions executed for {target}");
    Ok(())
}
