//! razel-wire — the razel daemon's wire contract.
//!
//! Native Rust types + a deterministic-CBOR codec, **generated** from the
//! governed taut IR at `wire/razel.taut.py` (the single source of truth). The
//! daemon (server) and any client — razel-cli, or a cross-language GripLab
//! client driven by the *same* IR — share these types; that shared dependency is
//! why this is its own crate rather than a module inside `razel-daemon`.
//!
//! Generation is **out of the Cargo build graph** on purpose (the generator is
//! Python/`tautc`, and `cargo build`/`install` must stay pure-Rust and offline):
//!   - `cargo xtask codegen`          regenerates `src/generated.rs`
//!   - `cargo xtask codegen --check`  fails if the committed output is stale
//!
//! The contract is `(name, in, out, shape)` per method; see `wire/razel.taut.py`
//! for the surface (build / sync_file / version / affected / build.subscribe).

pub mod cbor;
// Generated code (`tautc` output, do not edit) — exempt from style lints; its
// shape is the generator's responsibility, not razel's.
#[allow(clippy::redundant_closure)]
mod generated;

pub use cbor::{Cbor, decode, encode};
pub use generated::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end through the generated codec: a populated value survives
    /// `to_cbor -> encode -> decode -> from_cbor` unchanged. Pure Rust — no
    /// Python, no corpus — so it runs offline as part of `cargo test`.
    /// (Cross-language byte-parity against taut's golden corpus is the separate
    /// conformance harness; this proves the Rust codec is internally sound.)
    #[test]
    fn build_result_roundtrips_through_the_wire() {
        let original = BuildResult {
            target: "//pkg/sub:lib".into(),
            status: BuildStatus::Built,
            recomputes: 7,
            outputs: vec![
                OutputArtifact {
                    path: "out/lib.a".into(),
                    digest: vec![0xde, 0xad, 0xbe, 0xef],
                },
                OutputArtifact {
                    path: "out/lib.o".into(),
                    digest: vec![],
                },
            ],
            message: None,
        };
        let restored = BuildResult::from_cbor(&decode(&encode(&original.to_cbor())));
        assert_eq!(restored, original);
    }

    #[test]
    fn optional_field_present_and_absent() {
        let failed = BuildResult {
            target: "//x:y".into(),
            status: BuildStatus::Failed,
            recomputes: 0,
            outputs: vec![],
            message: Some("link error: undefined symbol".into()),
        };
        let restored = BuildResult::from_cbor(&decode(&encode(&failed.to_cbor())));
        assert_eq!(
            restored.message.as_deref(),
            Some("link error: undefined symbol")
        );
        assert_eq!(restored, failed);
    }

    #[test]
    fn impact_set_with_nested_target_refs() {
        let impact = ImpactSet {
            sources: vec!["src/a.cc".into(), "src/b.h".into()],
            targets: vec![TargetRef {
                label: "//app:bin".into(),
                kind: TargetKind::Binary,
            }],
            tests: vec![
                TargetRef {
                    label: "//app:unit_test".into(),
                    kind: TargetKind::Test,
                },
                TargetRef {
                    label: "//lib:lib_test".into(),
                    kind: TargetKind::Test,
                },
            ],
        };
        let restored = ImpactSet::from_cbor(&decode(&encode(&impact.to_cbor())));
        assert_eq!(restored, impact);
    }

    #[test]
    fn enum_wire_values_match_the_ir() {
        // Locked by the IR: library=0, binary=1, test=2; cached=0, built=1, failed=2.
        assert_eq!(TargetKind::Library.wire(), 0);
        assert_eq!(TargetKind::Binary.wire(), 1);
        assert_eq!(TargetKind::Test.wire(), 2);
        assert_eq!(BuildStatus::Cached.wire(), 0);
        assert_eq!(BuildStatus::Built.wire(), 1);
        assert_eq!(BuildStatus::Failed.wire(), 2);
        assert_eq!(TargetKind::from_wire(2), TargetKind::Test);
    }
}
