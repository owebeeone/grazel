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
    fn idempotent() {
        let raw = "  Outputs: [bazel-out/c/bin/x]\n  Inputs: [external/r/y]\n  '-mmacosx-version-min=26.4'";
        let once = normalize(raw);
        assert_eq!(normalize(&once), once, "normalization must be idempotent");
    }
}
