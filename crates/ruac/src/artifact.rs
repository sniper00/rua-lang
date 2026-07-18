//! Versioned, portable compiler artifacts.
//!
//! The generated Lua remains ordinary text. This module stores the metadata
//! needed to load it in another process and convert Lua locations back to Rua
//! source locations without invoking the compiler again.

use crate::codegen::{GeneratedLua, GeneratedLuaModules, LuaSourceMapping};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Schema version for the on-disk Rua artifact manifest.
pub const ARTIFACT_SCHEMA_VERSION: u32 = 1;
/// ABI exposed by the embedded `rua_std` runtime used by generated code.
pub const RUA_RUNTIME_ABI: u32 = 2;

/// One compiler result type for both single-file and module-oriented output.
#[derive(Clone, Debug)]
pub enum RuaArtifact {
    Bundle(GeneratedLua),
    Modules(GeneratedLuaModules),
}

impl RuaArtifact {
    pub fn manifest(&self) -> RuaArtifactManifest {
        self.manifest_for(Path::new("main.lua"))
    }

    pub fn manifest_for(&self, output: &Path) -> RuaArtifactManifest {
        match self {
            Self::Bundle(artifact) => RuaArtifactManifest::from_bundle(output, artifact),
            Self::Modules(artifact) => RuaArtifactManifest::from_modules(artifact),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Bundle,
    Modules,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct ArtifactFile {
    /// Lua `require` name. The root uses its output file stem.
    pub module_name: String,
    /// Path relative to the manifest directory.
    pub output_path: String,
    pub is_root: bool,
    /// Stable non-cryptographic hash used to reject stale source maps.
    pub source_hash: String,
    pub source_map: Vec<LuaSourceMapping>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct RuaArtifactManifest {
    pub schema_version: u32,
    pub compiler_version: String,
    pub runtime_abi: u32,
    pub kind: ArtifactKind,
    pub root_output_path: String,
    /// Source paths indexed by `LuaSourceMapping::source.file`.
    pub source_files: Vec<String>,
    pub files: Vec<ArtifactFile>,
}

impl RuaArtifactManifest {
    pub fn from_bundle(output_path: &Path, artifact: &GeneratedLua) -> Self {
        let output_path = output_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("main.lua")
            .to_string();
        Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            compiler_version: env!("CARGO_PKG_VERSION").to_string(),
            runtime_abi: RUA_RUNTIME_ABI,
            kind: ArtifactKind::Bundle,
            root_output_path: output_path.clone(),
            source_files: artifact.source_files.clone(),
            files: vec![ArtifactFile {
                module_name: module_name_from_output(&output_path),
                output_path,
                is_root: true,
                source_hash: source_hash(&artifact.source),
                source_map: artifact.source_map.clone(),
            }],
        }
    }

    pub fn from_modules(artifact: &GeneratedLuaModules) -> Self {
        Self {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            compiler_version: env!("CARGO_PKG_VERSION").to_string(),
            runtime_abi: RUA_RUNTIME_ABI,
            kind: ArtifactKind::Modules,
            root_output_path: artifact.root_output_path.clone(),
            source_files: artifact.source_files.clone(),
            files: artifact
                .modules
                .iter()
                .map(|module| ArtifactFile {
                    module_name: module.module_name.clone(),
                    output_path: module.output_path.clone(),
                    is_root: module.is_root,
                    source_hash: source_hash(&module.source),
                    source_map: module.source_map.clone(),
                })
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported Rua artifact schema {}; expected {}",
                self.schema_version, ARTIFACT_SCHEMA_VERSION
            ));
        }
        if self.runtime_abi != RUA_RUNTIME_ABI {
            return Err(format!(
                "unsupported Rua runtime ABI {}; expected {}",
                self.runtime_abi, RUA_RUNTIME_ABI
            ));
        }
        if self.root_output_path.is_empty() || self.files.is_empty() {
            return Err("Rua artifact has no root output or files".to_string());
        }
        let roots = self.files.iter().filter(|file| file.is_root).count();
        if roots != 1 {
            return Err(format!(
                "Rua artifact has {roots} root files; expected exactly one"
            ));
        }
        for file in &self.files {
            validate_relative_path(&file.output_path)?;
            if file.source_hash.is_empty() {
                return Err(format!(
                    "Rua artifact file `{}` has no source hash",
                    file.output_path
                ));
            }
        }
        if !self
            .files
            .iter()
            .any(|file| file.output_path == self.root_output_path)
        {
            return Err(format!(
                "Rua artifact root `{}` is not listed in files",
                self.root_output_path
            ));
        }
        Ok(())
    }
}

pub fn bundle_sidecar_path(lua_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.rua-map.json", lua_path.display()))
}

pub fn modules_manifest_path(output_dir: &Path) -> PathBuf {
    output_dir.join("rua-artifact.json")
}

pub fn write_bundle(path: &Path, artifact: &GeneratedLua) -> Result<PathBuf, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
    }
    fs::write(path, &artifact.source)
        .map_err(|error| format!("writing {}: {error}", path.display()))?;
    let sidecar = bundle_sidecar_path(path);
    write_manifest(&sidecar, &RuaArtifactManifest::from_bundle(path, artifact))?;
    Ok(sidecar)
}

/// Write either artifact shape through one host-facing API. Bundle artifacts
/// take a Lua output path; module artifacts take an output directory.
pub fn write(artifact: &RuaArtifact, output: &Path) -> Result<PathBuf, String> {
    match artifact {
        RuaArtifact::Bundle(artifact) => write_bundle(output, artifact),
        RuaArtifact::Modules(artifact) => write_modules(output, artifact),
    }
}

pub fn write_modules(output_dir: &Path, artifact: &GeneratedLuaModules) -> Result<PathBuf, String> {
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("creating {}: {error}", output_dir.display()))?;
    for module in &artifact.modules {
        let output = output_dir.join(&module.output_path);
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        }
        fs::write(&output, &module.source)
            .map_err(|error| format!("writing {}: {error}", output.display()))?;
    }
    let manifest_path = modules_manifest_path(output_dir);
    write_manifest(&manifest_path, &RuaArtifactManifest::from_modules(artifact))?;
    Ok(manifest_path)
}

pub fn read_manifest(path: &Path) -> Result<RuaArtifactManifest, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let manifest: RuaArtifactManifest = serde_json::from_str(&text)
        .map_err(|error| format!("parsing {}: {error}", path.display()))?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn source_hash(source: &str) -> String {
    // FNV-1a is deterministic and sufficient for detecting a stale map. It is
    // deliberately not presented as a security or authenticity mechanism.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn write_manifest(path: &Path, manifest: &RuaArtifactManifest) -> Result<(), String> {
    manifest.validate()?;
    let text = serde_json::to_string_pretty(manifest)
        .map_err(|error| format!("serializing {}: {error}", path.display()))?;
    fs::write(path, format!("{text}\n"))
        .map_err(|error| format!("writing {}: {error}", path.display()))
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "Rua artifact path `{path}` must be relative and cannot contain `..`"
        ));
    }
    Ok(())
}

fn module_name_from_output(output_path: &str) -> String {
    output_path
        .strip_suffix(".lua")
        .unwrap_or(output_path)
        .replace('/', ".")
        .replace('\\', ".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{annotations::AnnotationIndex, token::SourceRange};

    #[test]
    fn bundle_manifest_round_trips_and_validates_hash() {
        let artifact = GeneratedLua {
            source: "return 1\n".to_string(),
            source_map: vec![LuaSourceMapping {
                generated_start: 0,
                generated_end: 9,
                source: SourceRange {
                    start: 0,
                    len: 1,
                    line: 1,
                    file: 0,
                },
            }],
            source_files: vec!["main.rua".to_string()],
            annotations: AnnotationIndex::default(),
        };
        let manifest = RuaArtifactManifest::from_bundle(Path::new("main.lua"), &artifact);
        assert_eq!(manifest.schema_version, ARTIFACT_SCHEMA_VERSION);
        assert_eq!(manifest.files[0].source_hash, source_hash(&artifact.source));
        manifest.validate().unwrap();
    }

    #[test]
    fn manifest_rejects_traversal() {
        let manifest = RuaArtifactManifest {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            compiler_version: "test".to_string(),
            runtime_abi: RUA_RUNTIME_ABI,
            kind: ArtifactKind::Modules,
            root_output_path: "main.lua".to_string(),
            source_files: Vec::new(),
            files: vec![ArtifactFile {
                module_name: "main".to_string(),
                output_path: "../main.lua".to_string(),
                is_root: true,
                source_hash: "hash".to_string(),
                source_map: Vec::new(),
            }],
        };
        assert!(manifest.validate().is_err());
    }
}
