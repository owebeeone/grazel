//! P2.2: the `recordedInputs` grammar (RazelCrateUniverseDesign §2.3) — the staleness inputs the
//! lock records as a flat list of escaped, tagged strings (`RepoRecordedInput`). Source-pinned
//! wire format: `<PREFIX>:escape(id) escape(value)`, where `escape()` maps space→`\s`,
//! newline→`\n`, null→`\0`. A naïve split on `:`/space misparses labels + null values — this
//! unescapes.

/// One parsed recorded input. The five prefixes (§2.3).
#[derive(Debug, Clone, PartialEq)]
pub enum RecordedInput {
    /// `FILE:<label> <value>` — value ∈ {DIR, ENOENT, hex sha256}. These gate staleness.
    File { label: String, value: FileValue },
    /// `DIRENTS:<id> <value>` — directory listing fingerprint.
    Dirents { id: String, value: String },
    /// `DIRTREE:<id> <value>` — directory subtree fingerprint.
    Dirtree { id: String, value: String },
    /// `ENV:<NAME> <value|\0>` — generator env; `None` = unset.
    Env { name: String, value: Option<String> },
    /// `REPO_MAPPING:<source>,<apparent> <canonical|\0>`.
    RepoMapping { source: String, apparent: String, canonical: Option<String> },
}

/// A `FILE` input's value.
#[derive(Debug, Clone, PartialEq)]
pub enum FileValue {
    Dir,
    Enoent,
    Sha256(String),
}

/// Parse the raw `recordedInputs` list; an unknown prefix is a loud error.
pub fn parse_recorded(lines: &[String]) -> Result<Vec<RecordedInput>, String> {
    lines.iter().map(|l| parse_one(l)).collect()
}

/// `escape()`'s inverse: `\s`→space, `\n`→newline, `\0`→NUL, `\\`→`\`.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('s') => out.push(' '),
                Some('n') => out.push('\n'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_one(line: &str) -> Result<RecordedInput, String> {
    let (prefix, rest) = line
        .split_once(':')
        .ok_or_else(|| format!("recorded input has no `PREFIX:` tag: `{line}`"))?;
    // id and value split on the first LITERAL space (real spaces are escaped `\s`).
    let (raw_id, raw_value) = rest.split_once(' ').unwrap_or((rest, ""));
    let id = unescape(raw_id);
    // A value of `\0` is the null/unset sentinel.
    let value: Option<String> = if raw_value == "\\0" { None } else { Some(unescape(raw_value)) };

    match prefix {
        "FILE" => Ok(RecordedInput::File {
            label: id,
            value: match value.as_deref() {
                Some("DIR") => FileValue::Dir,
                Some("ENOENT") => FileValue::Enoent,
                Some(h) => FileValue::Sha256(h.to_string()),
                None => return Err(format!("FILE recorded input has no value: `{line}`")),
            },
        }),
        "DIRENTS" => Ok(RecordedInput::Dirents { id, value: value.unwrap_or_default() }),
        "DIRTREE" => Ok(RecordedInput::Dirtree { id, value: value.unwrap_or_default() }),
        "ENV" => Ok(RecordedInput::Env { name: id, value }),
        "REPO_MAPPING" => {
            let (source, apparent) = id.split_once(',').unwrap_or((id.as_str(), ""));
            Ok(RecordedInput::RepoMapping {
                source: source.to_string(),
                apparent: apparent.to_string(),
                canonical: value,
            })
        }
        other => Err(format!("unknown recorded-input prefix `{other}` in `{line}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(s: &str) -> RecordedInput {
        parse_one(s).unwrap_or_else(|e| panic!("parse `{s}`: {e}"))
    }

    #[test]
    fn file_value_variants() {
        assert_eq!(
            one("FILE:@@//Cargo.lock abc123"),
            RecordedInput::File { label: "@@//Cargo.lock".into(), value: FileValue::Sha256("abc123".into()) }
        );
        assert_eq!(one("FILE:@@//d DIR"), RecordedInput::File { label: "@@//d".into(), value: FileValue::Dir });
        assert_eq!(one("FILE:@@//x ENOENT"), RecordedInput::File { label: "@@//x".into(), value: FileValue::Enoent });
        // a label containing ':' survives (the first ':' is the prefix separator).
        assert_eq!(
            one("FILE:@@//pkg:name sha"),
            RecordedInput::File { label: "@@//pkg:name".into(), value: FileValue::Sha256("sha".into()) }
        );
    }

    #[test]
    fn env_unset_and_set() {
        assert_eq!(one("ENV:CARGO_BAZEL_REPIN \\0"), RecordedInput::Env { name: "CARGO_BAZEL_REPIN".into(), value: None });
        assert_eq!(one("ENV:FOO bar"), RecordedInput::Env { name: "FOO".into(), value: Some("bar".into()) });
    }

    #[test]
    fn unescapes_space_newline_null() {
        // `a\sb` → "a b"; `x\ny` → "x\ny".
        assert_eq!(one("ENV:K a\\sb"), RecordedInput::Env { name: "K".into(), value: Some("a b".into()) });
        assert_eq!(one("ENV:K x\\ny"), RecordedInput::Env { name: "K".into(), value: Some("x\ny".into()) });
    }

    #[test]
    fn repo_mapping_and_unknown_prefix() {
        assert_eq!(
            one("REPO_MAPPING:src,apparent @@canonical+"),
            RecordedInput::RepoMapping {
                source: "src".into(),
                apparent: "apparent".into(),
                canonical: Some("@@canonical+".into()),
            }
        );
        assert!(parse_one("WAT:x y").unwrap_err().contains("unknown recorded-input prefix `WAT`"));
        assert!(parse_one("noprefix").unwrap_err().contains("no `PREFIX:`"));
    }

    #[test]
    fn parses_a_real_block() {
        let lines: Vec<String> = [
            "ENV:CARGO_BAZEL_REPIN \\0",
            "FILE:@@//Cargo.lock 3ab",
            "FILE:@@//crates/razel-core/Cargo.toml 9cd",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let parsed = parse_recorded(&lines).unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(matches!(parsed[0], RecordedInput::Env { value: None, .. }));
        assert!(matches!(parsed[1], RecordedInput::File { value: FileValue::Sha256(_), .. }));
    }
}
