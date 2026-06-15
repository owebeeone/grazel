// The build script: emit a single `rustc-cfg` directive the crate consumes. Minimal on purpose —
// the parity target is the EDGE (the flags-file → the crate's rustc), not the directive variety.
fn main() {
    println!("cargo::rustc-cfg=buildscript_ran");
}
