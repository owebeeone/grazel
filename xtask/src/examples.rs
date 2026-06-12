//! `examples [--capture]` (S4b, ws-razel/RazelReleaseSpike §2): the EXAMPLES-AS-GOLDENS
//! harness — bazelbuild/examples workspaces as the low bar, graph + output parity.
//!
//! CAPTURE (bazel-touching, authoring-only — like `capture-goldens`): per example,
//! `bazel aquery deps(<target>)` → `razel_parity::normalize` → `parity/examples/
//! <name>/graph.golden`, and `bazel run <target>` stdout → time-masked →
//! `…/stdout.golden`. Bazel is the oracle at capture time only.
//!
//! VERIFY (razel-only — CI, joins the probe): per example, BOTH in default mode and
//! `--strict_bazel`:
//!   - graph: `analyze_workspace_with` in Adopt-Bazel toolchain mode → normalized
//!     action set → `razel_parity::diff` vs graph.golden (deviations are per-example
//!     OMIT lists, documented here — never silent).
//!   - output: Native-mode build + exec the DefaultInfo binary → time-masked stdout
//!     vs stdout.golden (byte compare).

use razel_loading::{CcToolchainMode, GlobalFlags, analyze_workspace_with};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The corpus: (name, workspace dir under third-party/examples, target).
const EXAMPLES: &[(&str, &str, &str)] = &[
    ("cpp-tutorial-stage1", "cpp-tutorial/stage1", "//main:hello-world"),
    ("cpp-tutorial-stage2", "cpp-tutorial/stage2", "//main:hello-world"),
    ("cpp-tutorial-stage3", "cpp-tutorial/stage3", "//main:hello-world"),
];

/// Per-example mnemonics razel deliberately does not model (documented deviations —
/// the diff lists them as `omitted`, never silently; rationale in
/// parity/examples/README.md):
/// - CppModuleMap: the corpus-wide omit (razel models no module maps).
/// - SourceSymlinkManifest/SymlinkTree/RepoMappingManifest: bazel's runfiles
///   plumbing — razel's runfiles model is the spike's later runfiles step.
/// - CcStrip + FileWrite(.dwp): bazel's strip/fission siblings of the binary.
/// - TranslateBuildInfo/Symlink: bazel's build-info stamping (volatile/redacted).
fn omit_for(_name: &str) -> &'static [&'static str] {
    &[
        "CppModuleMap",
        "SourceSymlinkManifest",
        "SymlinkTree",
        "RepoMappingManifest",
        "CcStrip",
        "FileWrite",
        "TranslateBuildInfo",
        "Symlink",
    ]
}

fn examples_root(repo_root: &Path) -> PathBuf {
    repo_root.join("../third-party/examples")
}

fn goldens_dir(repo_root: &Path, name: &str) -> PathBuf {
    repo_root.join("parity/examples").join(name)
}

/// Mask wall-clock lines (the cpp-tutorial binary prints ctime): any line carrying a
/// 4-digit year token AND a `:`-separated clock becomes `<TIME>`.
fn mask_time(stdout: &str) -> String {
    stdout
        .lines()
        .map(|l| {
            let clocky = l.matches(':').count() >= 2
                && l.split_whitespace().any(|t| t.len() == 4 && t.chars().all(|c| c.is_ascii_digit()));
            if clocky { "<TIME>" } else { l }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// CAPTURE: bazel as the oracle, results committed as goldens.
pub(crate) fn capture(repo_root: &Path) -> Result<(), String> {
    let bazel = std::env::var("BAZEL").unwrap_or_else(|_| "bazel".into());
    let ob = std::env::var("RAZEL_GOLDEN_OB").unwrap_or_else(|_| "/tmp/razel-examples-ob".into());
    for (name, ws_rel, target) in EXAMPLES {
        let ws = examples_root(repo_root).join(ws_rel);
        let dir = goldens_dir(repo_root, name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        eprintln!("capturing {name} ({target}) …");
        // Graph golden.
        let out = Command::new(&bazel)
            .current_dir(&ws)
            .arg(format!("--output_base={ob}"))
            .args(["aquery", &format!("deps({target})"), "--output=text", "--noshow_progress"])
            .output()
            .map_err(|e| format!("spawn bazel: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{name}: bazel aquery failed:\n{}",
                String::from_utf8_lossy(&out.stderr).trim_end()
            ));
        }
        let graph =
            razel_parity::normalize(&crate::filter_aquery(&String::from_utf8_lossy(&out.stdout)));
        std::fs::write(dir.join("graph.golden"), &graph).map_err(|e| e.to_string())?;
        // Stdout golden (bazel run — the program's own output, engine-neutral).
        let out = Command::new(&bazel)
            .current_dir(&ws)
            .arg(format!("--output_base={ob}"))
            .args(["run", "--noshow_progress", target])
            .output()
            .map_err(|e| format!("spawn bazel run: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "{name}: bazel run failed:\n{}",
                String::from_utf8_lossy(&out.stderr).trim_end()
            ));
        }
        let stdout = mask_time(String::from_utf8_lossy(&out.stdout).trim_end());
        std::fs::write(dir.join("stdout.golden"), format!("{stdout}\n"))
            .map_err(|e| e.to_string())?;
        eprintln!("  wrote {}/{{graph,stdout}}.golden", dir.display());
    }
    Ok(())
}

/// VERIFY: razel-only — graph parity (Adopt-Bazel mode) + run-output parity (Native),
/// each in default AND strict_bazel modes. Returns the failure list.
pub(crate) fn verify(repo_root: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, ws_rel, target) in EXAMPLES {
        let ws = examples_root(repo_root).join(ws_rel);
        let dir = goldens_dir(repo_root, name);
        let Ok(graph_golden) = std::fs::read_to_string(dir.join("graph.golden")) else {
            failures.push(format!("{name}: no graph.golden (run `cargo xtask examples --capture`)"));
            continue;
        };
        let golden = razel_parity::parse_golden(&graph_golden);
        for strict in [false, true] {
            let mode = if strict { "strict" } else { "default" };
            // Graph parity in Adopt-Bazel toolchain mode (bazel's declared graph).
            let flags = GlobalFlags {
                cc_toolchain: CcToolchainMode::AdoptBazel,
                strict_bazel: strict,
                ..Default::default()
            };
            match analyze_workspace_with(&ws, target, flags) {
                Err(e) => failures.push(format!("{name}[{mode}]: analyze failed: {e}")),
                Ok(targets) => {
                    let n = |s: &str| razel_parity::normalize(s).trim_end().to_string();
                    let razel: Vec<razel_parity::Action> = targets
                        .iter()
                        .flat_map(|t| t.actions.iter())
                        .map(|a| {
                            let mut inputs: Vec<String> = a.inputs.iter().map(|s| n(s)).collect();
                            inputs.sort();
                            let mut outputs: Vec<String> =
                                a.outputs.iter().map(|s| n(s)).collect();
                            outputs.sort();
                            razel_parity::Action {
                                mnemonic: a.mnemonic.clone(),
                                argv: a.argv.iter().map(|s| n(s)).collect(),
                                inputs,
                                outputs,
                            }
                        })
                        .collect();
                    let report = razel_parity::diff(&razel, &golden, omit_for(name));
                    // DOCUMENTED argv deviation (never silent — logged each run):
                    // CppCompile carries bazel's bzlmod -iquote set (external/rules_cc+,
                    // external/bazel_tools + their bin twins), which is module-graph-
                    // derived; razel's include model grows it with the bzlmod arc.
                    // Allowlisted: argv-only CppCompile mismatches (inputs equal).
                    let (allowed, real): (Vec<_>, Vec<_>) =
                        report.mismatched.iter().partition(|m| {
                            m.key.starts_with("CppCompile") && !m.inputs_differ
                        });
                    for m in &allowed {
                        eprintln!(
                            "  examples[{name},{mode}]: DOCUMENTED argv deviation \
                             (bzlmod -iquote set): {}",
                            m.key
                        );
                    }
                    if !report.missing.is_empty()
                        || !report.extra.is_empty()
                        || !real.is_empty()
                    {
                        failures.push(format!(
                            "{name}[{mode}]: graph diverges (beyond documented \
                             deviations):\n{report:#?}"
                        ));
                    }
                }
            }
        }
        // Output parity: Native build + exec, time-masked stdout vs golden.
        let Ok(stdout_golden) = std::fs::read_to_string(dir.join("stdout.golden")) else {
            failures.push(format!("{name}: no stdout.golden"));
            continue;
        };
        let cache_dir = std::env::temp_dir().join(format!("razel-examples-cache-{name}"));
        let _ = std::fs::remove_dir_all(&cache_dir);
        match razel_exec_run(&ws, target, &cache_dir) {
            Err(e) => failures.push(format!("{name}: razel build+run failed: {e}")),
            Ok(stdout) => {
                let got = format!("{}\n", mask_time(stdout.trim_end()));
                if got != stdout_golden {
                    failures.push(format!(
                        "{name}: stdout diverges\n--- golden ---\n{stdout_golden}--- razel ---\n{got}"
                    ));
                }
            }
        }
    }
    failures
}

/// Build via razel (Native) and exec the DefaultInfo binary, capturing stdout.
fn razel_exec_run(ws: &Path, target: &str, cache_dir: &Path) -> Result<String, String> {
    let cache = razel_exec_cache(cache_dir)?;
    let report = razel_build::build_workspace_with(ws, target, &cache, GlobalFlags::default())?;
    let bin = report
        .default_outputs
        .first()
        .ok_or_else(|| "no default output".to_string())?;
    let out = Command::new(ws.join(bin))
        .current_dir(ws)
        .output()
        .map_err(|e| format!("exec {bin}: {e}"))?;
    if !out.status.success() {
        return Err(format!("{bin} exited {:?}", out.status.code()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn razel_exec_cache(dir: &Path) -> Result<razel_exec::Cache, String> {
    razel_exec::Cache::new(dir).map_err(|e| e.to_string())
}

/// `examples --survey` (S5/Gianni 2026-06-13: "all the blaze examples" as a tracked
/// burn-down): walk EVERY workspace under third-party/examples, load+analyze all its
/// packages, and write the per-workspace status table to parity/examples/SURVEY.md.
/// The build/run/test/affected columns are the CURATED goldens corpus's job (the
/// EXAMPLES table above — verbs run per curated target); the survey is the FRONTIER
/// view: which workspaces load at all, and the first error per red one (the ticket).
pub(crate) fn survey(repo_root: &Path) -> Result<String, String> {
    let root = examples_root(repo_root);
    // Workspace roots: any dir carrying a boundary file (nested ones are their own rows).
    let mut roots = Vec::new();
    find_workspace_roots(&root, &mut roots);
    roots.sort();
    let mut md = String::from(
        "# Examples survey — the burn-down (generated: `cargo xtask examples --survey`)\n\n\
         Frontier view: load+analyze per workspace (the curated goldens corpus in\n\
         examples.rs owns the per-verb columns for its members). Re-run to refresh.\n\n\
         | workspace | pkgs | green | first error |\n|---|---|---|---|\n",
    );
    for ws in &roots {
        let rel = ws.strip_prefix(&root).unwrap_or(ws).to_string_lossy().to_string();
        let mut pkgs = Vec::new();
        find_packages(ws, ws, &mut pkgs);
        pkgs.sort();
        if pkgs.is_empty() {
            md.push_str(&format!("| {rel} | 0 | – | (no BUILD packages) |\n"));
            continue;
        }
        let report = razel_loading::load_tree_report(ws, Default::default(), &pkgs);
        let green = report.iter().filter(|(_, r)| r.is_ok()).count();
        let first_err = report
            .iter()
            .find_map(|(p, r)| r.as_ref().err().map(|e| {
                let line = e.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                format!("`{p}`: {}", line.chars().take(90).collect::<String>())
            }))
            .unwrap_or_else(|| "—".into());
        md.push_str(&format!("| {rel} | {} | {green} | {first_err} |\n", pkgs.len()));
        eprintln!("survey {rel}: {green}/{} packages green", pkgs.len());
    }
    Ok(md)
}

fn find_workspace_roots(dir: &Path, out: &mut Vec<PathBuf>) {
    let boundary = ["MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel"]
        .iter()
        .any(|f| dir.join(f).is_file());
    if boundary {
        out.push(dir.to_path_buf());
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            // Skip bazel's output/convenience symlink trees (bazel-bin, bazel-<ws>…)
            // and dotdirs — they are not corpus.
            let keep = p.is_dir()
                && p.file_name().is_some_and(|n| {
                    let n = n.to_string_lossy();
                    !n.starts_with("bazel-") && !n.starts_with('.')
                });
            if keep {
                find_workspace_roots(&p, out);
            }
        }
    }
}

/// Packages of ONE workspace: dirs with BUILD[.bazel], not crossing into nested
/// workspace roots.
fn find_packages(dir: &Path, ws_root: &Path, out: &mut Vec<String>) {
    if dir != ws_root
        && ["MODULE.bazel", "WORKSPACE", "WORKSPACE.bazel"].iter().any(|f| dir.join(f).is_file())
    {
        return; // nested workspace — its own survey row
    }
    if ["BUILD", "BUILD.bazel"].iter().any(|f| dir.join(f).is_file()) {
        out.push(dir.strip_prefix(ws_root).unwrap_or(dir).to_string_lossy().replace('\\', "/"));
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && p.file_name().is_some_and(|n| !n.to_string_lossy().starts_with("bazel-")) {
                find_packages(&p, ws_root, out);
            }
        }
    }
}

pub(crate) fn run_command(repo_root: &Path, capture_mode: bool) -> std::process::ExitCode {
    if capture_mode {
        return match capture(repo_root) {
            Ok(()) => {
                eprintln!("examples: goldens captured.");
                std::process::ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("examples --capture: FAIL — {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let failures = verify(repo_root);
    if failures.is_empty() {
        eprintln!("examples: OK — graph + stdout goldens green (default + strict).");
        std::process::ExitCode::SUCCESS
    } else {
        for f in &failures {
            eprintln!("EXAMPLES FAIL: {f}");
        }
        eprintln!("\nexamples: {} failure(s).", failures.len());
        std::process::ExitCode::FAILURE
    }
}
