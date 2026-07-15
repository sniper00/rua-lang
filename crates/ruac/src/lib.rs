//! Rua compiler library.
//!
//! Rua transpiles Rust-style source to readable Lua 5.5 (see
//! `docs/rua-design.md`). This crate owns the strict parser, resolved HIR,
//! identity-driven type checking, backend layout, and structured Lua IR.
//!
//! IDE semantics live in `rua-analysis`; this crate exposes only compiler
//! parsing, checking, and code-generation APIs.

pub mod ast;
mod backend_layout;
pub mod builtins;
pub mod check;
pub mod codegen;
pub mod diag;
pub mod hir;
pub mod lexer;
mod lua_ir;
pub mod parser;
pub mod resolve;
pub mod token;
pub mod tokenize;
pub mod typeck;
pub mod typed_ir;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{error::Error, fmt};

use crate::diag::Diag;
use rua_project::{ProjectSpec, SourceProvider};

/// Filesystem compilation inputs that are independent from Rua source text.
#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    /// Optional standard-library root containing `std.toml`.
    pub std_path: Option<PathBuf>,
    /// External `.ruai` files or directories searched by module filename.
    pub library: Vec<PathBuf>,
    /// Logical root module name to external `.ruai` file or module directory.
    pub library_mounts: BTreeMap<String, PathBuf>,
}

/// Load builtin `.ruai` declarations into a declaration-only semantic module.
///
/// With no explicit directory, the compiler uses its embedded sysroot. An
/// explicit directory replaces that sysroot and is never allowed to fail open.
pub fn load_builtins(program: &mut ast::Program, dir: Option<&Path>) -> Result<(), Diag> {
    let loaded = if let Some(dir) = dir {
        builtins::load_builtins_dir(dir)
    } else {
        builtins::load_embedded_builtins()
    }
    .map_err(|error| Diag::bare(rua_core::DiagnosticCode::HostBuiltinInvalid, error))?;
    program.standard_library = Some(loaded.metadata);
    for entry in &mut program.source_order {
        if let ast::ChunkEntry::Item(index) = entry {
            *index += 1;
        }
    }
    program.items.insert(
        0,
        ast::Item::Mod(ast::ModDecl {
            name: "__rua_builtin".to_string(),
            documentation: None,
            items: loaded.items,
            chunk: ast::Block {
                stmts: Vec::new(),
                statement_blank_before: Vec::new(),
                tail: None,
                tail_blank_before: false,
            },
            source_order: Vec::new(),
            is_pub: false,
            is_file: false,
            is_decl: true,
        }),
    );
    Ok(())
}

/// Compile Rua source text to Lua 5.5 source text. File modules (`mod name;`)
/// are not available here (no base directory); use [`compile_path`] for those.
/// Uses default builtins directory resolution.
pub fn compile_str(src: &str) -> Result<String, CompileFailure> {
    _compile_str(src, None)
}

/// Like [`compile_str`] but with an explicit builtins directory.
pub fn compile_str_with_builtins(src: &str, builtins_dir: &Path) -> Result<String, CompileFailure> {
    compile_str_with_std(src, builtins_dir)
}

/// Compile with an explicit standard-library root containing `std.toml`.
pub fn compile_str_with_std(src: &str, std_path: &Path) -> Result<String, CompileFailure> {
    _compile_str(src, Some(std_path))
}

/// Compile a complete logical project without filesystem access.
///
/// The project owns stable file identities and logical module paths; the host
/// supplies source text through [`SourceProvider`]. Builtins come from the
/// embedded sysroot, so this entry point has no CWD or disk dependency.
pub fn compile_project<P: SourceProvider>(
    project: &ProjectSpec,
    provider: &P,
) -> Result<String, CompileFailure> {
    compile_project_artifact(project, provider).map(|artifact| artifact.source)
}

/// Compile an IO-free logical project and retain generated-to-Rua source maps.
pub fn compile_project_artifact<P: SourceProvider>(
    project: &ProjectSpec,
    provider: &P,
) -> Result<codegen::GeneratedLua, CompileFailure> {
    compile_project_with_diagnostics(project, provider)
}

#[derive(Clone, Debug)]
pub struct CompileFailure {
    pub diagnostics: Vec<Diag>,
    pub files: Vec<String>,
}

impl CompileFailure {
    fn single(diagnostic: Diag, files: Vec<String>) -> Self {
        Self {
            diagnostics: vec![diagnostic],
            files,
        }
    }

    pub fn structured_diagnostics(
        &self,
    ) -> impl ExactSizeIterator<Item = &rua_core::StructuredDiagnostic> {
        self.diagnostics
            .iter()
            .map(|diagnostic| &diagnostic.diagnostic)
    }

    /// Convenience for callers migrating from text-only compiler errors.
    pub fn contains(&self, pattern: &str) -> bool {
        self.to_string().contains(pattern)
    }

    /// Convenience for callers migrating from text-only compiler errors.
    pub fn starts_with(&self, pattern: &str) -> bool {
        self.to_string().starts_with(pattern)
    }
}

impl fmt::Display for CompileFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&diag::render_all(&self.diagnostics, &self.files))
    }
}

impl Error for CompileFailure {}

/// Compile an IO-free project and return structured failures for host tooling.
pub fn compile_project_with_diagnostics<P: SourceProvider>(
    project: &ProjectSpec,
    provider: &P,
) -> Result<codegen::GeneratedLua, CompileFailure> {
    let fail = CompileFailure::single;
    let root_path = project
        .files
        .iter()
        .find_map(|(path, file_id)| (*file_id == project.root_file).then_some(path))
        .ok_or_else(|| {
            fail(
                Diag::bare(
                    rua_core::DiagnosticCode::HostProjectInvalid,
                    "project root_file is not present in ProjectSpec.files".to_string(),
                ),
                Vec::new(),
            )
        })?;
    let mut files = vec![String::new(); project.root_file.index() as usize + 1];
    files[project.root_file.index() as usize] = root_path.to_string();
    let source = provider.load(project.root_file).map_err(|error| {
        fail(
            Diag::bare(
                rua_core::DiagnosticCode::HostSourceRead,
                format!("reading `{root_path}`: {error}"),
            ),
            files.clone(),
        )
    })?;
    let mut program = parser::parse_with_semantic_file(&source.text, project.root_file.index())
        .map_err(|error| {
            fail(
                Diag::from_structured(
                    error.diagnostic().clone(),
                    error.line(),
                    format!("parse error: {}", error.message()),
                ),
                files.clone(),
            )
        })?;
    program.is_decl = root_path.as_str().ends_with(".ruai");
    resolve::set_file_program(&mut program, project.root_file.index());
    let scope_dir = root_path.parent().unwrap_or_default();
    resolve::resolve_modules_with_provider_diagnostics(
        &mut program.items,
        &scope_dir,
        project,
        provider,
        &mut files,
    )
    .map_err(|diagnostic| fail(diagnostic, files.clone()))?;
    if program.is_decl {
        resolve::validate_declaration_program(&program, project.root_file.index())
            .map_err(|diagnostic| fail(diagnostic, files.clone()))?;
        resolve::mark_decl(&mut program.items);
    }
    load_builtins(&mut program, None).map_err(|error| fail(error, files.clone()))?;
    let hir = hir::resolve(&program);
    let structural_diagnostics = check::collect_diags_resolved(&program, &hir);
    if !structural_diagnostics.is_empty() {
        return Err(CompileFailure {
            diagnostics: structural_diagnostics,
            files,
        });
    }
    let info = typeck::check_resolved_diagnostics(&program, &hir).map_err(|diagnostics| {
        CompileFailure {
            diagnostics,
            files: files.clone(),
        }
    })?;
    let typed = typed_ir::TypedProgram::new(program, hir, info);
    Ok(codegen::generate_with_source_map(
        &typed,
        &builtins::CodegenRules::default(),
    ))
}

fn _compile_str(src: &str, builtins_dir: Option<&Path>) -> Result<String, CompileFailure> {
    let mut files = vec![String::new()];
    let mut program = parser::parse(src).map_err(|error| {
        CompileFailure::single(
            Diag::from_structured(
                error.diagnostic().clone(),
                error.line(),
                format!("parse error: {}", error.message()),
            ),
            files.clone(),
        )
    })?;
    load_builtins(&mut program, builtins_dir)
        .map_err(|error| CompileFailure::single(error, files.clone()))?;
    resolve::resolve_modules(&mut program.items, None, &mut files)
        .map_err(|error| CompileFailure::single(error, files.clone()))?;
    let hir = hir::resolve(&program);
    check::check_resolved(&program, &hir).map_err(|diagnostics| CompileFailure {
        diagnostics,
        files: files.clone(),
    })?;
    let info = typeck::check_resolved(&program, &hir).map_err(|diagnostics| CompileFailure {
        diagnostics,
        files: files.clone(),
    })?;
    let rules = builtins::CodegenRules::default();
    let typed = typed_ir::TypedProgram::new(program, hir, info);
    Ok(codegen::generate(&typed, &rules))
}

/// Compile a Rua source file (resolving `mod name;` file modules relative to the
/// file's directory) to Lua 5.5 source text. Uses default builtins resolution.
pub fn compile_path(path: &Path) -> Result<String, CompileFailure> {
    compile_path_artifact(path).map(|artifact| artifact.source)
}

/// Compile a source file with explicit standard-library and external-library
/// inputs.
pub fn compile_path_with_options(
    path: &Path,
    options: &CompileOptions,
) -> Result<String, CompileFailure> {
    compile_path_artifact_with_options(path, options).map(|artifact| artifact.source)
}

/// Like [`compile_path`] but with an explicit builtins directory.
pub fn compile_path_with_builtins(
    path: &Path,
    builtins_dir: &Path,
) -> Result<String, CompileFailure> {
    compile_path_with_std(path, builtins_dir)
}

/// Compile a source file with an explicit `std.toml` root.
pub fn compile_path_with_std(path: &Path, std_path: &Path) -> Result<String, CompileFailure> {
    compile_path_artifact_with_std(path, std_path).map(|artifact| artifact.source)
}

/// Compile a Rua source file and retain generated-to-source mappings.
pub fn compile_path_artifact(path: &Path) -> Result<codegen::GeneratedLua, CompileFailure> {
    compile_path_artifact_with_options(path, &CompileOptions::default())
}

/// Like [`compile_path_artifact`] but with an explicit builtins directory.
pub fn compile_path_artifact_with_builtins(
    path: &Path,
    builtins_dir: &Path,
) -> Result<codegen::GeneratedLua, CompileFailure> {
    compile_path_artifact_with_std(path, builtins_dir)
}

/// Compile a source file with an explicit `std.toml` root and retain mappings.
pub fn compile_path_artifact_with_std(
    path: &Path,
    std_path: &Path,
) -> Result<codegen::GeneratedLua, CompileFailure> {
    compile_path_artifact_with_options(
        path,
        &CompileOptions {
            std_path: Some(std_path.to_path_buf()),
            library: Vec::new(),
            library_mounts: BTreeMap::new(),
        },
    )
}

/// Compile a source file with explicit inputs and retain source mappings.
pub fn compile_path_artifact_with_options(
    path: &Path,
    options: &CompileOptions,
) -> Result<codegen::GeneratedLua, CompileFailure> {
    let (mut program, files) =
        parse_and_load_modules_with_libraries(path, &options.library, &options.library_mounts)?;
    load_builtins(&mut program, options.std_path.as_deref())
        .map_err(|error| CompileFailure::single(error, files.clone()))?;
    let hir = hir::resolve(&program);
    check::check_resolved(&program, &hir).map_err(|diagnostics| CompileFailure {
        diagnostics,
        files: files.clone(),
    })?;
    let info = typeck::check_resolved(&program, &hir).map_err(|diagnostics| CompileFailure {
        diagnostics,
        files: files.clone(),
    })?;
    let rules = builtins::CodegenRules::default();
    let typed = typed_ir::TypedProgram::new(program, hir, info);
    Ok(codegen::generate_with_source_map(&typed, &rules))
}

/// Parse a file and splice in its file modules, without semantic checks. Returns
/// the merged program plus the file registry (index = file id) for diagnostics.
pub fn parse_and_resolve(path: &Path) -> Result<(ast::Program, Vec<String>), CompileFailure> {
    parse_and_load_modules(path)
}

pub fn parse_and_load_modules(path: &Path) -> Result<(ast::Program, Vec<String>), CompileFailure> {
    parse_and_load_modules_with_libraries(path, &[], &BTreeMap::new())
}

fn parse_and_load_modules_with_libraries(
    path: &Path,
    library: &[PathBuf],
    library_mounts: &BTreeMap<String, PathBuf>,
) -> Result<(ast::Program, Vec<String>), CompileFailure> {
    let files = vec![path.display().to_string()];
    let src = std::fs::read_to_string(path).map_err(|error| {
        CompileFailure::single(
            Diag::bare(
                rua_core::DiagnosticCode::HostSourceRead,
                format!("reading {}: {error}", path.display()),
            ),
            files.clone(),
        )
    })?;
    let mut program = parser::parse(&src).map_err(|error| {
        CompileFailure::single(
            Diag::from_structured(
                error.diagnostic().clone(),
                error.line(),
                format!("parse error: {}", error.message()),
            ),
            files.clone(),
        )
    })?;
    program.is_decl = path
        .extension()
        .is_some_and(|extension| extension == "ruai");
    // The root file is id 0; child files are appended during resolution.
    let mut files = files;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    resolve::resolve_modules_with_libraries(
        &mut program.items,
        Some(dir),
        library,
        library_mounts,
        &mut files,
    )
    .map_err(|error| CompileFailure::single(error, files.clone()))?;
    if program.is_decl {
        resolve::validate_declaration_program(&program, 0)
            .map_err(|error| CompileFailure::single(error, files.clone()))?;
        resolve::mark_decl(&mut program.items);
    }
    Ok((program, files))
}

/// Run parse + resolve + structural-check + typeck on an in-memory source string
/// and return every diagnostic with its byte-offset span (if available), plus the
/// file registry (index = file id, for resolving `Diag.file` to a path).
///
/// Compiler-native structured diagnostics for an in-memory source.
///
/// Production LSP diagnostics come from `rua-analysis`; hosts that explicitly
/// request compiler checks can consume this API without parsing CLI text.
pub fn check_diagnostics(src: &str) -> (Vec<Diag>, Vec<String>) {
    let mut diags: Vec<Diag> = Vec::new();

    // 1. Parse
    let mut program = match parser::parse(src) {
        Ok(p) => p,
        Err(e) => {
            diags.push(Diag::from_structured(
                e.diagnostic().clone(),
                e.line(),
                format!("parse error: {}", e.message()),
            ));
            return (diags, vec![String::new()]);
        }
    };
    if let Err(error) = load_builtins(&mut program, None) {
        diags.push(error);
        return (diags, vec![String::new()]);
    }

    // 2. Resolve (file modules will fail for in-memory sources — that's OK).
    // File id 0 with an empty path: diagnostics fall back to `line: msg`.
    let mut files = vec![String::new()];
    if let Err(error) = resolve::resolve_modules(&mut program.items, None, &mut files) {
        diags.push(error);
        // Continue with what we have — structural/type checks may still find
        // issues in the rest of the program.
    }
    let hir = hir::resolve(&program);

    // 3. Structural checks.
    diags.extend(check::collect_diags_resolved(&program, &hir));

    // 4. Type-checking.
    diags.extend(typeck::collect_diags_resolved(&program, &hir));

    (diags, files)
}

/// Backward-compatible name for [`check_diagnostics`].
pub fn check_diags(src: &str) -> (Vec<Diag>, Vec<String>) {
    check_diagnostics(src)
}

#[cfg(test)]
mod tests;
