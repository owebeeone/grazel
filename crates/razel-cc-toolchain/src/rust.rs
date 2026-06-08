//! Derive a `rust_library`'s declared `Rustc` action — the rules_rust command line.
//!
//! Unlike cc (a `Constrain` feature-config), rules_rust assembles the `rustc` argv directly, so
//! this is a faithful TEMPLATE over the path model + the crate's direct deps (`--extern`). It's
//! the rust analogue of `derive::derive_cc_library_actions`, parity-checked against the captured
//! golden. (This lives in the cc-named crate for now, reusing [`DeclaredAction`]; the crate is
//! becoming the multi-language rule-rep and should be renamed `razel-toolchain`.)

use crate::DeclaredAction;

/// Derive the `Rustc` action for a `rust_library` crate, reproducing the rules_rust command line
/// (`process_wrapper` → `rustc` + flags). `cfg`/`repo`/`target_triple` are the host/toolchain
/// params (normalized placeholders in the golden); `deps` are the DIRECT crate deps (`--extern`);
/// `edition` is the rule attr. Per-crate content hashes are `<hash>` (razel doesn't reproduce
/// Bazel's metadata hash — normalized, like `<sdk>`).
pub fn derive_rust_library_action(
    cfg: &str,
    repo: &str,
    target_triple: &str,
    pkg: &str,
    name: &str,
    src: &str,
    deps: &[&str],
    edition: &str,
) -> DeclaredAction {
    let bin = format!("bazel-out/{cfg}/bin");
    let toolchain = format!("{bin}/external/{repo}/rust_toolchain");
    let out_dir = format!("{bin}/{pkg}");
    let rlib = format!("{out_dir}/lib{name}-<hash>.rlib");

    let mut argv = vec![
        format!("{bin}/external/{repo}/util/process_wrapper/process_wrapper"),
        "--subst".into(),
        "pwd=${pwd}".into(),
        "--subst".into(),
        "exec_root=${exec_root}".into(),
        "--subst".into(),
        "output_base=${output_base}".into(),
        "--".into(),
        format!("{toolchain}/bin/rustc"),
        format!("{pkg}/{src}"),
        format!("--crate-name={name}"),
        "--crate-type=rlib".into(),
        "--error-format=human".into(),
        "--codegen=metadata=-<hash>".into(),
        "--codegen=extra-filename=-<hash>".into(),
        format!("--out-dir={out_dir}"),
        "--codegen=opt-level=0".into(),
        "--codegen=debuginfo=0".into(),
        "--codegen=strip=none".into(),
        "--remap-path-prefix=${pwd}=.".into(),
        "--remap-path-prefix=${exec_root}=.".into(),
        "--remap-path-prefix=${output_base}=.".into(),
        "--emit=dep-info,link".into(),
        "--color=always".into(),
        format!("--target={target_triple}"),
        "-L".into(),
        format!("{toolchain}/lib/rustlib/{target_triple}/lib"),
        format!("--edition={edition}"),
        "-Cembed-bitcode=no".into(),
    ];
    // Direct crate deps → `--extern` (transitive rlibs are found via `-Ldependency`'s dir).
    for dep in deps {
        argv.push(format!("--extern={dep}={out_dir}/lib{dep}-<hash>.rlib"));
    }
    argv.push(format!("-Ldependency={out_dir}"));
    argv.push(format!("--sysroot={toolchain}"));

    DeclaredAction {
        mnemonic: "Rustc".into(),
        argv,
        // Source-level inputs; the toolchain rlibs + dep rlibs are Bazel sandbox detail (scoping).
        inputs: vec![format!("{pkg}/{src}")],
        outputs: vec![rlib],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the `Command Line: (exec … )` of the golden action whose header contains `header`.
    fn golden_argv(golden: &str, header: &str) -> Vec<String> {
        let block = &golden[golden.find(&format!("action '{header}")).expect("action")..];
        let after = &block[block.find("Command Line: (exec ").expect("cmdline")
            + "Command Line: (exec ".len()..];
        let end = after.find(')').unwrap_or(after.len());
        after[..end]
            .split_whitespace()
            .filter(|t| *t != "\\")
            .map(|t| t.trim_matches('\'').to_string())
            .collect()
    }

    #[test]
    fn parity_rust_derive_reproduces_the_golden_rustc_command_line() {
        // razel's derive reproduces Bazel's ACTUAL captured Rustc command line for util→base,
        // byte-for-byte (against the normalized golden).
        let golden = include_str!("../../../parity/corpus/rust/transitive/golden.txt");
        let action = derive_rust_library_action(
            "<cfg>",
            "<repo>",
            "aarch64-apple-darwin",
            "corpus/rust/transitive",
            "util",
            "util.rs",
            &["base"],
            "2021",
        );
        assert_eq!(action.argv, golden_argv(golden, "Compiling Rust rlib util"));
        assert_eq!(
            action.outputs,
            ["bazel-out/<cfg>/bin/corpus/rust/transitive/libutil-<hash>.rlib"]
        );
    }
}
