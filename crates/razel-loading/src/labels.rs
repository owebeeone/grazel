//! The Bazel `Label` value (canonical str(), hashable, repo-aware).

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::values::{
    Heap, NoSerialize, StarlarkValue, Value, ValueLike,
    starlark_value,
};
use std::fmt;


// ---- Label ------------------------------------------------------------------------------------


/// A Bazel `Label` value: `.package`/`.name` fields and — critically — `str(label)` is the
/// canonical `//pkg:name` form (real macros do `str(Label(x))`; a struct repr breaks them).
#[derive(Debug, ProvidesStaticType, NoSerialize, Allocative)]
pub(crate) struct LabelV {
    /// `Some("@xla")` for an external-repo label; `None` = the main workspace.
    pub(crate) repo: Option<String>,
    pub(crate) package: String,
    pub(crate) name: String,
}


starlark::starlark_simple_value!(LabelV);


impl fmt::Display for LabelV {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.repo {
            Some(r) => write!(f, "{r}//{}:{}", self.package, self.name),
            None => write!(f, "//{}:{}", self.package, self.name),
        }
    }
}


#[starlark_value(type = "Label")]
impl<'v> StarlarkValue<'v> for LabelV {
    /// Labels are dict keys in real `.bzl` (`select({Label(...): …})`).
    fn write_hash(&self, hasher: &mut starlark::collections::StarlarkHasher) -> starlark::Result<()> {
        use std::hash::Hash;
        (&self.repo, &self.package, &self.name).hash(hasher);
        Ok(())
    }
    fn equals(&self, other: Value<'v>) -> starlark::Result<bool> {
        Ok(other
            .downcast_ref::<LabelV>()
            .is_some_and(|o| o.repo == self.repo && o.package == self.package && o.name == self.name))
    }
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        match attribute {
            "package" => Some(heap.alloc(self.package.as_str())),
            "name" => Some(heap.alloc(self.name.as_str())),
            "workspace_root" | "workspace_name" => Some(heap.alloc("")),
            _ => None,
        }
    }
    fn get_methods() -> Option<&'static Methods> {
        Some(LABEL_METHODS.methods())
    }
}

starlark::methods_static!(LABEL_METHODS = label_methods);

/// Parse `pkg:name` (or the `pkg` shorthand whose name is the last segment).
fn split_pkg_name(rest: &str) -> (String, String) {
    match rest.split_once(':') {
        Some((p, n)) => (p.to_string(), n.to_string()),
        None => (rest.to_string(), rest.rsplit('/').next().unwrap_or(rest).to_string()),
    }
}

#[starlark::starlark_module]
fn label_methods(b: &mut MethodsBuilder) {
    /// `label.relative(rel)` — resolve a label string relative to this label's repo/package.
    fn relative<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] rel: String,
        eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let me = this.downcast_ref::<LabelV>().unwrap();
        let out = if let Some(rest) = rel.strip_prefix("//") {
            let (package, name) = split_pkg_name(rest);
            LabelV { repo: me.repo.clone(), package, name }
        } else if rel.starts_with('@') {
            match rel.split_once("//") {
                Some((r, rest)) => {
                    let (package, name) = split_pkg_name(rest);
                    LabelV { repo: Some(r.to_string()), package, name }
                }
                // `@repo` shorthand = `@repo//:repo`.
                None => LabelV {
                    repo: Some(rel.clone()),
                    package: String::new(),
                    name: rel.trim_start_matches('@').to_string(),
                },
            }
        } else {
            // `:x` or bare `x` — same package.
            LabelV {
                repo: me.repo.clone(),
                package: me.package.clone(),
                name: rel.trim_start_matches(':').to_string(),
            }
        };
        Ok(eval.heap().alloc(out))
    }
    /// `label.same_package_label(name)` — the modern same-package variant.
    fn same_package_label<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = pos)] name: String,
        eval: &mut starlark::eval::Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let me = this.downcast_ref::<LabelV>().unwrap();
        Ok(eval.heap().alloc(LabelV {
            repo: me.repo.clone(),
            package: me.package.clone(),
            name: name.trim_start_matches(':').to_string(),
        }))
    }
}
