//! razel-build `affected` — split from `lib.rs`.

use super::*;

/// Expand a Bazel target PATTERN to concrete target labels under `root`. Supports `//...`,
/// `//...:all`, `//pkg/...`, `//pkg/...:all`, and `//pkg:all`; a concrete label (`//pkg:name`,
/// bare name — no `...`/`:all`) is returned as-is (no discovery). Discovers packages
/// (BUILD/BUILD.bazel/BUILD.razel), keeps the ones the pattern's package part selects,
/// analyzes them once, and returns the matching target labels (sorted, deduped).
pub fn expand_pattern(root: &Path, pattern: &str, flags: GlobalFlags) -> Result<Vec<String>, String> {
    if !pattern.contains("...") && !pattern.ends_with(":all") {
        return Ok(vec![pattern.to_string()]); // concrete — no discovery
    }
    let body = pattern.strip_prefix("//").unwrap_or(pattern);
    let pkgs = razel_loading::discover_packages(root, flags.strict_bazel);
    let matched: Vec<String> = if body == "..." || body == "...:all" {
        pkgs
    } else if let Some(pfx) = body
        .strip_suffix("/...:all")
        .or_else(|| body.strip_suffix("/..."))
    {
        pkgs.into_iter().filter(|p| p == pfx || p.starts_with(&format!("{pfx}/"))).collect()
    } else {
        // `//pkg:all` → that one package.
        let pkg = body.split_once(':').map(|(p, _)| p).unwrap_or(body);
        pkgs.into_iter().filter(|p| p == pkg).collect()
    };
    if matched.is_empty() {
        return Err(format!("no packages match `{pattern}` under {}", root.display()));
    }
    let (_report, _loaded, targets) =
        load_tree_report_with_targets(root, flags, &matched, Vec::new(), 1);
    let mut labels: Vec<String> = targets.into_iter().map(|t| t.name).collect();
    labels.sort();
    labels.dedup();
    if labels.is_empty() {
        return Err(format!("`{pattern}` matched {} package(s) but no targets", matched.len()));
    }
    Ok(labels)
}

// `discover_packages` moved to `razel_loading::patterns` (P1.2 / R1#1) so query enumerates without
// the build driver; `expand_pattern` above calls it via `razel_loading::discover_packages`.


/// A target surfaced by the impact query: its canonical label + coarse kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedTarget {
    pub label: String,
    pub kind: TargetKind,
}


/// The reverse (rdep) impact of changing `sources`: the affected deliverables and
/// tests. This is the AI-agent / test-selection query — "edit these files → rebuild
/// these, re-run those" — answered by the IR's stored reverse edges (O(affected)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    pub sources: Vec<String>,
    pub targets: Vec<AffectedTarget>,
    pub tests: Vec<AffectedTarget>,
}


/// Compute the impact of editing `files` (paths relative to `package`): analyze the
/// BUILD, wire it into the IR, and walk reverse edges from each file to its dependent
/// targets. No execution — a pure graph query.
pub fn affected(build_src: &str, package: &str, files: &[String]) -> Result<Affected, String> {
    let analyzed = analyze_starlark("BUILD", build_src)?;
    let g = wire_to_ir(package, &analyzed);

    let mut tests = BTreeSet::new();
    let mut deliverables = BTreeSet::new();
    for f in files {
        let fid = FileId::new(format!("{package}/{f}"));
        let (t, d) = g.impacted_targets(&fid);
        tests.extend(t);
        deliverables.extend(d);
    }

    let to_ref = |tid: &TargetId| AffectedTarget {
        label: tid.0.clone(),
        kind: g.target(tid).map(|n| n.kind).unwrap_or(TargetKind::Library),
    };
    Ok(Affected {
        sources: files.to_vec(),
        targets: deliverables.iter().map(to_ref).collect(),
        tests: tests.iter().map(to_ref).collect(),
    })
}


