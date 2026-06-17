//! `state::paths` — split from `state.rs` (facade in `mod.rs`).

use super::*;

/// Canonicalize a target name/label against the current package. Single-package
/// mode keeps bare names; workspace mode produces `//pkg:name`.
pub(crate) fn canon_label(sess: &Session, s: &str) -> String {
    canonicalize_crate_repo(sess, canon_label_inner(sess, s))
}


/// P3.1e (§11.3): rewrite a `@crates`/`@crates__*` label to its canonical `@@rules_rust++crate+…`
/// identity, accepting either form (apparent OR already-canonical) — the rule that makes
/// `@crates//:blake3` and `@@rules_rust++crate+crates//:blake3` the SAME target. A no-op unless the
/// build seeded [`GlobalFlags::crate_lock`], and only for repos the lock defines (other repos —
/// `@rules_rust`, `@platforms` — keep their single-`@` apparent form, untouched).
pub(crate) fn canonicalize_crate_repo(sess: &Session, label: String) -> String {
    match &sess.global.crate_lock {
        Some(lock) => canonicalize_crate_repo_lock(lock, &label),
        None => label,
    }
}

/// Lock-only core of [`canonicalize_crate_repo`] — no `Session` (the query driver canonicalizes
/// patterns up-front from a seeded lock, §11.3 / q4). `@crates`/`@crates__*` → `@@rules_rust++crate+…`;
/// a `//` label, the main repo, or a non-crate `@repo` (`@rules_rust`/`@platforms`) returns as-is.
pub(crate) fn canonicalize_crate_repo_lock(lock: &crate::lock::CrateLock, label: &str) -> String {
    let trimmed = label.trim_start_matches('@');
    let Some((repo, rest)) = trimmed.split_once("//") else { return label.to_string() };
    if repo.is_empty() {
        return label.to_string(); // main repo (`@@//`, `//`) — never a crate repo
    }
    match lock.canonical_repo(repo) {
        Some(canon) => format!("@@{canon}//{rest}"),
        None => label.to_string(),
    }
}


pub(crate) fn canon_label_inner(sess: &Session, s: &str) -> String {
    // Package shorthand: `//a/b` ≡ `//a/b:b` (same for `@repo//a/b`).
    let expand = |label: String| -> String {
        if let Some(rest) = label.rsplit("//").next()
            && !rest.contains(':')
            && !rest.is_empty()
        {
            let last = rest.rsplit('/').next().unwrap_or(rest);
            return format!("{label}:{last}");
        }
        label
    };
    // An `@repo//…` label is already canonical (external labels don't take the current package).
    if s.starts_with('@') {
        // Bare `@repo` shorthand ≡ `@repo//:repo`.
        if !s.contains("//") {
            let name = s.trim_start_matches('@');
            return format!("{s}//:{name}");
        }
        return expand(s.to_string());
    }
    match sess.current_pkg() {
        None => s.strip_prefix(':').unwrap_or(s).to_string(),
        // Inside an EXTERNAL package (`current_pkg == "@repo//pkg"`): labels resolve within that
        // repo — `//x:y` → `@repo//x:y`; `:n`/`n` → `@repo//pkg:n` (Bazel label semantics).
        Some(pkg) if pkg.starts_with('@') => {
            let repo = pkg.split("//").next().unwrap_or_default();
            if let Some(rest) = s.strip_prefix("//") {
                expand(format!("{repo}//{rest}"))
            } else if let Some(name) = s.strip_prefix(':') {
                format!("{pkg}:{name}")
            } else {
                format!("{pkg}:{s}")
            }
        }
        Some(pkg) => {
            if let Some(rest) = s.strip_prefix("//") {
                expand(format!("//{rest}"))
            } else if let Some(name) = s.strip_prefix(':') {
                format!("//{pkg}:{name}")
            } else {
                format!("//{pkg}:{s}")
            }
        }
    }
}


/// Package-qualify a source/output path (`x.cc` → `pkg/x.cc` in workspace mode).
pub(crate) fn qualify(sess: &Session, path: &str) -> String {
    match sess.current_pkg() {
        // External package: FILE paths take Bazel's exec-root form, `external/<repo>/<pkg>/…`
        // (`@repo//pkg` is a label, not a path). Trim ALL leading `@` so the canonical `@@repo//`
        // form (§11.3) yields `external/<repo>/…` (not `external/@<repo>/…`) — a lone
        // `strip_prefix('@')` left the stray `@` that diverged blake3's `--out-dir`/`-Ldependency`
        // from Bazel's; same family as the glob/deps/decls fixes.
        Some(pkg) if pkg.starts_with('@') => {
            let rest = pkg.trim_start_matches('@');
            match rest.split_once("//") {
                Some((repo, sub)) if sub.is_empty() => format!("external/{repo}/{path}"),
                Some((repo, sub)) => format!("external/{repo}/{sub}/{path}"),
                None => format!("external/{rest}/{path}"),
            }
        }
        Some(pkg) if pkg.is_empty() => path.to_string(),
        Some(pkg) => format!("{pkg}/{path}"),
        None => path.to_string(),
    }
}


/// The path of a GENERATED output (rlib, bin, build-script files) as it appears in the action
/// graph. Default: workspace-relative (`qualify`). Under `--bazel_build_compat` (the parity posture,
/// RazelRustParityPlan A2): rooted in Bazel's output tree `bazel-out/<config>/bin/<pkg>/<name>` so
/// razel's declared outputs + the argv paths that reference them (`--out-dir`, `--extern` rlibs, the
/// build-script flags-file/`OUT_DIR`) match `bazel aquery`'s. Sources stay `qualify` (Bazel keeps
/// them workspace-relative too). `normalize` tokenizes the `<config>` segment, so razel's single
/// config matches Bazel's per-action exec/target configs.
pub(crate) fn out_path(sess: &Session, name: &str) -> String {
    let p = qualify(sess, name);
    if sess.global.bazel_build_compat {
        format!("bazel-out/{}/bin/{p}", bazel_config(sess))
    } else {
        p
    }
}


/// The current package's GENERATED-output directory — `<pkg>` (default) or
/// `bazel-out/<config>/bin/<pkg>` (under `--bazel_build_compat`). For rustc's `--out-dir=` (the
/// faithful output model, RazelRustParityPlan A5).
pub(crate) fn out_dir(sess: &Session) -> String {
    out_path(sess, "").trim_end_matches('/').to_string()
}


/// Bazel's configuration mnemonic for this build, e.g. `darwin_arm64-fastbuild` — the
/// `<cpu>-<compilation_mode>` segment of `<out>/<config>/bin`, computable from the
/// compilation mode alone (no `Session`) so the CLI can mint matching convenience symlinks.
pub fn config_segment(compilation_mode: &str) -> String {
    let cpu = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin_arm64",
        ("macos", "x86_64") => "darwin_x86_64",
        ("linux", "x86_64") => "k8",
        (_, arch) => arch,
    };
    let mode = match compilation_mode {
        "" => "fastbuild",
        m => m,
    };
    format!("{cpu}-{mode}")
}


/// `<cpu>-<compilation_mode>` segment of `<out>/<config>/bin`.
pub(crate) fn bazel_config(sess: &Session) -> String {
    config_segment(&sess.global.compilation_mode)
}


/// Bazel-style convenience symlinks for a build: `(link-name, relative-target)` pairs to
/// create in the workspace root. Default → `razel-bin`/`razel-testlogs` →
/// `razel-out/<config>/{bin,testlogs}`; under `--bazel_build_compat` → `bazel-bin`/
/// `bazel-testlogs` → `bazel-out/<config>/…` (mirroring Bazel exactly).
pub fn convenience_symlinks(flags: &GlobalFlags) -> Vec<(String, String)> {
    let r = if flags.bazel_build_compat { "bazel" } else { "razel" };
    let cfg = config_segment(&flags.compilation_mode);
    vec![
        (format!("{r}-bin"), format!("{r}-out/{cfg}/bin")),
        (format!("{r}-testlogs"), format!("{r}-out/{cfg}/testlogs")),
    ]
}


/// The output bin root for this build. `--bazel_build_compat` → the real Bazel
/// `bazel-out/<config>/bin` (so the parity goldens, captured from `bazel aquery`, match
/// byte-for-byte). Otherwise, when the driver opts into the output tree
/// (`bin_tree_layout`, which the CLI sets) → razel's own `razel-out/<config>/bin` (same
/// structure, razel name; generated files never pollute the source tree, `razel-bin`
/// mirrors `bazel-bin`). With neither — the bare library default used by analysis tests —
/// it's the legacy `bazel-out/bin` fiction that `$(BINDIR)` has always substituted, and
/// outputs stay package-relative (in-tree).
pub(crate) fn bin_dir(sess: &Session) -> String {
    if sess.global.bazel_build_compat {
        format!("bazel-out/{}/bin", bazel_config(sess))
    } else if sess.global.bin_tree_layout {
        format!("razel-out/{}/bin", bazel_config(sess))
    } else {
        "bazel-out/bin".to_string()
    }
}


/// Like [`qualify`], but for OUTPUT (generated) files. When the build opts into the output
/// tree (`--bazel_build_compat` → `bazel-out/…`, or the CLI's `bin_tree_layout` →
/// `razel-out/…`), outputs live in `<root>/<config>/bin/<pkg>/…`, so the path carries the
/// prefix into command lines, declared outputs, and dependents' references BY CONSTRUCTION
/// (matching Bazel; no rewrite). Otherwise it is exactly [`qualify`] (in-tree).
pub(crate) fn qualify_output(sess: &Session, path: &str) -> String {
    bin_prefix(sess, &qualify(sess, path))
}


/// Prefix an ALREADY-package-qualified output path with the bin root ([`bin_dir`]) when the
/// build uses the output tree; otherwise a no-op. For outputs whose name derives from a
/// qualified path (e.g. a `.o` named `<qualified-src>.o`) where re-qualifying would double
/// the package.
pub(crate) fn bin_prefix(sess: &Session, qualified: &str) -> String {
    if sess.global.bazel_build_compat || sess.global.bin_tree_layout {
        format!("{}/{qualified}", bin_dir(sess))
    } else {
        qualified.to_string()
    }
}


/// The package of a canonical label `//pkg:name`.
pub(crate) fn pkg_of(label: &str) -> Option<String> {
    // External: `@repo//pkg:name` → `@repo//pkg` (an external-package key for load_package).
    if let Some(rest) = label.strip_prefix('@') {
        let (repo, pkgname) = rest.split_once("//")?;
        let (pkg, _) = pkgname.split_once(':')?;
        return Some(format!("@{repo}//{pkg}"));
    }
    label
        .strip_prefix("//")?
        .split_once(':')
        .map(|(p, _)| p.to_string())
}


