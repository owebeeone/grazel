//! The crate consuming the build script's `rustc-cfg` (the §4.3 edge). `aquery` is analysis-only,
//! so this need not compile for the golden — but it stays valid for the execution-parity follow-up.

#[cfg(buildscript_ran)]
pub fn marker() -> &'static str {
    "compiled with the build-script cfg"
}
