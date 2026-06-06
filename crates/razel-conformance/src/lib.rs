//! Class-A conformance harness: a `.star` golden runner.
//!
//! Embeds `starlark` and runs Bazel/Buck2 `.star` eval-golden files: a file is
//! split into independent cases on lines that are exactly `---`; each case is
//! evaluated in a fresh module with `assert_eq`/`assert_`/`assert_ne`/`assert_fails`
//! injected as builtins. Bazel's expected-error convention is honored: a `### <regex>`
//! comment marks the case as expected to fail with an error matching `<regex>`.
//!
//! KNOWN APPROXIMATION (tracked, not hidden): the `### <regex>` expectation is applied
//! at *case* granularity (the whole `---` chunk is expected to fail), not attached to the
//! single preceding statement as upstream Bazel does. Per-statement attachment is a
//! follow-up; it only matters for multi-statement cases that mix a passing statement with
//! an expected failure in the same chunk.

use regex::Regex;
use starlark::environment::{GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::Value;
use starlark::values::none::NoneType;

/// Extended-stdlib extensions to register (the public mirror of starlark's private
/// `LibraryExtension::all()`): the closest match to Bazel's available builtins.
const EXTENSIONS: &[LibraryExtension] = &[
    LibraryExtension::StructType,
    LibraryExtension::RecordType,
    LibraryExtension::EnumType,
    LibraryExtension::NamespaceType,
    LibraryExtension::Map,
    LibraryExtension::Filter,
    LibraryExtension::Partial,
    LibraryExtension::Debug,
    LibraryExtension::Print,
    LibraryExtension::Pprint,
    LibraryExtension::Pstr,
    LibraryExtension::Prepr,
    LibraryExtension::Breakpoint,
    LibraryExtension::Json,
    LibraryExtension::Typing,
    LibraryExtension::Internal,
    LibraryExtension::CallStack,
    LibraryExtension::SetType,
];

/// Result of running one `---`-delimited case.
#[derive(Debug, Clone)]
pub struct CaseResult {
    pub index: usize,
    pub passed: bool,
    pub message: Option<String>,
}

/// Aggregate over a whole file.
#[derive(Debug, Clone)]
pub struct FileReport {
    pub filename: String,
    pub cases: Vec<CaseResult>,
}

impl FileReport {
    pub fn passed(&self) -> usize {
        self.cases.iter().filter(|c| c.passed).count()
    }
    pub fn total(&self) -> usize {
        self.cases.len()
    }
}

/// The injected assertion builtins.
#[starlark::starlark_module]
fn asserts(builder: &mut GlobalsBuilder) {
    fn assert_eq<'v>(a: Value<'v>, b: Value<'v>) -> anyhow::Result<NoneType> {
        if a.equals(b).map_err(|e| anyhow::anyhow!("{e}"))? {
            Ok(NoneType)
        } else {
            anyhow::bail!("assert_eq failed: {a} != {b}")
        }
    }
    fn assert_ne<'v>(a: Value<'v>, b: Value<'v>) -> anyhow::Result<NoneType> {
        if a.equals(b).map_err(|e| anyhow::anyhow!("{e}"))? {
            anyhow::bail!("assert_ne failed: {a} == {b}")
        } else {
            Ok(NoneType)
        }
    }
    fn assert_<'v>(cond: Value<'v>) -> anyhow::Result<NoneType> {
        if cond.to_bool() {
            Ok(NoneType)
        } else {
            anyhow::bail!("assert_ failed: condition is false")
        }
    }
    /// `assert_fails(fn, regex)`: call `fn()` with no args, expect it to raise an error
    /// whose message matches `regex`.
    fn assert_fails<'v>(
        f: Value<'v>,
        regex: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        match eval.eval_function(f, &[], &[]) {
            Ok(_) => anyhow::bail!("assert_fails: function did not fail (expected /{regex}/)"),
            Err(e) => {
                let msg = format!("{e}");
                let re = Regex::new(regex)?;
                if re.is_match(&msg) {
                    Ok(NoneType)
                } else {
                    anyhow::bail!("assert_fails: error /{msg}/ did not match /{regex}/")
                }
            }
        }
    }
}

/// Split a `.star` source into cases on lines that are exactly `---`.
fn split_cases(source: &str) -> Vec<String> {
    let mut cases = Vec::new();
    let mut cur = String::new();
    for line in source.lines() {
        if line.trim_end() == "---" {
            cases.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    cases.push(cur);
    // Drop a trailing fully-empty case (a file ending in `---`).
    while matches!(cases.last(), Some(c) if c.trim().is_empty()) && cases.len() > 1 {
        cases.pop();
    }
    cases
}

/// Extract a `### <regex>` expected-error marker from a case, if present.
fn extract_expected_error(case: &str) -> Option<String> {
    for line in case.lines() {
        if let Some(idx) = line.find("###") {
            let re = line[idx + 3..].trim();
            if !re.is_empty() {
                return Some(re.to_string());
            }
        }
    }
    None
}

/// Remove `### …` markers from each line. They are harness metadata (and Starlark
/// comments, so eval-inert); stripping them keeps the marker text out of Starlark's
/// rendered error diagnostics, which echo the offending source line — otherwise the
/// expected-error regex would spuriously match its own marker.
fn strip_expected_markers(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("###") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Evaluate one case in a fresh module; `Ok(())` on clean eval, `Err` with the
/// rendered error message otherwise.
fn eval_case(filename: &str, src: &str) -> Result<(), String> {
    let ast = AstModule::parse(filename, strip_expected_markers(src), &Dialect::Extended)
        .map_err(|e| format!("{e}"))?;
    let globals = GlobalsBuilder::extended_by(EXTENSIONS)
        .with(asserts)
        .build();
    Module::with_temp_heap(|module| {
        let mut eval = Evaluator::new(&module);
        eval.eval_module(ast, &globals)
            .map_err(|e| format!("{e}"))?;
        Ok(())
    })
}

/// Run every case in a `.star` source and report pass/fail.
pub fn run_star_source(filename: &str, source: &str) -> FileReport {
    let cases = split_cases(source)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let expected = extract_expected_error(&chunk);
            let outcome = eval_case(filename, &chunk);
            let (passed, message) = match (outcome, expected) {
                (Ok(()), None) => (true, None),
                (Ok(()), Some(re)) => (
                    false,
                    Some(format!(
                        "expected error matching /{re}/, but evaluated cleanly"
                    )),
                ),
                (Err(e), None) => (false, Some(format!("unexpected error: {e}"))),
                (Err(e), Some(re)) => match Regex::new(&re) {
                    Ok(rx) if rx.is_match(&e) => (true, None),
                    Ok(_) => (false, Some(format!("error did not match /{re}/: {e}"))),
                    Err(rxe) => (
                        false,
                        Some(format!("bad expected-error regex /{re}/: {rxe}")),
                    ),
                },
            };
            CaseResult {
                index,
                passed,
                message,
            }
        })
        .collect();
    FileReport {
        filename: filename.to_string(),
        cases,
    }
}

/// Known starlark-rust ↔ Bazel divergences in Bazel's eval testdata — the
/// quarantine list (xfail) for the Class-A gate (decision: "Bazel testdata +
/// quarantine"). `(file, expected_failing_cases, citation)`. razel's Starlark **is**
/// the `starlark` crate (D4); these cases differ from Bazel's Java Starlark in error
/// wording, builtin signatures, or value semantics and cannot be reconciled without
/// forking the crate — so they are documented, not chased.
pub const DIVERGENCES: &[(&str, usize, &str)] = &[
    (
        "comprehension.star",
        1,
        "comprehension variable scoping differs",
    ),
    (
        "cycles.star",
        3,
        "recursion/cycle handling differs (starlark-rust permits recursion)",
    ),
    (
        "dict.star",
        2,
        "dict operator/builtin semantics differ (e.g. `|=`)",
    ),
    ("fields.star", 2, "struct/field access semantics differ"),
    ("float.star", 1, "float formatting/semantics differ"),
    (
        "function.star",
        6,
        "permits recursion; type-name `function` vs `builtin_function_or_method`",
    ),
    (
        "int.star",
        1,
        "missing Bazel harness builtin `int_mul_slow`; int error text differs",
    ),
    (
        "int_constructor.star",
        1,
        "int() base-inference error differs",
    ),
    ("json.star", 1, "json builtin output/error differs"),
    ("list.star", 1, "index-out-of-bound error wording differs"),
    (
        "list_mutation.star",
        1,
        "negative-index error wording differs",
    ),
    ("list_slices.star", 1, "slice index error differs"),
    (
        "loop.star",
        18,
        "sequence/loop error wording differs from Bazel",
    ),
    (
        "min_max.star",
        1,
        "min/max mixed-type comparison error differs",
    ),
    ("range.star", 1, "range() argument signature/error differs"),
    (
        "set.star",
        1,
        "missing Bazel harness builtin `freeze`; set semantics differ",
    ),
    (
        "sorted.star",
        1,
        "sorted() mixed-type comparison error differs",
    ),
    (
        "string_elems.star",
        1,
        "string.elems() returns an iterator (no len) vs Bazel sequence",
    ),
    (
        "string_format.star",
        12,
        ".format() error wording differs from Bazel",
    ),
    (
        "string_misc.star",
        1,
        "string.replace negative-count handling differs",
    ),
    (
        "string_split.star",
        1,
        "split() named-arg signature differs from Bazel",
    ),
];

/// Expected (quarantined) divergent-case count for a file; 0 if not listed.
pub fn expected_divergences(file: &str) -> usize {
    DIVERGENCES
        .iter()
        .find(|(f, _, _)| *f == file)
        .map_or(0, |(_, n, _)| *n)
}

/// Aggregate Class-A gate result over per-file `(name, pass, total)` tuples.
#[derive(Debug, Default)]
pub struct Gate {
    pub raw_pass: usize,
    pub total: usize,
    pub quarantined: usize,
    /// Files with MORE failures than documented — undocumented regressions (gate-breaking).
    pub regressions: Vec<String>,
    /// Files with FEWER failures than documented — manifest is stale (a divergence was fixed).
    pub stale: Vec<String>,
}

impl Gate {
    pub fn nonquarantined_total(&self) -> usize {
        self.total - self.quarantined
    }
    pub fn nonquarantined_pct(&self) -> f64 {
        let d = self.nonquarantined_total();
        if d == 0 {
            100.0
        } else {
            100.0 * self.raw_pass as f64 / d as f64
        }
    }
    pub fn raw_pct(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            100.0 * self.raw_pass as f64 / self.total as f64
        }
    }
    /// Gate passes iff no undocumented regressions AND ≥95% of non-quarantined cases green.
    pub fn passed(&self) -> bool {
        self.regressions.is_empty() && self.nonquarantined_pct() >= 95.0
    }
}

/// Evaluate the Class-A gate from per-file `(name, pass, total)` results.
pub fn evaluate_gate(per_file: &[(String, usize, usize)]) -> Gate {
    let mut g = Gate::default();
    for (name, pass, total) in per_file {
        g.raw_pass += *pass;
        g.total += *total;
        let fail = total - pass;
        let expected = expected_divergences(name);
        g.quarantined += expected.min(fail);
        if fail > expected {
            g.regressions.push(format!(
                "{name}: {fail} failures > {expected} documented divergences"
            ));
        } else if fail < expected {
            g.stale.push(format!(
                "{name}: {fail} failures < {expected} documented (divergence fixed?)"
            ));
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_quarantines_documented_divergences() {
        // A documented all-red file is fully quarantined; gate stays green.
        let g = evaluate_gate(&[("comprehension.star".into(), 0, 1)]);
        assert_eq!(g.quarantined, 1);
        assert_eq!(g.nonquarantined_total(), 0);
        assert!(g.regressions.is_empty());
        assert!(g.passed());
    }

    #[test]
    fn gate_flags_undocumented_regression() {
        // A file not in the manifest that fails is a gate-breaking regression.
        let g = evaluate_gate(&[("brand_new.star".into(), 0, 1)]);
        assert_eq!(g.quarantined, 0);
        assert_eq!(g.regressions.len(), 1);
        assert!(!g.passed());
    }

    #[test]
    fn gate_flags_stale_manifest_when_divergence_fixed() {
        // comprehension expects 1 divergence; if it now passes, manifest is stale.
        let g = evaluate_gate(&[("comprehension.star".into(), 1, 1)]);
        assert_eq!(g.stale.len(), 1);
        assert!(g.passed()); // stale is informational, not gate-breaking
    }

    #[test]
    fn gate_nonquarantined_rate_excludes_quarantined() {
        // 1 clean-pass file + 1 documented all-red file → 100% of non-quarantined.
        let g = evaluate_gate(&[("tuple.star".into(), 1, 1), ("loop.star".into(), 3, 21)]);
        assert_eq!(g.quarantined, 18);
        assert_eq!(g.nonquarantined_total(), 4); // 22 - 18
        assert_eq!(g.raw_pass, 4);
        assert!((g.nonquarantined_pct() - 100.0).abs() < 1e-9);
        assert!(g.passed());
    }

    fn run(src: &str) -> FileReport {
        run_star_source("test.star", src)
    }

    #[test]
    fn assert_eq_pass_and_fail() {
        let r = run("assert_eq(1 + 1, 2)");
        assert_eq!(r.total(), 1);
        assert!(r.cases[0].passed, "{:?}", r.cases[0]);

        let r = run("assert_eq(1, 2)");
        assert!(!r.cases[0].passed);
        assert!(
            r.cases[0]
                .message
                .as_ref()
                .unwrap()
                .contains("assert_eq failed")
        );
    }

    #[test]
    fn assert_truthiness_and_ne() {
        assert!(run("assert_(True)").cases[0].passed);
        assert!(!run("assert_(False)").cases[0].passed);
        assert!(run("assert_ne(1, 2)").cases[0].passed);
        assert!(!run("assert_ne(1, 1)").cases[0].passed);
    }

    #[test]
    fn splits_on_triple_dash() {
        let r = run("assert_eq(1, 1)\n---\nassert_eq(2, 2)\n---\nassert_eq(3, 3)");
        assert_eq!(r.total(), 3);
        assert_eq!(r.passed(), 3);
    }

    #[test]
    fn expected_error_convention() {
        // fail() raises; the `### boom` marks it expected → case passes.
        let r = run("fail('boom')  ### boom");
        assert!(r.cases[0].passed, "{:?}", r.cases[0]);

        // expected an error but the case evaluated cleanly → fails.
        let r = run("assert_eq(1, 1)  ### boom");
        assert!(!r.cases[0].passed);

        // errored, but the message does not match the expected regex → fails.
        let r = run("fail('different')  ### boom");
        assert!(!r.cases[0].passed);
    }

    #[test]
    fn standard_builtins_available() {
        assert!(run("assert_eq(len([1, 2, 3]), 3)").cases[0].passed);
        assert!(run("assert_eq([x * 2 for x in [1, 2]], [2, 4])").cases[0].passed);
        assert!(run("def f(x):\n    return x + 1\nassert_eq(f(1), 2)").cases[0].passed);
    }

    #[test]
    fn assert_fails_builtin() {
        assert!(run("assert_fails(lambda: fail('nope'), 'nope')").cases[0].passed);
        assert!(!run("assert_fails(lambda: 1, 'nope')").cases[0].passed);
    }
}
