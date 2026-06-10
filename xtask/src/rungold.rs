//! The L4 RUN-golden: analyze a real upstream cc target and EXECUTE its action set —
//! the analyzes→builds ratchet. Outputs must exist afterwards (loud on any failure).

use razel_loading::{GlobalFlags, analyze_workspace_with};
use std::path::Path;
use std::process::Command;

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
    let mut ran = 0;
    for a in &t.actions {
        for o in &a.outputs {
            if let Some(dir) = Path::new(o).parent() {
                std::fs::create_dir_all(ws.join(dir)).map_err(|e| e.to_string())?;
            }
        }
        // Tool resolution at the RUN boundary: the Constrain config carries Bazel's
        // `external/<repo>/cc_wrapper.sh` (itself a clang wrapper) — the host executor
        // resolves it to the real driver. (Executor-level tool resolution; registered debt.)
        let exe = if a.argv[0].ends_with("cc_wrapper.sh") { "cc" } else { a.argv[0].as_str() };
        let st = Command::new(exe)
            .args(&a.argv[1..])
            .current_dir(&ws)
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
            if !ws.join(o).exists() {
                return Err(format!("action `{}` did not produce `{o}`", a.mnemonic));
            }
        }
        ran += 1;
    }
    println!("xtask rungold: OK — {ran} real actions executed for {target} (outputs verified)");
    Ok(())
}
