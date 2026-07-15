//! IO-free project model shared by compiler hosts and analysis loaders.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use rua_core::{FileId, ProjectId, SourceRootId};

pub const PROJECT_CONFIG_FILE: &str = ".ruarc.toml";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub workspace: WorkspaceConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub library: Vec<String>,
    pub library_mounts: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub std_path: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedProjectConfig {
    pub library: Vec<PathBuf>,
    pub library_mounts: BTreeMap<String, PathBuf>,
    pub std_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfigError(String);

impl fmt::Display for ProjectConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProjectConfigError {}

pub fn parse_project_config(source: &str) -> Result<ProjectConfig, ProjectConfigError> {
    toml::from_str(source).map_err(|error| ProjectConfigError(error.to_string()))
}

impl ProjectConfig {
    pub fn resolve(
        &self,
        workspace_root: &Path,
    ) -> Result<ResolvedProjectConfig, ProjectConfigError> {
        let mut library_mounts = BTreeMap::new();
        for (name, configured) in &self.workspace.library_mounts {
            validate_module_name(name)?;
            library_mounts.insert(
                name.clone(),
                resolve_config_path(workspace_root, configured),
            );
        }
        Ok(ResolvedProjectConfig {
            library: self
                .workspace
                .library
                .iter()
                .map(|path| resolve_config_path(workspace_root, path))
                .collect(),
            library_mounts,
            std_path: self
                .runtime
                .std_path
                .as_deref()
                .map(|path| resolve_config_path(workspace_root, path)),
        })
    }
}

fn resolve_config_path(workspace_root: &Path, configured: &str) -> PathBuf {
    let workspace = workspace_root.to_string_lossy();
    let expanded = configured
        .replace("${workspaceFolder}", &workspace)
        .replace("{workspaceFolder}", &workspace);
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn validate_module_name(name: &str) -> Result<(), ProjectConfigError> {
    let mut chars = name.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic());
    if valid_start && chars.all(|character| character == '_' || character.is_ascii_alphanumeric()) {
        Ok(())
    } else {
        Err(ProjectConfigError(format!(
            "invalid library mount module name `{name}`"
        )))
    }
}

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

    #[test]
    fn project_config_uses_snake_case_and_resolves_workspace_paths() {
        let config = parse_project_config(
            r#"
            [workspace]
            library = ["types", "${workspaceFolder}/vendor/api.ruai"]
            [workspace.library_mounts]
            host = "../host/host.ruai"

            [runtime]
            std_path = "std"
            "#,
        )
        .unwrap();
        let resolved = config.resolve(Path::new("/workspace/project")).unwrap();
        assert_eq!(
            resolved.library,
            [
                PathBuf::from("/workspace/project/types"),
                PathBuf::from("/workspace/project/vendor/api.ruai"),
            ]
        );
        assert_eq!(
            resolved.library_mounts["host"],
            PathBuf::from("/workspace/project/../host/host.ruai")
        );
        assert_eq!(
            resolved.std_path,
            Some(PathBuf::from("/workspace/project/std"))
        );
    }

    #[test]
    fn project_config_rejects_unknown_and_camel_case_settings() {
        assert!(parse_project_config("[workspace]\nlibrary_mounts = []").is_err());
        assert!(parse_project_config("[workspace]\nworkspace_roots = []").is_err());
        assert!(parse_project_config("unknown = true").is_err());
    }
}
