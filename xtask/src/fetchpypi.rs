//! Fetch R5 (pypi — RazelFetchPlan, decision Gianni 2026-06-11: the FAITHFUL path, not a
//! stub hub): razel's `whl_library`/`pip_parse` equivalent, mirroring rules_python's
//! on-disk shapes byte-for-byte (ground truth: bazel-7.7.0 fetch of @pypi//lit + numpy).
//!
//! Hub `external/pypi/`: per-package dirs with `pkg_aliases` BUILDs + the generated
//! `requirements.bzl` + empty WORKSPACE/REPO.bazel. Spokes `external/pypi_<norm>/`: the
//! wheel itself, `site-packages/` (the unpacked wheel), entry-point shims, and a
//! `whl_library_targets(...)` BUILD; all four boundary files empty (rules_python's shape).
//! Wheels are sha256-verified against TF's OWN requirements lock; URLs resolve through
//! PyPI's JSON API by digest match — the lock is the contract.

use crate::fetchcmd::{fetch_archive, razel_output_root, sh, ws_hash};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct LockPkg {
    /// PyPI name as written (`absl-py`).
    name: String,
    /// Bazel-normalized (`absl_py`).
    norm: String,
    version: String,
    hashes: Vec<String>,
}

fn normalize(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c == '-' || c == '.' || c == '_' { '_' } else { c })
        .collect()
}

/// Parse a pip-compile lock: `name==version \` lines followed by indented
/// `--hash=sha256:…` continuation lines.
fn parse_lock(path: &Path) -> Result<Vec<LockPkg>, String> {
    let src =
        std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out: Vec<LockPkg> = Vec::new();
    for line in src.lines() {
        let t = line.trim_end_matches('\\').trim();
        if !line.starts_with(' ') && !line.starts_with('#') && t.contains("==") {
            let spec = t.split(';').next().unwrap_or(t).trim();
            if let Some((name, version)) = spec.split_once("==") {
                out.push(LockPkg {
                    name: name.trim().to_string(),
                    norm: normalize(name.trim()),
                    version: version.trim().to_string(),
                    hashes: Vec::new(),
                });
            }
        } else if let Some(h) = t.strip_prefix("--hash=sha256:")
            && let Some(last) = out.last_mut()
        {
            last.hashes.push(h.trim().to_string());
        }
    }
    Ok(out)
}

/// Resolve a locked package to a host-usable wheel via PyPI's JSON API, selecting by
/// DIGEST membership in the lock and then by platform specificity (the rules_python
/// choice, verified on numpy: cp-tag + macosx_arm64 wheel beats py3-none-any).
fn resolve_wheel(pkg: &LockPkg) -> Result<Option<(String, String, String)>, String> {
    let url = format!("https://pypi.org/pypi/{}/{}/json", pkg.name, pkg.version);
    let body = sh("curl", &["-fsSL", "--retry", "2", &url])?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("pypi json for {}: {e}", pkg.name))?;
    let empty = Vec::new();
    let urls = v["urls"].as_array().unwrap_or(&empty);
    let mut best: Option<(i32, String, String, String)> = None;
    for u in urls {
        let sha = u["digests"]["sha256"].as_str().unwrap_or_default().to_string();
        if !pkg.hashes.iter().any(|h| h == &sha) {
            continue;
        }
        let filename = u["filename"].as_str().unwrap_or_default().to_string();
        let link = u["url"].as_str().unwrap_or_default().to_string();
        if !filename.ends_with(".whl") {
            // sdist: usable for PURE-PYTHON packages (lit) — kept as the last resort;
            // rules_python pip-builds a wheel here, razel lays out the sources directly.
            if best.is_none() {
                best = Some((1, filename, link, sha));
            }
            continue;
        }
        let mac = filename.contains("macosx");
        let arm = filename.contains("arm64") || filename.contains("universal2");
        let any = filename.contains("none-any");
        // Among platform wheels, bazel/rules_python picks the HIGHEST compatible macosx
        // version (ground truth: numpy macosx_14_0 over macosx_11_0) — fold it into score.
        let macver: i32 = filename
            .split("macosx_")
            .nth(1)
            .and_then(|r| r.split('_').next())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let score = if mac && arm {
            1000 + macver
        } else if any {
            2
        } else {
            0 // a foreign-platform wheel is useless on this host
        };
        if score > 0 && best.as_ref().map(|(s, ..)| score > *s).unwrap_or(true) {
            best = Some((score, filename, link, sha));
        }
    }
    Ok(best.map(|(_, f, l, s)| (f, l, s)))
}

/// Unpack the wheel into the spoke (rules_python's layout): everything under
/// `site-packages/`; console-script entry points become `rules_python_wheel_entry_point_*`
/// shims (the observed template).
fn materialize_spoke(
    external: &Path,
    pkg: &LockPkg,
    wheel_file: &Path,
    wheel_name: &str,
    lock_norms: &std::collections::BTreeSet<String>,
) -> Result<(), String> {
    let spoke = external.join(format!("pypi_{}", pkg.norm));
    let _ = std::fs::remove_dir_all(&spoke);
    let site = spoke.join("site-packages");
    std::fs::create_dir_all(&site).map_err(|e| format!("{}: {e}", site.display()))?;
    std::fs::copy(wheel_file, spoke.join(wheel_name)).map_err(|e| format!("wheel copy: {e}"))?;
    if wheel_name.ends_with(".whl") {
        sh("unzip", &["-q", &wheel_file.display().to_string(), "-d", &site.display().to_string()])?;
    } else {
        // sdist (tar.gz): extract, then hoist the top-level package dirs (those with
        // __init__.py) + dist-info-equivalent metadata into site-packages.
        let tmp = spoke.join(".razel-sdist");
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        sh("tar", &["-xzf", &wheel_file.display().to_string(), "-C", &tmp.display().to_string()])?;
        let root = std::fs::read_dir(&tmp)
            .map_err(|e| format!("sdist read: {e}"))?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .ok_or("empty sdist")?;
        for entry in std::fs::read_dir(&root).map_err(|e| format!("sdist: {e}"))?.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("__init__.py").exists() {
                let dst = site.join(entry.file_name());
                sh("cp", &["-R", &p.display().to_string(), &dst.display().to_string()])?;
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    // dist-info: deps (Requires-Dist ∩ lock, marker-free) + console_scripts.
    let dist_info = site.join(format!(
        "{}-{}.dist-info",
        pkg.name.replace('-', "_"),
        pkg.version
    ));
    let mut deps: Vec<String> = Vec::new();
    if let Ok(meta) = std::fs::read_to_string(dist_info.join("METADATA")) {
        for line in meta.lines() {
            if let Some(req) = line.strip_prefix("Requires-Dist:") {
                if req.contains(';') {
                    continue; // marker-gated (extras, platform) — out of the default set
                }
                let dep = req.trim().split(|c: char| "<>=!~ (".contains(c)).next().unwrap_or("");
                let norm = normalize(dep);
                if lock_norms.contains(&norm) && norm != pkg.norm {
                    deps.push(norm);
                }
            }
        }
    }
    deps.sort();
    deps.dedup();
    let mut entry_points: Vec<(String, String)> = Vec::new();
    if let Ok(eps) = std::fs::read_to_string(dist_info.join("entry_points.txt")) {
        let mut in_console = false;
        for line in eps.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_console = t == "[console_scripts]";
            } else if in_console
                && let Some((ep, target)) = t.split_once('=')
            {
                entry_points.push((ep.trim().to_string(), target.trim().to_string()));
            }
        }
    }
    for (ep, target) in &entry_points {
        let (module, func) = target.split_once(':').unwrap_or((target.as_str(), "main"));
        let shim = format!(
            "#!/usr/bin/env python3\nimport sys\nfrom {module} import {func}\nif __name__ == \"__main__\":\n    sys.exit({func}())\n"
        );
        std::fs::write(spoke.join(format!("rules_python_wheel_entry_point_{ep}.py")), shim)
            .map_err(|e| format!("entry point: {e}"))?;
    }
    let deps_str = deps
        .iter()
        .map(|d| format!("\"@pypi_{d}//:pkg\""))
        .collect::<Vec<_>>()
        .join(", ");
    let eps_str = entry_points
        .iter()
        .map(|(ep, _)| format!("\n        \"{ep}\": \"rules_python_wheel_entry_point_{ep}.py\","))
        .collect::<Vec<_>>()
        .join("");
    let eps_block =
        if eps_str.is_empty() { "{}".to_string() } else { format!("{{{eps_str}\n    }}") };
    // TF's pip annotations (python_init_pip.bzl) append additive_build_content per
    // package — numpy is the only one; mirrored verbatim (ground truth: the bazel spoke).
    let annotation = if pkg.norm == "numpy" {
        "\ncc_library(\n    name = \"numpy_headers_2\",\n    hdrs = glob([\"site-packages/numpy/_core/include/**/*.h\"]),\n    strip_include_prefix=\"site-packages/numpy/_core/include/\",\n)\ncc_library(\n    name = \"numpy_headers_1\",\n    hdrs = glob([\"site-packages/numpy/core/include/**/*.h\"]),\n    strip_include_prefix=\"site-packages/numpy/core/include/\",\n)\ncc_library(\n    name = \"numpy_headers\",\n    deps = [\":numpy_headers_2\", \":numpy_headers_1\"],\n    # For the layering check to work we need to re-export the headers from the\n    # dependencies.\n    hdrs = glob([\"site-packages/numpy/_core/include/**/*.h\"]) +\n           glob([\"site-packages/numpy/core/include/**/*.h\"]),\n)\n"
    } else {
        ""
    };
    let build = format!(
        "load(\"@rules_python//python/private/pypi:whl_library_targets.bzl\", \"whl_library_targets\")\n\n\
         package(default_visibility = [\"//visibility:public\"])\n\n\
         whl_library_targets(\n    copy_executables = {{}},\n    copy_files = {{}},\n    data = [],\n    data_exclude = [],\n    dep_template = \"@pypi_{{name}}//:{{target}}\",\n    dependencies = [{deps_str}],\n    dependencies_by_platform = {{}},\n    enable_implicit_namespace_pkgs = False,\n    entry_points = {eps_block},\n    group_deps = [],\n    group_name = \"\",\n    name = \"{wheel_name}\",\n    namespace_package_files = [],\n    sdist_filename = None,\n    srcs_exclude = [],\n    tags = [\n        \"pypi_name={}\",\n        \"pypi_version={}\",\n    ],\n)\n{annotation}",
        pkg.name, pkg.version
    );
    std::fs::write(spoke.join("BUILD.bazel"), build).map_err(|e| format!("BUILD: {e}"))?;
    // rules_python's spoke boundary shape: all four, empty (observed).
    for f in ["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel", "REPO.bazel"] {
        std::fs::write(spoke.join(f), "").map_err(|e| format!("{f}: {e}"))?;
    }
    Ok(())
}

fn materialize_hub(external: &Path, pkgs: &[LockPkg]) -> Result<(), String> {
    let hub = external.join("pypi");
    let _ = std::fs::remove_dir_all(&hub);
    std::fs::create_dir_all(&hub).map_err(|e| format!("{}: {e}", hub.display()))?;
    std::fs::write(
        hub.join("BUILD.bazel"),
        "package(default_visibility = [\"//visibility:public\"])\n\n\
         # Ensure the `requirements.bzl` source can be accessed by stardoc, since users load() from it\n\
         exports_files([\"requirements.bzl\"])\n",
    )
    .map_err(|e| format!("hub BUILD: {e}"))?;
    for f in ["WORKSPACE", "REPO.bazel"] {
        std::fs::write(hub.join(f), "").map_err(|e| format!("{f}: {e}"))?;
    }
    let mut all_req = String::new();
    let mut all_whl = String::new();
    for p in pkgs {
        all_req.push_str(&format!("    \"@pypi//{}:pkg\",\n", p.norm));
        all_whl.push_str(&format!("    \"{}\": \"@pypi//{}:whl\",\n", p.norm, p.norm));
        let dir = hub.join(&p.norm);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        std::fs::write(
            dir.join("BUILD.bazel"),
            format!(
                "load(\"@rules_python//python/private/pypi:pkg_aliases.bzl\", \"pkg_aliases\")\n\n\
                 package(default_visibility = [\"//visibility:public\"])\n\n\
                 pkg_aliases(\n    name = \"{}\",\n    actual = \"pypi_{}\",\n)",
                p.norm, p.norm
            ),
        )
        .map_err(|e| format!("alias BUILD: {e}"))?;
    }
    let req = format!(
        "\"\"\"Starlark representation of locked requirements.\n\n@generated by razel fetch-pypi (rules_python pip_parse shape).\n\"\"\"\n\n\
         all_requirements = [\n{all_req}]\n\n\
         all_whl_requirements_by_package = {{\n{all_whl}}}\n\n\
         all_whl_requirements = all_whl_requirements_by_package.values()\n\n\
         def requirement(name):\n    return \"@pypi//{{}}:pkg\".format(name.replace(\"-\", \"_\").replace(\".\", \"_\").lower())\n\n\
         def whl_requirement(name):\n    return \"@pypi//{{}}:whl\".format(name.replace(\"-\", \"_\").replace(\".\", \"_\").lower())\n\n\
         def install_deps(**kwargs):\n    pass\n",
    );
    std::fs::write(hub.join("requirements.bzl"), req).map_err(|e| format!("requirements.bzl: {e}"))?;
    Ok(())
}

pub(crate) fn fetch_pypi(root: &Path, names: &[String]) -> Result<(), String> {
    let ws = root.join("../third-party/tensorflow");
    // The hermetic python TF defaults to (ground truth: bazel chose cp310 wheels).
    let pyver = std::env::var("RAZEL_PY_VERSION").unwrap_or_else(|_| "3_10".to_string());
    let lock = ws.join(format!("requirements_lock_{pyver}.txt"));
    let pkgs = parse_lock(&lock)?;
    let lock_norms: std::collections::BTreeSet<String> =
        pkgs.iter().map(|p| p.norm.clone()).collect();
    let external = razel_output_root().join(ws_hash(&ws)?).join("external");
    std::fs::create_dir_all(&external).map_err(|e| format!("{}: {e}", external.display()))?;
    materialize_hub(&external, &pkgs)?;
    println!("fetch-pypi: hub @pypi ({} packages) → {}", pkgs.len(), external.join("pypi").display());
    let all = names.len() == 1 && names[0] == "--all";
    let want: Vec<&LockPkg> = if all {
        pkgs.iter().collect()
    } else {
        names
            .iter()
            .map(|n| {
                let norm = normalize(n);
                pkgs.iter()
                    .find(|p| p.norm == norm)
                    .ok_or_else(|| format!("`{n}` is not in {}", lock.display()))
            })
            .collect::<Result<_, _>>()?
    };
    let mut skipped: Vec<String> = Vec::new();
    for p in want {
        match resolve_wheel(p)? {
            None => {
                skipped.push(p.norm.clone());
                continue;
            }
            Some((filename, url, sha)) => {
                let archive = fetch_archive(&razel_output_root(), &[url], &sha)?;
                materialize_spoke(&external, p, &archive, &filename, &lock_norms)?;
                println!("fetch-pypi: {} {} → pypi_{}", p.name, p.version, p.norm);
            }
        }
    }
    if !skipped.is_empty() {
        println!(
            "fetch-pypi: {} package(s) have NO host-usable wheel (linux-only/sdist) — spokes \
             not materialized, named holes: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    Ok(())
}
