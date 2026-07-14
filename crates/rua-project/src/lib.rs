//! IO-free project model shared by compiler hosts and analysis loaders.

use std::collections::BTreeMap;
use std::fmt;

pub use rua_core::{FileId, ProjectId, SourceRootId};

/// Normalized logical path. It always uses `/` and cannot escape its root.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogicalSourcePath(String);

impl LogicalSourcePath {
    pub fn new(path: impl AsRef<str>) -> Result<Self, InvalidLogicalPath> {
        let mut parts = Vec::new();
        let normalized = path.as_ref().replace('\\', "/");
        for part in normalized.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(InvalidLogicalPath);
                    }
                }
                value => parts.push(value),
            }
        }
        Ok(Self(parts.join("/")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parent(&self) -> Option<Self> {
        let (parent, _) = self.0.rsplit_once('/')?;
        Some(Self(parent.to_string()))
    }

    pub fn join(&self, child: impl AsRef<str>) -> Result<Self, InvalidLogicalPath> {
        Self::new(format!("{}/{}", self.0, child.as_ref()))
    }
}

impl fmt::Display for LogicalSourcePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidLogicalPath;

impl fmt::Display for InvalidLogicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("logical path escapes its source root")
    }
}

impl std::error::Error for InvalidLogicalPath {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceRootKind {
    Workspace,
    Library,
    Std,
    Virtual,
}

impl SourceRootKind {
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::Library | Self::Std)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRootSpec {
    pub id: SourceRootId,
    pub kind: SourceRootKind,
    pub logical_base: LogicalSourcePath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryMount {
    pub name: String,
    pub root: SourceRootId,
    pub logical_base: LogicalSourcePath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSpec {
    pub id: ProjectId,
    pub root_file: FileId,
    pub roots: Vec<SourceRootSpec>,
    pub libraries: Vec<LibraryMount>,
    pub files: BTreeMap<LogicalSourcePath, FileId>,
}

impl ProjectSpec {
    pub fn file_for_path(&self, path: &LogicalSourcePath) -> Option<FileId> {
        self.files.get(path).copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceText {
    pub text: String,
}

pub trait SourceProvider {
    type Error: std::error::Error + Send + Sync + 'static;

    fn load(&self, file: FileId) -> Result<SourceText, Self::Error>;

    fn file_for_path(&self, path: &LogicalSourcePath) -> Result<Option<FileId>, Self::Error>;
}

/// Candidate paths for `mod name;`, in deterministic priority order.
pub fn module_candidates(
    from: &LogicalSourcePath,
    name: &str,
) -> Result<[LogicalSourcePath; 4], InvalidLogicalPath> {
    let base = from.parent().unwrap_or_default();
    Ok([
        base.join(format!("{name}.rua"))?,
        base.join(format!("{name}/mod.rua"))?,
        base.join(format!("{name}.ruai"))?,
        base.join(format!("{name}/mod.ruai"))?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_paths_normalize_without_escaping() {
        assert_eq!(
            LogicalSourcePath::new("src/./api/../main.rua")
                .unwrap()
                .as_str(),
            "src/main.rua"
        );
        assert!(LogicalSourcePath::new("../outside.rua").is_err());
    }

    #[test]
    fn module_candidate_order_is_stable() {
        let candidates =
            module_candidates(&LogicalSourcePath::new("src/main.rua").unwrap(), "math").unwrap();
        assert_eq!(candidates[0].as_str(), "src/math.rua");
        assert_eq!(candidates[3].as_str(), "src/math/mod.ruai");
    }
}
