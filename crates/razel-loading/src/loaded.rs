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
use crate::state::{Session, canon_label, pkg_of, session};
use razel_ir::TargetKind;
use starlark::collections::SmallMap;
use starlark::eval::Evaluator;
use starlark::values::dict::DictRef;
use starlark::values::list::ListRef;
use starlark::values::tuple::TupleRef;
use starlark::values::{Heap, Value, ValueLike};
use std::fmt::Write as _;
use std::path::Path;

/// A loading-phase attribute value — the closed, serializable, de-Starlark'd model. Captured at
/// load BEFORE freeze; `select()`/`+` are retained UNRESOLVED (query reads them raw; the build
/// resolves them). Design §11.1.
#[derive(Debug, Clone, PartialEq)]
pub enum RawAttr {
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
    pub fn canonical(&self) -> String {
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
pub fn value_to_raw<'v>(heap: Heap<'v>, v: Value<'v>) -> RawAttr {
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
pub struct RawLabelRef {
    pub label: String,
    pub attr: String,
    pub in_select_condition: bool,
}

/// **R1 (the schema-aware fix):** native attrs arrive as plain strings, so a value-only walk
/// can't tell a label from a string. This is the per-`rule_class` set of LABEL-valued attrs;
/// only these are walked for label refs (so `tags`/`crate_features`/`edition` contribute none).
/// Extend as rules land — for a Starlark-defined rule this comes from its `attr.label*` decls.
pub fn label_attrs(rule_class: &str) -> &'static [&'static str] {
    match rule_class {
        "filegroup" => &["srcs", "data"],
        "alias" => &["actual"],
        "genrule" => &["srcs", "tools", "exec_tools"],
        "config_setting" => &["constraint_values", "flag_values"],
        "rust_library" | "rust_binary" | "rust_shared_library" | "rust_library_group"
        | "rust_proc_macro" => {
            &["srcs", "deps", "proc_macro_deps", "aliases", "compile_data", "data"]
        }
        "cc_library" | "cc_binary" => &["srcs", "hdrs", "deps", "data"],
        // P5.1 (§11.2): the build-script runner's label-valued attrs — its build-deps/srcs/data are
        // `deps()` edges, plus `link_deps` (the §6 cross-build-script channel) + the `rustc_env_files`
        // → `cargo_toml_env_vars` rule edge. (The `:_bs_`/`:_bs-` macro children are added as
        // synthetic edges by `capture_cargo_build_script`, not via attrs.)
        "cargo_build_script" => &["srcs", "deps", "data", "compile_data", "link_deps", "rustc_env_files"],
        _ => &[],
    }
}

/// Walk a label-valued attr's `RawAttr`, collecting raw label refs. `Str`/`Label` are refs;
/// containers + `Concat` recurse; `select()` CONDITION labels are flagged (and arms/default
/// recurse); a `Dict` walks KEYS only (label-keyed attrs like `aliases` map label → rename
/// string — the value is not a label). `None`/scalars contribute nothing.
pub fn extract_label_refs(attr: &str, raw: &RawAttr, out: &mut Vec<RawLabelRef>) {
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

// ── P0.3: the loading-phase node model (LoadedTarget / QueryNode / Edge) — design §11.1 ─────────

/// A typed label-edge in the loading-phase graph. `kind` is assigned by the P0.4 resolution pass
/// (it needs the package's `output_index`/`aliases`/`config_specs`); `attr` is the provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub to: String,
    pub kind: EdgeKind,
    pub attr: String,
}

/// The edge-kind taxonomy (design §11.2). `deps()`/`rdeps()` traverse the union; `--implicit_deps`
/// gates `Implicit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Rule,
    Alias,
    SourceFile,
    GeneratedFile,
    /// A `select()` condition label (`@platforms//…`, a `config_setting`).
    ConfigSetting,
    Implicit,
}

/// A loading-phase rule node: raw attrs (UNRESOLVED `select()`s), the query-facing `rule_class`,
/// and the typed label-edges. Serializable + `Send` (no Starlark heap escapes). Design §11.1.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedTarget {
    /// Identity: the bzlmod-canonical label (§11.3).
    pub label: String,
    pub repo: String,
    pub package: String,
    /// The registered rule macro the target was declared with ("rust_library", "alias", …) — the
    /// QUERY-facing kind, NOT the coarse `TargetKind`.
    pub rule_class: String,
    /// The build's coarse kind (action minting only); derivable from the label at load.
    pub kind: TargetKind,
    pub attrs: std::collections::BTreeMap<String, RawAttr>,
    /// Resolved typed edges (filled by `finalize_edges` post-load; empty at capture).
    pub edges: Vec<Edge>,
    /// Captured label refs (canonicalized at capture), pending edge-kind resolution. The
    /// intermediate between P0.2 capture and P0.4 resolution (cleared into `edges` at finalize).
    pub raw_refs: Vec<RawLabelRef>,
}

/// A node in the query graph — Bazel `deps()` emits file labels too, and `kind()` classifies them
/// (design §11.1). `attr()`/`labels()` apply only to `Target`.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryNode {
    Target(LoadedTarget),
    SourceFile { label: String },
    /// An output label → the rule that generates it.
    GeneratedFile { label: String, by: String },
    /// A synthesized implicit/toolchain node (§13).
    Implicit { label: String, rule_class: String },
}

impl QueryNode {
    /// The canonical label of this node.
    pub fn label(&self) -> &str {
        match self {
            QueryNode::Target(t) => &t.label,
            QueryNode::SourceFile { label }
            | QueryNode::GeneratedFile { label, .. }
            | QueryNode::Implicit { label, .. } => label,
        }
    }

    /// The `kind()` / `--output=label_kind` string (design §12), an OPEN set: a rule node prints
    /// `"<rule_class> rule"`, a source file `"source file"`, a generated file `"generated file"`.
    pub fn kind_string(&self) -> String {
        match self {
            QueryNode::Target(t) => format!("{} rule", t.rule_class),
            QueryNode::SourceFile { .. } => "source file".to_string(),
            QueryNode::GeneratedFile { .. } => "generated file".to_string(),
            QueryNode::Implicit { rule_class, .. } => format!("{rule_class} rule"),
        }
    }
}

// ── P0.4: edge-kind resolution (post-declaration) — design §11.2, R4 precedence ─────────────────

/// The classification facts for one canonical label, gathered from the package's declarations.
/// (P0.5 fills these from the session `aliases`/`output_index`/`config_specs` + the declared-label
/// set + a source-file check; here they keep the precedence pure + testable.)
#[derive(Debug, Clone, Copy, Default)]
pub struct EdgeFacts {
    pub is_alias: bool,
    pub is_generated: bool,
    pub is_config_setting: bool,
    pub is_declared_rule: bool,
    pub is_source_file: bool,
}

/// The R4 PRECEDENCE — an alias is *also* a declared rule, so order is load-bearing:
/// alias → generated → (config_setting only via a select condition) → declared rule → source
/// file → unresolved (`None`, the caller errors). Pure.
pub fn classify_kind(in_select_condition: bool, f: &EdgeFacts) -> Option<EdgeKind> {
    if f.is_alias {
        Some(EdgeKind::Alias)
    } else if f.is_generated {
        Some(EdgeKind::GeneratedFile)
    } else if in_select_condition && f.is_config_setting {
        Some(EdgeKind::ConfigSetting)
    } else if f.is_declared_rule {
        Some(EdgeKind::Rule)
    } else if f.is_source_file {
        Some(EdgeKind::SourceFile)
    } else {
        None
    }
}

/// Resolve raw label refs (P0.2) to typed `Edge`s. `canon` canonicalizes a label relative to the
/// declaring repo/package (§11.3); `gather` returns the [`EdgeFacts`] for a canonical label. An
/// unresolved ref is a LOUD error (precedence step 6). Parameterized by closures so the session
/// backing is supplied at P0.5 and the logic stays unit-testable.
pub fn resolve_edges(
    refs: &[RawLabelRef],
    canon: impl Fn(&str) -> String,
    gather: impl Fn(&str) -> EdgeFacts,
) -> Result<Vec<Edge>, String> {
    let mut edges = Vec::with_capacity(refs.len());
    for r in refs {
        let c = canon(&r.label);
        match classify_kind(r.in_select_condition, &gather(&c)) {
            Some(kind) => edges.push(Edge { to: c, kind, attr: r.attr.clone() }),
            None => return Err(format!("unresolved label `{}` in attr `{}`", r.label, r.attr)),
        }
    }
    Ok(edges)
}

// ── P0.5: capture-at-load + finalize (the invasive wiring) — design §5.6, §11 ───────────────────

/// The coarse `TargetKind` (action minting) from the rule class — derivable at load.
fn kind_for(rule_class: &str) -> TargetKind {
    if rule_class.contains("test") {
        TargetKind::Test
    } else if rule_class.contains("binary") {
        TargetKind::Binary
    } else {
        TargetKind::Library
    }
}

/// Capture a declared target into the loading-phase graph (`sess.loaded_targets`): snapshot every
/// attr Value into `RawAttr` (P0.2), extract + canonicalize the label refs of the schema's
/// label-valued attrs (P0.2/R1), and store a `LoadedTarget` with edges deferred to
/// [`finalize_edges`]. Additive — the build path never reads `loaded_targets`. Called at the
/// rule's record point, where the raw attr Values (incl. unresolved `select()`) are still intact.
pub fn capture_loaded<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
    rule_class: &str,
    attrs: &SmallMap<String, Value<'v>>,
) {
    let heap = eval.heap();
    let sess = session(eval);
    let mut attr_map = std::collections::BTreeMap::new();
    for (name, v) in attrs.iter() {
        attr_map.insert(name.clone(), value_to_raw(heap, *v));
    }
    let mut refs = Vec::new();
    for attr_name in label_attrs(rule_class) {
        if let Some(raw) = attr_map.get(*attr_name) {
            extract_label_refs(attr_name, raw, &mut refs);
        }
    }
    for r in &mut refs {
        r.label = canon_label(sess, &r.label); // canonicalize while the package context is set
    }
    let node = LoadedTarget {
        label: label.to_string(),
        repo: String::new(), // workspace targets; repo identity (§11.3) lands with @crates
        package: pkg_of(label).unwrap_or_default(),
        rule_class: rule_class.to_string(),
        kind: kind_for(rule_class),
        attrs: attr_map,
        edges: Vec::new(),
        raw_refs: refs,
    };
    sess.loaded_targets.borrow_mut().insert(label.to_string(), node);
}

/// Convenience over [`capture_loaded`]: build the attr map from a rule's named attrs (skipping
/// `None`) plus its kwargs, then capture. Shared by the native rule families (rust/cc/dialect).
pub fn capture_rule<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
    rule_class: &str,
    named: &[(&str, Option<Value<'v>>)],
    kw: &SmallMap<String, Value<'v>>,
) {
    let mut attrs = SmallMap::new();
    for (name, v) in named {
        if let Some(v) = v {
            attrs.insert((*name).to_string(), *v);
        }
    }
    for (k, v) in kw.iter() {
        attrs.insert(k.clone(), *v);
    }
    capture_loaded(eval, label, rule_class, &attrs);
}

/// P5.1 (§11.2): capture a `cargo_build_script`'s loading-phase query nodes. The rules_rust macro
/// expands at load into `:<name>` (the runner, `cargo_build_script`), `:<name>_` (the build-script
/// `rust_binary`), and `:<name>-` (`cargo_build_script_runfiles`); Bazel `deps()` traverses into all
/// three. So declare the two macro children as their own nodes and give the runner synthetic Rule
/// edges to them — `deps(<crate>)` → `:build_script_build` (alias) → runner → children. `labels("deps")`
/// is unaffected: the children are reached by TRAVERSAL, never listed in the consuming crate's `deps`
/// attr (which carries only the `:build_script_build` alias, §12).
pub fn capture_cargo_build_script<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    label: &str,
    named: &[(&str, Option<Value<'v>>)],
    kw: &SmallMap<String, Value<'v>>,
) {
    let bin = format!("{label}_"); // `:<name>_` — the host build-script bin (rust_binary)
    let runfiles = format!("{label}-"); // `:<name>-` — its runfiles
    // The children are leaf query nodes (their own attrs aren't q4-golden surface); declare them
    // FIRST so the runner's synthetic refs resolve to `Rule` edges at `finalize_edges`.
    let empty = SmallMap::new();
    capture_loaded(eval, &bin, "rust_binary", &empty);
    capture_loaded(eval, &runfiles, "cargo_build_script_runfiles", &empty);
    // The runner, with its real label-attrs (build-deps/srcs/link_deps/…) per `label_attrs`.
    capture_rule(eval, label, "cargo_build_script", named, kw);
    // …plus the synthetic macro-child edges (not attr-derived).
    let sess = session(eval);
    if let Some(t) = sess.loaded_targets.borrow_mut().get_mut(label) {
        for child in [bin, runfiles] {
            t.raw_refs.push(RawLabelRef { label: child, attr: String::new(), in_select_condition: false });
        }
    }
}

/// Resolve every captured target's `raw_refs` into typed `edges` (the P0.4 pass over the whole
/// declared set). **Lenient in P0.5:** an unresolved ref is skipped, not a loud error, because not
/// all rule families capture yet — the strict error turns on with full rule coverage. Idempotent.
pub fn finalize_edges(sess: &Session, root: &Path) {
    let labels: Vec<String> = sess.loaded_targets.borrow().keys().cloned().collect();
    for label in labels {
        let raw_refs = match sess.loaded_targets.borrow().get(&label) {
            Some(t) => t.raw_refs.clone(),
            None => continue,
        };
        let mut edges = Vec::with_capacity(raw_refs.len());
        for r in &raw_refs {
            let facts = gather_facts(sess, &r.label, root);
            if let Some(kind) = classify_kind(r.in_select_condition, &facts) {
                edges.push(Edge { to: r.label.clone(), kind, attr: r.attr.clone() });
            }
        }
        if let Some(t) = sess.loaded_targets.borrow_mut().get_mut(&label) {
            t.edges = edges;
        }
    }
}

/// Classification facts for a canonical label, read from the session indexes + the captured set +
/// (only if nothing else matched) the filesystem.
fn gather_facts(sess: &Session, canon: &str, root: &Path) -> EdgeFacts {
    let is_alias = sess.aliases.borrow().contains_key(canon);
    let is_generated = sess.output_index.borrow().contains_key(canon);
    let is_config_setting = sess.config_specs.borrow().contains_key(canon);
    let is_declared_rule = sess.loaded_targets.borrow().contains_key(canon);
    // Only stat the filesystem when source-file-ness is the deciding fact (avoids O(refs) stats).
    let is_source_file =
        !(is_alias || is_generated || is_declared_rule) && source_exists(root, canon);
    EdgeFacts { is_alias, is_generated, is_config_setting, is_declared_rule, is_source_file }
}

/// Does `//pkg:name` name a source file on disk under `root`?
fn source_exists(root: &Path, canon: &str) -> bool {
    canon
        .trim_start_matches("//")
        .split_once(':')
        .is_some_and(|(pkg, name)| root.join(pkg).join(name).exists())
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

    // ── P0.3: node kind strings ──────────────────────────────────────────────────────────────

    #[test]
    fn query_node_kind_strings_match_bazel_shapes() {
        let lt = LoadedTarget {
            label: "@crates//:blake3".into(),
            repo: "crates".into(),
            package: "".into(),
            rule_class: "rust_library".into(),
            kind: TargetKind::Library,
            attrs: std::collections::BTreeMap::new(),
            edges: vec![],
            raw_refs: vec![],
        };
        let t = QueryNode::Target(lt);
        assert_eq!(t.kind_string(), "rust_library rule");
        assert_eq!(t.label(), "@crates//:blake3");
        assert_eq!(QueryNode::SourceFile { label: "//p:a.rs".into() }.kind_string(), "source file");
        assert_eq!(
            QueryNode::GeneratedFile { label: "//p:gen.rs".into(), by: "//p:g".into() }.kind_string(),
            "generated file"
        );
        let alias = QueryNode::Target(LoadedTarget {
            label: "@crates//:x".into(),
            repo: "crates".into(),
            package: "".into(),
            rule_class: "alias".into(),
            kind: TargetKind::Library,
            attrs: std::collections::BTreeMap::new(),
            edges: vec![],
            raw_refs: vec![],
        });
        assert_eq!(alias.kind_string(), "alias rule"); // open set — falls out of rule_class
    }

    // ── P0.4: edge-kind resolution precedence (pure) ─────────────────────────────────────────

    #[test]
    fn classify_precedence_puts_alias_before_rule() {
        // R4: an alias IS also a declared rule — alias must win, else aliases misclassify as Rule.
        let alias_and_rule = EdgeFacts { is_alias: true, is_declared_rule: true, ..Default::default() };
        assert_eq!(classify_kind(false, &alias_and_rule), Some(EdgeKind::Alias));
    }

    #[test]
    fn classify_precedence_full_order() {
        let generated = EdgeFacts { is_generated: true, is_declared_rule: true, ..Default::default() };
        assert_eq!(classify_kind(false, &generated), Some(EdgeKind::GeneratedFile));
        // a config_setting is ConfigSetting ONLY when reached via a select condition…
        let cfg = EdgeFacts { is_config_setting: true, is_declared_rule: true, ..Default::default() };
        assert_eq!(classify_kind(true, &cfg), Some(EdgeKind::ConfigSetting));
        // …otherwise it's just a declared Rule.
        assert_eq!(classify_kind(false, &cfg), Some(EdgeKind::Rule));
        let rule = EdgeFacts { is_declared_rule: true, ..Default::default() };
        assert_eq!(classify_kind(false, &rule), Some(EdgeKind::Rule));
        let src = EdgeFacts { is_source_file: true, ..Default::default() };
        assert_eq!(classify_kind(false, &src), Some(EdgeKind::SourceFile));
        // nothing matches → unresolved.
        assert_eq!(classify_kind(false, &EdgeFacts::default()), None);
    }

    #[test]
    fn resolve_edges_maps_refs_and_errors_on_unresolved() {
        let refs = vec![
            RawLabelRef { label: ":base".into(), attr: "deps".into(), in_select_condition: false },
            RawLabelRef { label: "@p//:cfg".into(), attr: "deps".into(), in_select_condition: true },
        ];
        let canon = |l: &str| format!("//pkg{}", l.strip_prefix(':').map(|n| format!(":{n}")).unwrap_or_else(|| l.to_string()));
        let gather = |c: &str| {
            if c.contains("cfg") {
                EdgeFacts { is_config_setting: true, is_declared_rule: true, ..Default::default() }
            } else {
                EdgeFacts { is_declared_rule: true, ..Default::default() }
            }
        };
        let edges = resolve_edges(&refs, canon, gather).unwrap();
        assert_eq!(edges[0].kind, EdgeKind::Rule);
        assert_eq!(edges[1].kind, EdgeKind::ConfigSetting); // in_select_condition + config_setting

        // an unresolved ref is a loud error.
        let bad = vec![RawLabelRef { label: ":ghost".into(), attr: "deps".into(), in_select_condition: false }];
        let err = resolve_edges(&bad, |l| l.to_string(), |_| EdgeFacts::default());
        assert!(err.is_err());
    }

    // ── P0.5: finalize over a session-backed graph ───────────────────────────────────────────

    #[test]
    fn finalize_resolves_a_deps_ref_to_a_rule_edge() {
        use std::collections::BTreeMap;
        let sess = crate::state::Session::new(None, crate::state::GlobalFlags::default());
        let mk = |label: &str, refs: Vec<RawLabelRef>| LoadedTarget {
            label: label.into(),
            repo: String::new(),
            package: "app".into(),
            rule_class: "rust_library".into(),
            kind: TargetKind::Library,
            attrs: BTreeMap::new(),
            edges: vec![],
            raw_refs: refs,
        };
        sess.loaded_targets.borrow_mut().insert("//app:base".into(), mk("//app:base", vec![]));
        sess.loaded_targets.borrow_mut().insert(
            "//app:util".into(),
            mk("//app:util", vec![RawLabelRef {
                label: "//app:base".into(),
                attr: "deps".into(),
                in_select_condition: false,
            }]),
        );
        // //app:base is a captured (declared) rule, so util's deps ref resolves to a Rule edge.
        finalize_edges(&sess, std::path::Path::new("/nonexistent"));
        let loaded = sess.loaded_targets.borrow();
        assert_eq!(
            loaded.get("//app:util").unwrap().edges,
            vec![Edge { to: "//app:base".into(), kind: EdgeKind::Rule, attr: "deps".into() }]
        );
    }
}
