//! Analysis (Phase 3). Starts with `depset` — the nested set with order semantics that
//! rule providers carry. Flatten orders ported from Bazel
//! `collect/nestedset/NestedSetTest.java` (Class E).
//!
//! Implemented: **postorder** (Bazel default / `STABLE_ORDER`: transitives before directs)
//! and **preorder** (directs before transitives). `LINK_ORDER`/`TOPOLOGICAL` have intricate
//! linker dedup semantics and are deferred (tracked) rather than approximated.

pub mod analysis;
pub use analysis::{Analysis, DefaultInfo, analyze, wire_to_ir};

use std::collections::HashSet;
use std::hash::Hash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// Bazel default (`STABLE_ORDER`): transitives flattened first, then directs.
    Postorder,
    /// Directs first, then transitives.
    Preorder,
}

/// A nested set: direct elements plus transitive child sets, with a flatten order.
/// (Transitives owned here for clarity; structural sharing via `Rc` is a later perf step.)
#[derive(Clone, Debug)]
pub struct Depset<T> {
    order: Order,
    direct: Vec<T>,
    transitive: Vec<Depset<T>>,
}

impl<T: Clone + Eq + Hash> Depset<T> {
    pub fn new(order: Order, direct: Vec<T>, transitive: Vec<Depset<T>>) -> Self {
        Depset {
            order,
            direct,
            transitive,
        }
    }
    pub fn leaf(order: Order, direct: Vec<T>) -> Self {
        Depset::new(order, direct, Vec::new())
    }
    pub fn order(&self) -> Order {
        self.order
    }

    /// Flatten to a deduplicated list (first occurrence wins) in this set's order.
    pub fn to_list(&self) -> Vec<T> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.collect(&mut out, &mut seen);
        out
    }

    fn collect(&self, out: &mut Vec<T>, seen: &mut HashSet<T>) {
        let emit_directs = |out: &mut Vec<T>, seen: &mut HashSet<T>| {
            for d in &self.direct {
                if seen.insert(d.clone()) {
                    out.push(d.clone());
                }
            }
        };
        match self.order {
            Order::Preorder => {
                emit_directs(out, seen);
                for t in &self.transitive {
                    t.collect(out, seen);
                }
            }
            Order::Postorder => {
                for t in &self.transitive {
                    t.collect(out, seen);
                }
                emit_directs(out, seen);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn postorder_transitives_before_directs() {
        // Ported: nestedSetBuilder("a").addTransitive({b1,b2}) -> [b1, b2, a].
        let b = Depset::leaf(Order::Postorder, s(&["b1", "b2"]));
        let set = Depset::new(Order::Postorder, s(&["a"]), vec![b]);
        assert_eq!(set.to_list(), s(&["b1", "b2", "a"]));
    }

    #[test]
    fn postorder_two_transitives_then_direct() {
        // Ported: directs {a}, transitives {b1,b2},{c1,c2} -> [b1, b2, c1, c2, a].
        let b = Depset::leaf(Order::Postorder, s(&["b1", "b2"]));
        let c = Depset::leaf(Order::Postorder, s(&["c1", "c2"]));
        let set = Depset::new(Order::Postorder, s(&["a"]), vec![b, c]);
        assert_eq!(set.to_list(), s(&["b1", "b2", "c1", "c2", "a"]));
    }

    #[test]
    fn postorder_transitive_only() {
        // Ported: addTransitive({b1,b2}) with no directs -> [b1, b2].
        let b = Depset::leaf(Order::Postorder, s(&["b1", "b2"]));
        let set = Depset::new(Order::Postorder, vec![], vec![b]);
        assert_eq!(set.to_list(), s(&["b1", "b2"]));
    }

    #[test]
    fn preorder_directs_before_transitives() {
        let b = Depset::leaf(Order::Preorder, s(&["b1", "b2"]));
        let c = Depset::leaf(Order::Preorder, s(&["c1", "c2"]));
        let set = Depset::new(Order::Preorder, s(&["a"]), vec![b, c]);
        assert_eq!(set.to_list(), s(&["a", "b1", "b2", "c1", "c2"]));
    }

    #[test]
    fn dedups_first_occurrence() {
        // Shared element across transitives appears once, at its first traversal position.
        let b = Depset::leaf(Order::Postorder, s(&["x", "b"]));
        let c = Depset::leaf(Order::Postorder, s(&["x", "c"]));
        let set = Depset::new(Order::Postorder, s(&["a"]), vec![b, c]);
        assert_eq!(set.to_list(), s(&["x", "b", "c", "a"]));
    }
}
