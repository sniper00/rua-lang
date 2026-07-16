//! Versioned standard-library resources shared by the compiler and IDE.
//!
//! Both embedded resources and an explicitly configured directory are loaded
//! through the same `std.toml` schema. This crate owns bytes and validation;
//! parsers and semantic identities remain the responsibility of consumers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

use include_dir::{Dir, include_dir};
use serde::Deserialize;

static EMBEDDED_RESOURCES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/resources/std");
static EMBEDDED_STD: OnceLock<Result<StdLibrary, StdError>> = OnceLock::new();
static MATERIALIZED_STD: OnceLock<Result<PathBuf, StdError>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StdManifest {
    pub version: u32,
    pub declarations: Vec<String>,
    pub runtime_sources: Vec<String>,
    #[serde(default)]
    pub modules: Vec<StdModule>,
    #[serde(default)]
    pub runtime_helpers: BTreeMap<String, StdRuntime>,
    #[serde(default)]
    pub lang_items: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StdModule {
    pub name: String,
    pub declaration: String,
    pub runtime: String,
    #[serde(default)]
    pub export: Option<String>,
    /// Preferred Lua local for the exported runtime table. Codegen keeps this
    /// readable when possible and disambiguates it from user bindings.
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub abi: Option<u32>,
    #[serde(default)]
    pub dispatch: StdDispatch,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StdRuntime {
    pub module: String,
    #[serde(default)]
    pub export: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub abi: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StdDispatch {
    /// Associated functions are exported by the configured runtime table;
    /// instance methods use the value's metatable.
    #[default]
    Method,
    /// Both associated functions and instance methods are plain runtime-module
    /// functions. The receiver is passed as the first argument.
    Module,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdSource {
    path: String,
    text: Arc<str>,
}

impl StdSource {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdLibrary {
    manifest: StdManifest,
    declarations: Vec<StdSource>,
    runtime_sources: Vec<StdSource>,
}

impl StdLibrary {
    pub const fn manifest(&self) -> &StdManifest {
        &self.manifest
    }

    pub fn declarations(&self) -> &[StdSource] {
        &self.declarations
    }

    pub fn runtime_sources(&self) -> &[StdSource] {
        &self.runtime_sources
    }

    pub fn declaration(&self, path: &str) -> Option<&StdSource> {
        self.declarations.iter().find(|source| source.path == path)
    }

    pub fn declaration_by_name(&self, name: &str) -> Option<&StdSource> {
        self.declarations
            .iter()
            .find(|source| source.name() == name)
    }

    pub fn lang_item(&self, name: &str) -> Option<&str> {
        self.manifest.lang_items.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdError {
    message: String,
}

impl StdError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StdError {}

pub fn embedded_std() -> Result<&'static StdLibrary, &'static StdError> {
    EMBEDDED_STD.get_or_init(load_embedded_std).as_ref()
}

pub fn load_std_dir(root: &Path) -> Result<StdLibrary, StdError> {
    let manifest_path = root.join("std.toml");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|error| StdError::new(format!("reading {}: {error}", manifest_path.display())))?;
    build_library(&manifest_text, |path| {
        let full_path = root.join(path);
        fs::read_to_string(&full_path)
            .map(Arc::<str>::from)
            .map_err(|error| StdError::new(format!("reading {}: {error}", full_path.display())))
    })
}

pub fn materialized_embedded_std() -> Result<&'static Path, &'static StdError> {
    MATERIALIZED_STD
        .get_or_init(|| {
            let library = embedded_std().map_err(Clone::clone)?;
            let root = std::env::temp_dir()
                .join("rua-std")
                .join(format!("v{}", library.manifest().version));
            fs::create_dir_all(&root)
                .map_err(|error| StdError::new(format!("creating {}: {error}", root.display())))?;
            write_if_changed(&root.join("std.toml"), &embedded_text("std.toml")?)?;
            for source in library.declarations() {
                write_if_changed(&root.join(source.path()), &source.text)?;
            }
            for source in library.runtime_sources() {
                write_if_changed(&root.join(source.path()), &source.text)?;
            }
            Ok(root)
        })
        .as_ref()
        .map(PathBuf::as_path)
}

fn write_if_changed(path: &Path, text: &str) -> Result<(), StdError> {
    if fs::read_to_string(path).is_ok_and(|current| current == text) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| StdError::new(format!("creating {}: {error}", parent.display())))?;
    }
    fs::write(path, text)
        .map_err(|error| StdError::new(format!("writing {}: {error}", path.display())))
}

fn load_embedded_std() -> Result<StdLibrary, StdError> {
    let manifest_text = embedded_text("std.toml")?;
    build_library(&manifest_text, embedded_text)
}

fn embedded_text(path: &str) -> Result<Arc<str>, StdError> {
    let file = EMBEDDED_RESOURCES
        .get_file(path)
        .ok_or_else(|| StdError::new(format!("embedded standard resource `{path}` is missing")))?;
    let text = file
        .contents_utf8()
        .ok_or_else(|| StdError::new(format!("standard resource `{path}` is not UTF-8")))?;
    Ok(Arc::from(text))
}

fn build_library(
    manifest_text: &str,
    mut read: impl FnMut(&str) -> Result<Arc<str>, StdError>,
) -> Result<StdLibrary, StdError> {
    let mut manifest: StdManifest = toml::from_str(manifest_text)
        .map_err(|error| StdError::new(format!("parsing std.toml: {error}")))?;
    if manifest.version != 1 {
        return Err(StdError::new(format!(
            "unsupported std.toml version {}; expected 1",
            manifest.version
        )));
    }
    if manifest.declarations.is_empty() {
        return Err(StdError::new("std.toml declares no declaration files"));
    }

    let mut seen = BTreeSet::new();
    let mut declarations = Vec::with_capacity(manifest.declarations.len());
    for configured_path in &manifest.declarations {
        let path = normalize_relative_path(configured_path)?;
        if !path.ends_with(".ruai") {
            return Err(StdError::new(format!(
                "standard declaration `{path}` must use the .ruai extension"
            )));
        }
        if !seen.insert(path.clone()) {
            return Err(StdError::new(format!(
                "standard declaration `{path}` is listed more than once"
            )));
        }
        declarations.push(StdSource {
            text: read(&path)?,
            path,
        });
    }
    manifest.declarations = declarations
        .iter()
        .map(|source| source.path.clone())
        .collect();

    if manifest.runtime_sources.is_empty() {
        return Err(StdError::new("std.toml declares no Lua runtime sources"));
    }
    let mut runtime_paths = BTreeSet::new();
    let mut runtime_sources = Vec::with_capacity(manifest.runtime_sources.len());
    for configured_path in &manifest.runtime_sources {
        let path = normalize_relative_path(configured_path)?;
        if !path.ends_with(".lua") {
            return Err(StdError::new(format!(
                "standard runtime source `{path}` must use the .lua extension"
            )));
        }
        if !runtime_paths.insert(path.clone()) {
            return Err(StdError::new(format!(
                "standard runtime source `{path}` is listed more than once"
            )));
        }
        runtime_sources.push(StdSource {
            text: read(&path)?,
            path,
        });
    }
    manifest.runtime_sources = runtime_sources
        .iter()
        .map(|source| source.path.clone())
        .collect();

    let declaration_paths = declarations
        .iter()
        .map(|source| source.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut module_names = BTreeSet::new();
    let mut module_declarations = BTreeSet::new();
    for module in &mut manifest.modules {
        let declaration = normalize_relative_path(&module.declaration)?;
        module.declaration.clone_from(&declaration);
        if !declaration_paths.contains(declaration.as_str()) {
            return Err(StdError::new(format!(
                "module `{}` references undeclared source `{declaration}`",
                module.name
            )));
        }
        if !module_names.insert(module.name.as_str()) {
            return Err(StdError::new(format!(
                "standard module `{}` is listed more than once",
                module.name
            )));
        }
        if module.name.trim().is_empty() {
            return Err(StdError::new("standard module name cannot be empty"));
        }
        if !module_declarations.insert(declaration.clone()) {
            return Err(StdError::new(format!(
                "standard declaration `{declaration}` is bound to more than one runtime module"
            )));
        }
        if module.runtime.trim().is_empty() {
            return Err(StdError::new(format!(
                "standard module `{}` has an empty runtime module path",
                module.name
            )));
        }
        if module.alias.as_deref().is_some_and(str::is_empty) {
            return Err(StdError::new(format!(
                "standard module `{}` has an empty runtime alias",
                module.name
            )));
        }
        if module.export.as_deref().is_some_and(str::is_empty) {
            return Err(StdError::new(format!(
                "standard module `{}` has an empty runtime export",
                module.name
            )));
        }
    }

    for required in ["format", "number"] {
        if !manifest.runtime_helpers.contains_key(required) {
            return Err(StdError::new(format!(
                "std.toml is missing required runtime helper `{required}`"
            )));
        }
    }
    for (name, runtime) in &manifest.runtime_helpers {
        validate_runtime_binding(
            name,
            &runtime.module,
            runtime.export.as_deref(),
            runtime.alias.as_deref(),
        )?;
    }

    validate_runtime_packages(&manifest)?;

    validate_lang_items(&manifest.lang_items)?;

    Ok(StdLibrary {
        manifest,
        declarations,
        runtime_sources,
    })
}

fn validate_runtime_binding(
    name: &str,
    module: &str,
    export: Option<&str>,
    alias: Option<&str>,
) -> Result<(), StdError> {
    if name.trim().is_empty() {
        return Err(StdError::new("runtime helper name cannot be empty"));
    }
    if module.trim().is_empty() {
        return Err(StdError::new(format!(
            "runtime helper `{name}` has an empty Lua module path"
        )));
    }
    if alias.is_some_and(str::is_empty) {
        return Err(StdError::new(format!(
            "runtime helper `{name}` has an empty runtime alias"
        )));
    }
    if export.is_some_and(str::is_empty) {
        return Err(StdError::new(format!(
            "runtime helper `{name}` has an empty runtime export"
        )));
    }
    Ok(())
}

fn validate_runtime_packages(manifest: &StdManifest) -> Result<(), StdError> {
    let mut package_abis = BTreeMap::<&str, Option<u32>>::new();
    let mut exports = BTreeSet::<(&str, &str)>::new();
    let bindings = manifest
        .modules
        .iter()
        .map(|module| {
            (
                module.runtime.as_str(),
                module.export.as_deref(),
                module.abi,
            )
        })
        .chain(manifest.runtime_helpers.values().map(|runtime| {
            (
                runtime.module.as_str(),
                runtime.export.as_deref(),
                runtime.abi,
            )
        }));
    for (package, export, abi) in bindings {
        if let Some(previous) = package_abis.insert(package, abi)
            && previous != abi
        {
            return Err(StdError::new(format!(
                "runtime package `{package}` has conflicting ABI requirements"
            )));
        }
        if let Some(export) = export
            && !exports.insert((package, export))
        {
            return Err(StdError::new(format!(
                "runtime export `{package}.{export}` is bound more than once"
            )));
        }
    }
    Ok(())
}

fn validate_lang_items(items: &BTreeMap<String, String>) -> Result<(), StdError> {
    const REQUIRED: &[&str] = &[
        "option",
        "option_some",
        "option_none",
        "option_map",
        "option_unwrap",
        "option_expect",
        "option_unwrap_or",
        "option_is_some",
        "option_is_none",
        "result",
        "result_ok",
        "result_err",
        "result_map",
        "result_unwrap",
        "result_expect",
        "result_unwrap_or",
        "result_is_ok",
        "result_is_err",
    ];
    let mut targets = BTreeSet::new();
    for key in REQUIRED {
        let Some(path) = items.get(*key) else {
            return Err(StdError::new(format!(
                "std.toml is missing required language item `{key}`"
            )));
        };
        if path.split("::").any(|segment| segment.is_empty()) {
            return Err(StdError::new(format!(
                "language item `{key}` has invalid path `{path}`"
            )));
        }
        if !targets.insert(path) {
            return Err(StdError::new(format!(
                "more than one language item targets `{path}`"
            )));
        }
    }
    Ok(())
}

fn normalize_relative_path(path: &str) -> Result<String, StdError> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(StdError::new(format!(
            "standard resource path `{}` must be relative",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StdError::new(format!(
                    "standard resource path `{}` escapes its root",
                    path.display()
                )));
            }
        }
    }
    normalized
        .to_str()
        .map(|path| path.replace('\\', "/"))
        .filter(|path| !path.is_empty())
        .ok_or_else(|| StdError::new("standard resource path is empty or not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_manifest_loads_all_declarations() {
        let library = embedded_std().expect("embedded standard library");
        assert_eq!(library.manifest().version, 1);
        assert_eq!(library.declarations().len(), 6);
        assert_eq!(library.runtime_sources().len(), 1);
        assert_eq!(library.lang_item("option"), Some("Option"));
        assert_eq!(library.lang_item("result"), Some("Result"));
        assert_eq!(library.manifest().modules[1].alias.as_deref(), Some("map"));
        assert!(library.runtime_sources()[0].text().contains("return std"));
    }

    #[test]
    fn manifest_rejects_path_traversal() {
        let error = build_library(
            "version = 1\ndeclarations = [\"../outside.ruai\"]\nruntime_sources = [\"rua_std.lua\"]",
            |_| Ok(Arc::from("")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("escapes its root"));
    }

    #[test]
    fn manifest_persists_normalized_declaration_paths() {
        let manifest = r#"
version = 1
declarations = ["./std/option.ruai", "std/result.ruai"]
runtime_sources = ["./rua_std.lua"]

[runtime_helpers.format]
module = "rua_std"
export = "fmt"

[runtime_helpers.number]
module = "rua_std"
export = "number"

[lang_items]
option = "Option"
option_some = "Option::Some"
option_none = "Option::None"
option_map = "Option::map"
option_unwrap = "Option::unwrap"
option_expect = "Option::expect"
option_unwrap_or = "Option::unwrap_or"
option_is_some = "Option::is_some"
option_is_none = "Option::is_none"
result = "Result"
result_ok = "Result::Ok"
result_err = "Result::Err"
result_map = "Result::map"
result_unwrap = "Result::unwrap"
result_expect = "Result::expect"
result_unwrap_or = "Result::unwrap_or"
result_is_ok = "Result::is_ok"
result_is_err = "Result::is_err"

[[modules]]
name = "std::option"
declaration = "./std/option.ruai"
runtime = "rua_std.option"
export = "option"
"#;
        let library = build_library(manifest, |_| Ok(Arc::from("pub struct Empty {}")))
            .expect("normalized manifest");
        assert_eq!(library.manifest().declarations[0], "std/option.ruai");
        assert_eq!(library.manifest().modules[0].declaration, "std/option.ruai");
    }

    #[test]
    fn manifest_rejects_conflicting_abis_within_one_runtime_package() {
        let manifest = include_str!("../resources/std/std.toml").replacen("abi = 2", "abi = 3", 1);
        let error = build_library(&manifest, |_| Ok(Arc::from(""))).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("runtime package `rua_std` has conflicting ABI requirements"),
            "{error}"
        );
    }
}
