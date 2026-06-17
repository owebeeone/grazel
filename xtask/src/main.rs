//! `cargo xtask` — project automation.
//!
//! `codegen [--check]`  regenerate `crates/razel-wire/src/{generated,cbor}.rs` from
//!                      the taut IR (types + codec, and the vendored CBOR runtime)
//! `corpus  [--check]`  regenerate `crates/razel-wire/src/vectors.rs` — the golden
//!                      cross-language corpus, encoded by taut's Python codec
//!
//! `--check` fails (exit 2) if the committed output is stale. Both shell out to
//! Python (`tautc` / `corpus.py`) — deliberately kept OUT of `cargo build` so the
//! crate graph stays pure-Rust and offline — then run the output through rustfmt
//! so the committed file is repo-consistent AND drift-stable (regeneration is
//! deterministic, so committed == freshly-formatted).
//!
//! Tool resolution is via env, with the caller supplying `taut` on PYTHONPATH:
//!   TAUTC   codegen CLI       (default `tautc`)
//!   PYTHON  interpreter       (default `python3`, used for corpus.py)
//! e.g. `TAUTC="python3 -m taut.cli" PYTHONPATH=../taut/src cargo xtask codegen`
//!      `PYTHONPATH=../taut/src cargo xtask corpus --check`

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/xtask
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

mod examples;
mod fetchcmd;
mod fetchnpm;
mod fetchpypi;
mod probe;
mod rungold;
mod stress;
mod tfload;
mod perfgate;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let cmd = args.next();
    let rest: Vec<String> = args.collect();
    let check = rest.first().map(String::as_str) == Some("--check");
    match cmd.as_deref() {
        Some("codegen") => codegen(check),
        Some("corpus") => corpus(check),
        Some("flags") => flags(check),
        Some("gates") => gates(),
        Some("probe") => probe::probe(workspace_root()),
        Some("tfload") => match tfload::tfload(&workspace_root()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask tfload: FAIL — {e}");
                ExitCode::FAILURE
            }
        },
        Some("stress") => match stress::stress(&workspace_root()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask stress: FAIL — {e}");
                ExitCode::FAILURE
            }
        },
        Some("perfgate") => match perfgate::perfgate(&workspace_root()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask perfgate: FAIL — {e}");
                ExitCode::FAILURE
            }
        },
        Some("rungold") => match rungold::rungold(&workspace_root()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask rungold: FAIL — {e}");
                ExitCode::FAILURE
            }
        },
        Some("fetch-extract") => match fetchcmd::fetch_extract(&workspace_root()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("xtask fetch-extract: FAIL — {e}");
                ExitCode::FAILURE
            }
        },
        Some("fetch-npm") => {
            let (Some(lock), Some(out)) = (rest.first(), rest.get(1)) else {
                eprintln!("usage: xtask fetch-npm <package-lock.json> <out-dir>");
                return ExitCode::from(2);
            };
            match fetchnpm::fetch_npm(Path::new(lock), Path::new(out)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("xtask fetch-npm: FAIL — {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fetch-pypi") => {
            let names: Vec<String> = rest.clone();
            match fetchpypi::fetch_pypi(&workspace_root(), &names) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("xtask fetch-pypi: FAIL — {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fetch") => {
            let names: Vec<String> = rest.clone();
            match fetchcmd::fetch(&workspace_root(), &names) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("xtask fetch: FAIL — {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("capture-goldens") => capture_goldens(&rest),
        Some("capture-query-goldens") => capture_query_goldens(),
        Some("capture-crates-query-goldens") => capture_crates_query_goldens(),
        Some("normalize-golden") => normalize_golden(&rest),
        Some("examples") if rest.first().map(String::as_str) == Some("--survey") => {
            match examples::survey(&workspace_root()) {
                Ok(md) => {
                    let p = workspace_root().join("parity/examples/SURVEY.md");
                    match std::fs::write(&p, md) {
                        Ok(()) => {
                            eprintln!("examples --survey: wrote {}", p.display());
                            ExitCode::SUCCESS
                        }
                        Err(e) => {
                            eprintln!("examples --survey: write {}: {e}", p.display());
                            ExitCode::FAILURE
                        }
                    }
                }
                Err(e) => {
                    eprintln!("examples --survey: FAIL — {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("examples") => examples::run_command(
            &workspace_root(),
            rest.first().map(String::as_str) == Some("--capture"),
        ),
        other => {
            eprintln!(
                "unknown xtask {other:?}\n\
                 usage: cargo xtask <codegen|corpus|flags|gates|probe|capture-goldens> [--check]"
            );
            ExitCode::from(64)
        }
    }
}

// ── Phase 0.3: parity goldens (RazelParityHarness.md) ───────────────────────────
// `cargo xtask normalize-golden <in> <out>` normalizes a MANUALLY-captured aquery (e.g. an external
// `@crates//` closure that `capture-goldens` — which iterates `parity/corpus` BUILD cases — cannot
// reach) the SAME way as the local goldens: `filter_aquery` + `razel-parity::normalize`. The capture
// itself stays manual + reproducible (e.g. `bazel … aquery 'deps(@crates//:blake3)' --output=text`).
fn normalize_golden(args: &[String]) -> ExitCode {
    let (Some(infile), Some(outfile)) = (args.first(), args.get(1)) else {
        eprintln!("usage: cargo xtask normalize-golden <raw-aquery-in> <golden-out>");
        return ExitCode::from(2);
    };
    let raw = match std::fs::read_to_string(infile) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("normalize-golden: cannot read {infile}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let golden = razel_parity::normalize(&filter_aquery(&raw));
    match std::fs::write(outfile, &golden) {
        Ok(()) => {
            eprintln!("normalize-golden: wrote {outfile} ({} bytes)", golden.len());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("normalize-golden: cannot write {outfile}: {e}");
            ExitCode::FAILURE
        }
    }
}

// `cargo xtask capture-goldens` runs `bazel aquery` over each corpus case under
// `parity/corpus/**` (dirs with a BUILD file), normalizes via `razel-parity`, and writes
// `golden.txt` into the case dir. This is the ONLY bazel-touching step — dev/authoring-only;
// the (future) hermetic runner consumes the committed goldens with no bazel/toolchain.
// Env: BAZEL (default `bazel`), RAZEL_GOLDEN_OB (bazel --output_base; default /tmp/razel-parity-ob).
fn capture_goldens(filter: &[String]) -> ExitCode {
    let root = workspace_root();
    let parity = root.join("parity");
    let corpus = parity.join("corpus");
    let bazel = std::env::var("BAZEL").unwrap_or_else(|_| "bazel".into());
    let ob = std::env::var("RAZEL_GOLDEN_OB").unwrap_or_else(|_| "/tmp/razel-parity-ob".into());

    let mut cases = Vec::new();
    find_build_packages(&corpus, &corpus, &mut cases);
    // Optional substring filter (e.g. `capture-goldens rust/build_script`) — capture ONLY matching
    // cases, so refreshing one case doesn't re-touch every other golden.
    if !filter.is_empty() {
        cases.retain(|(_, pkg)| filter.iter().any(|f| pkg.contains(f.as_str())));
    }
    if cases.is_empty() {
        eprintln!("capture-goldens: no corpus cases (BUILD files) under {}", corpus.display());
        return ExitCode::from(1);
    }
    let mut failed = 0usize;
    for (dir, pkg) in &cases {
        let label = format!("//corpus/{pkg}:all");
        eprintln!("capturing {label} …");
        let out = Command::new(&bazel)
            .current_dir(&parity)
            .arg(format!("--output_base={ob}"))
            .args(["aquery", &label, "--output=text", "--noshow_progress"])
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                eprintln!("  FAIL spawn bazel: {e}");
                failed += 1;
                continue;
            }
        };
        if !out.status.success() {
            eprintln!("  FAIL aquery:\n{}", String::from_utf8_lossy(&out.stderr).trim_end());
            failed += 1;
            continue;
        }
        let raw = String::from_utf8_lossy(&out.stdout);
        let golden = razel_parity::normalize(&filter_aquery(&raw));
        let path = dir.join("golden.txt");
        match std::fs::write(&path, &golden) {
            Ok(()) => eprintln!("  wrote {} ({} bytes)", path.display(), golden.len()),
            Err(e) => {
                eprintln!("  FAIL write {}: {e}", path.display());
                failed += 1;
            }
        }
    }
    if failed > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

/// The live-query parity battery (`{P}` = the package label) — patterns + the v1 operators, each
/// captured `--noimplicit_deps` (razel query's v1 default; implicit-deps parity is Phase 6). Mirrors
/// the `qg` sentinels (q1–q3) against LIVE `bazel query`.
const QUERY_BATTERY: &[&str] = &[
    "{P}:all",
    "{P}:*",
    "kind(\"rust_library rule\", {P}:all)",
    "deps({P}:util)",
    "rdeps({P}:all, {P}:base)",
    "labels(srcs, {P}:util)",
    "somepath({P}:util, {P}:base)",
    // siblings: every target in base's package (== `{P}:*`) — gates the verb against bazel (P6.Q2).
    "siblings({P}:base)",
    // same_pkg_direct_rdeps: same-package targets that directly depend on base (P6.Q3).
    "same_pkg_direct_rdeps({P}:base)",
];

/// The `--output=package` battery (P6.Q4) — LITERAL exprs (the cross-package union spans two corpus
/// packages, so no `{P}` template). Gates razel's `Output::Package` renderer against bazel's actual
/// `--output=package` text: the main-repo package FORM (no leading `//`, no repo) and the deduped,
/// sorted MULTI-package render (the union case). The result SETS are already label-parity-gated by
/// [`QUERY_BATTERY`]; this isolates the package projection.
const PACKAGE_QUERY_BATTERY: &[&str] = &[
    // single package: every result lands in `corpus/rust/transitive` — proves bazel's package form.
    "//corpus/rust/transitive:*",
    "deps(//corpus/rust/transitive:util)",
    // cross-package union → two packages: the deduped, sorted multi-package render.
    "//corpus/rust/transitive:all + //corpus/rust/build_script:all",
];

/// The `tests()` battery (P6.Q5) over the test-bearing `corpus/rust/tests` package — LITERAL exprs.
/// Gates razel's `tests()` verb against bazel: a `test_suite` EXPANDED to its members; a mixed `:all`
/// (tests kept, suite expanded, library dropped); a non-test target → EMPTY.
const TESTS_QUERY_BATTERY: &[&str] = &[
    // a test_suite expands to its constituent tests.
    "tests(//corpus/rust/tests:suite)",
    // mixed package set: the two rust_test rules kept + the suite expanded; the library dropped.
    "tests(//corpus/rust/tests:all)",
    // a non-test target yields the empty set.
    "tests(//corpus/rust/tests:lib)",
];

/// Run `bazel query --noimplicit_deps [--output=<mode>] <expr>` in `dir` (output base `ob`),
/// returning the golden block `@@ <expr>\n<sorted result lines>\n\n`, or `None` (logged) on failure.
/// The single bazel-touching primitive shared by the label + package batteries.
fn bazel_query_block(
    bazel: &str,
    dir: &Path,
    ob: &str,
    output: Option<&str>,
    expr: &str,
) -> Option<String> {
    eprintln!("query {}{expr} …", output.map(|o| format!("--output={o} ")).unwrap_or_default());
    let mut cmd = Command::new(bazel);
    cmd.current_dir(dir)
        .arg(format!("--output_base={ob}"))
        .args(["query", "--noimplicit_deps", "--noshow_progress"]);
    if let Some(o) = output {
        cmd.arg(format!("--output={o}"));
    }
    cmd.arg(expr);
    match cmd.output() {
        Ok(o) if o.status.success() => {
            let mut lines: Vec<String> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            lines.sort();
            Some(format!("@@ {expr}\n{}\n\n", lines.join("\n")))
        }
        Ok(o) => {
            eprintln!("  FAIL: {}", String::from_utf8_lossy(&o.stderr).trim_end());
            None
        }
        Err(e) => {
            eprintln!("  FAIL spawn bazel: {e}");
            None
        }
    }
}

/// Capture a battery of (already-templated) exprs through bazel into the golden file at `parity/<rel>`
/// (block format), returning the failure count. The data-driven core of [`capture_query_goldens`].
fn capture_battery(
    bazel: &str,
    parity: &Path,
    ob: &str,
    exprs: &[String],
    output: Option<&str>,
    rel: &str,
) -> usize {
    let mut blocks = String::new();
    let mut failed = 0usize;
    for expr in exprs {
        match bazel_query_block(bazel, parity, ob, output, expr) {
            Some(b) => blocks.push_str(&b),
            None => failed += 1,
        }
    }
    let path = parity.join(rel);
    if let Err(e) = std::fs::write(&path, &blocks) {
        eprintln!("FAIL write {}: {e}", path.display());
        return failed + 1;
    }
    eprintln!("wrote {} ({} bytes)", path.display(), blocks.len());
    failed
}

/// `cargo xtask capture-query-goldens` runs `bazel query --noimplicit_deps <expr>` for three batteries
/// and writes one golden per battery (block format: `@@ <expr>` then the SORTED result lines): the
/// [`QUERY_BATTERY`] labels + [`PACKAGE_QUERY_BATTERY`] `--output=package` over `corpus/rust/transitive`
/// (P6.Q4), and [`TESTS_QUERY_BATTERY`] over `corpus/rust/tests` (P6.Q5). Dev/authoring-only — the only
/// bazel-touching step; the `live_query_parity` tests consume the committed goldens with no bazel.
fn capture_query_goldens() -> ExitCode {
    let parity = workspace_root().join("parity");
    let bazel = std::env::var("BAZEL").unwrap_or_else(|_| "bazel".into());
    let ob = std::env::var("RAZEL_GOLDEN_OB").unwrap_or_else(|_| "/tmp/razel-parity-ob".into());
    let pkg = "//corpus/rust/transitive";

    let label_exprs: Vec<String> = QUERY_BATTERY.iter().map(|t| t.replace("{P}", pkg)).collect();
    let pkg_exprs: Vec<String> = PACKAGE_QUERY_BATTERY.iter().map(|s| s.to_string()).collect();
    let tests_exprs: Vec<String> = TESTS_QUERY_BATTERY.iter().map(|s| s.to_string()).collect();

    let failed = capture_battery(&bazel, &parity, &ob, &label_exprs, None,
            "corpus/rust/transitive/query_goldens.txt")
        + capture_battery(&bazel, &parity, &ob, &pkg_exprs, Some("package"),
            "corpus/rust/transitive/query_goldens_package.txt")
        + capture_battery(&bazel, &parity, &ob, &tests_exprs, None,
            "corpus/rust/tests/query_goldens.txt");

    if failed > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

/// The q4 `@crates` query battery — LITERAL exprs (no `{P}`), each isolating ONE q4 feature over
/// razel's own blake3 closure. `labels()` targets the CANONICAL crate label (the apparent
/// `@crates//:blake3` is an alias with no `deps`/`target_compatible_with` of its own); `deps(...,1)`
/// uses the apparent alias to exercise alias transparency. Both razel and bazel accept either form.
const CRATES_QUERY_BATTERY: &[&str] = &[
    // alias transparency: the apparent alias `@crates//:blake3` AND its canonical actual both appear.
    "deps(@crates//:blake3, 1)",
    // per-attr labels(deps): blake3's dep crates + the `:build_script_build` alias (P5.1).
    "labels(deps, @@rules_rust++crate+crates__blake3-1.8.2//:blake3)",
    // per-attr labels(target_compatible_with): the select's only value = @platforms//:incompatible.
    "labels(target_compatible_with, @@rules_rust++crate+crates__blake3-1.8.2//:blake3)",
    // select-condition edges: the `target_compatible_with` config_setting keys (the 7 triples).
    "kind(\"config_setting rule\", deps(@@rules_rust++crate+crates__blake3-1.8.2//:blake3, 1))",
    // P6.Q1 (glob fidelity — was the deferred 5th): `labels(compile_data, …)` over a real external
    // crate. Now byte-equal to bazel (modulo the canonical-repo norm) via: Q1.a external source files
    // key at their crate repo (`@crates__blake3-1.8.2//:c/blake3.c`, not `//:`); Q1.b the glob lists
    // the HIDDEN files bazel does (`.github/*`, `.gitignore`, `.cargo/config.toml`); Q1.c materialize
    // writes bazel's empty `REPO.bazel` marker. (cargo_toml_env_vars was build-product leakage on a
    // dirty tree — the parity driver strips products first.)
    "labels(compile_data, @@rules_rust++crate+crates__blake3-1.8.2//:blake3)",
];

/// `cargo xtask capture-crates-query-goldens` runs `bazel query --noimplicit_deps <expr>` for
/// [`CRATES_QUERY_BATTERY`] over razel's OWN `@crates` graph — the repo ROOT, the only workspace
/// where `@crates` resolves — and writes `parity/corpus/rust/crate_blake3/query_goldens.txt` (same
/// `@@ <expr>` + sorted-labels block format as [`capture_query_goldens`]). Dev/authoring-only (the
/// bazel-touching step); the committed golden is consumed by the `#[ignore]` `crates_query_parity`
/// driver (P5.3b, q4) which normalizes the canonical `@@rules_rust++crate+…` display to apparent.
fn capture_crates_query_goldens() -> ExitCode {
    let root = workspace_root();
    let bazel = std::env::var("BAZEL").unwrap_or_else(|_| "bazel".into());
    let mut blocks = String::new();
    let mut failed = 0usize;
    for expr in CRATES_QUERY_BATTERY {
        eprintln!("query {expr} …");
        let out = Command::new(&bazel)
            .current_dir(&root)
            .args(["query", "--noimplicit_deps", "--noshow_progress", expr])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let mut labels: Vec<String> = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                labels.sort();
                blocks.push_str(&format!("@@ {expr}\n{}\n\n", labels.join("\n")));
            }
            Ok(o) => {
                eprintln!("  FAIL: {}", String::from_utf8_lossy(&o.stderr).trim_end());
                failed += 1;
            }
            Err(e) => {
                eprintln!("  FAIL spawn bazel: {e}");
                failed += 1;
            }
        }
    }
    let path = root.join("parity/corpus/rust/crate_blake3/query_goldens.txt");
    if let Err(e) = std::fs::write(&path, &blocks) {
        eprintln!("FAIL write {}: {e}", path.display());
        return ExitCode::from(1);
    }
    eprintln!("wrote {} ({} bytes)", path.display(), blocks.len());
    if failed > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

/// Recursively collect (dir, package-path) for every dir under `corpus_root` holding a BUILD file.
fn find_build_packages(dir: &Path, corpus_root: &Path, out: &mut Vec<(PathBuf, String)>) {
    if dir.join("BUILD").is_file() {
        let rel = dir.strip_prefix(corpus_root).unwrap_or(dir).to_string_lossy().replace('\\', "/");
        out.push((dir.to_path_buf(), rel));
    }
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                find_build_packages(&p, corpus_root, out);
            }
        }
    }
}

/// Drop bazel CLI chatter that may reach stdout, leaving only the action-graph lines.
pub(crate) fn filter_aquery(raw: &str) -> String {
    let drop = |t: &str| {
        ["INFO:", "Loading", "Analyzing", "Computing", "Starting", "WARNING", "Fetching", "DEBUG", "Use "]
            .iter()
            .any(|p| t.starts_with(p))
    };
    raw.lines()
        .filter(|l| !drop(l.trim_start()))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Phase 0.2: forcing gate (AD2 — no ambient state) ────────────────────────────
// `cargo xtask gates` fails if `thread_local!` / `static mut` appear in `crates/` outside
// the explicit allowlist (existing sites tracked for Phase-1 removal). New ambient state
// cannot land — razel's "forcing from line 1" discipline (REQ-TEST-004).
const BANNED: &[(&str, &str)] = &[
    ("thread_local!", "ambient state — pass the Analysis/DDS handle instead (AD2)"),
    ("static mut", "ambient mutable state (AD2)"),
    // Runtime-init globals (F13): banned for env-derived / per-analysis state — resolve once per
    // `Session`, not process-wide. The sin is not mutability (these end up immutable) but a value
    // owned by no Session. A PURE, deterministic cell (compiled regex, const table — no env, no
    // per-request meaning) may be allowlisted by adding its file to GATE_ALLOWLIST with a rationale.
    ("OnceLock", "runtime-init global — per-Session state, not a process global (AD2/F13)"),
    ("OnceCell", "runtime-init global — per-Session state, not a process global (AD2/F13)"),
    ("LazyLock", "runtime-init global — per-Session state, not a process global (AD2/F13)"),
    ("lazy_static", "runtime-init global — per-Session state, not a process global (AD2/F13)"),
];
// AD2 ENFORCED — no ambient request/analysis state anywhere in crates/: the 7 thread-locals became
// `Session` fields (Phase 1.5) and the host-cc `OnceLock` became `Session::resolved_cc` (F13). The ban
// now spans runtime-init cells too (the gate is substring-based, so a genuinely-pure const cell is
// allowlisted per-file here with a rationale — there are none today).
const GATE_ALLOWLIST: &[&str] = &[];

struct GateViolation {
    path: String,
    line: usize,
    pattern: &'static str,
    reason: &'static str,
}

/// Pure check (unit-testable without the filesystem) — the heart of the gate.
fn gate_violations(rel_path: &str, content: &str) -> Vec<GateViolation> {
    if GATE_ALLOWLIST.contains(&rel_path) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (n, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue; // a comment mentioning the pattern is not a use
        }
        for (pattern, reason) in BANNED {
            if line.contains(pattern) {
                out.push(GateViolation { path: rel_path.into(), line: n + 1, pattern, reason });
            }
        }
    }
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_none_or(|n| n != "target") {
                collect_rs(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

// ── Phase 2.1b: the DDS dependency-boundary (AD/§3) ─────────────────────────────
// `razel-dds` is the L1 spine: everything depends DOWN to it, so it must depend only on L0
// (razel-core + razel-wire). A razel-* dep outside this set would invert the boundary.
const DDS_ALLOWED_DEPS: &[&str] = &["razel-core", "razel-wire"];

/// Scan a `[dependencies]` block for `razel-*` crates outside the allowlist (a boundary break).
/// S0 (V3sh1, PublicSurfaces §1d): the grazel ARROW gate — no `razel-*` crate may
/// depend on a `grazel-*` crate or on iroh, in ANY dependency section (the dependency
/// direction enforced mechanically, same class as the DDS boundary below). grazel-*
/// crates are exempt: consuming razel-* and iroh is their whole point.
fn grazel_arrow_violations(crate_name: &str, cargo_toml: &str) -> Vec<String> {
    if !crate_name.starts_with("razel-") {
        return Vec::new();
    }
    let mut in_deps = false;
    let mut out = Vec::new();
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.contains("dependencies]"); // [dependencies], [dev-…], [build-…]
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let name = t.split([' ', '=']).next().unwrap_or("");
        if name.starts_with("grazel-") || name == "iroh" || name.starts_with("iroh-") {
            out.push(name.to_string());
        }
    }
    out
}

fn dds_boundary_violations(cargo_toml: &str) -> Vec<String> {
    let mut in_deps = false;
    let mut out = Vec::new();
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let name = t.split([' ', '=']).next().unwrap_or("");
        if name.starts_with("razel-") && !DDS_ALLOWED_DEPS.contains(&name) {
            out.push(name.to_string());
        }
    }
    out
}

/// P1.0 (RazelCrateUniversePlan, R3): `razel-query` reads the loading graph and NEVER analyzes,
/// so its manifest must not depend on the build driver / analysis / execution / engine crates.
/// A DENYLIST (not an allowlist) so `regex` + other leaf utils are fine.
fn query_boundary_violations(cargo_toml: &str) -> Vec<String> {
    const FORBIDDEN: &[&str] =
        &["razel-build", "razel-analysis", "razel-exec", "razel-engine"];
    let mut in_deps = false;
    let mut out = Vec::new();
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t.contains("dependencies]");
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let name = t.split([' ', '=']).next().unwrap_or("");
        if FORBIDDEN.contains(&name) {
            out.push(name.to_string());
        }
    }
    out
}

// ── Phase 2.1c: no language name leaks into the engine core (RazelHookSeam C3c) ─────────────
// The generic loader engine must not hardcode a language/provider — adding a language is a
// registration (provider registry / toolchain registry / ruleset table), not a core edit. We ban
// DISTINCTIVE tokens (not bare `cc`/`java`, which false-positive on `macos`/paths/prose), scanning
// only the engine-core modules; the per-language rules + the registries + the ruleset registration
// site legitimately name languages and are allowlisted.
const LANG_BANNED: &[&str] = &["CcInfo", "JavaInfo", "PyInfo", "macos_core_config", "cc_provider_map"];
const LANG_ALLOWLIST: &[&str] = &[
    "crates/razel-loading/src/native_cc.rs",
    "crates/razel-loading/src/rust_rules.rs",
    "crates/razel-loading/src/py_rules.rs",
    "crates/razel-loading/src/sh_rules.rs",
    "crates/razel-loading/src/shims.rs",
    "crates/razel-loading/src/registry.rs",
    "crates/razel-loading/src/toolchains.rs",
    "crates/razel-loading/src/rules.rs", // the loader + `ruleset_modules` registration table
];

/// A language/provider name hardcoded in a generic engine-core module (C3c). Scans only
/// `razel-loading/src` (the loader engine); skips comments + the per-file `#[cfg(test)]` fixtures.
fn language_leak_violations(rel_path: &str, content: &str) -> Vec<GateViolation> {
    if !rel_path.starts_with("crates/razel-loading/src/") || LANG_ALLOWLIST.contains(&rel_path) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (n, line) in content.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue; // a comment naming a provider is not a use
        }
        if t.starts_with("#[cfg(test)]") {
            break; // test fixtures may name providers; the core is the non-test code above
        }
        for pattern in LANG_BANNED {
            if line.contains(pattern) {
                out.push(GateViolation {
                    path: rel_path.into(),
                    line: n + 1,
                    pattern,
                    reason: "language/provider name in the engine core — register it (provider/toolchain registry), don't hardcode it (C3c)",
                });
            }
        }
    }
    out
}

fn gates() -> ExitCode {
    let root = workspace_root();
    let mut files = Vec::new();
    collect_rs(&root.join("crates"), &mut files);
    let mut violations = Vec::new();
    let mut lang = Vec::new();
    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        if let Ok(content) = std::fs::read_to_string(f) {
            violations.extend(gate_violations(&rel, &content));
            lang.extend(language_leak_violations(&rel, &content));
        }
    }
    // DDS dependency-boundary (2.1b): razel-dds may depend only on core+wire.
    let boundary: Vec<String> = std::fs::read_to_string(root.join("crates/razel-dds/Cargo.toml"))
        .map(|c| dds_boundary_violations(&c))
        .unwrap_or_default();
    // P1.0: razel-query reads the loading graph, never analyzes — no build/analysis/exec/engine dep.
    let query_boundary: Vec<String> =
        std::fs::read_to_string(root.join("crates/razel-query/Cargo.toml"))
            .map(|c| query_boundary_violations(&c))
            .unwrap_or_default();
    // S0: the grazel arrow — no razel-* crate depends on grazel-*/iroh (§1d).
    let mut arrow = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("crates")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Ok(c) = std::fs::read_to_string(e.path().join("Cargo.toml")) {
                arrow.extend(
                    grazel_arrow_violations(&name, &c).into_iter().map(|d| (name.clone(), d)),
                );
            }
        }
    }

    if violations.is_empty()
        && boundary.is_empty()
        && query_boundary.is_empty()
        && lang.is_empty()
        && arrow.is_empty()
    {
        eprintln!(
            "xtask gates: OK — no ambient state anywhere in crates/ (AD2); razel-dds boundary intact (core+wire only); razel-query reads-only (no analysis dep — P1.0); no language name in the engine core (C3c); grazel arrow intact (no razel-*→grazel-*/iroh dep — S0)"
        );
        return ExitCode::SUCCESS;
    }
    for v in &violations {
        eprintln!("  BANNED {}:{}  `{}` — {}", v.path, v.line, v.pattern, v.reason);
    }
    for v in &lang {
        eprintln!("  LANGUAGE-IN-CORE {}:{}  `{}` — {}", v.path, v.line, v.pattern, v.reason);
    }
    for b in &boundary {
        eprintln!("  BOUNDARY razel-dds must not depend on `{b}` — the DDS spine is core+wire only");
    }
    for b in &query_boundary {
        eprintln!("  QUERY-BOUNDARY razel-query must not depend on `{b}` — query never analyzes (P1.0)");
    }
    for (krate, dep) in &arrow {
        eprintln!(
            "  ARROW {krate} must not depend on `{dep}` — razel never grows a grazel/iroh dep (S0, §1d)"
        );
    }
    eprintln!(
        "\nxtask gates: FAIL — {} ambient-state + {} language-in-core + {} boundary + {} query-boundary + {} arrow violation(s).",
        violations.len(),
        lang.len(),
        boundary.len(),
        query_boundary.len(),
        arrow.len()
    );
    ExitCode::from(1)
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn query_boundary_denies_the_analysis_path_but_allows_leaf_utils() {
        let bad = "[dependencies]\nrazel-loading = { path = \"..\" }\nrazel-build = { path = \"..\" }\n";
        assert_eq!(query_boundary_violations(bad), vec!["razel-build".to_string()]);
        let ok = "[dependencies]\nrazel-loading = { path = \"..\" }\nrazel-core = { path = \"..\" }\nregex = \"1\"\n";
        assert!(query_boundary_violations(ok).is_empty());
    }

    #[test]
    fn language_leak_gate_catches_core_hardcodes_but_allows_registration() {
        // C3c negative: a provider literal in a core module is caught.
        let core = "crates/razel-loading/src/dialect.rs";
        assert_eq!(language_leak_violations(core, "    set_provider(\"CcInfo\", \"hdrs\", v);").len(), 1);
        assert_eq!(language_leak_violations(core, "    macos_core_config()").len(), 1);
        // …but the registration modules + the loader's ruleset table legitimately name languages.
        assert!(language_leak_violations("crates/razel-loading/src/registry.rs", "r.register(\"CcInfo\", ..)").is_empty());
        assert!(language_leak_violations("crates/razel-loading/src/toolchains.rs", "macos_core_config()").is_empty());
        // …a comment naming a provider is not a use, and test fixtures (after #[cfg(test)]) are skipped.
        assert!(language_leak_violations(core, "    // CcInfo captures headers").is_empty());
        assert!(language_leak_violations(core, "#[cfg(test)]\nmod t { let x = \"CcInfo\"; }").is_empty());
    }

    #[test]
    fn grazel_arrow_gate_red_test() {
        // S0 (V3sh1): the deny rule MUST catch a razel-* crate growing a grazel-*/iroh
        // dep (the §1d arrow, red-tested per the S0 exit condition)…
        let bad = "[dependencies]\ngrazel-cli-lib = { path = \"../grazel-cli-lib\" }\n";
        assert_eq!(grazel_arrow_violations("razel-cli", bad), vec!["grazel-cli-lib"]);
        let bad_iroh = "[dependencies]\niroh = \"0.35\"\n";
        assert_eq!(grazel_arrow_violations("razel-loading", bad_iroh), vec!["iroh"]);
        // …while the legal direction (grazel-* consuming razel-*) and razel-*'s own
        // deps pass untouched.
        let legal = "[dependencies]\nrazel-daemon = { path = \"../razel-daemon\" }\niroh = \"0.35\"\n";
        assert!(grazel_arrow_violations("grazel-node", legal).is_empty());
        assert!(grazel_arrow_violations("razel-cli", "[dependencies]\nrazel-wire = {}\n").is_empty());
        // dev-dependencies count too (the arrow has no test exemption).
        let dev = "[dev-dependencies]\ngrazel-cli-lib = {}\n";
        assert_eq!(grazel_arrow_violations("razel-build", dev), vec!["grazel-cli-lib"]);
    }

    #[test]
    fn flags_new_ambient_state() {
        // REQ-TEST-004 negative: the gate MUST catch a new violation.
        let v = gate_violations(
            "crates/razel-dds/src/store.rs",
            "fn f() { thread_local! { static X: u32 = const { 0 }; } }",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pattern, "thread_local!");
    }

    #[test]
    fn flags_static_mut() {
        let v = gate_violations("crates/k/src/x.rs", "static mut COUNTER: u64 = 0;");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pattern, "static mut");
    }

    #[test]
    fn flags_runtime_init_global() {
        // F13: a runtime-init once-cell holding analysis state is ambient — the gate must catch it
        // (it slipped through when only thread_local!/static mut were banned).
        let v = gate_violations(
            "crates/razel-loading/src/rules.rs",
            "static RESOLVED: OnceLock<String> = OnceLock::new();",
        );
        assert!(v.iter().any(|x| x.pattern == "OnceLock"), "OnceLock must be flagged (AD2/F13)");
    }

    #[test]
    fn no_file_is_exempt_now() {
        // Phase 1.5 emptied the allowlist — even the loading crate is now subject to the ban.
        let v = gate_violations(
            "crates/razel-loading/src/rules.rs",
            "thread_local! { static S: () = (); }",
        );
        assert_eq!(v.len(), 1, "AD2 fully enforced: no file is exempt from the ambient-state ban");
    }

    #[test]
    fn passes_clean_code() {
        assert!(gate_violations("crates/razel-dds/src/lib.rs", "pub struct Dds;").is_empty());
    }

    #[test]
    fn ignores_comment_mentions() {
        assert!(
            gate_violations("crates/x/src/y.rs", "    // thread_local! is banned by AD2").is_empty()
        );
    }

    #[test]
    fn dds_boundary_flags_an_upward_dep() {
        // razel-dds depending on razel-engine would invert the L1→L0 boundary.
        let toml = "[package]\nname = \"razel-dds\"\n[dependencies]\nrazel-core = { path = \"..\" }\nrazel-engine = { path = \"..\" }\n";
        assert_eq!(dds_boundary_violations(toml), vec!["razel-engine".to_string()]);
    }

    #[test]
    fn dds_boundary_allows_core_wire_and_external() {
        let toml = "[dependencies]\nrazel-core = { path = \"..\" }\nrazel-wire = { path = \"..\" }\nanyhow = \"1\"\n";
        assert!(dds_boundary_violations(toml).is_empty());
    }
}

/// Regenerate the Bazel-flag recognition table (`razel-cli/src/bazel_flags.rs`) from
/// the committed JSON inventory via `gen_flags_table.py --stdout`. To refresh the
/// inventory itself against a newer Bazel, run `extract_bazel_flags.py <bazel-src>`
/// and diff with `--diff` (the version-comparison oracle).
fn flags(check: bool) -> ExitCode {
    let root = workspace_root();
    let script = root.join("crates/razel-cli/flags/gen_flags_table.py");
    let target = root.join("crates/razel-cli/src/bazel_flags.rs");
    let raw = std::env::temp_dir().join("razel-bazel-flags.rs");

    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python).arg(&script).arg("--stdout").output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            eprintln!(
                "gen_flags_table.py failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("could not run {python:?} on gen_flags_table.py: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&raw, out) {
        eprintln!("could not write temp flags table: {e}");
        return ExitCode::FAILURE;
    }
    finalize(&raw, &target, "flags", check)
        .err()
        .unwrap_or(ExitCode::SUCCESS)
}

/// Regenerate the wire types **and** the CBOR runtime from the taut IR via `tautc`.
/// taut ≥0.4 emits the vendored runtime (`--with-runtime`), so `cbor.rs` is now
/// generated too rather than hand-maintained — both files are drift-gated.
fn codegen(check: bool) -> ExitCode {
    let root = workspace_root();
    let schema = root.join("crates/razel-wire/wire/razel.taut.py");
    let wire = root.join("crates/razel-wire/src");
    let out_dir = std::env::temp_dir().join("razel-wire-codegen");
    let _ = std::fs::remove_dir_all(&out_dir);

    let tautc = std::env::var("TAUTC").unwrap_or_else(|_| "tautc".into());
    let mut parts = tautc.split_whitespace();
    let prog = parts.next().expect("empty $TAUTC");
    let status = Command::new(prog)
        .args(parts)
        .arg("gen")
        .arg(&schema)
        .arg("-o")
        .arg(&out_dir)
        .args(["-l", "rust", "--api-only", "--with-runtime"])
        .status();
    if let Err(code) = ok_status(
        status,
        &tautc,
        "is taut installed? (pip install -e ../taut)",
    ) {
        return code;
    }
    let rust = out_dir.join("rust");
    for (src, dst, label) in [
        (rust.join("api.rs"), wire.join("generated.rs"), "codegen"),
        (
            rust.join("cbor.rs"),
            wire.join("cbor.rs"),
            "codegen-runtime",
        ),
    ] {
        if let Err(code) = finalize(&src, &dst, label, check) {
            return code;
        }
    }
    ExitCode::SUCCESS
}

/// Regenerate the golden corpus via `corpus.py --stdout`.
fn corpus(check: bool) -> ExitCode {
    let root = workspace_root();
    let script = root.join("crates/razel-wire/wire/corpus.py");
    let target = root.join("crates/razel-wire/src/vectors.rs");
    let raw = std::env::temp_dir().join("razel-wire-vectors.rs");

    let python = std::env::var("PYTHON").unwrap_or_else(|_| "python3".into());
    let out = Command::new(&python).arg(&script).arg("--stdout").output();
    let out = match out {
        Ok(o) if o.status.success() => o.stdout,
        Ok(o) => {
            eprintln!("corpus.py failed: {}", String::from_utf8_lossy(&o.stderr));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "could not run {python:?} on corpus.py: {e}\n  PYTHONPATH must include taut (PYTHONPATH=../taut/src)"
            );
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::write(&raw, out) {
        eprintln!("could not write temp corpus: {e}");
        return ExitCode::FAILURE;
    }
    finalize(&raw, &target, "corpus", check)
        .err()
        .unwrap_or(ExitCode::SUCCESS)
}

/// rustfmt `raw` in place, then compare to / overwrite `target`. `Ok(())` on
/// success; `Err(exit)` on drift (exit 2) or a tooling failure — so callers that
/// finalize several files can bail on the first problem.
fn finalize(raw: &Path, target: &Path, label: &str, check: bool) -> Result<(), ExitCode> {
    let fmt = Command::new("rustfmt")
        .args(["--edition", "2024"])
        .arg(raw)
        .status();
    ok_status(fmt, &"rustfmt".into(), "is rustfmt installed?")?;
    let fresh = std::fs::read_to_string(raw).expect("read generated output");

    if check {
        let committed = std::fs::read_to_string(target).unwrap_or_default();
        if committed == fresh {
            println!("{label}: {} is up to date", target.display());
            Ok(())
        } else {
            eprintln!(
                "{label} DRIFT: {} is stale.\n  run `cargo xtask {label}` and commit.",
                target.display()
            );
            Err(ExitCode::from(2))
        }
    } else {
        std::fs::write(target, fresh).expect("write generated output");
        println!("{label}: wrote {}", target.display());
        Ok(())
    }
}

/// Map a spawned command's status to Ok / an ExitCode to bubble up.
fn ok_status(
    status: std::io::Result<std::process::ExitStatus>,
    prog: &String,
    hint: &str,
) -> Result<(), ExitCode> {
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!("{prog} failed: {s}");
            Err(ExitCode::FAILURE)
        }
        Err(e) => {
            eprintln!("could not run {prog:?}: {e}\n  {hint}");
            Err(ExitCode::FAILURE)
        }
    }
}
