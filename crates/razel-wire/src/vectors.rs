// GENERATED golden corpus (taut Python codec) — do not edit.
// Regenerate with `cargo xtask corpus`. Each entry is (name, message,
// cbor-hex): the bytes razel's Rust codec must reproduce exactly.
pub static VECTORS: &[(&str, &str, &str)] = &[
    (
        "output/lib",
        "OutputArtifact",
        "a2016d6f75742f6c69626d6174682e610244deadbeef",
    ),
    (
        "targetref/binary",
        "TargetRef",
        "a201692f2f6170703a62696e0201",
    ),
    (
        "build/success",
        "BuildResult",
        "a501692f2f706b673a6c6962020103070481a201686c6962706b672e61024301020305f6",
    ),
    (
        "build/failure",
        "BuildResult",
        "a501652f2f783a790202030004800578226c696e6b206572726f723a20756e646566696e65642073796d626f6c206061646460",
    ),
    ("sync/ack", "SyncAck", "a101182a"),
    ("version/info", "VersionInfo", "a20165302e312e300201"),
    (
        "impact/set",
        "ImpactSet",
        "a30182687372632f612e6363677372632f622e680281a201692f2f6170703a62696e02010382a2016f2f2f6170703a756e69745f746573740202a2016e2f2f6c69623a6c69625f746573740202",
    ),
    (
        "targetstatus/cached",
        "TargetStatus",
        "a401672f2f6c3a6c696202000300044200ff",
    ),
    (
        "buildstate/one",
        "BuildState",
        "a201030281a401672f2f6c3a6c6962020003010440",
    ),
];
