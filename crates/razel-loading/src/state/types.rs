//! `state::types` — split from `state.rs` (facade in `mod.rs`).

use razel_dds::{FieldId, FieldValue, ProviderTypeId, Scalar};
use std::collections::{BTreeMap, HashSet};

/// One action registered by a rule impl (`ctx.actions.run`/`write`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedAction {
    pub mnemonic: String,
    /// Full command: `[executable, args…]` — what the executor spawns.
    pub argv: Vec<String>,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// A human, per-action progress label for the build bar (bazel's `progress_message`): the rule
    /// that builds the action sets it from what IT knows — the crate + version for rust, the source
    /// file for C++, etc. Empty ⇒ the bar falls back to the primary output's basename. This is the
    /// language-agnostic mechanism — the engine never parses target labels per language.
    pub description: String,
}


/// The captured analysis of one target: a rule impl ran and produced these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalyzedTarget {
    pub name: String,
    /// Resolved dependency target names (from the `deps` attr).
    pub deps: Vec<String>,
    pub actions: Vec<AnalyzedAction>,
    /// `DefaultInfo(files=…)`.
    pub default_info: Vec<String>,
    /// The target's OWN providers (C2d) — the razel-dds value algebra IS the storage: `CcInfo.hdrs`/
    /// `.cflags` are `Set`, `JavaInfo.compile_jars`/`.runtime_jars` are `OrderedDepset`, `.neverlink`
    /// is `Scalar`. The TRANSITIVE closure a dependent sees is a `DdsRead` fold over [`crate::dds`].
    /// (Replaces the former five flat fields — one representation, no hand-reflection.)
    pub providers: BTreeMap<(ProviderTypeId, FieldId), FieldValue>,
}


pub(crate) fn scalar_str(s: &Scalar) -> Option<String> {
    if let Scalar::Str(x) = s {
        Some(x.to_string())
    } else {
        None
    }
}


impl AnalyzedTarget {
    /// A provider field's string elements (`Set` or `OrderedDepset`), empty if absent. Generic — the
    /// caller names the provider/field (language modules + tests); `state` stays language-free (C3a.5b).
    pub fn field_strs(&self, ty: &str, field: &str) -> Vec<String> {
        match self
            .providers
            .get(&(ProviderTypeId::new(ty), FieldId::new(field)))
        {
            Some(FieldValue::Set(s)) => s.iter().filter_map(scalar_str).collect(),
            Some(FieldValue::OrderedDepset(v)) => v.iter().filter_map(scalar_str).collect(),
            _ => Vec::new(),
        }
    }
    /// A `Scalar(Bool)` provider field (e.g. java `neverlink`), false if absent. Generic.
    pub fn scalar_bool(&self, ty: &str, field: &str) -> bool {
        matches!(
            self.providers
                .get(&(ProviderTypeId::new(ty), FieldId::new(field))),
            Some(FieldValue::Scalar(Scalar::Bool(true)))
        )
    }
    /// Set a provider field (the capture write — `razel_build.info` + the native rules).
    pub fn set_provider(&mut self, ty: &str, field: &str, value: FieldValue) {
        self.providers
            .insert((ProviderTypeId::new(ty), FieldId::new(field)), value);
    }
    /// Set a `Set`-valued provider field from strings — the common native-rule capture. Generic: the
    /// rule names its own provider (allowed); `state` stays language-free.
    pub(crate) fn set_set(&mut self, ty: &str, field: &str, values: Vec<String>) {
        self.set_provider(
            ty,
            field,
            FieldValue::Set(values.into_iter().map(|v| Scalar::Str(v.into())).collect()),
        );
    }
}


#[derive(Default)]
pub(crate) struct AnalysisState {
    pub(crate) targets: Vec<AnalyzedTarget>,
}


/// P4a: ONE WORKER's eval-stack state — the package/repo context, the in-flight target, the
/// mid-analysis guard set. These mirror a single thread's nested-eval call stack and must
/// never be shared: round 23 proved that Session-wide copies corrupt every concurrent eval
/// (labels canonicalize against another worker's package). Keyed by `ThreadId` on the shared
/// Session (AD2: Session-owned, not a process global; dies with the Session).
#[derive(Default)]
pub(crate) struct EvalStack {
    /// The package this worker is currently evaluating (`None` ⇒ single-package mode).
    pub(crate) current_pkg: Option<String>,
    /// The (repo, pkg) of the module this worker is loading/evaluating (lexical binding).
    pub(crate) bzl_repo: Vec<Option<(String, String)>>,
    /// The in-flight `AnalyzedTarget` being built by this worker's current rule analysis.
    pub(crate) current: Option<AnalyzedTarget>,
    /// Targets mid-analysis on THIS worker (cycle detection; cross-worker duplicate analysis
    /// is allowed — results overwrite by label, the established idempotent re-analysis).
    pub(crate) analyzing: HashSet<String>,
    /// F4 (restart): cross-thread partial-state reads this worker consumed (CycleProceed
    /// grants, failed-dep declaration waits). Cross-thread-only by construction — sequential
    /// re-entry takes the Reentry arm — so threads=1 never advances it. The tree driver
    /// retries failed entry loads whose count advanced.
    pub(crate) partial_reads: usize,
}


