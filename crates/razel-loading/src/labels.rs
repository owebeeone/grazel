//! The Bazel `Label` value (canonical str(), hashable, repo-aware).

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
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
}
