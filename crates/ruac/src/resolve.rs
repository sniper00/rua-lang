//! Path-based module discovery and compiler-internal module-tree construction.

use crate::ast::*;
use crate::diag::Diag;
use crate::token::SourceRange;
use rua_core::DiagnosticCode;
use rua_project::{
    FileId, LogicalSourcePath, ProjectSpec, SourceProvider, module_path_from_relative_file,
};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone)]
struct DiscoveredFile<S> {
    source: S,
    display: String,
    is_declaration: bool,
    precedence: usize,
}

struct DiscoveredModule<S> {
    source: Option<DiscoveredFile<S>>,
    children: BTreeMap<String, DiscoveredModule<S>>,
}

impl<S> Default for DiscoveredModule<S> {
    fn default() -> Self {
        Self {
            source: None,
            children: BTreeMap::new(),
        }
    }
}

/// Discover every project source and declaration file by path. User-authored
/// `mod` declarations are not part of this model; the resulting `ModDecl`s are
/// compiler-internal nodes consumed by the existing semantic pipeline.
pub fn discover_modules_from_filesystem(
    program: &mut Program,
    root_file: &Path,
    library: &[PathBuf],
    library_mounts: &BTreeMap<String, PathBuf>,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let source_root = root_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let root_canonical = root_file.canonicalize().map_err(|error| {
        module_error(
            DiagnosticCode::HostSourceRead,
            format!("canonicalizing {}: {error}", root_file.display()),
        )
    })?;
    let mut discovered = DiscoveredModule::default();
    collect_filesystem_root(
        &mut discovered,
        source_root,
        &[],
        "rua",
        0,
        Some(&root_canonical),
    )?;
    collect_filesystem_root(
        &mut discovered,
        source_root,
        &[],
        "ruai",
        1,
        Some(&root_canonical),
    )?;
    for (index, root) in library.iter().enumerate() {
        collect_library_input(&mut discovered, root, &[], index + 2)?;
    }
    let mount_precedence = library.len() + 2;
    for (index, (name, root)) in library_mounts.iter().enumerate() {
        collect_library_input(
            &mut discovered,
            root,
            std::slice::from_ref(name),
            mount_precedence + index,
        )?;
    }
    materialize_filesystem_modules(
        &mut program.items,
        &mut program.source_order,
        discovered.children,
        files,
    )
}

/// IO-free counterpart of [`discover_modules_from_filesystem`]. The project
/// file table is already a complete snapshot, so no provider path probing is
/// needed.
pub fn discover_modules_with_provider<P: SourceProvider>(
    program: &mut Program,
    scope_dir: &LogicalSourcePath,
    project: &ProjectSpec,
    provider: &P,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let mut discovered = DiscoveredModule::default();
    let scope = Path::new(scope_dir.as_str());
    for (logical_path, file_id) in &project.files {
        if *file_id == project.root_file {
            continue;
        }
        let path = Path::new(logical_path.as_str());
        let Ok(relative) = path.strip_prefix(scope) else {
            continue;
        };
        let extension = relative
            .extension()
            .and_then(|extension| extension.to_str());
        if !matches!(extension, Some("rua" | "ruai")) {
            continue;
        }
        insert_discovered_file(
            &mut discovered,
            module_path(relative)?,
            DiscoveredFile {
                source: (*file_id, logical_path.clone()),
                display: logical_path.to_string(),
                is_declaration: extension == Some("ruai"),
                precedence: usize::from(extension == Some("ruai")),
            },
        )?;
    }
    for (mount_index, mount) in project.libraries.iter().enumerate() {
        let base = Path::new(mount.logical_base.as_str());
        for (logical_path, file_id) in &project.files {
            let path = Path::new(logical_path.as_str());
            let Ok(relative) = path.strip_prefix(base) else {
                continue;
            };
            if relative
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("ruai")
            {
                continue;
            }
            let mut segments = vec![mount.name.clone()];
            segments.extend(module_path_with_root(relative, true)?);
            insert_discovered_file(
                &mut discovered,
                segments,
                DiscoveredFile {
                    source: (*file_id, logical_path.clone()),
                    display: logical_path.to_string(),
                    is_declaration: true,
                    precedence: mount_index + 2,
                },
            )?;
        }
    }
    materialize_provider_modules(
        &mut program.items,
        &mut program.source_order,
        discovered.children,
        provider,
        files,
    )
}

fn module_path(relative: &Path) -> Result<Vec<String>, Diag> {
    module_path_with_root(relative, false)
}

fn module_path_with_root(relative: &Path, allow_root: bool) -> Result<Vec<String>, Diag> {
    let path = module_path_from_relative_file(relative)
        .map_err(|error| module_error(DiagnosticCode::NameInvalidModulePath, error.to_string()))?
        .ok_or_else(|| {
            module_error(
                DiagnosticCode::NameInvalidModulePath,
                format!("not a Rua module source: {}", relative.display()),
            )
        })?;
    if path.is_empty() && !allow_root {
        return Err(module_error(
            DiagnosticCode::NameAmbiguousImport,
            format!(
                "root-level `{}` needs an explicit library mount or must be the project entry",
                relative.display()
            ),
        ));
    }
    Ok(path)
}

fn insert_discovered_file<S>(
    root: &mut DiscoveredModule<S>,
    segments: Vec<String>,
    source: DiscoveredFile<S>,
) -> Result<(), Diag> {
    let mut module = root;
    for segment in segments {
        module = module.children.entry(segment).or_default();
    }
    let Some(existing) = &module.source else {
        module.source = Some(source);
        return Ok(());
    };
    if source.precedence == existing.precedence {
        return Err(module_error(
            DiagnosticCode::NameAmbiguousImport,
            format!(
                "multiple files map to one module: {}, {}",
                existing.display, source.display
            ),
        ));
    }
    if source.precedence < existing.precedence {
        module.source = Some(source);
    }
    Ok(())
}

fn collect_filesystem_root(
    discovered: &mut DiscoveredModule<PathBuf>,
    root: &Path,
    prefix: &[String],
    extension: &str,
    precedence: usize,
    excluded: Option<&Path>,
) -> Result<(), Diag> {
    if !root.is_dir() {
        return Err(module_error(
            DiagnosticCode::HostProjectInvalid,
            format!("module source root is not a directory: {}", root.display()),
        ));
    }
    let mut paths = Vec::new();
    let mut visited = HashSet::new();
    collect_source_files(root, extension, &mut paths, &mut visited)?;
    for path in paths {
        let canonical = path.canonicalize().map_err(|error| {
            module_error(
                DiagnosticCode::HostSourceRead,
                format!("canonicalizing {}: {error}", path.display()),
            )
        })?;
        if excluded.is_some_and(|excluded| canonical == excluded) {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|error| {
            module_error(
                DiagnosticCode::HostProjectInvalid,
                format!(
                    "mapping {} below {}: {error}",
                    path.display(),
                    root.display()
                ),
            )
        })?;
        let mut segments = prefix.to_vec();
        segments.extend(module_path_with_root(relative, !prefix.is_empty())?);
        insert_discovered_file(
            discovered,
            segments,
            DiscoveredFile {
                display: path.display().to_string(),
                source: path,
                is_declaration: extension == "ruai",
                precedence,
            },
        )?;
    }
    Ok(())
}

fn collect_library_input(
    discovered: &mut DiscoveredModule<PathBuf>,
    input: &Path,
    prefix: &[String],
    precedence: usize,
) -> Result<(), Diag> {
    if input.is_dir() {
        return collect_filesystem_root(discovered, input, prefix, "ruai", precedence, None);
    }
    if !input.is_file()
        || input.extension().and_then(|extension| extension.to_str()) != Some("ruai")
    {
        return Err(module_error(
            DiagnosticCode::HostProjectInvalid,
            format!(
                "declaration library must be a `.ruai` file or directory: {}",
                input.display()
            ),
        ));
    }
    let mut segments = prefix.to_vec();
    if segments.is_empty() {
        segments.extend(module_path(input.file_name().map(Path::new).ok_or_else(
            || {
                module_error(
                    DiagnosticCode::HostProjectInvalid,
                    format!("declaration library has no filename: {}", input.display()),
                )
            },
        )?)?);
    }
    insert_discovered_file(
        discovered,
        segments,
        DiscoveredFile {
            source: input.to_path_buf(),
            display: input.display().to_string(),
            is_declaration: true,
            precedence,
        },
    )
}

fn collect_source_files(
    directory: &Path,
    extension: &str,
    files: &mut Vec<PathBuf>,
    visited: &mut HashSet<PathBuf>,
) -> Result<(), Diag> {
    let canonical = directory.canonicalize().map_err(|error| {
        module_error(
            DiagnosticCode::HostSourceRead,
            format!("canonicalizing {}: {error}", directory.display()),
        )
    })?;
    if !visited.insert(canonical) {
        return Ok(());
    }
    let mut entries = std::fs::read_dir(directory)
        .map_err(|error| {
            module_error(
                DiagnosticCode::HostSourceRead,
                format!("reading directory {}: {error}", directory.display()),
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            module_error(
                DiagnosticCode::HostSourceRead,
                format!("reading directory {}: {error}", directory.display()),
            )
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            module_error(
                DiagnosticCode::HostSourceRead,
                format!("reading file type for {}: {error}", path.display()),
            )
        })?;
        if file_type.is_dir() {
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | "node_modules" | "target" | "dist")
            ) {
                continue;
            }
            collect_source_files(&path, extension, files, visited)?;
        } else if file_type.is_file()
            && path.extension().and_then(|candidate| candidate.to_str()) == Some(extension)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn materialize_filesystem_modules(
    items: &mut Vec<Item>,
    source_order: &mut Vec<ChunkEntry>,
    modules: BTreeMap<String, DiscoveredModule<PathBuf>>,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let mut entries = Vec::new();
    for (name, module) in modules {
        let item_index = items.len();
        items.push(Item::Mod(materialize_filesystem_module(
            name, module, files,
        )?));
        entries.push(ChunkEntry::Item(item_index));
    }
    entries.append(source_order);
    *source_order = entries;
    Ok(())
}

fn materialize_filesystem_module(
    name: String,
    module: DiscoveredModule<PathBuf>,
    files: &mut Vec<String>,
) -> Result<ModDecl, Diag> {
    let (mut items, chunk, mut source_order, source_is_declaration, source_file) =
        if let Some(source) = module.source {
            let text = std::fs::read_to_string(&source.source).map_err(|error| {
                module_error(
                    DiagnosticCode::HostSourceRead,
                    format!("reading {}: {error}", source.source.display()),
                )
            })?;
            let file = files.len() as u32;
            files.push(source.source.display().to_string());
            let mut program =
                crate::parser::parse_with_semantic_file(&text, file).map_err(parser_diagnostic)?;
            set_file_program(&mut program, file);
            (
                program.items,
                program.chunk,
                program.source_order,
                source.is_declaration,
                Some(file),
            )
        } else {
            (Vec::new(), empty_block(), Vec::new(), false, None)
        };
    materialize_filesystem_modules(&mut items, &mut source_order, module.children, files)?;
    let is_declaration = source_is_declaration
        || (!items.is_empty()
            && items
                .iter()
                .all(|item| matches!(item, Item::Mod(child) if child.is_decl))
            && chunk.stmts.is_empty());
    if let Some(source_file) = source_file.filter(|_| source_is_declaration) {
        validate_declaration_contents(&items, &chunk, source_file)?;
    }
    if is_declaration {
        mark_decl(&mut items);
    }
    Ok(ModDecl {
        name,
        documentation: None,
        items,
        chunk,
        source_order,
        is_pub: true,
        is_file: true,
        is_decl: is_declaration,
    })
}

fn materialize_provider_modules<P: SourceProvider>(
    items: &mut Vec<Item>,
    source_order: &mut Vec<ChunkEntry>,
    modules: BTreeMap<String, DiscoveredModule<(FileId, LogicalSourcePath)>>,
    provider: &P,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let mut entries = Vec::new();
    for (name, module) in modules {
        let item_index = items.len();
        items.push(Item::Mod(materialize_provider_module(
            name, module, provider, files,
        )?));
        entries.push(ChunkEntry::Item(item_index));
    }
    entries.append(source_order);
    *source_order = entries;
    Ok(())
}

fn materialize_provider_module<P: SourceProvider>(
    name: String,
    module: DiscoveredModule<(FileId, LogicalSourcePath)>,
    provider: &P,
    files: &mut Vec<String>,
) -> Result<ModDecl, Diag> {
    let (mut items, chunk, mut source_order, source_is_declaration, source_file) =
        if let Some(source) = module.source {
            let (file_id, logical_path) = source.source;
            ensure_file_registry(files, file_id, &logical_path);
            let text = provider.load(file_id).map_err(|error| {
                module_error(
                    DiagnosticCode::HostSourceRead,
                    format!("reading `{logical_path}`: {error}"),
                )
            })?;
            let mut program = crate::parser::parse_with_semantic_file(&text.text, file_id.index())
                .map_err(parser_diagnostic)?;
            set_file_program(&mut program, file_id.index());
            (
                program.items,
                program.chunk,
                program.source_order,
                source.is_declaration,
                Some(file_id.index()),
            )
        } else {
            (Vec::new(), empty_block(), Vec::new(), false, None)
        };
    materialize_provider_modules(
        &mut items,
        &mut source_order,
        module.children,
        provider,
        files,
    )?;
    let is_declaration = source_is_declaration
        || (!items.is_empty()
            && items
                .iter()
                .all(|item| matches!(item, Item::Mod(child) if child.is_decl))
            && chunk.stmts.is_empty());
    if let Some(source_file) = source_file.filter(|_| source_is_declaration) {
        validate_declaration_contents(&items, &chunk, source_file)?;
    }
    if is_declaration {
        mark_decl(&mut items);
    }
    Ok(ModDecl {
        name,
        documentation: None,
        items,
        chunk,
        source_order,
        is_pub: true,
        is_file: true,
        is_decl: is_declaration,
    })
}

fn empty_block() -> Block {
    Block {
        stmts: Vec::new(),
        statement_blank_before: Vec::new(),
        tail: None,
        tail_blank_before: false,
    }
}

/// Recursively load file modules under `items`. `dir` is the directory used to
/// resolve this scope's file modules; `None` disables file modules (e.g. when
/// compiling from an in-memory string). `files` is the compile-time file registry
/// (index = file id); each newly loaded file is appended and its AST spans are
/// stamped with the resulting id so diagnostics can attribute `path:line`.
#[cfg(test)]
pub fn resolve_modules(
    items: &mut [Item],
    dir: Option<&Path>,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    resolve_modules_with_libraries(items, dir, &[], &BTreeMap::new(), files)
}

/// Resolve filesystem modules with external declaration roots and explicit
/// logical root-module mounts.
#[cfg(test)]
pub fn resolve_modules_with_libraries(
    items: &mut [Item],
    dir: Option<&Path>,
    library: &[PathBuf],
    library_mounts: &BTreeMap<String, PathBuf>,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let mut loading = HashSet::new();
    resolve_modules_inner(
        items,
        dir,
        library,
        library_mounts,
        true,
        files,
        &mut loading,
    )
}

/// Resolve file modules through an IO-free project/provider contract.
#[cfg(test)]
pub fn resolve_modules_with_provider<P: SourceProvider>(
    items: &mut [Item],
    scope_dir: &LogicalSourcePath,
    project: &ProjectSpec,
    provider: &P,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    resolve_modules_with_provider_diagnostics(items, scope_dir, project, provider, files)
}

/// Resolve project modules while preserving machine-readable diagnostic data.
#[cfg(test)]
pub fn resolve_modules_with_provider_diagnostics<P: SourceProvider>(
    items: &mut [Item],
    scope_dir: &LogicalSourcePath,
    project: &ProjectSpec,
    provider: &P,
    files: &mut Vec<String>,
) -> Result<(), Diag> {
    let mut loading = HashSet::new();
    resolve_project_modules_inner(items, scope_dir, project, provider, files, &mut loading)
}

#[cfg(test)]
fn resolve_project_modules_inner<P: SourceProvider>(
    items: &mut [Item],
    scope_dir: &LogicalSourcePath,
    project: &ProjectSpec,
    provider: &P,
    files: &mut Vec<String>,
    loading: &mut HashSet<FileId>,
) -> Result<(), Diag> {
    for item in items.iter_mut() {
        let Item::Mod(module) = item else {
            continue;
        };
        let child_scope = scope_dir.join(&module.name).map_err(|error| {
            module_error(
                DiagnosticCode::NameInvalidModulePath,
                format!("module `{}`: {error}", module.name),
            )
        })?;
        if module.is_file {
            let (file_id, logical_path, is_declaration) =
                resolve_project_module(project, scope_dir, &module.name)?;
            if !loading.insert(file_id) {
                return Err(module_error(
                    DiagnosticCode::NameModuleCycle,
                    format!("module cycle while loading `{logical_path}`"),
                ));
            }
            ensure_file_registry(files, file_id, &logical_path);
            let source = provider.load(file_id).map_err(|error| {
                module_error(
                    DiagnosticCode::HostSourceRead,
                    format!("reading `{logical_path}`: {error}"),
                )
            })?;
            let mut program =
                crate::parser::parse_with_semantic_file(&source.text, file_id.index())
                    .map_err(parser_diagnostic)?;
            set_file_program(&mut program, file_id.index());
            module.items = program.items;
            module.chunk = program.chunk;
            module.source_order = program.source_order;
            module.is_decl = is_declaration;
            resolve_project_modules_inner(
                &mut module.items,
                &child_scope,
                project,
                provider,
                files,
                loading,
            )?;
            if is_declaration {
                validate_declaration_contents(&module.items, &module.chunk, file_id.index())?;
                mark_decl(&mut module.items);
            }
            loading.remove(&file_id);
        } else {
            resolve_project_modules_inner(
                &mut module.items,
                &child_scope,
                project,
                provider,
                files,
                loading,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
fn resolve_project_module(
    project: &ProjectSpec,
    scope_dir: &LogicalSourcePath,
    name: &str,
) -> Result<(FileId, LogicalSourcePath, bool), Diag> {
    let candidates = [
        (scope_dir.join(format!("{name}.rua")), false),
        (scope_dir.join(format!("{name}/mod.rua")), false),
        (scope_dir.join(format!("{name}.ruai")), true),
        (scope_dir.join(format!("{name}/mod.ruai")), true),
    ];
    let mut found = Vec::new();
    for (path, is_declaration) in candidates {
        let path = path.map_err(|error| {
            module_error(DiagnosticCode::NameInvalidModulePath, error.to_string())
        })?;
        if let Some(file_id) = project.file_for_path(&path) {
            found.push((file_id, path, is_declaration));
        }
    }
    if found.len() > 1 {
        return Err(Diag::bare(
            DiagnosticCode::NameAmbiguousImport,
            format!(
                "ambiguous module `{name}`: {}",
                found
                    .iter()
                    .map(|(_, path, _)| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    found.into_iter().next().ok_or_else(|| {
        module_error(
            DiagnosticCode::NameModuleNotFound,
            format!("cannot find file for module `{name}` under logical root `{scope_dir}`"),
        )
    })
}

fn module_error(code: DiagnosticCode, message: String) -> Diag {
    Diag::bare(code, message)
}

fn parser_diagnostic(error: crate::parser::ParseError) -> Diag {
    Diag::from_structured(
        error.diagnostic().clone(),
        error.line(),
        format!("parse error: {}", error.message()),
    )
}

fn ensure_file_registry(files: &mut Vec<String>, file_id: FileId, path: &LogicalSourcePath) {
    let index = file_id.index() as usize;
    if files.len() <= index {
        files.resize(index + 1, String::new());
    }
    files[index] = path.to_string();
}

#[cfg(test)]
fn resolve_modules_inner(
    items: &mut [Item],
    dir: Option<&Path>,
    library: &[PathBuf],
    library_mounts: &BTreeMap<String, PathBuf>,
    allow_library_mounts: bool,
    files: &mut Vec<String>,
    loading: &mut HashSet<PathBuf>,
) -> Result<(), Diag> {
    for item in items.iter_mut() {
        let Item::Mod(m) = item else { continue };
        if m.is_file {
            let dir = dir.ok_or_else(|| {
                module_error(
                    DiagnosticCode::NameModuleNotFound,
                    format!(
                        "`mod {};` (file module) requires compiling from a file, not a string",
                        m.name
                    ),
                )
            })?;
            let resolved = resolve_mod_file(dir, &m.name);
            let (file, child_dir, is_decl) = match resolved {
                Ok(resolved) => resolved,
                Err(error)
                    if allow_library_mounts
                        && error.diagnostic.code == DiagnosticCode::NameModuleNotFound
                        && library_mounts.contains_key(&m.name) =>
                {
                    resolve_library_mount(
                        &m.name,
                        library_mounts
                            .get(&m.name)
                            .expect("mount existence checked above"),
                    )?
                }
                Err(error)
                    if allow_library_mounts
                        && error.diagnostic.code == DiagnosticCode::NameModuleNotFound =>
                {
                    resolve_library_module(&m.name, library).map_err(|missing| {
                        if missing.diagnostic.code == DiagnosticCode::NameModuleNotFound {
                            error
                        } else {
                            missing
                        }
                    })?
                }
                Err(error) => return Err(error),
            };
            let canonical = file.canonicalize().map_err(|error| {
                module_error(
                    DiagnosticCode::HostSourceRead,
                    format!("canonicalizing {}: {error}", file.display()),
                )
            })?;
            if !loading.insert(canonical.clone()) {
                return Err(module_error(
                    DiagnosticCode::NameModuleCycle,
                    format!("module cycle while loading {}", canonical.display()),
                ));
            }
            let src = std::fs::read_to_string(&file).map_err(|error| {
                module_error(
                    DiagnosticCode::HostSourceRead,
                    format!("reading {}: {error}", file.display()),
                )
            })?;
            let id = files.len() as u32;
            files.push(file.display().to_string());
            let mut sub =
                crate::parser::parse_with_semantic_file(&src, id).map_err(parser_diagnostic)?;
            set_file_items(&mut sub.items, id);
            set_file_block(&mut sub.chunk, id);
            m.items = sub.items;
            m.chunk = sub.chunk;
            m.source_order = sub.source_order;
            // A `.ruai` module (and everything under it) is declaration-only.
            m.is_decl = is_decl;
            resolve_modules_inner(
                &mut m.items,
                Some(&child_dir),
                library,
                library_mounts,
                false,
                files,
                loading,
            )?;
            if is_decl {
                validate_declaration_contents(&m.items, &m.chunk, id)?;
                mark_decl(&mut m.items);
            }
            loading.remove(&canonical);
        } else {
            // Inline module: its file-children live under `dir/<name>/`.
            let child_dir = dir.map(|d| d.join(&m.name));
            resolve_modules_inner(
                &mut m.items,
                child_dir.as_deref(),
                library,
                library_mounts,
                false,
                files,
                loading,
            )?;
        }
    }
    Ok(())
}

// --- file-id stamping -------------------------------------------------------
//
// Freshly parsed file modules carry `file = 0` in every span (the parser is
// file-agnostic). After loading a file we walk its AST and stamp the correct
// file id onto each expression span, so a later diagnostic knows which file it
// came from even though all files are merged into one program.

fn set_file_items(items: &mut [Item], id: u32) {
    for it in items.iter_mut() {
        match it {
            Item::Fn(f) => {
                f.name_span.file = id;
                set_file_generics(&mut f.generics, id);
                set_file_signature(&mut f.params, f.ret.as_mut(), id);
                set_file_block(&mut f.body, id);
            }
            Item::Struct(s) => {
                set_file_generics(&mut s.generics, id);
                for field in &mut s.fields {
                    field.name_span.file = id;
                    set_file_type(&mut field.ty, id);
                }
            }
            Item::Enum(e) => {
                set_file_generics(&mut e.generics, id);
                for variant in &mut e.variants {
                    match &mut variant.kind {
                        VariantKind::Unit => {}
                        VariantKind::Tuple(types) => {
                            for ty in types {
                                set_file_type(ty, id);
                            }
                        }
                        VariantKind::Struct(fields) => {
                            for field in fields {
                                field.name_span.file = id;
                                set_file_type(&mut field.ty, id);
                            }
                        }
                    }
                }
            }
            Item::Impl(im) => {
                set_file_generics(&mut im.generics, id);
                for m in &mut im.methods {
                    m.name_span.file = id;
                    set_file_generics(&mut m.generics, id);
                    set_file_signature(&mut m.params, m.ret.as_mut(), id);
                    set_file_block(&mut m.body, id);
                }
            }
            Item::Trait(t) => {
                set_file_generics(&mut t.generics, id);
                for tm in &mut t.methods {
                    tm.name_span.file = id;
                    set_file_generics(&mut tm.generics, id);
                    set_file_signature(&mut tm.params, tm.ret.as_mut(), id);
                    if let Some(b) = &mut tm.default {
                        set_file_block(b, id);
                    }
                }
            }
            Item::Extern(block) => {
                for function in &mut block.fns {
                    function.name_span.file = id;
                    set_file_signature(&mut function.params, function.ret.as_mut(), id);
                }
            }
            // A nested inline module shares this file's id.
            Item::Mod(m) => {
                set_file_items(&mut m.items, id);
                set_file_block(&mut m.chunk, id);
            }
            Item::Use(_) => {}
        }
    }
}

pub(crate) fn set_file_program(program: &mut Program, file: u32) {
    set_file_items(&mut program.items, file);
    set_file_block(&mut program.chunk, file);
}

pub(crate) fn validate_declaration_program(program: &Program, file: u32) -> Result<(), Diag> {
    validate_declaration_contents(&program.items, &program.chunk, file)
}

fn validate_declaration_contents(items: &[Item], chunk: &Block, file: u32) -> Result<(), Diag> {
    if block_has_executable_body(chunk) {
        return Err(invalid_declaration(
            fallback_span(file),
            "declaration files cannot contain top-level executable statements".to_string(),
        ));
    }
    validate_declaration_items(items, file)
}

fn validate_declaration_items(items: &[Item], file: u32) -> Result<(), Diag> {
    for item in items {
        match item {
            Item::Fn(function) if block_has_executable_body(&function.body) => {
                return Err(invalid_declaration(
                    function.name_span,
                    format!(
                        "function `{}` in a declaration file must have an empty body",
                        function.name
                    ),
                ));
            }
            Item::Impl(implementation) => {
                for method in &implementation.methods {
                    if block_has_executable_body(&method.body) {
                        return Err(invalid_declaration(
                            method.name_span,
                            format!(
                                "method `{}` in a declaration file must have an empty body",
                                method.name
                            ),
                        ));
                    }
                }
            }
            Item::Trait(trait_decl) => {
                for method in &trait_decl.methods {
                    if method
                        .default
                        .as_ref()
                        .is_some_and(block_has_executable_body)
                    {
                        return Err(invalid_declaration(
                            method.name_span,
                            format!(
                                "trait method `{}` in a declaration file must have an empty body",
                                method.name
                            ),
                        ));
                    }
                }
            }
            Item::Mod(module) => {
                if block_has_executable_body(&module.chunk) {
                    return Err(invalid_declaration(
                        fallback_span(file),
                        format!(
                            "module `{}` in a declaration file cannot contain executable statements",
                            module.name
                        ),
                    ));
                }
                validate_declaration_items(&module.items, file)?;
            }
            Item::Fn(_) | Item::Struct(_) | Item::Enum(_) | Item::Extern(_) | Item::Use(_) => {}
        }
    }
    Ok(())
}

fn block_has_executable_body(block: &Block) -> bool {
    !block.stmts.is_empty() || block.tail.is_some()
}

fn fallback_span(file: u32) -> SourceRange {
    let mut span = SourceRange::new(0, 0, 1);
    span.file = file;
    span
}

fn invalid_declaration(span: SourceRange, message: String) -> Diag {
    Diag::new(
        DiagnosticCode::NameInvalidDeclaration,
        span.file,
        span.start,
        span.len,
        span.line,
        message,
    )
}

fn set_file_generics(generics: &mut [GenericParam], file: u32) {
    for generic in generics {
        generic.id.file = file;
        for bound in &mut generic.bounds {
            bound.id.file = file;
        }
    }
}

fn set_file_signature(parameters: &mut [Param], return_type: Option<&mut Type>, file: u32) {
    for parameter in parameters {
        parameter.name_span.file = file;
        set_file_type(&mut parameter.ty, file);
    }
    if let Some(return_type) = return_type {
        set_file_type(return_type, file);
    }
}

fn set_file_type(ty: &mut Type, file: u32) {
    match ty {
        Type::Path { id, args, .. } => {
            id.file = file;
            for argument in args {
                set_file_type(argument, file);
            }
        }
        Type::Ref { inner, .. } => set_file_type(inner, file),
        Type::Function { params, ret } => {
            for parameter in params {
                set_file_type(parameter, file);
            }
            set_file_type(ret, file);
        }
        Type::Tuple(items) => {
            for item in items {
                set_file_type(item, file);
            }
        }
        Type::Unit => {}
    }
}

fn set_file_block(b: &mut Block, id: u32) {
    for s in &mut b.stmts {
        set_file_stmt(s, id);
    }
    if let Some(t) = &mut b.tail {
        set_file_expr(t, id);
    }
}

fn set_file_stmt(s: &mut Stmt, id: u32) {
    match s {
        Stmt::Let {
            name_span,
            ty,
            init,
            ..
        } => {
            name_span.file = id;
            if let Some(ty) = ty {
                set_file_type(ty, id);
            }
            set_file_expr(init, id);
        }
        Stmt::Expr(e) => set_file_expr(e, id),
        Stmt::Return(Some(e)) => set_file_expr(e, id),
        Stmt::Return(None) => {}
        Stmt::While { cond, body } => {
            set_file_expr(cond, id);
            set_file_block(body, id);
        }
        Stmt::Loop { body } => set_file_block(body, id),
        Stmt::For {
            var_span,
            iter,
            body,
            ..
        } => {
            var_span.file = id;
            set_file_expr(iter, id);
            set_file_block(body, id);
        }
        Stmt::WhileLet {
            pat, expr, body, ..
        } => {
            set_file_pattern(pat, id);
            set_file_expr(expr, id);
            set_file_block(body, id);
        }
        Stmt::Break(value) => {
            if let Some(value) = value {
                set_file_expr(value, id);
            }
        }
        Stmt::Continue => {}
    }
}

fn set_file_expr(e: &mut Expr, id: u32) {
    e.id.file = id;
    e.span.file = id;
    match &mut e.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Str(_)
        | ExprKind::Bool(_)
        | ExprKind::Path(_) => {}
        ExprKind::Closure {
            params, ret, body, ..
        } => {
            for parameter in params {
                parameter.name_span.file = id;
                if let Some(ty) = &mut parameter.ty {
                    set_file_type(ty, id);
                }
            }
            if let Some(ret) = ret {
                set_file_type(ret, id);
            }
            match body {
                ClosureBody::Expr(expr) => set_file_expr(expr, id),
                ClosureBody::Block(block) => set_file_block(block, id),
            }
        }
        ExprKind::Unary { expr, .. } => set_file_expr(expr, id),
        ExprKind::Binary { lhs, rhs, .. } => {
            set_file_expr(lhs, id);
            set_file_expr(rhs, id);
        }
        ExprKind::Loop(body) => set_file_block(body, id),
        ExprKind::Call { callee, args } => {
            set_file_expr(callee, id);
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::MethodCall {
            recv,
            type_args,
            args,
            method_span,
            ..
        } => {
            method_span.file = id;
            set_file_expr(recv, id);
            for ty in type_args {
                set_file_type(ty, id);
            }
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::Field {
            base, name_span, ..
        } => {
            name_span.file = id;
            set_file_expr(base, id);
        }
        ExprKind::StructLit { fields, .. } => {
            for (_, v) in fields {
                set_file_expr(v, id);
            }
        }
        ExprKind::MapLit(entries) => {
            for (key, value) in entries {
                set_file_expr(key, id);
                set_file_expr(value, id);
            }
        }
        ExprKind::Try { expr } => set_file_expr(expr, id),
        ExprKind::Range { start, end, .. } => {
            set_file_expr(start, id);
            set_file_expr(end, id);
        }
        ExprKind::Index { base, index } => {
            set_file_expr(base, id);
            set_file_expr(index, id);
        }
        ExprKind::MacroCall { args, .. } => {
            for a in args {
                set_file_expr(a, id);
            }
        }
        ExprKind::If {
            cond,
            then_block,
            else_block,
        } => {
            set_file_expr(cond, id);
            set_file_block(then_block, id);
            set_file_else(else_block, id);
        }
        ExprKind::IfLet {
            pat,
            expr,
            then_block,
            else_block,
            ..
        } => {
            set_file_pattern(pat, id);
            set_file_expr(expr, id);
            set_file_block(then_block, id);
            set_file_else(else_block, id);
        }
        ExprKind::Block(b) => set_file_block(b, id),
        ExprKind::Assign { target, value, .. } => {
            set_file_expr(target, id);
            set_file_expr(value, id);
        }
        ExprKind::Match { scrut, arms } => {
            set_file_expr(scrut, id);
            for arm in arms {
                for pattern in &mut arm.pats {
                    set_file_pattern(pattern, id);
                }
                if let Some(g) = &mut arm.guard {
                    set_file_expr(g, id);
                }
                set_file_expr(&mut arm.body, id);
            }
        }
    }
}

fn set_file_pattern(pattern: &mut Pattern, file: u32) {
    match pattern {
        Pattern::Wildcard => {}
        Pattern::Binding(_, span) => span.file = file,
        Pattern::Literal(expression) => set_file_expr(expression, file),
        Pattern::Range { lo, hi, .. } => {
            set_file_expr(lo, file);
            set_file_expr(hi, file);
        }
        Pattern::Path { id, .. } => id.file = file,
        Pattern::TupleVariant { id, elems, .. } => {
            id.file = file;
            for element in elems {
                set_file_pattern(element, file);
            }
        }
        Pattern::StructVariant { id, fields, .. } => {
            id.file = file;
            for (_, field) in fields {
                set_file_pattern(field, file);
            }
        }
    }
}

fn set_file_else(else_block: &mut Option<Box<ElseBranch>>, id: u32) {
    match else_block.as_deref_mut() {
        Some(ElseBranch::Block(b)) => set_file_block(b, id),
        Some(ElseBranch::If(inner)) => set_file_expr(inner, id),
        None => {}
    }
}

/// Locate the file backing `mod <name>;` and the directory for its children,
/// plus whether it is a declaration-only `.ruai` file. Search order:
/// `dir/name.rua`, `dir/name/mod.rua`, then the `.ruai` equivalents.
#[cfg(test)]
fn resolve_mod_file(dir: &Path, name: &str) -> Result<(PathBuf, PathBuf, bool), Diag> {
    let child_dir = dir.join(name);
    let candidates = [
        (dir.join(format!("{name}.rua")), false),
        (child_dir.join("mod.rua"), false),
        (dir.join(format!("{name}.ruai")), true),
        (child_dir.join("mod.ruai"), true),
    ];
    let found = candidates
        .iter()
        .filter(|(path, _)| path.is_file())
        .collect::<Vec<_>>();
    if found.len() > 1 {
        return Err(module_error(
            DiagnosticCode::NameAmbiguousImport,
            format!(
                "ambiguous module `{name}`: {}",
                found
                    .iter()
                    .map(|(path, _)| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if let Some((file, is_decl)) = found.first() {
        return Ok((file.clone(), child_dir, *is_decl));
    }
    let flat = &candidates[0].0;
    let nested = &candidates[1].0;
    Err(module_error(
        DiagnosticCode::NameModuleNotFound,
        format!(
            "cannot find file for module `{}` (looked for {}, {} and their `.ruai` forms)",
            name,
            flat.display(),
            nested.display()
        ),
    ))
}

#[cfg(test)]
fn resolve_library_mount(name: &str, mount: &Path) -> Result<(PathBuf, PathBuf, bool), Diag> {
    if mount.is_file() {
        if mount.extension().and_then(|extension| extension.to_str()) != Some("ruai") {
            return Err(module_error(
                DiagnosticCode::HostProjectInvalid,
                format!(
                    "library mount `{name}` must point to a `.ruai` file or a directory, found {}",
                    mount.display()
                ),
            ));
        }
        let parent = mount.parent().unwrap_or_else(|| Path::new("."));
        return Ok((mount.to_path_buf(), parent.join(name), true));
    }

    if mount.is_dir() {
        let declaration = mount.join("mod.ruai");
        if declaration.is_file() {
            return Ok((declaration, mount.to_path_buf(), true));
        }
        return Err(module_error(
            DiagnosticCode::HostProjectInvalid,
            format!(
                "library mount `{name}` directory {} does not contain `mod.ruai`",
                mount.display()
            ),
        ));
    }

    Err(module_error(
        DiagnosticCode::HostProjectInvalid,
        format!("library mount `{name}` does not exist: {}", mount.display()),
    ))
}

#[cfg(test)]
fn resolve_library_module(
    name: &str,
    library: &[PathBuf],
) -> Result<(PathBuf, PathBuf, bool), Diag> {
    let mut found = Vec::new();
    for root in library {
        if root.is_file() {
            if root.file_stem().and_then(|stem| stem.to_str()) == Some(name)
                && root.extension().and_then(|extension| extension.to_str()) == Some("ruai")
            {
                let parent = root.parent().unwrap_or_else(|| Path::new("."));
                found.push((root.to_path_buf(), parent.join(name), true));
            }
            continue;
        }
        if !root.is_dir() {
            continue;
        }
        let child = root.join(name);
        for candidate in [root.join(format!("{name}.ruai")), child.join("mod.ruai")] {
            if candidate.is_file() {
                found.push((candidate, child.clone(), true));
            }
        }
    }
    if found.len() > 1 {
        return Err(module_error(
            DiagnosticCode::NameAmbiguousImport,
            format!(
                "ambiguous external module `{name}`: {}",
                found
                    .iter()
                    .map(|(path, _, _)| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    found.into_iter().next().ok_or_else(|| {
        module_error(
            DiagnosticCode::NameModuleNotFound,
            format!("cannot find external declaration module `{name}`"),
        )
    })
}

/// Recursively mark nested inline modules of a `.ruai` module as declaration-only
/// (file sub-modules are marked at load time in `resolve_modules`).
pub(crate) fn mark_decl(items: &mut [Item]) {
    for it in items.iter_mut() {
        if let Item::Mod(m) = it {
            m.is_decl = true;
            mark_decl(&mut m.items);
        }
    }
}
