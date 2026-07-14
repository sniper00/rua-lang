use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

pub(super) struct LibraryConfig {
    pub(super) roots: Vec<PathBuf>,
    pub(super) mounts: HashMap<String, PathBuf>,
    pub(super) bases: Vec<PathBuf>,
    pub(super) project_bases: BTreeMap<u32, Vec<PathBuf>>,
    pub(super) files: Vec<LibraryFile>,
}

pub(super) struct LibraryScanRequest {
    roots: Vec<PathBuf>,
    mounts: HashMap<String, PathBuf>,
    projects: Vec<ProjectLibraryScanRequest>,
}

struct ProjectLibraryScanRequest {
    project_index: u32,
    roots: Vec<PathBuf>,
    mounts: HashMap<String, PathBuf>,
}

#[derive(Clone, Debug)]
pub(super) struct WorkspaceScan {
    pub(super) project_index: usize,
    pub(super) root: PathBuf,
    pub(super) files: Vec<(PathBuf, String)>,
}

#[derive(Clone, Debug)]
pub(super) struct LibraryFile {
    pub(super) physical_path: PathBuf,
    pub(super) analysis_path: PathBuf,
    pub(super) text: String,
}

impl LibraryScanRequest {
    pub(super) fn from_settings(settings: &serde_json::Value) -> Result<Self, String> {
        let nested = settings.get("rua");
        let library = settings
            .get("rua.library")
            .or_else(|| nested.and_then(|rua| rua.get("library")))
            .or_else(|| settings.get("library"));
        let mounts = settings
            .get("rua.libraryMounts")
            .or_else(|| nested.and_then(|rua| rua.get("libraryMounts")))
            .or_else(|| settings.get("libraryMounts"));

        let roots = parse_library_paths(library)?;
        let mounts = parse_mounts(mounts)?;
        let workspace_settings = nested
            .and_then(|rua| rua.get("workspaceSettings"))
            .or_else(|| settings.get("workspaceSettings"));
        let projects = workspace_settings
            .map(|settings| {
                settings
                    .as_array()
                    .ok_or_else(|| "rua.workspaceSettings must be an array".to_string())?
                    .iter()
                    .map(|setting| {
                        let project_index = setting
                            .get("projectIndex")
                            .and_then(serde_json::Value::as_u64)
                            .ok_or_else(|| {
                                "rua.workspaceSettings.projectIndex must be an integer".to_string()
                            })? as u32;
                        Ok(ProjectLibraryScanRequest {
                            project_index,
                            roots: parse_library_paths(setting.get("library"))?,
                            mounts: parse_mounts(setting.get("libraryMounts"))?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            roots,
            mounts,
            projects,
        })
    }

    pub(super) fn scan(
        self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LibraryConfig, String> {
        let mut roots = self.roots;
        let mut mounts = self.mounts;
        let (mut bases, mut files) = scan_library_inputs(&roots, &mounts, cancelled)?;
        let mut project_bases = BTreeMap::new();
        for project in self.projects {
            let project_roots = project.roots;
            let project_mounts = project.mounts;
            let (scanned_bases, scanned_files) =
                scan_library_inputs(&project_roots, &project_mounts, cancelled)?;
            project_bases.insert(project.project_index, scanned_bases);
            files.extend(scanned_files);
            roots.extend(project_roots);
            mounts.extend(
                project_mounts
                    .into_iter()
                    .map(|(name, path)| (format!("{}:{name}", project.project_index), path)),
            );
        }

        roots.sort();
        roots.dedup();
        bases.sort();
        bases.dedup();
        files.sort_by(|left, right| left.analysis_path.cmp(&right.analysis_path));
        files.dedup_by(|left, right| left.analysis_path == right.analysis_path);

        Ok(LibraryConfig {
            roots,
            mounts,
            bases,
            project_bases,
            files,
        })
    }
}

fn scan_library_inputs(
    roots: &[PathBuf],
    mounts: &HashMap<String, PathBuf>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(Vec<PathBuf>, Vec<LibraryFile>), String> {
    let mut bases = Vec::new();
    let mut files = Vec::new();
    for root in roots {
        if cancelled() {
            return Err("library scan cancelled".to_string());
        }
        let mut scanned = Vec::new();
        scan_library_root_cancellable(root, &mut scanned, cancelled);
        if let Ok(canonical) = std::fs::canonicalize(root) {
            bases.push(if canonical.is_dir() {
                canonical.clone()
            } else {
                canonical.parent().unwrap_or(Path::new("")).to_path_buf()
            });
        }
        files.extend(
            scanned
                .into_iter()
                .map(|(physical_path, text)| LibraryFile {
                    analysis_path: physical_path.clone(),
                    physical_path,
                    text,
                }),
        );
    }

    for (name, path) in mounts {
        if cancelled() {
            return Err("library scan cancelled".to_string());
        }
        let Ok(canonical) = std::fs::canonicalize(path) else {
            continue;
        };
        let base = canonical.parent().unwrap_or(Path::new("")).to_path_buf();
        bases.push(base.clone());
        if canonical.is_file() {
            if let Ok(text) = std::fs::read_to_string(&canonical) {
                let extension = canonical
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("ruai")
                    .to_string();
                files.push(LibraryFile {
                    physical_path: canonical,
                    analysis_path: base.join(format!("{name}.{extension}")),
                    text,
                });
            }
        } else if canonical.is_dir() {
            let mut scanned = Vec::new();
            scan_library_root_cancellable(&canonical, &mut scanned, cancelled);
            for (physical_path, text) in scanned {
                let Ok(relative) = physical_path
                    .strip_prefix(&canonical)
                    .map(Path::to_path_buf)
                else {
                    continue;
                };
                files.push(LibraryFile {
                    physical_path,
                    analysis_path: base.join(name).join(relative),
                    text,
                });
            }
        }
    }
    bases.sort();
    bases.dedup();
    Ok((bases, files))
}

fn parse_library_paths(value: Option<&serde_json::Value>) -> Result<Vec<PathBuf>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| "rua.library must be an array of paths".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(PathBuf::from)
                .ok_or_else(|| "rua.library entry must be a string path".to_string())
        })
        .collect()
}

fn parse_mounts(value: Option<&serde_json::Value>) -> Result<HashMap<String, PathBuf>, String> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| "rua.libraryMounts must be an object".to_string())?
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|path| (name.clone(), PathBuf::from(path)))
                .ok_or_else(|| format!("rua.libraryMounts.{name} must be a string path"))
        })
        .collect()
}

#[cfg(test)]
pub(super) fn scan_library_root(root: &Path, files: &mut Vec<(PathBuf, String)>) {
    scan_library_root_cancellable(root, files, &mut || false);
}

fn scan_library_root_cancellable(
    root: &Path,
    files: &mut Vec<(PathBuf, String)>,
    cancelled: &mut impl FnMut() -> bool,
) {
    scan_project_files(root, "ruai", cancelled, &mut |path, text| {
        files.push((path.to_path_buf(), text.to_string()));
    });
}

#[cfg(test)]
pub(super) fn scan_workspace_files(root: &Path, cb: &mut dyn FnMut(&Path, &str)) {
    scan_project_files(root, "rua", &mut || false, cb);
}

pub(super) fn scan_workspace_roots(
    roots: Vec<PathBuf>,
    cancelled: &mut impl FnMut() -> bool,
) -> Vec<WorkspaceScan> {
    let mut scans = Vec::new();
    for (project_index, root) in roots.into_iter().enumerate() {
        if cancelled() {
            break;
        }
        let mut files = Vec::new();
        scan_project_files(&root, "rua", cancelled, &mut |path, text| {
            files.push((path.to_path_buf(), text.to_string()));
        });
        scans.push(WorkspaceScan {
            project_index,
            root,
            files,
        });
    }
    scans
}

fn scan_project_files(
    root: &Path,
    extension: &str,
    cancelled: &mut impl FnMut() -> bool,
    cb: &mut dyn FnMut(&Path, &str),
) {
    if cancelled() {
        return;
    }
    let Ok(canonical) = std::fs::canonicalize(root) else {
        return;
    };
    if canonical.is_file() {
        if canonical.extension().and_then(|value| value.to_str()) == Some(extension)
            && let Ok(text) = std::fs::read_to_string(&canonical)
        {
            cb(&canonical, &text);
        }
        return;
    }
    if canonical.is_dir() {
        let matcher = project_ignore(&canonical);
        scan_project_dir(
            &canonical,
            extension,
            &mut HashSet::new(),
            matcher.as_ref(),
            cancelled,
            cb,
        );
    }
}

fn scan_project_dir(
    dir: &Path,
    extension: &str,
    visited: &mut HashSet<PathBuf>,
    matcher: Option<&ignore::gitignore::Gitignore>,
    cancelled: &mut impl FnMut() -> bool,
    cb: &mut dyn FnMut(&Path, &str),
) {
    if cancelled() {
        return;
    }
    let Ok(canonical) = std::fs::canonicalize(dir) else {
        return;
    };
    if !visited.insert(canonical.clone()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&canonical) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    paths.sort();
    for path in paths {
        if cancelled() {
            return;
        }
        if is_project_ignored(matcher, &path) {
            continue;
        }
        if path.is_dir() {
            if !should_skip_directory(&path) {
                scan_project_dir(&path, extension, visited, matcher, cancelled, cb);
            }
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension)
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            cb(&path, &text);
        }
    }
}

fn project_ignore(root: &Path) -> Option<ignore::gitignore::Gitignore> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    for file in [".gitignore", ".ignore", ".ruaignore"] {
        let path = root.join(file);
        if path.is_file() {
            let _ = builder.add(path);
        }
    }
    builder.build().ok()
}

fn is_project_ignored(matcher: Option<&ignore::gitignore::Gitignore>, path: &Path) -> bool {
    matcher.is_some_and(|matcher| {
        matcher
            .matched_path_or_any_parents(path, path.is_dir())
            .is_ignore()
    })
}

fn should_skip_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') || matches!(name, "target" | "node_modules"))
}
