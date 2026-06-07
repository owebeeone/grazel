// GENERATED native Rust types + codec — do not edit.
#![allow(dead_code)]
use crate::cbor::Cbor;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum TargetKind {
    #[default]
    Library,
    Binary,
    Test,
}
impl TargetKind {
    pub fn wire(self) -> i64 {
        match self {
            Self::Library => 0,
            Self::Binary => 1,
            Self::Test => 2,
        }
    }
    pub fn from_wire(v: i64) -> Self {
        match v {
            0 => Self::Library,
            1 => Self::Binary,
            2 => Self::Test,
            _ => panic!("bad TargetKind wire value {}", v),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum BuildStatus {
    #[default]
    Cached,
    Built,
    Failed,
}
impl BuildStatus {
    pub fn wire(self) -> i64 {
        match self {
            Self::Cached => 0,
            Self::Built => 1,
            Self::Failed => 2,
        }
    }
    pub fn from_wire(v: i64) -> Self {
        match v {
            0 => Self::Cached,
            1 => Self::Built,
            2 => Self::Failed,
            _ => panic!("bad BuildStatus wire value {}", v),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct OutputArtifact {
    pub path: String,
    pub digest: Vec<u8>,
}
impl OutputArtifact {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Text(self.path.clone())),
            (2, Cbor::Bytes(self.digest.clone())),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            path: c.get(1).text(),
            digest: c.get(2).bytes(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TargetRef {
    pub label: String,
    pub kind: TargetKind,
}
impl TargetRef {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Text(self.label.clone())),
            (2, Cbor::Int(self.kind.wire())),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            label: c.get(1).text(),
            kind: TargetKind::from_wire(c.get(2).int()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct BuildResult {
    pub target: String,
    pub status: BuildStatus,
    pub recomputes: i64,
    pub outputs: Vec<OutputArtifact>,
    pub message: Option<String>,
}
impl BuildResult {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Text(self.target.clone())),
            (2, Cbor::Int(self.status.wire())),
            (3, Cbor::Int(self.recomputes)),
            (
                4,
                Cbor::Array(self.outputs.iter().map(|x| x.to_cbor()).collect()),
            ),
            (
                5,
                match &self.message {
                    Some(v) => Cbor::Text(v.clone()),
                    None => Cbor::Null,
                },
            ),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            target: c.get(1).text(),
            status: BuildStatus::from_wire(c.get(2).int()),
            recomputes: c.get(3).int(),
            outputs: c
                .get(4)
                .array()
                .iter()
                .map(|x| OutputArtifact::from_cbor(x))
                .collect(),
            message: {
                let v = c.get(5);
                if v.is_null() { None } else { Some(v.text()) }
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SyncAck {
    pub revision: i64,
}
impl SyncAck {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![(1, Cbor::Int(self.revision))])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            revision: c.get(1).int(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct VersionInfo {
    pub version: String,
    pub protocol: i64,
}
impl VersionInfo {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Text(self.version.clone())),
            (2, Cbor::Int(self.protocol)),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            version: c.get(1).text(),
            protocol: c.get(2).int(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ImpactSet {
    pub sources: Vec<String>,
    pub targets: Vec<TargetRef>,
    pub tests: Vec<TargetRef>,
}
impl ImpactSet {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (
                1,
                Cbor::Array(self.sources.iter().map(|x| Cbor::Text(x.clone())).collect()),
            ),
            (
                2,
                Cbor::Array(self.targets.iter().map(|x| x.to_cbor()).collect()),
            ),
            (
                3,
                Cbor::Array(self.tests.iter().map(|x| x.to_cbor()).collect()),
            ),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            sources: c.get(1).array().iter().map(|x| x.text()).collect(),
            targets: c
                .get(2)
                .array()
                .iter()
                .map(|x| TargetRef::from_cbor(x))
                .collect(),
            tests: c
                .get(3)
                .array()
                .iter()
                .map(|x| TargetRef::from_cbor(x))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct TargetStatus {
    pub label: String,
    pub kind: TargetKind,
    pub status: BuildStatus,
    pub output_digest: Vec<u8>,
}
impl TargetStatus {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Text(self.label.clone())),
            (2, Cbor::Int(self.kind.wire())),
            (3, Cbor::Int(self.status.wire())),
            (4, Cbor::Bytes(self.output_digest.clone())),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            label: c.get(1).text(),
            kind: TargetKind::from_wire(c.get(2).int()),
            status: BuildStatus::from_wire(c.get(3).int()),
            output_digest: c.get(4).bytes(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct BuildState {
    pub revision: i64,
    pub targets: Vec<TargetStatus>,
}
impl BuildState {
    pub fn to_cbor(&self) -> Cbor {
        Cbor::Map(vec![
            (1, Cbor::Int(self.revision)),
            (
                2,
                Cbor::Array(self.targets.iter().map(|x| x.to_cbor()).collect()),
            ),
        ])
    }
    pub fn from_cbor(c: &Cbor) -> Self {
        Self {
            revision: c.get(1).int(),
            targets: c
                .get(2)
                .array()
                .iter()
                .map(|x| TargetStatus::from_cbor(x))
                .collect(),
        }
    }
}
