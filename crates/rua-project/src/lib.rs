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
    pub lua_library: Vec<LuaLibraryConfig>,
}

/// One convention-mapped Lua library. Every `.ruai` below the declaration
/// root maps to the Lua module with the same relative path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LuaLibraryConfig {
    /// Shorthand for colocated declarations and Lua runtime modules.
    pub root: Option<String>,
    /// Root recursively indexed for `.ruai` declarations.
    pub declaration_root: Option<String>,
    /// Root prepended to Lua `package.path` at code generation time.
    pub runtime_root: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub std_path: Option<String>,
    pub lua_path: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedProjectConfig {
    pub library: Vec<PathBuf>,
    pub library_mounts: BTreeMap<String, PathBuf>,
    pub lua_library: Vec<ResolvedLuaLibrary>,
    pub std_path: Option<PathBuf>,
    pub lua_path: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLuaLibrary {
    pub declaration_root: PathBuf,
    pub runtime_root: PathBuf,
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
        let lua_library = self
            .workspace
            .lua_library
            .iter()
            .map(|library| library.resolve(workspace_root))
            .collect::<Result<Vec<_>, _>>()?;
        let mut library = self
            .workspace
            .library
            .iter()
            .map(|path| resolve_config_path(workspace_root, path))
            .collect::<Vec<_>>();
        let mut lua_path = self
            .runtime
            .lua_path
            .iter()
            .map(|path| resolve_config_path(workspace_root, path))
            .collect::<Vec<_>>();
        for configured in &lua_library {
            push_unique(&mut library, configured.declaration_root.clone());
            push_unique(&mut lua_path, configured.runtime_root.clone());
        }
        Ok(ResolvedProjectConfig {
            library,
            library_mounts,
            lua_library,
            std_path: self
                .runtime
                .std_path
                .as_deref()
                .map(|path| resolve_config_path(workspace_root, path)),
            lua_path,
        })
    }
}

impl LuaLibraryConfig {
    fn resolve(&self, workspace_root: &Path) -> Result<ResolvedLuaLibrary, ProjectConfigError> {
        let (declaration_root, runtime_root) = match (
            self.root.as_deref(),
            self.declaration_root.as_deref(),
            self.runtime_root.as_deref(),
        ) {
            (Some(root), None, None) => (root, root),
            (None, Some(declaration_root), Some(runtime_root)) => (declaration_root, runtime_root),
            (Some(_), _, _) => {
                return Err(ProjectConfigError(
                    "workspace.lua_library.root cannot be combined with declaration_root or runtime_root"
                        .to_string(),
                ));
            }
            (None, Some(_), None) | (None, None, Some(_)) => {
                return Err(ProjectConfigError(
                    "workspace.lua_library requires both declaration_root and runtime_root"
                        .to_string(),
                ));
            }
            (None, None, None) => {
                return Err(ProjectConfigError(
                    "workspace.lua_library requires root or declaration_root/runtime_root"
                        .to_string(),
                ));
            }
        };
        Ok(ResolvedLuaLibrary {
            declaration_root: resolve_config_path(workspace_root, declaration_root),
            runtime_root: resolve_config_path(workspace_root, runtime_root),
        })
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidModuleFilePath(String);

impl fmt::Display for InvalidModuleFilePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidModuleFilePath {}

/// Map a source file relative to one source root to its module path.
///
/// `domain/product.rua` and `domain/product/mod.rua` both map to
/// `domain::product`. Non-Rua files return `Ok(None)`. A root-level `mod.rua`
/// maps to an empty path so the caller can diagnose a second root module.
pub fn module_path_from_relative_file(
    relative: &Path,
) -> Result<Option<Vec<String>>, InvalidModuleFilePath> {
    let extension = relative
        .extension()
        .and_then(|extension| extension.to_str());
    if !matches!(extension, Some("rua" | "ruai")) {
        return Ok(None);
    }
    if relative.is_absolute() {
        return Err(InvalidModuleFilePath(format!(
            "module source path must be relative: {}",
            relative.display()
        )));
    }

    let stem = relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            InvalidModuleFilePath(format!(
                "module source filename is not valid UTF-8: {}",
                relative.display()
            ))
        })?;
    let mut segments = Vec::new();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            let std::path::Component::Normal(segment) = component else {
                return Err(InvalidModuleFilePath(format!(
                    "module source path is not normalized: {}",
                    relative.display()
                )));
            };
            let segment = segment.to_str().ok_or_else(|| {
                InvalidModuleFilePath(format!(
                    "module path segment is not valid UTF-8: {}",
                    relative.display()
                ))
            })?;
            validate_module_name(segment).map_err(|_| {
                InvalidModuleFilePath(format!(
                    "invalid module path segment `{segment}` in {}",
                    relative.display()
                ))
            })?;
            segments.push(segment.to_string());
        }
    }
    if stem != "mod" {
        validate_module_name(stem).map_err(|_| {
            InvalidModuleFilePath(format!(
                "invalid module filename `{stem}` in {}",
                relative.display()
            ))
        })?;
        segments.push(stem.to_string());
    }
    Ok(Some(segments))
}

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

/// Physical layouts that map to one logical child module.
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
    fn source_file_paths_map_to_modules_without_mod_declarations() {
        assert_eq!(
            module_path_from_relative_file(Path::new("domain/product.rua")).unwrap(),
            Some(vec!["domain".to_string(), "product".to_string()])
        );
        assert_eq!(
            module_path_from_relative_file(Path::new("domain/product/mod.rua")).unwrap(),
            Some(vec!["domain".to_string(), "product".to_string()])
        );
        assert_eq!(
            module_path_from_relative_file(Path::new("moon/http/client.ruai")).unwrap(),
            Some(vec![
                "moon".to_string(),
                "http".to_string(),
                "client".to_string(),
            ])
        );
        assert_eq!(
            module_path_from_relative_file(Path::new("notes.md")).unwrap(),
            None
        );
        assert_eq!(
            module_path_from_relative_file(Path::new("mod.rua")).unwrap(),
            Some(Vec::new())
        );
        assert!(module_path_from_relative_file(Path::new("bad-name.rua")).is_err());
        assert!(module_path_from_relative_file(Path::new("../escape.rua")).is_err());
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
            lua_path = ["dist", "${workspaceFolder}/vendor/lua"]
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
        assert_eq!(
            resolved.lua_path,
            [
                PathBuf::from("/workspace/project/dist"),
                PathBuf::from("/workspace/project/vendor/lua"),
            ]
        );
    }

    #[test]
    fn project_config_expands_lua_library_roots_into_declarations_and_runtime_paths() {
        let config = parse_project_config(
            r#"
            [workspace]
            library = ["legacy-types"]

            [[workspace.lua_library]]
            root = "vendor/colocated"

            [[workspace.lua_library]]
            declaration_root = "vendor/types"
            runtime_root = "vendor/lua"

            [runtime]
            lua_path = ["generated"]
            "#,
        )
        .unwrap();
        let resolved = config.resolve(Path::new("/workspace/project")).unwrap();
        assert_eq!(
            resolved.lua_library,
            [
                ResolvedLuaLibrary {
                    declaration_root: PathBuf::from("/workspace/project/vendor/colocated"),
                    runtime_root: PathBuf::from("/workspace/project/vendor/colocated"),
                },
                ResolvedLuaLibrary {
                    declaration_root: PathBuf::from("/workspace/project/vendor/types"),
                    runtime_root: PathBuf::from("/workspace/project/vendor/lua"),
                },
            ]
        );
        assert_eq!(
            resolved.library,
            [
                PathBuf::from("/workspace/project/legacy-types"),
                PathBuf::from("/workspace/project/vendor/colocated"),
                PathBuf::from("/workspace/project/vendor/types"),
            ]
        );
        assert_eq!(
            resolved.lua_path,
            [
                PathBuf::from("/workspace/project/generated"),
                PathBuf::from("/workspace/project/vendor/colocated"),
                PathBuf::from("/workspace/project/vendor/lua"),
            ]
        );
    }

    #[test]
    fn project_config_rejects_unknown_and_camel_case_settings() {
        assert!(parse_project_config("[workspace]\nlibrary_mounts = []").is_err());
        assert!(parse_project_config("[workspace]\nworkspace_roots = []").is_err());
        assert!(parse_project_config("[runtime]\nluaPath = []").is_err());
        assert!(
            parse_project_config(
                "[[workspace.lua_library]]\nroot = 'lib'\nruntime_root = 'runtime'"
            )
            .unwrap()
            .resolve(Path::new("/workspace"))
            .is_err()
        );
        assert!(
            parse_project_config("[[workspace.lua_library]]\ndeclaration_root = 'types'")
                .unwrap()
                .resolve(Path::new("/workspace"))
                .is_err()
        );
        assert!(parse_project_config("unknown = true").is_err());
    }
}
