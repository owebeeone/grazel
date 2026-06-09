//! `razel-parity` — golden compat-testing support (`dev-docs/RazelParityHarness.md`).
//!
//! V1 surface: [`normalize`] canonicalizes a raw `bazel aquery --output=text` dump into a
//! **reviewable, host/platform-independent golden** (the §5/§9 normalization spec). It is the
//! single piece of "matching logic," shared by the `capture-goldens` xtask (authoring) and —
//! later — the hermetic test runner (which renders razel's declared action graph to the same
//! form and diffs). Pure std, deterministic, offline, **idempotent**.
//!
//! It strips bazel-internal / host-volatile tokens (the per-action `ActionKey:` digest, the
//! `# Configuration:`/`# Execution platform:` hashes, the macOS SDK version) and normalizes the
//! variable config/repo path segments to placeholders (`bazel-out/<cfg>/…`, `external/<repo>/…`).
//! Argv **order is preserved** (a command line is order-significant). What is intentionally
//! NOT yet normalized (an allowlisted set-like flag sort; the toolchain-wrapper indirection)
//! is documented in `RazelParityHarness.md §5/§9` and added as the corpus demands it.

/// Normalize a raw `bazel aquery --output=text` dump into a canonical golden.
/// Deterministic, pure, and idempotent: `normalize(normalize(x)) == normalize(x)`.
pub fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.lines() {
        let line = line.trim_end();
        let trimmed = line.trim_start();
        // Drop bazel-internal / non-comparable metadata (comparison scope §5: we compare
        // mnemonic + argv + inputs + outputs + env — NOT bazel's ActionKey digest or its
        // config/exec-platform annotations, which razel does not reproduce verbatim).
        const DROP: &[&str] = &[
            "ActionKey:",
            "Configuration:",
            "# Configuration:",
            "Execution platform:",
            "# Execution platform:",
        ];
        if DROP.iter().any(|p| trimmed.starts_with(p)) {
            continue;
        }
        let mut s = line.to_string();
        // Normalize the variable config/repo path segment (keep the prefix → readable + idempotent).
        s = normalize_segment(&s, "bazel-out/", "<cfg>");
        s = normalize_segment(&s, "external/", "<repo>");
        // Host SDK version (macOS) — machine-specific, must be placeholdered.
        s = replace_value(&s, "-mmacosx-version-min=", "<sdk>");
        // rules_rust per-crate metadata hash (`-` + 6+ digits) — a content hash razel can't
        // reproduce, so host-volatile like the SDK (`libutil-12716653.rlib`, `metadata=-12716653`).
        s = normalize_rust_hash(&s);
        out.push_str(&s);
        out.push('\n');
    }
    out
}

/// Replace the single variable component in every `prefix<comp>/…` with `comp_placeholder`,
/// keeping the prefix (e.g. `bazel-out/darwin_arm64-fastbuild/bin/x` → `bazel-out/<cfg>/bin/x`).
/// Idempotent because the placeholder does not contain `prefix`.
fn normalize_segment(s: &str, prefix: &str, comp_placeholder: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(prefix) {
        result.push_str(&rest[..i]);
        result.push_str(prefix);
        let after = &rest[i + prefix.len()..];
        match after.find('/') {
            Some(j) => {
                result.push_str(comp_placeholder); // replaces after[..j] (the variable segment)
                result.push('/');
                rest = &after[j + 1..];
            }
            None => {
                rest = after; // prefix with no following `component/` — leave the tail as-is
            }
        }
    }
    result.push_str(rest);
    result
}

/// Replace `key<value>` with `key<placeholder>`, where `<value>` runs to the next whitespace
/// or quote (e.g. `-mmacosx-version-min=26.4` → `-mmacosx-version-min=<sdk>`). Idempotent.
fn replace_value(s: &str, key: &str, placeholder: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(key) {
        result.push_str(&rest[..i]);
        result.push_str(key);
        result.push_str(placeholder);
        let after = &rest[i + key.len()..];
        let end = after
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    result.push_str(rest);
    result
}

/// Replace every `-` + 6-or-more decimal digits with `-<hash>` — rules_rust's per-crate metadata
/// hash (`libutil-12716653.rlib`, `--codegen=metadata=-12716653`, `--extern=base=…-791554189.rlib`).
/// Razel can't reproduce Bazel's content hash, so it's normalized like a host value. Hex
/// toolchain-lib hashes have letters → not matched (they live only in inputs). Idempotent
/// (`<hash>` has no digits).
fn normalize_rust_hash(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '-' {
            let ndigits = s[i + 1..].chars().take_while(char::is_ascii_digit).count();
            if ndigits >= 6 {
                result.push_str("-<hash>");
                for _ in 0..ndigits {
                    chars.next();
                }
                continue;
            }
        }
        result.push(c);
    }
    result
}

// ── Graph-parity runner (RazelStarlarkBoundaryPlan §10 A0) ───────────────────────────
//
// Compare razel's declared action GRAPH (a *set* of actions) to the golden — not cherry-picked
// single actions, which is what let a missing action (e.g. `CppModuleMap`) go uncaught. `diff`
// takes an explicit `omit` allowlist of mnemonics razel intentionally does not model; they are
// recorded in `Report::omitted`, never silently dropped.

/// One declared action in comparable form. `argv` is order-significant; `inputs`/`outputs` are
/// compared as sorted sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub mnemonic: String,
    pub argv: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

impl Action {
    /// Set-pairing identity: mnemonic + outputs (stable once razel's path model matches Bazel's).
    pub fn key(&self) -> String {
        format!("{} -> {:?}", self.mnemonic, self.outputs)
    }
}

/// Parse a **normalized** `bazel aquery` golden into its full action set (every `action '…'` block).
pub fn parse_golden(golden: &str) -> Vec<Action> {
    golden.split("action '").skip(1).map(parse_action_block).collect()
}

fn parse_action_block(block: &str) -> Action {
    Action {
        mnemonic: line_value(block, "Mnemonic:").unwrap_or_default(),
        argv: parse_command_line(block),
        inputs: parse_bracket_list(block, "Inputs: ["),
        outputs: parse_bracket_list(block, "Outputs: ["),
    }
}

/// Text after `key` on its line, trimmed (`  Mnemonic: CppCompile` → `CppCompile`).
fn line_value(block: &str, key: &str) -> Option<String> {
    block.lines().find_map(|l| l.trim_start().strip_prefix(key).map(|v| v.trim().to_string()))
}

/// A sorted `key[a, b, c]` list (`Inputs: [...]` / `Outputs: [...]`).
fn parse_bracket_list(block: &str, key: &str) -> Vec<String> {
    let Some(start) = block.find(key) else { return Vec::new() };
    let rest = &block[start + key.len()..];
    let end = rest.find(']').unwrap_or(rest.len());
    let mut v: Vec<String> =
        rest[..end].split(", ").filter(|s| !s.is_empty()).map(str::to_string).collect();
    v.sort();
    v
}

/// The `Command Line: (exec … )` argv (drop `\` continuations; strip Bazel's single-quotes).
fn parse_command_line(block: &str) -> Vec<String> {
    const K: &str = "Command Line: (exec ";
    let Some(start) = block.find(K) else { return Vec::new() };
    let rest = &block[start + K.len()..];
    let end = rest.find(')').unwrap_or(rest.len());
    rest[..end]
        .split_whitespace()
        .filter(|t| *t != "\\")
        .map(|t| t.trim_matches('\'').to_string())
        .collect()
}

/// A discrepancy on a paired action (same `key`, differing argv/inputs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub key: String,
    pub argv_differs: bool,
    pub inputs_differ: bool,
}

/// A graph diff. `is_match()` ⇔ nothing mismatched/missing/extra (allowlisted `omitted` is fine).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub matched: Vec<String>,
    pub mismatched: Vec<Mismatch>,
    pub missing: Vec<String>, // in golden (not omitted), absent from razel
    pub extra: Vec<String>,   // in razel, absent from golden
    pub omitted: Vec<String>, // golden actions dropped by the allowlist (logged, not a failure)
}

impl Report {
    pub fn is_match(&self) -> bool {
        self.mismatched.is_empty() && self.missing.is_empty() && self.extra.is_empty()
    }
}

/// Diff razel's declared action set against the golden's. `omit` = mnemonics razel intentionally
/// does not model (e.g. `CppModuleMap`); recorded in `Report::omitted`, never silently ignored.
pub fn diff(razel: &[Action], golden: &[Action], omit: &[&str]) -> Report {
    use std::collections::{BTreeMap, BTreeSet};
    let mut report = Report::default();
    let razel_by: BTreeMap<String, &Action> = razel.iter().map(|a| (a.key(), a)).collect();
    let mut golden_keys = BTreeSet::new();

    for g in golden {
        if omit.contains(&g.mnemonic.as_str()) {
            report.omitted.push(g.key());
            continue;
        }
        golden_keys.insert(g.key());
        match razel_by.get(&g.key()) {
            None => report.missing.push(g.key()),
            Some(r) => {
                let argv_differs = r.argv != g.argv;
                let inputs_differ = r.inputs != g.inputs;
                if argv_differs || inputs_differ {
                    report.mismatched.push(Mismatch { key: g.key(), argv_differs, inputs_differ });
                } else {
                    report.matched.push(g.key());
                }
            }
        }
    }
    for r in razel {
        if !omit.contains(&r.mnemonic.as_str()) && !golden_keys.contains(&r.key()) {
            report.extra.push(r.key());
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bazel_out_config_segment() {
        assert_eq!(
            normalize("  Outputs: [bazel-out/darwin_arm64-fastbuild/bin/_objs/util/util.o]"),
            "  Outputs: [bazel-out/<cfg>/bin/_objs/util/util.o]\n"
        );
    }

    #[test]
    fn normalizes_external_repo_segment() {
        assert_eq!(
            normalize("    -c external/rules_cc++cc_configure_extension+local_config_cc/cc_wrapper.sh"),
            "    -c external/<repo>/cc_wrapper.sh\n"
        );
    }

    #[test]
    fn strips_noncomparable_metadata() {
        // ActionKey digest + Configuration + Execution platform (indented and `#` forms) drop;
        // Mnemonic/Outputs (the comparable core) stay.
        let raw = "  Mnemonic: CppCompile\n  Configuration: darwin_arm64-fastbuild\n  Execution platform: @@platforms//host:host\n  ActionKey: f003e9289ca5\n# Configuration: 680dcaf1\n  Outputs: [x]";
        assert_eq!(normalize(raw), "  Mnemonic: CppCompile\n  Outputs: [x]\n");
    }

    #[test]
    fn placeholders_macos_sdk_version() {
        assert_eq!(
            normalize("    '-mmacosx-version-min=26.4' \\"),
            "    '-mmacosx-version-min=<sdk>' \\\n"
        );
    }

    #[test]
    fn argv_order_preserved() {
        assert_eq!(
            normalize("  Command Line: (exec clang -c a.cc -o bazel-out/cfg/bin/a.o)"),
            "  Command Line: (exec clang -c a.cc -o bazel-out/<cfg>/bin/a.o)\n"
        );
    }

    #[test]
    fn normalizes_rust_metadata_hash() {
        assert_eq!(
            normalize("    '--codegen=metadata=-12716653' \\"),
            "    '--codegen=metadata=-<hash>' \\\n"
        );
        // Combined with the <cfg> segment normalization on an rlib output path.
        assert_eq!(
            normalize("  Outputs: [bazel-out/c/bin/libutil-12716653.rlib]"),
            "  Outputs: [bazel-out/<cfg>/bin/libutil-<hash>.rlib]\n"
        );
        // Short digit runs (opt-level, the cc date defines) are untouched.
        assert_eq!(normalize("    '--codegen=opt-level=0'"), "    '--codegen=opt-level=0'\n");
    }

    #[test]
    fn idempotent() {
        let raw = "  Outputs: [bazel-out/c/bin/libx-12716653.rlib]\n  Inputs: [external/r/y]\n  '-mmacosx-version-min=26.4'\n    '--codegen=metadata=-791554189'";
        let once = normalize(raw);
        assert_eq!(normalize(&once), once, "normalization must be idempotent");
    }

    #[test]
    fn parse_golden_reads_the_action_set() {
        let g = "preamble\n\
            action 'Compiling x'\n  Mnemonic: CppCompile\n  Inputs: [b.h, a.cc]\n  Outputs: [a.o]\n  \
            Command Line: (exec cc \\\n    -c \\\n    a.cc)\n\n\
            action 'Linking lib'\n  Mnemonic: CppArchive\n  Inputs: [a.o]\n  Outputs: [lib.a]\n  \
            Command Line: (exec ar rcs lib.a a.o)\n";
        let acts = parse_golden(g);
        assert_eq!(acts.len(), 2);
        assert_eq!(acts[0].mnemonic, "CppCompile");
        assert_eq!(acts[0].inputs, ["a.cc", "b.h"]); // sorted
        assert_eq!(acts[0].outputs, ["a.o"]);
        assert_eq!(acts[0].argv, ["cc", "-c", "a.cc"]); // `\` continuations dropped
        assert_eq!(acts[1].mnemonic, "CppArchive");
    }

    fn act(m: &str, argv: &[&str], out: &str) -> Action {
        Action {
            mnemonic: m.into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            inputs: vec![],
            outputs: vec![out.into()],
        }
    }

    #[test]
    fn diff_classifies_match_mismatch_missing_extra_omitted() {
        let golden = vec![
            act("CppCompile", &["cc", "-c", "x"], "x.o"),
            act("CppModuleMap", &["mm"], "x.cppmap"),
            act("CppArchive", &["ar"], "lib.a"),
        ];
        // razel: compile matches; archive argv differs; no cppmap; plus an extra link.
        let razel = vec![
            act("CppCompile", &["cc", "-c", "x"], "x.o"),
            act("CppArchive", &["libtool"], "lib.a"),
            act("CppLink", &["ld"], "bin"),
        ];
        let r = diff(&razel, &golden, &["CppModuleMap"]);
        assert_eq!(r.matched, ["CppCompile -> [\"x.o\"]"]);
        assert_eq!(r.mismatched.len(), 1);
        assert!(r.mismatched[0].argv_differs);
        assert_eq!(r.omitted, ["CppModuleMap -> [\"x.cppmap\"]"]); // logged, not missing
        assert!(r.missing.is_empty());
        assert_eq!(r.extra, ["CppLink -> [\"bin\"]"]);
        assert!(!r.is_match());
    }

    #[test]
    fn diff_matches_identical_sets_modulo_omit() {
        let golden = vec![act("CppCompile", &["t"], "x.o"), act("CppModuleMap", &["mm"], "x.cppmap")];
        let razel = vec![act("CppCompile", &["t"], "x.o")];
        let r = diff(&razel, &golden, &["CppModuleMap"]);
        assert!(r.is_match());
        assert_eq!(r.omitted.len(), 1);
    }
}
