//! Filesystem-facing adapter for the protocol-neutral analysis database.

use std::{
    collections::{BTreeMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
};

use rua_analysis::{AnalysisHost, Change, FileId, FileKind, SourceRootId, SourceRootKind};
use serde_json::Value;

const LIBRARY_ROOT_ID: SourceRootId = SourceRootId::new(u32::MAX);
const FIRST_LIBRARY_FILE_ID: u32 = 1 << 31;

#[derive(Debug, Default, PartialEq, Eq)]
struct LibraryConfig {
    libraries: Vec<PathBuf>,
    mounts: BTreeMap<String, PathBuf>,
}

impl LibraryConfig {
    fn from_settings(settings: &Value) -> Result<Self, ConfigError> {
        let nested = settings.get("rua");
        let library = settings
            .get("rua.library")
            .or_else(|| nested.and_then(|rua| rua.get("library")))
            .or_else(|| settings.get("library"));
        let mounts = settings
            .get("rua.libraryMounts")
            .or_else(|| nested.and_then(|rua| rua.get("libraryMounts")))
            .or_else(|| settings.get("libraryMounts"));

        Ok(Self {
            libraries: parse_library_paths(library)?,
            mounts: parse_mounts(mounts)?,
        })
    }

    fn is_empty(&self) -> bool {
        self.libraries.is_empty() && self.mounts.is_empty()
    }

    fn root_count(&self) -> usize {
        self.libraries.len() + self.mounts.len()
    }
}

fn parse_library_paths(value: Option<&Value>) -> Result<Vec<PathBuf>, ConfigError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| ConfigError::new("rua.library must be an array of paths"))?;
    entries
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| ConfigError::new(format!("rua.library[{index}] must be a path")))
        })
        .collect()
}

fn parse_mounts(value: Option<&Value>) -> Result<BTreeMap<String, PathBuf>, ConfigError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let entries = value
        .as_object()
        .ok_or_else(|| ConfigError::new("rua.libraryMounts must be an object"))?;
    entries
        .iter()
        .map(|(mount, value)| {
            value
                .as_str()
                .map(|path| (mount.clone(), PathBuf::from(path)))
                .ok_or_else(|| {
                    ConfigError::new(format!("rua.libraryMounts.{mount} must be a path"))
                })
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug, Default)]
pub(crate) struct LoadReport {
    pub(crate) configured_root_count: usize,
    pub(crate) file_count: usize,
    pub(crate) warnings: Vec<String>,
}

/// Owns stable path identities and submits fully loaded text to AnalysisHost.
#[derive(Debug)]
pub(crate) struct AnalysisInputs {
    host: AnalysisHost,
    file_ids: BTreeMap<PathBuf, FileId>,
    next_file_id: u32,
    has_library_root: bool,
}

impl AnalysisInputs {
    pub(crate) fn new() -> Self {
        Self {
            host: AnalysisHost::new(),
            file_ids: BTreeMap::new(),
            next_file_id: FIRST_LIBRARY_FILE_ID,
            has_library_root: false,
        }
    }

    pub(crate) fn reload_from_settings(
        &mut self,
        settings: &Value,
    ) -> Result<LoadReport, ConfigError> {
        let config = LibraryConfig::from_settings(settings)?;
        let mut report = LoadReport {
            configured_root_count: config.root_count(),
            ..LoadReport::default()
        };
        let files = scan_configured_files(&config, &mut report.warnings);
        let mut change = Change::new();

        if self.has_library_root {
            change.remove_source_root(LIBRARY_ROOT_ID);
        }
        if !config.is_empty() {
            change.set_source_root(LIBRARY_ROOT_ID, SourceRootKind::Library);
            for (path, text) in files {
                let file_id = self.file_id_for_path(path);
                change.set_file(file_id, LIBRARY_ROOT_ID, FileKind::Declaration, text);
                report.file_count += 1;
            }
        }

        self.host.apply_change(change);
        self.has_library_root = !config.is_empty();
        Ok(report)
    }

    fn file_id_for_path(&mut self, path: PathBuf) -> FileId {
        if let Some(file_id) = self.file_ids.get(&path) {
            return *file_id;
        }
        let file_id = FileId::new(self.next_file_id);
        self.next_file_id = self
            .next_file_id
            .checked_add(1)
            .expect("exhausted analysis file IDs");
        self.file_ids.insert(path, file_id);
        file_id
    }
}

fn scan_configured_files(
    config: &LibraryConfig,
    warnings: &mut Vec<String>,
) -> BTreeMap<PathBuf, String> {
    let mut files = BTreeMap::new();
    let mut visited_dirs = HashSet::new();
    for path in &config.libraries {
        scan_path(path, &mut files, &mut visited_dirs, warnings);
    }
    for path in config.mounts.values() {
        scan_path(path, &mut files, &mut visited_dirs, warnings);
    }
    files
}

fn scan_path(
    configured_path: &Path,
    files: &mut BTreeMap<PathBuf, String>,
    visited_dirs: &mut HashSet<PathBuf>,
    warnings: &mut Vec<String>,
) {
    let path = match fs::canonicalize(configured_path) {
        Ok(path) => path,
        Err(error) => {
            warnings.push(format!("{}: {error}", configured_path.display()));
            return;
        }
    };
    if path.is_file() {
        load_declaration(&path, files, warnings);
    } else if path.is_dir() {
        scan_directory(&path, files, visited_dirs, warnings);
    }
}

fn scan_directory(
    directory: &Path,
    files: &mut BTreeMap<PathBuf, String>,
    visited_dirs: &mut HashSet<PathBuf>,
    warnings: &mut Vec<String>,
) {
    let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    if !visited_dirs.insert(directory.clone()) {
        return;
    }
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("{}: {error}", directory.display()));
            return;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(error) => warnings.push(format!("{}: {error}", directory.display())),
        }
    }
    paths.sort();

    for path in paths {
        if path.is_dir() {
            if !is_hidden(&path) {
                scan_directory(&path, files, visited_dirs, warnings);
            }
        } else {
            load_declaration(&path, files, warnings);
        }
    }
}

fn load_declaration(
    path: &Path,
    files: &mut BTreeMap<PathBuf, String>,
    warnings: &mut Vec<String>,
) {
    if path.extension().and_then(|extension| extension.to_str()) != Some("ruai") {
        return;
    }
    match fs::read_to_string(path) {
        Ok(text) => {
            files.insert(path.to_path_buf(), text);
        }
        Err(error) => warnings.push(format!("{}: {error}", path.display())),
    }
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::{AnalysisInputs, LibraryConfig};
    use rua_analysis::{FileKind, SourceRootKind};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("rua-lsp-{name}-{}-{nonce}", std::process::id()));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parses_namespaced_and_dotted_library_settings() {
        let nested = LibraryConfig::from_settings(&json!({
            "rua": {
                "library": ["/lib/a"],
                "libraryMounts": {"clock": "/lib/clock.ruai"}
            }
        }))
        .expect("valid nested settings");
        let dotted = LibraryConfig::from_settings(&json!({
            "rua.library": ["/lib/a"],
            "rua.libraryMounts": {"clock": "/lib/clock.ruai"}
        }))
        .expect("valid dotted settings");

        assert_eq!(nested, dotted);
        assert_eq!(nested.root_count(), 2);
    }

    #[test]
    fn configured_ruai_files_become_read_only_analysis_inputs() {
        let temp = TestDir::new("library-inputs");
        let library = temp.path().join("meta");
        fs::create_dir_all(library.join("nested")).expect("create library tree");
        let api = library.join("nested/api.ruai");
        let ignored = library.join("source.rua");
        let mounted = temp.path().join("clock.ruai");
        fs::write(&api, "extern \"lua\" { pub fn api(); }").expect("write api declaration");
        fs::write(&ignored, "fn source() {}").expect("write source file");
        fs::write(&mounted, "extern \"lua\" { pub fn now() -> i64; }")
            .expect("write mounted declaration");

        let mut inputs = AnalysisInputs::new();
        let report = inputs
            .reload_from_settings(&json!({
                "rua.library": [library],
                "rua.libraryMounts": {"clock": mounted}
            }))
            .expect("load library configuration");

        assert_eq!(report.configured_root_count, 2);
        assert_eq!(report.file_count, 2);
        assert!(report.warnings.is_empty());
        let api_id = inputs.file_ids[&fs::canonicalize(api).expect("canonical api path")];
        let mounted_id =
            inputs.file_ids[&fs::canonicalize(mounted).expect("canonical mounted path")];
        let analysis = inputs.host.analysis();
        for file_id in [api_id, mounted_id] {
            assert_eq!(analysis.file_kind(file_id), Some(FileKind::Declaration));
            assert_eq!(
                analysis.source_root_kind(file_id),
                Some(SourceRootKind::Library)
            );
            assert!(analysis.is_file_read_only(file_id));
            assert!(analysis.parse(file_id).errors().is_empty());
        }
    }

    #[test]
    fn reload_updates_stable_file_and_removes_stale_library_root() {
        let temp = TestDir::new("library-reload");
        let declaration = temp.path().join("api.ruai");
        fs::write(&declaration, "pub fn before();").expect("write declaration");
        let settings = json!({"rua": {"library": [declaration]}});

        let mut inputs = AnalysisInputs::new();
        inputs
            .reload_from_settings(&settings)
            .expect("initial library load");
        let path = fs::canonicalize(&declaration).expect("canonical declaration path");
        let file_id = inputs.file_ids[&path];

        fs::write(&declaration, "pub fn after();").expect("update declaration");
        inputs
            .reload_from_settings(&settings)
            .expect("refresh library load");
        assert_eq!(inputs.file_ids[&path], file_id);
        assert_eq!(
            inputs
                .host
                .analysis()
                .parse(file_id)
                .syntax_node()
                .text()
                .to_string(),
            "pub fn after();"
        );

        inputs
            .reload_from_settings(&json!({"rua": {}}))
            .expect("remove library configuration");
        assert_eq!(inputs.host.analysis().file_kind(file_id), None);
    }
}
