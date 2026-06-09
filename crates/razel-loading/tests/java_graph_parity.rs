//! F16 — razel's java graph (via `analyze_workspace` over the bundled `java:defs.bzl`, served for
//! `@rules_java`) vs the captured golden, as a **structural** diff. This wires the java golden into a
//! test (it had none) and pins the action SHAPE so the spike can't silently drift.
//!
//! NOT byte-parity (that's Phase D). The known structural divergences, enumerated so a reader isn't
//! misled into thinking only flag-text differs:
//!   - golden Turbine = native `turbine_direct_graal`; razel = `java -jar turbine_deploy.jar`.
//!   - golden Javac emits 4 outputs (`libX.jar`, `libX-native-header.jar`, `libX.jdeps`,
//!     `libX.jar_manifest_proto`); razel emits 1 (`libX.jar`).
//!   - golden feeds base's **tjar** to Turbine's classpath but base's **hjar** to Javac; razel feeds
//!     the hjar to both.
//!   - JavaSourceJar = native `singlejar` vs razel `java -jar SourceJar_deploy.jar`.
//! What IS pinned: the mnemonic multiset (the 3-action-kind shape × 2 targets) and that razel's
//! primary jars appear among the golden's outputs.

use razel_loading::analyze_workspace;
use std::collections::BTreeMap;
use std::path::Path;

#[test]
fn java_graph_matches_the_golden_structurally() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../parity");
    let targets = analyze_workspace(&root, "//corpus/java/transitive:util").unwrap();

    let multiset = |it: &mut dyn Iterator<Item = &String>| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for x in it {
            *m.entry(x.clone()).or_default() += 1;
        }
        m
    };

    // Action SHAPE: the mnemonic multiset must match (Turbine ×2, Javac ×2, JavaSourceJar ×2).
    let razel_mnem =
        multiset(&mut targets.iter().flat_map(|t| t.actions.iter()).map(|a| &a.mnemonic));
    let golden = razel_parity::parse_golden(include_str!(
        "../../../parity/corpus/java/transitive/golden.txt"
    ));
    let golden_mnem = multiset(&mut golden.iter().map(|a| &a.mnemonic));
    assert_eq!(razel_mnem, golden_mnem, "java action shape (mnemonic multiset) must match the golden");

    // Coarse output check: razel's primary jars are among the golden's outputs (byte-parity = Phase D).
    let golden_out: Vec<&String> = golden.iter().flat_map(|a| a.outputs.iter()).collect();
    let razel_out: Vec<&String> =
        targets.iter().flat_map(|t| t.actions.iter()).flat_map(|a| a.outputs.iter()).collect();
    for jar in ["libutil.jar", "libutil-hjar.jar"] {
        assert!(razel_out.iter().any(|o| o.ends_with(jar)), "razel emits {jar}: {razel_out:?}");
        assert!(golden_out.iter().any(|o| o.ends_with(jar)), "golden has {jar}");
    }
}
