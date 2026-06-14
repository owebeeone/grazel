//! `loaded.rs` — the loading-phase node model (RazelCrateUniverseDesign §11.1; plan P0.1+).
//!
//! P0.1 lands the [`RawAttr`] model + its NORMATIVE canonical stringification — the string that
//! `attr(name, regex, x)` / `--output=build` match against, pinned so goldens are not "whatever
//! the first impl printed". The Value→`RawAttr` snapshot + label extraction (P0.2), the
//! `LoadedTarget`/`QueryNode` nodes (P0.3), edge-kind resolution (P0.4), and capture-at-load
//! (P0.5) build on top.

#![allow(dead_code)] // P0.1 lands the type + stringify; capture wiring consumes it in P0.2–P0.5.

use crate::labels::LabelV;
use crate::selects::{
    FrozenSelectBranches, FrozenSelectExpr, SelectBranches, SelectExpr, key_string,
};
use starlark::values::dict::DictRef;
use starlark::values::list::ListRef;
use starlark::values::tuple::TupleRef;
use starlark::values::{Heap, Value, ValueLike};
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

// ── P0.2: Value → RawAttr snapshot + schema-aware label extraction ──────────────────────────────

/// Snapshot a Starlark attr `Value` into a [`RawAttr`], retaining `select()`/`+` UNRESOLVED. No
/// heap escapes into the result; no resolution (that is the build's job). Mirrors the downcast
/// shape of `selects::resolve_attr_value` (live + frozen `SelectBranches`/`SelectExpr`), but
/// CAPTURES the structure instead of resolving it.
pub(crate) fn value_to_raw<'v>(heap: Heap<'v>, v: Value<'v>) -> RawAttr {
    // 1. an unresolved `select({...})` — capture its arms + the `//conditions:default` arm.
    let branches: Option<Vec<(Value<'v>, Value<'v>)>> =
        if let Some(sb) = v.downcast_ref::<SelectBranches<'v>>() {
            Some(sb.branches.clone())
        } else if let Some(sb) = v.downcast_ref::<FrozenSelectBranches>() {
            Some(sb.branches.iter().map(|(k, x)| (k.to_value(), x.to_value())).collect())
        } else {
            None
        };
    if let Some(branches) = branches {
        let mut arms = Vec::new();
        let mut default = None;
        for (k, val) in branches {
            let cond = key_string(heap, k).unwrap_or_else(|| k.to_string());
            let rv = value_to_raw(heap, val);
            if cond == "//conditions:default" {
                default = Some(Box::new(rv));
            } else {
                arms.push((cond, rv));
            }
        }
        return RawAttr::Select { arms, default };
    }
    // 2. a select EXPRESSION (`list + select(...) + ...`).
    let parts: Option<Vec<Value<'v>>> = if let Some(se) = v.downcast_ref::<SelectExpr<'v>>() {
        Some(se.parts.clone())
    } else if let Some(se) = v.downcast_ref::<FrozenSelectExpr>() {
        Some(se.parts.iter().map(|p| p.to_value()).collect())
    } else {
        None
    };
    if let Some(parts) = parts {
        return RawAttr::Concat(parts.into_iter().map(|p| value_to_raw(heap, p)).collect());
    }
    // 3. a `Label()` struct (`clean_dep()` results).
    if let Some(l) = v.downcast_ref::<LabelV>() {
        return RawAttr::Label(l.to_string());
    }
    // 4. scalars.
    if v.is_none() {
        return RawAttr::None;
    }
    if let Some(b) = v.unpack_bool() {
        return RawAttr::Bool(b);
    }
    if let Some(i) = v.unpack_i32() {
        return RawAttr::Int(i as i64);
    }
    if let Some(s) = v.unpack_str() {
        return RawAttr::Str(s.to_string());
    }
    // 5. containers.
    if let Some(list) = ListRef::from_value(v) {
        return RawAttr::List(list.iter().map(|e| value_to_raw(heap, e)).collect());
    }
    if let Some(t) = TupleRef::from_value(v) {
        return RawAttr::Tuple(t.iter().map(|e| value_to_raw(heap, e)).collect());
    }
    if let Some(d) = DictRef::from_value(v) {
        return RawAttr::Dict(
            d.iter().map(|(k, val)| (value_to_raw(heap, k), value_to_raw(heap, val))).collect(),
        );
    }
    // 6. fallback: an unknown value type → its display (capture never loses it silently).
    RawAttr::Str(v.to_string())
}

/// A raw label reference collected from a label-valued attr — the label string + which attr it
/// came from + whether it is a `select()` CONDITION (not a value). NO edge KIND yet — that is
/// P0.4, which needs the package's `output_index`/`aliases`/`config_specs` to classify.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawLabelRef {
    pub(crate) label: String,
    pub(crate) attr: String,
    pub(crate) in_select_condition: bool,
}

/// **R1 (the schema-aware fix):** native attrs arrive as plain strings, so a value-only walk
/// can't tell a label from a string. This is the per-`rule_class` set of LABEL-valued attrs;
/// only these are walked for label refs (so `tags`/`crate_features`/`edition` contribute none).
/// Extend as rules land — for a Starlark-defined rule this comes from its `attr.label*` decls.
pub(crate) fn label_attrs(rule_class: &str) -> &'static [&'static str] {
    match rule_class {
        "filegroup" => &["srcs", "data"],
        "alias" => &["actual"],
        "genrule" => &["srcs", "tools", "outs"],
        "config_setting" => &["constraint_values", "flag_values"],
        "rust_library" | "rust_binary" | "rust_shared_library" | "rust_library_group"
        | "rust_proc_macro" => {
            &["srcs", "deps", "proc_macro_deps", "aliases", "compile_data", "data"]
        }
        "cc_library" | "cc_binary" => &["srcs", "hdrs", "deps", "data"],
        _ => &[],
    }
}

/// Walk a label-valued attr's `RawAttr`, collecting raw label refs. `Str`/`Label` are refs;
/// containers + `Concat` recurse; `select()` CONDITION labels are flagged (and arms/default
/// recurse); a `Dict` walks KEYS only (label-keyed attrs like `aliases` map label → rename
/// string — the value is not a label). `None`/scalars contribute nothing.
pub(crate) fn extract_label_refs(attr: &str, raw: &RawAttr, out: &mut Vec<RawLabelRef>) {
    let push = |out: &mut Vec<RawLabelRef>, label: String, cond: bool| {
        out.push(RawLabelRef { label, attr: attr.to_string(), in_select_condition: cond });
    };
    match raw {
        RawAttr::Str(s) => push(out, s.clone(), false),
        RawAttr::Label(l) => push(out, l.clone(), false),
        RawAttr::List(xs) | RawAttr::Tuple(xs) | RawAttr::Concat(xs) => {
            xs.iter().for_each(|x| extract_label_refs(attr, x, out));
        }
        RawAttr::Dict(pairs) => pairs.iter().for_each(|(k, _v)| extract_label_refs(attr, k, out)),
        RawAttr::Select { arms, default } => {
            for (cond, v) in arms {
                push(out, cond.clone(), true);
                extract_label_refs(attr, v, out);
            }
            if let Some(d) = default {
                extract_label_refs(attr, d, out);
            }
        }
        RawAttr::Int(_) | RawAttr::Bool(_) | RawAttr::None => {}
    }
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

    // ── P0.2: schema-aware label extraction (pure over RawAttr) ──────────────────────────────

    fn refs(attr: &str, raw: &RawAttr) -> Vec<RawLabelRef> {
        let mut out = Vec::new();
        extract_label_refs(attr, raw, &mut out);
        out
    }

    #[test]
    fn deps_list_yields_label_refs_but_a_non_label_attr_is_not_walked() {
        // `deps = ["//:x", "//:y"]` → two refs (the loader only walks attrs the SCHEMA marks).
        let r = refs("deps", &RawAttr::List(vec![s("//:x"), s("//:y")]));
        assert_eq!(r.iter().map(|r| r.label.as_str()).collect::<Vec<_>>(), ["//:x", "//:y"]);
        assert!(r.iter().all(|r| !r.in_select_condition && r.attr == "deps"));
        // R1: `tags`/`crate_features`/`edition` are NOT label-valued, so the loader never walks
        // them — that is what stops `tags = ["x"]` from minting a bogus edge.
        let labelled = label_attrs("rust_library");
        assert!(labelled.contains(&"deps") && labelled.contains(&"srcs"));
        assert!(!labelled.contains(&"tags") && !labelled.contains(&"edition")
            && !labelled.contains(&"crate_features"));
    }

    #[test]
    fn select_yields_condition_labels_flagged_plus_arms_and_default() {
        let sel = RawAttr::Select {
            arms: vec![("@p//:cfg".into(), RawAttr::List(vec![s("//:a")]))],
            default: Some(Box::new(RawAttr::List(vec![s("//:d")]))),
        };
        let r = refs("deps", &sel);
        // the condition label is present AND flagged; the arm value + default are plain refs.
        assert_eq!(r[0], RawLabelRef { label: "@p//:cfg".into(), attr: "deps".into(), in_select_condition: true });
        assert_eq!(r[1].label, "//:a");
        assert_eq!(r[2].label, "//:d");
        assert!(!r[1].in_select_condition && !r[2].in_select_condition);
    }

    #[test]
    fn none_and_scalars_yield_nothing() {
        assert!(refs("deps", &RawAttr::None).is_empty());
        assert!(refs("deps", &RawAttr::Int(3)).is_empty());
        assert!(refs("deps", &RawAttr::Bool(true)).is_empty());
    }

    #[test]
    fn label_keyed_dict_walks_keys_not_rename_values() {
        // `aliases = {"@x//:lib": "renamed"}` → the KEY is the label; "renamed" is not.
        let d = RawAttr::Dict(vec![(RawAttr::Label("@x//:lib".into()), s("renamed"))]);
        let r = refs("aliases", &d);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "@x//:lib");
    }

    #[test]
    fn concat_recurses_lists_and_selects() {
        let c = RawAttr::Concat(vec![
            RawAttr::List(vec![s("//:base")]),
            RawAttr::Select { arms: vec![("@p//:c".into(), RawAttr::List(vec![s("//:x")]))], default: None },
        ]);
        let got: Vec<(String, bool)> =
            refs("deps", &c).into_iter().map(|r| (r.label, r.in_select_condition)).collect();
        assert_eq!(
            got,
            [("//:base".to_string(), false), ("@p//:c".into(), true), ("//:x".into(), false)]
        );
    }
}
