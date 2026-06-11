//! Fetch R1 (RazelFetchPlan §3): `xtask fetch-extract` — evaluate TF's WORKSPACE chain with
//! the repository_rule RECORDER and write the repo lockfile + a dry-run report. No network.

use razel_loading::{AttrV, GlobalFlags, extract_workspace_repos};
use std::path::Path;

fn attr_json(v: &AttrV) -> serde_json::Value {
    match v {
        AttrV::Str(s) => serde_json::Value::String(s.clone()),
        AttrV::List(xs) => serde_json::Value::Array(
            xs.iter().map(|s| serde_json::Value::String(s.clone())).collect(),
        ),
        AttrV::Dict(kvs) => serde_json::Value::Object(
            kvs.iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                .collect(),
        ),
        AttrV::Bool(b) => serde_json::Value::Bool(*b),
        AttrV::Int(i) => serde_json::Value::from(*i),
    }
}

pub(crate) fn fetch_extract(root: &Path) -> Result<(), String> {
    let ws = root.join("../third-party/tensorflow");
    let mut flags = GlobalFlags::default();
    flags.external_base = Some(root.join("../third-party"));
    let (specs, notes) = extract_workspace_repos(&ws, flags)?;
    let repos: Vec<serde_json::Value> = specs
        .iter()
        .map(|s| {
            let mut o = serde_json::Map::new();
            o.insert("name".into(), serde_json::Value::String(s.name.clone()));
            o.insert("kind".into(), serde_json::Value::String(s.kind.clone()));
            for (k, v) in &s.attrs {
                o.insert(k.clone(), attr_json(v));
            }
            serde_json::Value::Object(o)
        })
        .collect();
    let lock = serde_json::json!({
        "comment": "razel fetch lockfile — extracted from the WORKSPACE chain (RazelFetchPlan R1)",
        "workspace": ws.display().to_string(),
        "repos": repos,
    });
    let out = root.join("../third-party/tensorflow.razel-lock.json");
    std::fs::write(&out, serde_json::to_string_pretty(&lock).expect("json") + "\n")
        .map_err(|e| format!("write {}: {e}", out.display()))?;
    // The dry-run report: counts by kind + what the fetcher would need.
    let mut sha = 0usize;
    let mut patched = 0usize;
    let mut linked = 0usize;
    for s in &specs {
        if s.attrs.iter().any(|(k, _)| k == "sha256") {
            sha += 1;
        }
        if s.attrs.iter().any(|(k, v)| k == "patch_file" && !matches!(v, AttrV::List(x) if x.is_empty())) {
            patched += 1;
        }
        if s.attrs.iter().any(|(k, _)| k == "link_files" || k == "build_file") {
            linked += 1;
        }
    }
    println!(
        "fetch-extract: {} repos → {} ({} sha-pinned, {} patched, {} with build_file/link_files)",
        specs.len(),
        out.display(),
        sha,
        patched,
        linked
    );
    for n in &notes {
        println!("  note: {n}");
    }
    Ok(())
}
