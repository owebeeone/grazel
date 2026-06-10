//! The rule-authoring API: Ctx, the rule() object, rule_globals (rule/Label/select/providers). C0.

use crate::state::{AnalyzedTarget, canon_label, qualify, session, with_current};
use crate::values::{Depset, extract_files, file_path, unpack};
use crate::glob::do_glob;
use crate::deps::record_target;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::tuple::UnpackTuple;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::{
    Value, ValueLike,
};

// C0-style split (2059 → modules by axis of change). Glob re-exports keep every existing
// `crate::dialect::X` path working; tighten to explicit imports opportunistically.
pub(crate) use crate::decls::*;
pub(crate) use crate::genrule_cmd::*;
pub(crate) use crate::labels::*;
pub(crate) use crate::provider_values::*;
pub(crate) use crate::selects::*;

#[allow(non_snake_case)]
#[starlark::starlark_module]
pub(crate) fn rule_globals(b: &mut GlobalsBuilder) {
    /// `rule(implementation, attrs={})` → a callable rule object.
    fn rule<'v>(
        implementation: Value<'v>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        // D4: absorb the other rule() kwargs upstream rules pass (build_setting, doc, cfg, toolchains,
        // provides, executable, test, …). Not yet honored — enough to define the rule.
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // D1: keep the declared schema (was discarded) — instantiation consults it for defaults +
        // mandatory. alloc (freezable) so the rule survives module.freeze() (defined in a .bzl + load()ed).
        let attrs = attrs.unwrap_or_else(Value::new_none);
        let outputs = _kw.get("outputs").copied().unwrap_or_else(Value::new_none);
        Ok(eval.heap().alloc(RuleObjGen { implementation, attrs, outputs }))
    }

    /// `provider(doc=?, fields=?, init=?)` → a callable provider constructor (D4.2). With `init`
    /// it returns Bazel's 2-tuple `(Provider, raw_ctor)` (the `CcInfo, _raw = provider(...)`
    /// shape — rules_cc). Field-name validation not yet enforced.
    fn provider<'v>(
        #[starlark(args)] _args: UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let init = kw.get("init").copied().unwrap_or_else(Value::new_none);
        let main = heap.alloc(ProviderCallable { init });
        Ok(if init.is_none() {
            main
        } else {
            // Bazel: with init, provider() returns (Provider, raw_ctor).
            heap.alloc((main, heap.alloc(RawCtor { canonical: main })))
        })
    }

    /// Bazel builtin global providers `RunEnvironmentInfo(...)` / `OutputGroupInfo(...)` (D4 compat):
    /// construct an instance `struct` from the kwargs. Stubs so real upstream rules resolve + run; the
    /// instances aren't yet captured/consumed (D4.3+).
    fn RunEnvironmentInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
    fn AnalysisFailureInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
    fn AnalysisTestResultInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }
    fn OutputGroupInfo<'v>(
        #[starlark(kwargs)] kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let fields: Vec<(String, Value<'v>)> =
            kw.iter().map(|(k, v)| (k.clone(), *v)).collect();
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// Builtin global provider stubs real rulesets reference (PackageSpecificationInfo:
    /// package_group's provider; RunEnvironmentInfo/OutputGroupInfo are constructed). Globals
    /// referenced-but-not-modeled resolve to constructors that absorb.
    fn PackageSpecificationInfo<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `razel_config_setting_group(name, match_all=[], match_any=[])` — the host-native backing
    /// for skylib's `selects.config_setting_group` (Bazel implements groups via native
    /// alias/ConfigMatchingProvider chains razel doesn't model; the host provides the contract).
    fn razel_config_setting_group<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] match_all: Option<UnpackList<String>>,
        #[starlark(require = named)] match_any: Option<UnpackList<String>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let all_v = match_all.map(|l| l.items).unwrap_or_default();
        let any_v = match_any.map(|l| l.items).unwrap_or_default();
        let (all, members) = match (all_v.is_empty(), any_v.is_empty()) {
            (false, true) => (true, all_v),
            (true, false) => (false, any_v),
            _ => {
                return Err(anyhow::anyhow!(
                    "config_setting_group needs exactly one of match_all/match_any"
                ));
            }
        };
        let members = members.iter().map(|m| canon_label(sess, m)).collect();
        sess.config_specs.borrow_mut().insert(
            canon_label(sess, &name),
            crate::state::ConfigSpec { group: Some((all, members)), ..Default::default() },
        );
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `repository_rule(implementation, ...)` (compat stub): repo rules DEFINE at load; razel
    /// never fetches (vendored/host repos instead) — invoking one surfaces at analysis. L6/L7.
    fn repository_rule<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `module_extension(implementation, ...)` (compat stub) — bzlmod machinery, same posture.
    fn module_extension<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `tag_class(...)` (compat stub) — bzlmod machinery.
    fn tag_class<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `aspect(implementation, attrs=?, ...)` → a real aspect value, applied along label-attr
    /// edges at dep resolution (L5 MVP: attr_aspects propagation is the `deps` edge).
    fn aspect<'v>(
        #[starlark(require = named)] implementation: Option<Value<'v>>,
        #[starlark(require = named)] attrs: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(eval.heap().alloc(crate::provider_values::AspectObj {
            implementation: implementation.unwrap_or_else(Value::new_none),
            attrs: attrs.unwrap_or_else(Value::new_none),
        }))
    }

    /// `objc_library(...)` — declare-only stub (Apple rules; razel records the target name so
    /// labels resolve; no actions). Registered debt.
    fn objc_library<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `subrule(implementation, ...)` (compat stub): Bazel's subrule mechanism, absorbed —
    /// rules defining subrules load; invoking one surfaces at analysis (registered debt).
    fn subrule<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(Value::new_none())
    }

    /// `transition(implementation, inputs, outputs)` (D4 compat stub): real rules define config
    /// transitions + pass them as `rule(cfg=…)`. razel doesn't apply transitions yet — absorb the args
    /// so the rule defines; `rule()` already absorbs the `cfg` kwarg.
    fn transition<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `analysis_test_transition(...)` (compat stub) — skylib unittest machinery.
    fn analysis_test_transition<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `visibility(...)` — .bzl load-visibility declaration (compat stub: not enforced).
    fn visibility<'v>(
        #[starlark(args)] _a: UnpackTuple<Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `licenses(...)` — legacy license declaration (compat stub).
    fn licenses<'v>(#[starlark(args)] _a: UnpackTuple<Value<'v>>) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `exports_files(...)` — source files are package-visible in razel already (compat stub).
    fn exports_files<'v>(
        #[starlark(args)] _a: UnpackTuple<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `package_group(...)` — visibility grouping (compat stub: visibility not enforced).
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `exec_group(toolchains=?, exec_compatible_with=?)` (compat stub): execution groups are not
    /// modeled (single execution platform); absorb so real rules define.
    fn exec_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `configuration_field(fragment, name)` (D4 compat stub): a late-bound default razel doesn't model
    /// — absorb the args; the attr's default becomes `None`.
    fn configuration_field<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }

    /// `Label("//pkg:name")` — a minimal Label exposing `.package`/`.name`/
    /// `.workspace_root`/`.workspace_name`. razel treats everything as the main repo,
    /// so workspace_root/workspace_name are empty (matching Bazel on the main repo).
    fn Label<'v>(
        #[starlark(require = pos)] s: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let after = s.rsplit_once("//").map(|(_, a)| a).unwrap_or(s.as_str());
        let (pkg, name) = match after.split_once(':') {
            Some((p, n)) => (p.to_string(), n.to_string()),
            None => (
                after.to_string(),
                after.rsplit('/').next().unwrap_or(after).to_string(),
            ),
        };
        // A label written in `@repo` code resolves against that repo (clean_dep's whole point).
        // Binding: the CALL-SITE file (Bazel binds Label() to the .bzl containing the literal —
        // macros run at BUILD eval, so the load-time repo stack alone is not enough).
        let repo = if s.starts_with('@') {
            s.split("//").next().map(|r| r.to_string())
        } else {
            let call_site = eval
                .call_stack_top_location()
                .map(|l| l.file.filename().to_string())
                .filter(|f| f.starts_with('@'))
                .and_then(|f| f.split("//").next().map(String::from));
            call_site.or_else(|| {
                session(eval)
                    .current_bzl_repo
                    .borrow()
                    .last()
                    .cloned()
                    .flatten()
                    .filter(|(r, _)| !r.is_empty())
                    .map(|(r, _)| format!("@{r}"))
            })
        };
        Ok(eval.heap().alloc(LabelV { repo, package: pkg, name }))
    }

    /// BUILD package-declaration builtins. razel doesn't enforce visibility/licenses
    /// and tracks no separate file-export set, so these are no-op declarations —
    /// recognized so real BUILD files evaluate. (`package`, `package_group`,
    /// `licenses`, `exports_files`.)
    fn package<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn package_group<'v>(
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    fn licenses<'v>(
        #[starlark(require = pos)] _licenses: UnpackList<String>,
    ) -> anyhow::Result<NoneType> {
        Ok(NoneType)
    }
    /// Native build-graph builtins razel recognizes so real BUILD files evaluate.
    /// `config_setting`/`test_suite`/`alias` carry no buildable output here, so they
    /// record an empty target under their label (analysis-visible, builds to nothing);
    /// `filegroup` forwards its `srcs` as its outputs so dependents resolve them.
    /// Platform constraint rules (@platforms): `constraint_value` registers as a select
    /// condition matched against the REAL host (os/cpu); foreign constraint families are
    /// conservative-false. `constraint_setting`/`platform` are declare-only.
    fn constraint_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget { name: canon_label(sess, &name), ..Default::default() });
        Ok(NoneType)
    }
    fn constraint_value<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let canon = canon_label(sess, &name);
        let pkg = sess.current_pkg.borrow().clone().unwrap_or_default();
        let host_matches = crate::state::host_constraint_matches(&pkg, &name);
        sess.config_specs.borrow_mut().insert(
            canon.clone(),
            crate::state::ConfigSpec {
                // empty spec ⇒ always-match; unmodeled ⇒ never-match (CPU-host posture).
                unmodeled: !host_matches,
                ..Default::default()
            },
        );
        record_target(sess, AnalyzedTarget { name: canon, ..Default::default() });
        Ok(NoneType)
    }
    fn platform<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget { name: canon_label(sess, &name), ..Default::default() });
        Ok(NoneType)
    }

    /// `toolchain(...)` / `toolchain_type(...)` — toolchain registration targets (declare-only;
    /// resolution is L3).
    fn toolchain<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn toolchain_type<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `label_flag`/`label_setting` — build-setting label flags as BUILD globals (declare-only).
    fn label_flag<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] build_setting_default: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        // A label flag/setting FORWARDS to its default target (providers flow through) —
        // alias semantics; razel doesn't model flag overrides (registered debt).
        if let Some(d) = build_setting_default {
            let d = d.unpack_str().map(String::from).unwrap_or_else(|| d.to_string());
            if !d.is_empty() {
                let actual = crate::state::canon_label(sess, &d);
                sess.aliases
                    .borrow_mut()
                    .insert(crate::state::canon_label(sess, &name), actual);
            }
        }
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn label_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] build_setting_default: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        // A label flag/setting FORWARDS to its default target (providers flow through) —
        // alias semantics; razel doesn't model flag overrides (registered debt).
        if let Some(d) = build_setting_default {
            let d = d.unpack_str().map(String::from).unwrap_or_else(|| d.to_string());
            if !d.is_empty() {
                let actual = crate::state::canon_label(sess, &d);
                sess.aliases
                    .borrow_mut()
                    .insert(crate::state::canon_label(sess, &name), actual);
            }
        }
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `genrule(name, srcs, outs, cmd)` — Bazel's generic shell rule (razelV3): ONE bash action
    /// running `cmd` after Make-variable expansion (`$@`/`$<`/`$(SRCS)`/`$(OUTS)`/`$(location)`;
    /// `$$` escapes). A src that is a label resolves to its files (demand-driven, E0c-deferred).
    /// Unmodeled variables (`$(RULEDIR)`, tools=…) error loudly — registered debt, not silence.
    fn genrule<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] outs: UnpackList<String>,
        #[starlark(require = named)] cmd: Option<String>,
        #[starlark(require = named)] cmd_bash: Option<String>,
        #[starlark(require = named)] tools: Option<UnpackList<Value<'v>>>,
        #[starlark(require = named)] exec_tools: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let label = canon_label(session(eval), &name);
        // Stringify srcs/tools OUTSIDE the closure (native bodies capture only plain data).
        // tools/exec_tools join the location table + inputs (their labels resolve like srcs).
        let mut srcs = crate::values::unpack_strs(srcs);
        srcs.extend(crate::values::unpack_strs(tools));
        srcs.extend(crate::values::unpack_strs(exec_tools));
        // Bazel: `cmd` or the bash-explicit `cmd_bash` (razel always runs bash).
        let cmd = match cmd.or(cmd_bash) {
            Some(c) => c,
            None => return Err(anyhow::anyhow!("genrule `{name}` needs cmd (or cmd_bash)")),
        };
        // Output-file labels resolve statically (Bazel: outs are targets) — index them now.
        {
            let sess = session(eval);
            let mut idx = sess.output_index.borrow_mut();
            for o in &outs.items {
                idx.insert(canon_label(sess, o), (label.clone(), qualify(sess, o)));
            }
        }
        record_native(eval, label, crate::state::native_decl(move |eval| {
            let sess = session(eval);
            // Split srcs: labels resolve to their files (their package loads/analyzes on
            // demand); plain names are this package's files. `loc` keys keep the as-written
            // form for `$(location X)`.
            let (mut inputs, mut deps) = (Vec::new(), Vec::new());
            let mut loc: Vec<(String, Vec<String>)> = Vec::new();
            for s in srcs.clone() {
                if s.starts_with(':') || s.starts_with("//") {
                    let dep = crate::deps::resolve_dep(eval, &s)?;
                    loc.push((s.clone(), dep.libs.clone()));
                    inputs.extend(dep.libs);
                    deps.push(dep.canon);
                } else {
                    let q = qualify(sess, &s);
                    loc.push((s, vec![q.clone()]));
                    inputs.push(q);
                }
            }
            let outs_raw: Vec<String> = outs.items.clone();
            let outs: Vec<String> = outs.items.iter().map(|o| qualify(sess, o)).collect();
            // $(@D)/$(RULEDIR): the package's output root — qualified out minus the
            // as-written path (single-out @D is the output's own directory).
            let out_dir = match (outs.first(), outs_raw.first()) {
                (Some(q), Some(raw)) if outs.len() > 1 => q
                    .strip_suffix(raw.as_str())
                    .map(|d| d.trim_end_matches('/').to_string())
                    .unwrap_or_default(),
                (Some(q), _) => q.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default(),
                _ => String::new(),
            };
            let expanded = expand_genrule_cmd(&cmd, &inputs, &outs, &loc, &out_dir)?;
            record_target(sess, AnalyzedTarget {
                name: canon_label(sess, &name),
                deps,
                actions: vec![crate::state::AnalyzedAction {
                    mnemonic: "Genrule".into(),
                    argv: vec!["/bin/bash".into(), "-c".into(), expanded],
                    inputs,
                    outputs: outs.clone(),
                }],
                default_info: outs,
                ..Default::default()
            });
            Ok(())
        }))?;
        Ok(NoneType)
    }

    /// `config_setting(name, values=?, define_values=?)` — declare a constraint spec `select()`
    /// matches against the structured configuration (razelV3: real resolution, not a placeholder).
    fn config_setting<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] values: Option<SmallMap<String, String>>,
        #[starlark(require = named)] define_values: Option<SmallMap<String, String>>,
        #[starlark(require = named)] flag_values: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let spec = crate::state::ConfigSpec {
            values: values.map(|m| m.into_iter().collect()).unwrap_or_default(),
            define_values: define_values.map(|m| m.into_iter().collect()).unwrap_or_default(),
            group: None,
            // flag_values reference build-setting values razel doesn't model — CONSERVATIVE:
            // the condition never matches (CPU-host posture; registered debt).
            unmodeled: flag_values.is_some_and(|v| !v.is_none()),
        };
        sess.config_specs.borrow_mut().insert(canon_label(sess, &name), spec);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn test_suite<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn alias<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] actual: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        // Record the actual (selects resolve at consumption) — conditions/`deps` follow aliases.
        let actual = match actual {
            Some(v) => {
                let r = crate::dialect::resolve_attr_value(eval, v)?;
                r.unpack_str().map(String::from)
            }
            None => None,
        };
        let sess = session(eval);
        if let Some(a) = actual {
            let canon_actual = canon_label(sess, &a);
            sess.aliases.borrow_mut().insert(canon_label(sess, &name), canon_actual);
        }
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            ..Default::default()
        });
        Ok(NoneType)
    }
    fn filegroup<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] srcs: Option<UnpackList<Value<'v>>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let sess = session(eval);
        let files: Vec<String> = crate::values::unpack_strs(srcs).iter().map(|s| qualify(sess, s)).collect();
        // C3a.5: filegroup provides DefaultInfo only — it no longer fakes CcInfo to push files into
        // the cc header channel (that hack was untested; a real cc-on-filegroup case = cc reading dep
        // DefaultInfo files as inputs, Phase D).
        record_target(sess, AnalyzedTarget {
            name: canon_label(sess, &name),
            default_info: files,
            ..Default::default()
        });
        Ok(NoneType)
    }

    /// `DefaultInfo(files=…)` — the standard output provider. `files` may be a list
    /// (of Files/strings) or a `depset`; other kwargs absorbed.
    fn DefaultInfo<'v>(
        #[starlark(require = named)] files: Option<Value<'v>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        if let Some(f) = files {
            let paths = extract_files(f);
            with_current(session(eval), |c| c.default_info = paths);
        }
        Ok(NoneType)
    }

    // C3 (`razel_build.info`): the cc/java provider-capture builtins are gone — razel's `.bzl` rules
    // capture providers through the ONE generic `razel_build.info(provider, fields)` constructor
    // (engine.rs), schema-driven by the registry. No language-named capture builtin in the rule API.

    /// `dedup(list)` — the list with duplicate strings removed, **preserving first occurrence**. The
    /// cross-sibling dedup the rule()-path classpath/header assembly needs: a target's transitive
    /// closure folds *per-dep*, so a diamond (`app -> [x,y] -> base`) would otherwise list base's
    /// jar/header twice. `dedup()` makes the merge the engine's job, not silent duplication (F1).
    fn dedup(#[starlark(require = pos)] list: UnpackList<String>) -> anyhow::Result<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for s in list.items {
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// `depset(direct=[], transitive=[depsets], order=…)` — Bazel's transitive set.
    /// razel folds member paths from `direct` + each `transitive` depset, deduped.
    fn depset<'v>(
        direct: Option<Value<'v>>,
        #[starlark(require = named)] transitive: Option<Value<'v>>,
        #[starlark(require = named)] order: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // razel does not model depset traversal order yet (the 4-order family is reserved). WARN
        // loudly on a non-default order rather than silently produce a possibly-wrong sequence (F36).
        if let Some(o) = &order
            && o != "default"
        {
            eprintln!(
                "razel: warning: depset(order={o:?}) — traversal order not yet modeled, treating as \
                 default (F36; RazelGaps)"
            );
        }
        // Dedup by string path; store the live Value so map_each sees File attributes.
        let mut seen: Vec<String> = Vec::new();
        let mut items: Vec<Value<'v>> = Vec::new();
        let push = |v: Value<'v>, seen: &mut Vec<String>, items: &mut Vec<Value<'v>>| {
            let key = file_path(v);
            if !seen.contains(&key) {
                seen.push(key);
                items.push(v);
            }
        };
        if let Some(d) = direct
            && let Some(list) = ListRef::from_value(d)
        {
            for it in list.iter() {
                push(it, &mut seen, &mut items);
            }
        }
        if let Some(t) = transitive
            && let Some(list) = ListRef::from_value(t)
        {
            for dep in list.iter() {
                if let Some(ds) = dep.downcast_ref::<Depset>() {
                    for v in &ds.items {
                        push(*v, &mut seen, &mut items);
                    }
                }
            }
        }
        Ok(eval.heap().alloc(Depset { items }))
    }

    /// `select({condition: value, …})` — Bazel semantics (razelV3): HYBRID resolution. If every
    /// condition is a declared `config_setting` right now (loading its package on demand), the
    /// branch resolves eagerly (most-specialized wins; `//conditions:default` fallback; loud
    /// no-match/ambiguity errors). If any condition is not yet declared (real `.bzl` build
    /// module-level selects — XLA's tsl.bzl), select returns a DEFERRED value, resolved when an
    /// attr consumes it at analysis (by which time the conditions exist — the E0 split).
    /// Keys may be strings or `Label`s; `select + list` concatenation is supported (SelectExpr).
    fn select<'v>(
        branches: Value<'v>,
        #[starlark(require = named)] #[allow(unused_variables)] no_match_error: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let Some(d) = starlark::values::dict::DictRef::from_value(branches) else {
            return Err(anyhow::anyhow!("select() takes a dict of conditions"));
        };
        // Bazel binds select KEYS at the call site: a string key written in `@repo` code is a
        // label in THAT repo. Capture the repo now (the context is gone by analysis time).
        let bzl_repo = session(eval)
            .current_bzl_repo
            .borrow()
            .last()
            .cloned()
            .flatten()
            .filter(|(r, _)| !r.is_empty())
            .map(|(r, _)| r);
        let pairs: Vec<(Value<'v>, Value<'v>)> = d
            .iter()
            .map(|(k, v)| {
                let k = match (k.unpack_str(), &bzl_repo) {
                    (Some(ks), Some(repo))
                        if ks.starts_with("//") && ks != "//conditions:default" =>
                    {
                        let rest = ks.trim_start_matches("//");
                        let (pkg, name) = rest.split_once(':').unwrap_or((rest, rest));
                        eval.heap().alloc(LabelV {
                            repo: Some(format!("@{repo}")),
                            package: pkg.to_string(),
                            name: name.to_string(),
                        })
                    }
                    _ => k,
                };
                (k, v)
            })
            .collect();
        drop(d);
        // Bazel: select never resolves at LOAD time. The eager path consults only specs already
        // declared (no package loading — that recursed into mid-load packages); everything else
        // defers to attr consumption at analysis.
        match pick_branch(eval, &pairs, true, false)? {
            Some(v) => Ok(v),
            // Defer: a condition isn't declared yet — resolve at attr consumption (analysis).
            None => Ok(eval.heap().alloc(SelectBranches { branches: pairs })),
        }
    }

    /// `define_config(name, compile, archive=None, link=None)` — declare + register a
    /// toolchain transform (D7). Returns a struct of the transform fns (so a rule can call
    /// `cfg.compile(req)`); also records the name engine-side for host-config selection.
    fn define_config<'v>(
        #[starlark(require = named)] name: String,
        #[starlark(require = named)] compile: Value<'v>,
        #[starlark(require = named)] archive: Option<Value<'v>>,
        #[starlark(require = named)] link: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        session(eval).configs.borrow_mut().push(name);
        let mut fields: Vec<(String, Value<'v>)> = vec![("compile".to_string(), compile)];
        if let Some(a) = archive {
            fields.push(("archive".to_string(), a));
        }
        if let Some(l) = link {
            fields.push(("link".to_string(), l));
        }
        Ok(eval.heap().alloc(AllocStruct(fields)))
    }

    /// `glob(include, exclude=[])` — match the current package's files (workspace
    /// mode) against the patterns, returning package-relative paths. Requires a
    /// package on disk; errors in single-package (no-dir) mode.
    fn glob<'v>(
        include: Option<UnpackList<String>>,
        #[starlark(require = named)] exclude: Option<UnpackList<String>>,
        #[starlark(kwargs)] _kw: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Vec<String>> {
        do_glob(session(eval), include.map(|l| l.items).unwrap_or_default(), exclude.map(|l| l.items).unwrap_or_default())
    }
}
