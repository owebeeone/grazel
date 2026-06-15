//! crate-universe P3.7 (§6.1): the build-script **flags-file schema + parser** — the boundary
//! between the build-script run subcommand (P3.8, the WRITER) and the rustc subcommand (P3.9, the
//! READER). Co-locating both here (per the P3.8 seam decision) keeps ONE definition of the
//! flags-file contract. Parses a build script's stdout into emission-ordered, duplicate-preserving
//! directive records; the recognized `rustc-*` directives serialize to the JSONL flags file
//! (`{"kind","args"}` per line). Pure + golden-tested, no I/O.
//!
//! Grammar (§6.1): one directive per line, `cargo:KEY=VALUE` (pre-1.77) OR `cargo::KEY=VALUE`
//! (1.77+). Non-`cargo:` lines are ordinary script output and ignored.

use serde::{Deserialize, Serialize};

/// One flags-file line: a recognized `rustc-*` directive (emission order + duplicates preserved).
/// `args` is already tokenized — notably `rustc-flags` is split HERE (§6.1), so the rustc reader
/// never re-tokenizes or shell-quotes. (De)serializes to one JSON object per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagsRecord {
    pub kind: String,
    pub args: Vec<String>,
}

/// A recorded `rerun-if-*` directive (§5.2 slice-1: recorded, NOT yet narrowing the watch set; the
/// run action folds `rerun-if-env-changed` keys into its env allowlist / cache key).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rerun {
    Changed(String),    // rerun-if-changed=<path>
    EnvChanged(String), // rerun-if-env-changed=<var>
}

/// The structured result of parsing a build script's stdout (§6.1). `flags` is the flags file; the
/// rest are side channels the run subcommand consumes: `warnings`→stderr, `error`→fail the run,
/// `dep_metadata`→republished as `DEP_<LINKS>_<K>`, `rerun`→cache key, `deviations`→one parity-log
/// line each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BuildScriptParse {
    pub flags: Vec<FlagsRecord>,
    pub dep_metadata: Vec<(String, String)>,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub rerun: Vec<Rerun>,
    pub deviations: Vec<String>,
}

/// The recognized flags-file directives whose value is a SINGLE arg verbatim (only `rustc-flags` is
/// multi-arg — it tokenizes — and is handled separately).
const SINGLE_ARG_FLAGS: &[&str] = &[
    "rustc-cfg",
    "rustc-env",
    "rustc-link-lib",
    "rustc-link-search",
    "rustc-link-arg",
    "rustc-cdylib-link-arg",
];

/// Parse build-script stdout → [`BuildScriptParse`] (§6.1). Pure; emission order + duplicates
/// preserved. For a RESERVED key, single- vs double-colon is irrelevant (both → the directive); for
/// a non-reserved key, double-colon → a recorded deviation, single-colon → `links` metadata.
pub fn parse_build_script_output(stdout: &str) -> BuildScriptParse {
    let mut out = BuildScriptParse::default();
    for raw in stdout.lines() {
        let line = raw.trim_end(); // tolerate trailing CRLF / whitespace
        // `cargo::` (1.77+) is checked before `cargo:` (pre-1.77) — they share the `cargo:` head.
        let (body, double) = if let Some(b) = line.strip_prefix("cargo::") {
            (b, true)
        } else if let Some(b) = line.strip_prefix("cargo:") {
            (b, false)
        } else {
            continue; // ordinary script output
        };
        let Some((key, value)) = body.split_once('=') else {
            // A directive with no `=` is malformed: double-colon records a deviation; a bare
            // single-colon line is treated as ordinary noise (ignored).
            if double {
                out.deviations.push(format!(
                    "cargo::{body} — malformed build-script directive (no `=`; recorded, not applied)"
                ));
            }
            continue;
        };
        if SINGLE_ARG_FLAGS.contains(&key) {
            out.flags.push(FlagsRecord { kind: key.to_string(), args: vec![value.to_string()] });
        } else if key == "rustc-flags" {
            // §6.1: tokenize HERE (whitespace, no shell quoting) so the reader never re-parses.
            out.flags.push(FlagsRecord {
                kind: key.to_string(),
                args: value.split_whitespace().map(str::to_string).collect(),
            });
        } else if key == "warning" {
            out.warnings.push(value.to_string());
        } else if key == "error" {
            // First error wins; the run fails the moment any `error=` is present (§6.1).
            out.error.get_or_insert_with(|| value.to_string());
        } else if key == "rerun-if-changed" {
            out.rerun.push(Rerun::Changed(value.to_string()));
        } else if key == "rerun-if-env-changed" {
            out.rerun.push(Rerun::EnvChanged(value.to_string()));
        } else if key == "metadata" {
            // 1.77+ explicit metadata: `metadata=K=V` → `(K, V)`.
            if let Some((k, v)) = value.split_once('=') {
                out.dep_metadata.push((k.to_string(), v.to_string()));
            }
        } else if double {
            // Unknown reserved `cargo::<key>` → recorded + one deviation line (never fatal, §6.1).
            out.deviations.push(format!(
                "cargo::{key} — unrecognized build-script directive (recorded, not applied)"
            ));
        } else {
            // pre-1.77 single-colon, non-reserved → `links` metadata `(KEY, VALUE)`; the run action
            // republishes it as `DEP_<LINKS>_<KEY>` only when the crate sets `links`.
            out.dep_metadata.push((key.to_string(), value.to_string()));
        }
    }
    out
}

/// Serialize the flags records to the JSONL flags file (§6.1): one `{"kind","args"}` object per
/// line, in emission order. JSON escaping handles values containing tabs/spaces/`=`/quotes.
pub fn flags_file_jsonl(flags: &[FlagsRecord]) -> String {
    let mut s = String::new();
    for rec in flags {
        // serde_json never fails for this all-string shape; stay non-panicking defensively.
        s.push_str(&serde_json::to_string(rec).unwrap_or_else(|_| "{}".into()));
        s.push('\n');
    }
    s
}

/// Read a JSONL flags file back into records (the rustc subcommand's reader, P3.9). Blank lines are
/// skipped; a malformed line is an error (the contract is machine-written, never hand-edited).
pub fn read_flags_jsonl(jsonl: &str) -> Result<Vec<FlagsRecord>, String> {
    let mut out = Vec::new();
    for (n, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str::<FlagsRecord>(line)
                .map_err(|e| format!("flags file line {}: {e}", n + 1))?,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p37_parses_each_directive_kind_in_emission_order() {
        let stdout = "\
cargo::rustc-cfg=feature=\"simd\"\n\
cargo::rustc-env=BUILD_TS=123\n\
cargo::rustc-link-lib=static=blake3\n\
cargo::rustc-link-search=native=/opt/lib\n\
cargo::rustc-link-arg=-Wl,-z,now\n\
cargo::rustc-cdylib-link-arg=-undefined\n\
cargo::rustc-flags=-l dylib=foo -L /bar\n\
some ordinary script output, ignored\n\
cargo::rustc-cfg=feature=\"avx\"\n"; // a DUPLICATE kind — must be preserved
        let p = parse_build_script_output(stdout);
        let kinds: Vec<&str> = p.flags.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "rustc-cfg",
                "rustc-env",
                "rustc-link-lib",
                "rustc-link-search",
                "rustc-link-arg",
                "rustc-cdylib-link-arg",
                "rustc-flags",
                "rustc-cfg",
            ],
            "emission order + duplicates preserved; the non-cargo line dropped"
        );
        // Single-arg directives keep the whole value as ONE arg (no tokenization)…
        assert_eq!(p.flags[0].args, ["feature=\"simd\""]);
        assert_eq!(p.flags[1].args, ["BUILD_TS=123"], "rustc-env value (KEY=val) stays one arg");
        // …rustc-flags is tokenized at parse time (whitespace) so the reader never re-parses.
        let rf = p.flags.iter().find(|r| r.kind == "rustc-flags").unwrap();
        assert_eq!(rf.args, ["-l", "dylib=foo", "-L", "/bar"]);
        // JSONL: one object per line, ≥1 line per kind, JSON-escaped values; round-trips.
        let jsonl = flags_file_jsonl(&p.flags);
        assert_eq!(jsonl.lines().count(), 8, "one JSON line per record: {jsonl}");
        assert_eq!(
            jsonl.lines().next().unwrap(),
            r#"{"kind":"rustc-cfg","args":["feature=\"simd\""]}"#,
            "JSON object shape + escaping: {jsonl}"
        );
        assert_eq!(read_flags_jsonl(&jsonl).unwrap(), p.flags, "JSONL round-trips");
    }

    #[test]
    fn p37_side_channels_metadata_warning_error_rerun_deviation() {
        let stdout = "\
cargo::metadata=include=/usr/include/blake3\n\
cargo:nonreserved=legacy-value\n\
cargo::warning=deprecated API in use\n\
cargo::rerun-if-changed=build.rs\n\
cargo::rerun-if-env-changed=BLAKE3_FORCE_SOFT\n\
cargo::unknown-future-directive=whatever\n\
cargo::error=missing system blake3\n";
        let p = parse_build_script_output(stdout);
        // metadata (1.77+) AND a pre-1.77 non-reserved single-colon key both → dep_metadata.
        assert_eq!(
            p.dep_metadata,
            [
                ("include".to_string(), "/usr/include/blake3".to_string()),
                ("nonreserved".to_string(), "legacy-value".to_string()),
            ]
        );
        assert_eq!(p.warnings, ["deprecated API in use"]);
        assert_eq!(
            p.rerun,
            [Rerun::Changed("build.rs".into()), Rerun::EnvChanged("BLAKE3_FORCE_SOFT".into())]
        );
        assert_eq!(p.error.as_deref(), Some("missing system blake3"));
        // unknown reserved `cargo::<key>` → exactly one deviation line, never fatal.
        assert_eq!(p.deviations.len(), 1, "{:?}", p.deviations);
        assert!(p.deviations[0].contains("unknown-future-directive"), "{:?}", p.deviations);
        // none of the side channels leak into the flags file.
        assert!(p.flags.is_empty(), "side-channel directives are not flags: {:?}", p.flags);
    }

    #[test]
    fn p37_pre_177_single_colon_reserved_is_the_directive_not_metadata() {
        // §6.1: a pre-1.77 single-colon key that IS reserved is treated as that directive.
        let p = parse_build_script_output("cargo:rustc-link-lib=static=z\ncargo:rustc-flags=-L /x\n");
        assert_eq!(p.flags.len(), 2, "reserved single-colon → directives: {:?}", p.flags);
        assert_eq!(p.flags[0].kind, "rustc-link-lib");
        assert_eq!(p.flags[1].args, ["-L", "/x"], "rustc-flags tokenized even single-colon");
        assert!(p.dep_metadata.is_empty(), "reserved keys are not metadata: {:?}", p.dep_metadata);
    }
}
