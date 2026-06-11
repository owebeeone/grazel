//! Fetch R1–R3 (RazelFetchPlan §3): `xtask fetch-extract` — evaluate TF's WORKSPACE chain
//! with the repository_rule RECORDER and write the repo lockfile + a dry-run report (no
//! network) — and `xtask fetch <repo>…` — download (sha-pinned, mirror-first) into razel's
//! Bazel-mirrored cache and MATERIALIZE (extract → strip_prefix → patch -p1 → build_file/
//! link_files) into `<output base>/external/<name>/`. Ground truth verified against a real
//! `bazel-7.7.0 fetch @curl//:curl` (round 36): same content-address, same tree shape.

use razel_loading::{AttrV, GlobalFlags, extract_workspace_repos};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    // Bazel writes its lockfile (MODULE.bazel.lock) into the WORKSPACE ROOT; razel's
    // equivalent sits in the same place under its own name (mirror-Bazel placement).
    let out = ws.join("razel-lock.json");
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

// ---- R2/R3: fetch + materialize -----------------------------------------------------

fn sh(cmd: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// razel's output root — SIBLING to `_bazel_<user>`, identical layout below it
/// (RazelFetchPlan §3 R2; decision: Gianni). Override: RAZEL_OUTPUT_ROOT.
fn razel_output_root() -> PathBuf {
    if let Ok(r) = std::env::var("RAZEL_OUTPUT_ROOT") {
        return PathBuf::from(r);
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "razel".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(format!("/private/var/tmp/_razel_{user}"))
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(format!("{home}/.cache/razel/_razel_{user}"))
    }
}

/// Bazel's output-base key: md5 of the absolute workspace path (verified round 36 —
/// `md5("…/third-party/tensorflow")` == the live `_bazel_<user>` dir name).
fn ws_hash(ws: &Path) -> Result<String, String> {
    let p = ws.canonicalize().map_err(|e| format!("{}: {e}", ws.display()))?;
    Ok(sh("md5", &["-q", "-s", &p.display().to_string()])?.trim().to_string())
}

struct Lock {
    repos: Vec<serde_json::Map<String, serde_json::Value>>,
    /// tf_vendored name → in-workspace relative path (`@xla` → `third_party/xla`) — the
    /// label resolver for patch/build_file/link_files labels.
    vendored: BTreeMap<String, String>,
}

fn read_lock(ws: &Path) -> Result<Lock, String> {
    let path = ws.join("razel-lock.json");
    let v: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
    )
    .map_err(|e| format!("lockfile parse: {e}"))?;
    let repos: Vec<serde_json::Map<String, serde_json::Value>> = v["repos"]
        .as_array()
        .ok_or("lockfile has no repos")?
        .iter()
        .filter_map(|r| r.as_object().cloned())
        .collect();
    let mut vendored = BTreeMap::new();
    for r in &repos {
        if r.get("kind").and_then(|k| k.as_str()).is_some_and(|k| k.contains("_tf_vendored"))
            && let (Some(n), Some(p)) =
                (r.get("name").and_then(|x| x.as_str()), r.get("path").and_then(|x| x.as_str()))
        {
            vendored.insert(n.to_string(), p.to_string());
        }
    }
    Ok(Lock { repos, vendored })
}

/// The repo root against which a spec's BARE `//pkg:file` label strings resolve: the repo
/// of the DEFINING .bzl module — Bazel's attr-conversion semantics, proven on FP16 (its
/// `build_file = "//third_party/FP16:FP16.BUILD"` exists ONLY in @xla's tree, and the rule
/// was defined in `@xla//third_party:repo.bzl`). The spec's `kind` carries that module.
fn spec_label_root(ws: &Path, vendored: &BTreeMap<String, String>, kind: &str) -> PathBuf {
    if let Some(body) = kind.strip_prefix('@')
        && let Some((repo, _)) = body.split_once("//")
        && let Some(vp) = vendored.get(repo)
    {
        return ws.join(vp);
    }
    ws.to_path_buf()
}

/// Resolve a `//pkg:file` / `@repo//pkg:file` label to a real path (`bare_root` for bare
/// labels — see [`spec_label_root`]; tf_vendored map for qualified ones). Loud on anything
/// else — never a silent skip.
fn resolve_label(
    ws: &Path,
    vendored: &BTreeMap<String, String>,
    bare_root: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let (root, rest) = if let Some(rest) = label.strip_prefix("//") {
        (bare_root.to_path_buf(), rest)
    } else if let Some(body) = label.strip_prefix('@') {
        let (repo, rest) = body.split_once("//").ok_or_else(|| format!("bad label `{label}`"))?;
        let vp = vendored
            .get(repo)
            .ok_or_else(|| format!("label `{label}`: repo `@{repo}` is not tf_vendored — unresolvable"))?;
        (ws.join(vp), rest)
    } else {
        return Err(format!("bad label `{label}`"));
    };
    let (pkg, file) = rest.split_once(':').ok_or_else(|| format!("bad label `{label}`"))?;
    let p = root.join(pkg).join(file);
    if !p.is_file() {
        return Err(format!("label `{label}` → {} does not exist", p.display()));
    }
    Ok(p)
}

fn attr_str<'a>(r: &'a serde_json::Map<String, serde_json::Value>, k: &str) -> Option<&'a str> {
    r.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn attr_list(r: &serde_json::Map<String, serde_json::Value>, k: &str) -> Vec<String> {
    r.get(k)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Download into the content-addressed cache (skip when present — content addressing IS
/// the validity check, as in Bazel); returns the cached archive path.
fn fetch_archive(root: &Path, urls: &[String], sha256: &str) -> Result<PathBuf, String> {
    let dir = root.join("cache/repos/v1/content_addressable/sha256").join(sha256);
    let file = dir.join("file");
    if file.is_file() {
        return Ok(file);
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join("file.razel-tmp");
    let mut last_err = String::from("no urls");
    for url in urls {
        let _ = std::fs::remove_file(&tmp);
        match sh("curl", &["-fsSL", "--retry", "2", "-o", &tmp.display().to_string(), url]) {
            Err(e) => last_err = e,
            Ok(_) => {
                let got = sh("shasum", &["-a", "256", &tmp.display().to_string()])?;
                let got = got.split_whitespace().next().unwrap_or_default().to_string();
                if got == sha256 {
                    std::fs::rename(&tmp, &file).map_err(|e| format!("rename: {e}"))?;
                    return Ok(file);
                }
                last_err = format!("sha256 mismatch from {url}: got {got}, want {sha256}");
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err)
}

/// Extract + strip_prefix + patches + build_file/link_files → `<out base>/external/<name>`.
fn materialize(
    ws: &Path,
    lock: &Lock,
    spec: &serde_json::Map<String, serde_json::Value>,
    archive: &Path,
    external: &Path,
) -> Result<(), String> {
    let name = attr_str(spec, "name").ok_or("spec without name")?;
    let bare_root = spec_label_root(ws, &lock.vendored, attr_str(spec, "kind").unwrap_or(""));
    let urls = attr_list(spec, "urls");
    let url = urls.first().map(String::as_str).unwrap_or_default();
    let target = external.join(name);
    let tmp = external.join(format!(".razel-extract-{name}"));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&target);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let a = archive.display().to_string();
    let t = tmp.display().to_string();
    // Archive type: the explicit attr wins; else the URL extension (query-stripped).
    let kind = attr_str(spec, "type")
        .map(String::from)
        .unwrap_or_else(|| url.split('?').next().unwrap_or(url).to_string());
    if kind.ends_with(".zip") || kind == "zip" {
        sh("unzip", &["-q", &a, "-d", &t])?;
    } else if kind.ends_with(".tar.bz2") {
        sh("tar", &["-xjf", &a, "-C", &t])?;
    } else if kind.ends_with(".tar.xz") {
        sh("tar", &["-xJf", &a, "-C", &t])?;
    } else {
        // .tar.gz / .tgz / the github archive default
        sh("tar", &["-xzf", &a, "-C", &t])?;
    }
    let src = match attr_str(spec, "strip_prefix") {
        Some(sp) => {
            let p = tmp.join(sp);
            if !p.is_dir() {
                return Err(format!("strip_prefix `{sp}` not found in archive for `{name}`"));
            }
            p
        }
        None => tmp.clone(),
    };
    std::fs::rename(&src, &target).map_err(|e| format!("place {}: {e}", target.display()))?;
    let _ = std::fs::remove_dir_all(&tmp);
    // (Boundary files are written AFTER patches/links — see the end of this fn.)
    // Patches: repo.bzl applies ctx.patch(file, strip = 1).
    for pf in attr_list(spec, "patch_file") {
        let p = resolve_label(ws, &lock.vendored, &bare_root, &pf)?;
        sh(
            "patch",
            &["-p1", "-s", "-d", &target.display().to_string(), "-i", &p.display().to_string()],
        )
        .map_err(|e| format!("patch `{pf}` on `{name}`: {e}"))?;
    }
    if !attr_list(spec, "patch_cmds").is_empty() {
        return Err(format!("`{name}` carries patch_cmds (shell) — unsupported, refusing silently-wrong"));
    }
    // build_file → BUILD.bazel (replacing the repo's own); link_files {label: relpath}.
    // Fidelity split (round 36, read from both sources): TF-style `_tf_http_archive`
    // SYMLINKS build_file/link_files (`ctx.symlink`); bazel_tools' `http_archive` COPIES
    // its build_file (`ctx.file(ctx.read(...))`).
    let symlink_mode = attr_str(spec, "kind").unwrap_or("").contains("_tf_http_archive");
    let place = |srcp: &Path, dst: &Path| -> Result<(), String> {
        let _ = std::fs::remove_file(dst);
        if symlink_mode {
            std::os::unix::fs::symlink(srcp, dst).map_err(|e| format!("symlink: {e}"))
        } else {
            std::fs::copy(srcp, dst).map(|_| ()).map_err(|e| format!("copy: {e}"))
        }
    };
    if let Some(bf) = attr_str(spec, "build_file") {
        let p = resolve_label(ws, &lock.vendored, &bare_root, bf)?;
        let _ = std::fs::remove_file(target.join("BUILD"));
        place(&p, &target.join("BUILD.bazel")).map_err(|e| format!("build_file: {e}"))?;
    }
    if let Some(links) = spec.get("link_files").and_then(|v| v.as_object()) {
        for (label, rel) in links {
            let p = resolve_label(ws, &lock.vendored, &bare_root, label)?;
            let rel = rel.as_str().ok_or("link_files value not a string")?;
            let dst = target.join(rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            place(&p, &dst).map_err(|e| format!("link_files `{label}`: {e}"))?;
        }
    }
    // Bazel-core boundary rule, PROVEN on 7.7.0 (round 36 matrix, 2 impls x 4 archive
    // shapes; gemmlowp's upstream WORKSPACE is a 0-byte file — no anomalies): a repo
    // ending its rule with NONE of these four files gets empty WORKSPACE + REPO.bazel;
    // any ONE present (even empty) suppresses both writes.
    let has_boundary = ["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel", "REPO.bazel"]
        .iter()
        .any(|f| target.join(f).exists());
    if !has_boundary {
        std::fs::write(target.join("WORKSPACE"), "").map_err(|e| format!("WORKSPACE: {e}"))?;
        std::fs::write(target.join("REPO.bazel"), "").map_err(|e| format!("REPO.bazel: {e}"))?;
    }
    Ok(())
}

pub(crate) fn fetch(root: &Path, names: &[String]) -> Result<(), String> {
    if names.is_empty() {
        return Err("usage: xtask fetch <repo> [<repo>…]  (names from the lockfile)".into());
    }
    let ws = root.join("../third-party/tensorflow");
    let lock = read_lock(&ws)?;
    let out_root = razel_output_root();
    let external = out_root.join(ws_hash(&ws)?).join("external");
    std::fs::create_dir_all(&external).map_err(|e| format!("{}: {e}", external.display()))?;
    for name in names {
        let spec = lock
            .repos
            .iter()
            .find(|r| attr_str(r, "name") == Some(name.as_str()))
            .ok_or_else(|| format!("`{name}` is not in the lockfile"))?;
        let urls = attr_list(spec, "urls");
        let Some(sha) = attr_str(spec, "sha256") else {
            return Err(format!(
                "`{name}` is not archive-shaped (kind {}) — vendored/generated repos are not fetched",
                attr_str(spec, "kind").unwrap_or("?")
            ));
        };
        let archive = fetch_archive(&out_root, &urls, sha)?;
        materialize(&ws, &lock, spec, &archive, &external)?;
        println!("fetch: {name} → {}", external.join(name).display());
    }
    Ok(())
}

/// The fetched external root for `ws` when materializations exist — tfload/stress wire it
/// as the SECOND resolution base (fetch R4; vendored `third-party/` keeps precedence).
pub(crate) fn fetched_external_dir(ws: &Path) -> Option<PathBuf> {
    let d = razel_output_root().join(ws_hash(ws).ok()?).join("external");
    d.is_dir().then_some(d)
}
