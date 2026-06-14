//! `loaded.rs` — the loading-phase node model (RazelCrateUniverseDesign §11.1; plan P0.1+).
//!
//! P0.1 lands the [`RawAttr`] model + its NORMATIVE canonical stringification — the string that
//! `attr(name, regex, x)` / `--output=build` match against, pinned so goldens are not "whatever
//! the first impl printed". The Value→`RawAttr` snapshot + label extraction (P0.2), the
//! `LoadedTarget`/`QueryNode` nodes (P0.3), edge-kind resolution (P0.4), and capture-at-load
//! (P0.5) build on top.

#![allow(dead_code)] // P0.1 lands the type + stringify; capture wiring consumes it in P0.2–P0.5.

use std::fmt::Write as _;

/// A loading-phase attribute value — the closed, serializable, de-Starlark'd model. Captured at
/// load BEFORE freeze; `select()`/`+` are retained UNRESOLVED (query reads them raw; the build
/// resolves them). Design §11.1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawAttr {
    Str(String),
    Int(i64),
    Bool(bool),
    None,
    List(Vec<RawAttr>),
    /// `selects.with_or` condition groups, pre-desugar.
    Tuple(Vec<RawAttr>),
    /// Insertion/source order preserved; a key MAY itself be a `Label`.
    Dict(Vec<(RawAttr, RawAttr)>),
    /// A canonical label `@repo//pkg:name`.
    Label(String),
    /// An UNRESOLVED `select()`: condition label → value, plus the optional default arm.
    Select { arms: Vec<(String, RawAttr)>, default: Option<Box<RawAttr>> },
    /// Models `list + select(...)` and friends.
    Concat(Vec<RawAttr>),
}

impl RawAttr {
    /// The NORMATIVE canonical stringification (design §11.1): Starlark-`repr`-shaped, stable, and
    /// deterministic, so `attr()` goldens are reproducible. Each `RawAttr` renders the same way
    /// wherever it appears (a `Str` is always quoted, a `Label` always bare), so dict keys and
    /// select conditions compose without special-casing.
    pub(crate) fn canonical(&self) -> String {
        let mut s = String::new();
        self.write_canonical(&mut s);
        s
    }

    fn write_canonical(&self, out: &mut String) {
        match self {
            RawAttr::Str(v) => write_quoted(out, v),
            RawAttr::Int(n) => {
                let _ = write!(out, "{n}");
            }
            RawAttr::Bool(b) => out.push_str(if *b { "True" } else { "False" }), // Starlark casing
            RawAttr::None => out.push_str("None"),
            RawAttr::List(items) => write_seq(out, '[', ']', items, false),
            RawAttr::Tuple(items) => write_seq(out, '(', ')', items, true),
            RawAttr::Dict(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    k.write_canonical(out);
                    out.push_str(": ");
                    v.write_canonical(out);
                }
                out.push('}');
            }
            RawAttr::Label(l) => out.push_str(l),
            RawAttr::Select { arms, default } => {
                out.push_str("select({");
                for (i, (cond, v)) in arms.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(cond); // canonical label, bare (consistent with `Label`)
                    out.push_str(": ");
                    v.write_canonical(out);
                }
                out.push('}');
                if let Some(d) = default {
                    out.push_str(", default=");
                    d.write_canonical(out);
                }
                out.push(')');
            }
            RawAttr::Concat(items) => {
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(" + ");
                    }
                    it.write_canonical(out);
                }
            }
        }
    }
}

/// `[a, b]` / `(a, b)`; a single-element tuple gets Starlark's trailing comma — `(a,)`.
fn write_seq(out: &mut String, open: char, close: char, items: &[RawAttr], tuple: bool) {
    out.push(open);
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        it.write_canonical(out);
    }
    if tuple && items.len() == 1 {
        out.push(',');
    }
    out.push(close);
}

/// Double-quote with `\"` / `\\` / `\n` escaping (design §11.1).
fn write_quoted(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> RawAttr {
        RawAttr::Str(v.into())
    }

    #[test]
    fn scalars_render_starlark_shaped() {
        assert_eq!(RawAttr::Int(42).canonical(), "42");
        assert_eq!(RawAttr::Bool(true).canonical(), "True");
        assert_eq!(RawAttr::Bool(false).canonical(), "False");
        assert_eq!(RawAttr::None.canonical(), "None");
    }

    #[test]
    fn strings_are_quoted_and_escaped() {
        assert_eq!(s("hi").canonical(), "\"hi\"");
        // a quote, a backslash, and a newline all escape.
        assert_eq!(s("a\"b\\c\nd").canonical(), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn lists_and_tuples() {
        assert_eq!(RawAttr::List(vec![s("a"), s("b")]).canonical(), "[\"a\", \"b\"]");
        assert_eq!(RawAttr::List(vec![]).canonical(), "[]");
        assert_eq!(RawAttr::Tuple(vec![s("a"), s("b")]).canonical(), "(\"a\", \"b\")");
        // a singleton tuple keeps Starlark's trailing comma.
        assert_eq!(RawAttr::Tuple(vec![s("a")]).canonical(), "(\"a\",)");
    }

    #[test]
    fn dict_preserves_source_order_and_label_keys_render_bare() {
        let d = RawAttr::Dict(vec![
            (s("z"), RawAttr::Int(1)),
            (RawAttr::Label("@p//:x".into()), s("v")),
        ]);
        // insertion order, NOT sorted; the label key is bare, the string key quoted.
        assert_eq!(d.canonical(), "{\"z\": 1, @p//:x: \"v\"}");
    }

    #[test]
    fn label_renders_bare() {
        assert_eq!(RawAttr::Label("@rules_rust++crate+crates__blake3-1.8.2//:blake3".into()).canonical(),
                   "@rules_rust++crate+crates__blake3-1.8.2//:blake3");
    }

    #[test]
    fn select_with_and_without_default() {
        let no_default = RawAttr::Select {
            arms: vec![("@p//:cfg".into(), RawAttr::List(vec![s("a")]))],
            default: None,
        };
        assert_eq!(no_default.canonical(), "select({@p//:cfg: [\"a\"]})");
        let with_default = RawAttr::Select {
            arms: vec![("@p//:cfg".into(), RawAttr::List(vec![s("a")]))],
            default: Some(Box::new(RawAttr::List(vec![s("d")]))),
        };
        assert_eq!(with_default.canonical(), "select({@p//:cfg: [\"a\"]}, default=[\"d\"])");
    }

    #[test]
    fn concat_models_list_plus_select() {
        let c = RawAttr::Concat(vec![
            RawAttr::List(vec![s("base")]),
            RawAttr::Select { arms: vec![("@p//:c".into(), RawAttr::List(vec![s("x")]))], default: None },
        ]);
        assert_eq!(c.canonical(), "[\"base\"] + select({@p//:c: [\"x\"]})");
    }

    #[test]
    fn nesting_round_trips_structurally() {
        let nested = RawAttr::List(vec![
            RawAttr::Dict(vec![(s("k"), RawAttr::List(vec![RawAttr::Int(1), RawAttr::None]))]),
        ]);
        assert_eq!(nested.canonical(), "[{\"k\": [1, None]}]");
    }
}
