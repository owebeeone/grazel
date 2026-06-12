//! genrule `cmd` Make-variable expansion.



/// Expand genrule `cmd` Make-variables: `$$`→`$`, `$@` (exactly one output), `$<` (exactly one
/// src), `$(SRCS)`/`$(OUTS)` (space-joined), `$(location X)` (exactly one path for the as-written
/// src/label `X`) / `$(locations X)` (all, space-joined). Anything else `$(…)` errors LOUDLY —
/// unmodeled Make variables must never pass through silently (Bazel-compat discipline).
pub(crate) fn expand_genrule_cmd(
    cmd: &str,
    srcs: &[String],
    outs: &[String],
    loc: &[(String, Vec<String>)],
    out_dir: &str,
) -> anyhow::Result<String> {
    let lookup = |x: &str| -> anyhow::Result<&Vec<String>> {
        loc.iter()
            .find(|(k, _)| k == x)
            // `:name` and `name` are the same label (round 42).
            .or_else(|| {
                loc.iter().find(|(k, _)| {
                    k.trim_start_matches(':') == x.trim_start_matches(':')
                })
            })
            .map(|(_, v)| v)
            .ok_or_else(|| {
                anyhow::anyhow!("$(location {x}): `{x}` is not in this genrule's srcs/outs/tools")
            })
    };
    let mut out = String::with_capacity(cmd.len());
    let mut it = cmd.chars().peekable();
    while let Some(c) = it.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('$') => out.push('$'),
            Some('@') => match outs {
                [one] => out.push_str(one),
                _ => return Err(anyhow::anyhow!("$@ requires exactly one output (genrule)")),
            },
            Some('<') => match srcs {
                [one] => out.push_str(one),
                _ => return Err(anyhow::anyhow!("$< requires exactly one src (genrule)")),
            },
            Some('(') => {
                let inner: String = it.by_ref().take_while(|&c| c != ')').collect();
                match inner.split_once(' ') {
                    None if inner == "SRCS" => out.push_str(&srcs.join(" ")),
                    None if inner == "OUTS" => out.push_str(&outs.join(" ")),
                    None if inner == "@D" || inner == "RULEDIR" => out.push_str(out_dir),
                    // Bazel merged genfiles into bin (0.25+): both roots are the bin root —
                    // razel's single output root (ctx.var's BINDIR constant).
                    None if inner == "GENDIR" || inner == "BINDIR" => {
                        out.push_str("bazel-out/bin")
                    }
                    // The build's -c mode (bazel ctx.var COMPILATION_MODE); razel's
                    // default mirrors bazel's: fastbuild. (Wired to GlobalFlags.mode()
                    // via the cmd expansion seam when flags reach genrule — survey
                    // unblocks at the default today.)
                    None if inner == "COMPILATION_MODE" => out.push_str("fastbuild"),
                    // Host cpu segment (matches the adopted config's _CFG host value).
                    None if inner == "TARGET_CPU" => out.push_str("darwin_arm64"),
                    None if inner == "@" => match outs {
                        [one] => out.push_str(one),
                        _ => return Err(anyhow::anyhow!("$(@) needs exactly one output")),
                    },
                    // rootpath/execpath: runfiles- vs exec-rooted forms — identical in razel's
                    // single-root model; resolved like location.
                    Some(("location" | "rootpath" | "execpath", x)) => match lookup(x.trim())?.as_slice() {
                        [one] => out.push_str(one),
                        many => {
                            return Err(anyhow::anyhow!(
                                "$(location {x}) matches {} files — use $(locations …)",
                                many.len()
                            ));
                        }
                    },
                    Some(("locations" | "rootpaths" | "execpaths", x)) => {
                        out.push_str(&lookup(x.trim())?.join(" "))
                    }
                    _ => {
                        return Err(anyhow::anyhow!(
                            "$({inner}) is not a modeled genrule Make variable \
                             (razel models SRCS/OUTS/location/locations)"
                        ));
                    }
                }
            }
            other => {
                return Err(anyhow::anyhow!(
                    "unsupported `$` escape `${}` in genrule cmd",
                    other.map(String::from).unwrap_or_default()
                ));
            }
        }
    }
    Ok(out)
}
