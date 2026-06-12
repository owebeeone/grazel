//! T0 (PublicSurfaces §6): WIRE GOLDENS — the deterministic-CBOR encoding pinned as
//! bytes. The Rust codec must ENCODE to these exact vectors and round-trip them;
//! taut's TS backend decodes the same vectors (GR5). A schema change that moves these
//! bytes is a deliberate golden update in the SAME commit as the IR change — an
//! accidental wire break cannot land silently.

use razel_wire::{Hello, InvocationEvent, Progress, decode, encode};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn hello_encodes_to_the_golden_bytes_and_round_trips() {
    let h = Hello {
        build_version: "1.0.0".into(),
        protocol: 1,
        workspace_root: "/ws/app".into(),
    };
    let bytes = encode(&h.to_cbor());
    assert_eq!(hex(&bytes), "a30165312e302e30020103672f77732f617070");
    let back = Hello::from_cbor(&decode(&bytes));
    assert_eq!(back.build_version, "1.0.0");
    assert_eq!(back.protocol, 1);
    assert_eq!(back.workspace_root, "/ws/app");
}

#[test]
fn invocation_event_encodes_to_the_golden_bytes_and_round_trips() {
    let ev = InvocationEvent {
        invocation_id: "inv-1".into(),
        seq: 1,
        progress: Some(Progress {
            invocation_id: "inv-1".into(),
            phase: "load".into(),
            done: 300,
            total: 2000,
            detail: None,
        }),
        result: None,
    };
    let bytes = encode(&ev.to_cbor());
    assert_eq!(
        hex(&bytes),
        "a40165696e762d31020103a50165696e762d3102646c6f61640319012c041907d005f604f6"
    );
    let back = InvocationEvent::from_cbor(&decode(&bytes));
    assert_eq!(back.seq, 1);
    let p = back.progress.expect("progress arm");
    assert_eq!((p.done, p.total, p.phase.as_str()), (300, 2000, "load"));
    assert!(back.result.is_none());
}
