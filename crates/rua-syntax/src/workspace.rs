//! Cross-file workspace index for go-to-def, hover, references, and rename.
//!
//! Owns multiple [`Analysis`](crate::analysis::Analysis) instances (keyed by
//! canonical path) plus a module graph. Disk-agnostic and testable via a
//! [`FileLoader`] callback.
//!
//! # Architecture
//!
//! - **W1 (lazy go-to-def / hover)**: When single-file resolution fails, checks
//!   whether the cursor is on a path whose prefix resolves to a file module;
//!   lazily loads that module's file and resolves the target segment in its
//!   symbol table. No up-front full-workspace scan needed.
//! - **W2 (references / rename)**: Uses W1 to find the canonical definition,
//!   then searches all indexed files for matching references. Local bindings
//!   stay single-file; item references span the workspace.
//!
//! `rowan` never leaks through the public API — all return types are plain data
//! (paths, byte ranges, strings).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::analysis::{Analysis, CompletionMember};
use crate::ast::{Item, Named, SourceFile};
use crate::nameres::{self, RefKind, RenameError};
use crate::symbols::Symbol;

// --- FileLoader ---------------------------------------------------------------

/// Content provider for files in the workspace.
///
/// LSP implementations check open buffers first (so unsaved changes are seen),
/// then fall back to disk. Tests use in-memory string maps.
pub trait FileLoader {
    fn load(&self, path: &Path) -> Option<String>;

    /// List every `.rua` / `.ruai` source under `root` (recursively) so the
    /// workspace can eagerly index files that are never explicitly opened —
    /// without which cross-file references / rename would silently miss them.
    ///
    /// The default returns empty: loaders that cannot enumerate their backing
    /// store opt out of eager indexing (queries still work lazily).
    fn list_sources(&self, _root: &Path) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// A [`FileLoader`] that reads from the filesystem via [`std::fs::read_to_string`].
pub struct DiskLoader;

impl FileLoader for DiskLoader {
    fn load(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn list_sources(&self, root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk_rua_files(root, &mut out);
        out
    }
}

/// Recursively collect `.rua` / `.ruai` files under `dir`, skipping hidden
/// directories (`.git`, etc.) and the Rust `target/` build dir.
fn walk_rua_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.') || n == "target");
            if !skip {
                walk_rua_files(&path, out);
            }
        } else if file_type.is_file() {
            let is_rua = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "rua" || e == "ruai");
            if is_rua {
                out.push(path);
            }
        }
    }
}

// --- Workspace ----------------------------------------------------------------

/// Grouped rename edits: file path → list of `(start, end, replacement)`.
pub type RenameEdits = HashMap<PathBuf, Vec<(usize, usize, String)>>;

/// Cross-file workspace: owns per-file [`Analysis`] caches and resolves
/// symbols across file boundaries.
///
/// Type parameter `L` is the [`FileLoader`] — use [`DiskLoader`] for production
/// or a custom impl for tests.
pub struct Workspace<L: FileLoader> {
    loader: L,
    /// Per-file analysis cache, keyed by canonical file path.
    analyses: HashMap<PathBuf, Analysis>,
    /// Direct source overrides (LSP open buffers). These take priority over
    /// the [`FileLoader`] so unsaved changes are always seen.
    direct_sources: HashMap<PathBuf, String>,
}

impl<L: FileLoader> Workspace<L> {
    /// Create a new workspace backed by `loader`.
    pub fn new(loader: L) -> Self {
        Workspace {
            loader,
            analyses: HashMap::new(),
            direct_sources: HashMap::new(),
        }
    }

    /// Register (or replace) an in-memory source for `path`. Subsequent calls
    /// to [`analysis_of`](Self::analysis_of) will use this text instead of
    /// consulting the [`FileLoader`].
    ///
    /// Used by LSP `didOpen`/`didChange` to keep unsaved buffers visible.
    pub fn add_file(&mut self, path: &Path, src: &str) {
        let canonical = normalize_path(path);
        self.direct_sources
            .insert(canonical.clone(), src.to_string());
        // Invalidate any cached analysis so it picks up the new text.
        self.analyses.remove(&canonical);
    }

    /// Remove the in-memory source for `path` (LSP `didClose`) and drop its
    /// cached analysis, so the next access re-reads the on-disk content via the
    /// [`FileLoader`] instead of serving the (possibly unsaved) buffer.
    pub fn remove_file(&mut self, path: &Path) {
        let canonical = normalize_path(path);
        self.direct_sources.remove(&canonical);
        self.analyses.remove(&canonical);
    }

    /// Discard the cached [`Analysis`] for `path`. The next access will
    /// re-parse from the current source (direct override or disk).
    pub fn invalidate(&mut self, path: &Path) {
        let canonical = normalize_path(path);
        self.analyses.remove(&canonical);
    }

    /// Get (or lazily load) the [`Analysis`] for `path`.
    ///
    /// Returns `None` when no source is available (neither a direct override
    /// nor a successful [`FileLoader::load`]).
    pub fn analysis_of(&mut self, path: &Path) -> Option<&Analysis> {
        let canonical = normalize_path(path);
        if !self.analyses.contains_key(&canonical) {
            let src = if let Some(s) = self.direct_sources.get(&canonical) {
                Some(s.clone())
            } else {
                self.loader.load(&canonical)
            }?;
            let analysis = Analysis::new(&src);
            self.analyses.insert(canonical.clone(), analysis);
        }
        self.analyses.get(&canonical)
    }

    /// Eagerly index every `.rua` / `.ruai` source under `root` so cross-file
    /// references and rename see files the user never opened. Without this,
    /// [`references`](Self::references) and [`rename_edits`](Self::rename_edits)
    /// only cover files already loaded into the cache, silently under-reporting.
    ///
    /// Returns the number of files newly loaded. Best-effort: unreadable or
    /// unparseable files are skipped.
    pub fn index_root(&mut self, root: &Path) -> usize {
        let mut loaded = 0;
        for path in self.loader.list_sources(root) {
            if self.analysis_of(&path).is_some() {
                loaded += 1;
            }
        }
        loaded
    }

    /// Number of files currently cached.
    pub fn len(&self) -> usize {
        self.analyses.len()
    }

    /// True when no files have been loaded yet.
    pub fn is_empty(&self) -> bool {
        self.analyses.is_empty()
    }

    // --- W1: go-to-definition / hover -----------------------------------------

    /// Resolve the identifier at byte `offset` in `file` to its canonical
    /// definition, potentially crossing file boundaries.
    ///
    /// Returns `(target_file, name_range, kind, detail)` on success.
    pub fn goto_definition(
        &mut self,
        file: &Path,
        offset: usize,
    ) -> Option<(PathBuf, (usize, usize), RefKind, String)> {
        let analysis = self.analysis_of(file)?;
        let file_canon = normalize_path(file);

        // 1. Try single-file resolution first (covers same-file items, locals,
        //    and same-file member access via the single-file member index).
        if let Some(res) = analysis.definition_at(offset) {
            return Some((file_canon, res.target_range, res.kind, res.detail));
        }

        // 2. Cross-file: check whether we're on a path whose prefix is a file module.
        if let Some(res) = self.cross_file_definition(file, offset) {
            return Some(res);
        }

        // 3. Cross-file member access: the receiver's type is defined in another
        //    file, so the single-file member index (step 1) couldn't resolve it.
        self.cross_file_member(&file_canon, offset)
            .map(|(path, range, detail)| (path, range, RefKind::Item, detail))
    }

    /// The member access (`x.field` / `x.method()`) at `offset` in `file`,
    /// resolving the receiver's type across files. Returns
    /// `(target_file, name_range, detail)` pointing at the field/method
    /// definition, or `None` when `offset` is not on a resolvable member.
    ///
    /// Checks the single-file member index first (cheap, cached); only falls
    /// back to a multi-file type-check when the receiver type lives elsewhere.
    pub fn member_at(
        &mut self,
        file: &Path,
        offset: usize,
    ) -> Option<(PathBuf, (usize, usize), String)> {
        let file_canon = normalize_path(file);
        if let Some(a) = self.analysis_of(&file_canon)
            && let Some(m) = a.member_at(offset)
        {
            return Some((
                file_canon,
                (m.target_start, m.target_start + m.target_len),
                m.detail.clone(),
            ));
        }
        self.cross_file_member(&file_canon, offset)
    }

    /// Hover text for the identifier at `offset` in `file`.
    pub fn hover(&mut self, file: &Path, offset: usize) -> Option<String> {
        self.goto_definition(file, offset)
            .map(|(_, _, _, detail)| detail)
    }

    /// Cross-file field/method completions for `x.` at `offset` in `file`. Uses the
    /// multi-file type-check so receivers whose type lives in another file resolve.
    ///
    /// `None` = not a member slot (caller falls back to global completion);
    /// `Some(list)` = member slot (empty list when the receiver type is unknown,
    /// so the caller can suppress globals after the `.`).
    pub fn member_completions(
        &mut self,
        file: &Path,
        offset: usize,
    ) -> Option<Vec<CompletionMember>> {
        let file_canon = normalize_path(file);
        let ctx = crate::completion::completion_context(self.analysis_of(&file_canon)?.source_file(), offset)?;
        let src = self.source_text(&file_canon)?;
        let repaired = crate::completion::repair(&src, &ctx);
        Some(crate::transition::member_completions_src(
            &repaired,
            &file_canon,
            ctx.receiver_end,
        ))
    }

    /// Path-context completions (`Type::` / `mod::`) for `offset` in `file`.
    ///
    /// `None` = not a path slot (caller falls back to global completion);
    /// `Some(list)` = path slot naming a known container (empty list when it has
    /// no members, so the caller can suppress globals after the `::`).
    pub fn path_completions(&mut self, file: &Path, offset: usize) -> Option<Vec<Symbol>> {
        let file_canon = normalize_path(file);
        self.analysis_of(&file_canon)?.path_completions(offset)
    }

    // --- W2: references / rename ----------------------------------------------

    /// Find all references to the definition at `offset` in `file`, across
    /// all indexed files.
    ///
    /// Returns `(file, byte_range)` pairs sorted by (file, offset).
    pub fn references(
        &mut self,
        file: &Path,
        offset: usize,
        include_decl: bool,
    ) -> Vec<(PathBuf, (usize, usize))> {
        // 1. Find the canonical definition.
        let (def_file, def_range, kind, _detail) = match self.goto_definition(file, offset) {
            Some(d) => d,
            None => return Vec::new(),
        };

        // 2. Get the definition name.
        let def_name = match self.analysis_of(&def_file) {
            Some(def_analysis) => {
                let src = def_analysis.text();
                let end = def_range.1.min(src.len());
                if def_range.0 >= end {
                    return Vec::new();
                }
                src[def_range.0..end].to_string()
            }
            None => return Vec::new(),
        };

        // 3. Local bindings: only search within the defining file.
        if kind == RefKind::Local {
            let def_analysis = match self.analysis_of(&def_file) {
                Some(a) => a,
                None => return Vec::new(),
            };
            let mut refs = def_analysis.references_at(def_range.0);
            if !include_decl {
                refs.retain(|r| *r != def_range);
            }
            return refs
                .into_iter()
                .map(|r| (def_file.clone(), r))
                .collect();
        }

        // 4. Item references: search all indexed files.
        let mut results: Vec<(PathBuf, (usize, usize))> = Vec::new();

        // Collect candidate (file, name_range) pairs first (cannot hold
        // `self.analyses` borrow while calling `goto_definition`).
        let mut candidates: Vec<(PathBuf, (usize, usize))> = Vec::new();

        // Include the definition site itself.
        if include_decl {
            candidates.push((def_file.clone(), def_range));
        }

        // Scan all known files for idents with matching name.
        // (Use ident_offsets_by_name to find ALL idents, not just definition symbols.)
        let known: Vec<PathBuf> = self.analyses.keys().cloned().collect();
        for candidate_file in &known {
            let Some(candidate_analysis) = self.analyses.get(candidate_file) else {
                continue;
            };
            let offsets = candidate_analysis.ident_offsets_by_name(&def_name);
            for off in offsets {
                let range = (off, off + def_name.len());
                if range != def_range || candidate_file != &def_file {
                    candidates.push((candidate_file.clone(), range));
                }
            }
        }

        // Now resolve each candidate using workspace-level goto_definition.
        for (candidate_file, candidate_range) in candidates {
            let candidate_offset = candidate_range.0;
            if let Some((resolved_file, resolved_range, resolved_kind, _)) =
                self.goto_definition(&candidate_file, candidate_offset)
                && resolved_kind == kind
                && resolved_file == def_file
                && resolved_range == def_range
            {
                results.push((candidate_file, candidate_range));
            }
        }
        results.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.0.cmp(&b.1.0)));
        results.dedup();
        results
    }

    /// Produce rename edits for the identifier at `offset` in `file`, covering
    /// all references across the workspace.
    ///
    /// Returns a map from file path to `(start, end, replacement)` edits.
    /// Renaming inside `.ruai` declaration files is rejected.
    pub fn rename_edits(
        &mut self,
        file: &Path,
        offset: usize,
        new_name: &str,
    ) -> Result<RenameEdits, RenameError> {
        if !nameres::is_valid_ident(new_name) {
            return Err(RenameError::InvalidName);
        }

        // Get all references (including declaration).
        let refs = self.references(file, offset, true);
        if refs.is_empty() {
            return Err(RenameError::NoReferences);
        }

        // .ruai protection: reject renames that touch .ruai files.
        for (ref_file, _) in &refs {
            if is_ruai_file(ref_file) {
                return Err(RenameError::InvalidName); // Reuse InvalidName as "not allowed"
            }
        }

        // Group edits by file.
        let mut by_file: HashMap<PathBuf, Vec<(usize, usize, String)>> = HashMap::new();
        for (ref_file, (start, end)) in refs {
            by_file
                .entry(ref_file)
                .or_default()
                .push((start, end, new_name.to_string()));
        }

        // Sort edits within each file by ascending start offset. All edits are
        // equal-length, non-overlapping identifier replacements, so order does
        // not affect the result; ascending is the LSP-conventional ordering.
        for edits in by_file.values_mut() {
            edits.sort_by_key(|(s, _, _)| *s);
        }

        Ok(by_file)
    }
}

// --- cross-file resolution ----------------------------------------------------

impl<L: FileLoader> Workspace<L> {
    /// Current source text for `path`: the open buffer if present, else the
    /// loader's on-disk content.
    fn source_text(&self, path: &Path) -> Option<String> {
        let canonical = normalize_path(path);
        if let Some(s) = self.direct_sources.get(&canonical) {
            Some(s.clone())
        } else {
            self.loader.load(&canonical)
        }
    }

    /// Multi-file member resolution: type-check the `mod` tree rooted at
    /// `file_canon` (root text from the open buffer / disk; child modules from
    /// disk) and look up the member at `offset`.
    ///
    /// The cursor file is root (file id 0), so `offset` indexes directly; the
    /// hit's `target_file` is translated back to a path via the returned file
    /// registry. Returns `None` on parse/type errors or a miss.
    fn cross_file_member(
        &mut self,
        file_canon: &Path,
        offset: usize,
    ) -> Option<(PathBuf, (usize, usize), String)> {
        let root_src = self.source_text(file_canon)?;
        let (index, files) = crate::transition::member_index_src(&root_src, file_canon);
        let hit = index.at(0, offset)?;
        let target_path = files
            .get(hit.target_file as usize)
            .map(|p| normalize_path(Path::new(p)))?;
        Some((
            target_path,
            (hit.target_start, hit.target_start + hit.target_len),
            hit.detail.clone(),
        ))
    }

    /// Attempt cross-file definition resolution when single-file resolution
    /// returned `None`.
    fn cross_file_definition(
        &mut self,
        file: &Path,
        offset: usize,
    ) -> Option<(PathBuf, (usize, usize), RefKind, String)> {
        let analysis = self.analysis_of(file)?;

        // 1. Get the ident at offset.
        let hit = analysis.ident_at_offset(offset)?;
        let name = &hit.text;

        // 2. Check whether this ident is inside a PathExpr or PathType.
        let path_ctx =
            nameres::path_context(analysis.source_file(), offset)?;
        let segments = &path_ctx.segments;
        if segments.is_empty() {
            return None;
        }

        // 3. Find which segment the cursor is on.
        let cursor_idx = segments
            .iter()
            .position(|(seg_name, seg_range)| {
                seg_name == name
                    && offset >= seg_range.0
                    && offset < seg_range.1
            })?;

        // 4. Build the module chain from prefix segments, loading file
        //    modules as needed.
        let file_canon = normalize_path(file);
        let current_dir = file_canon.parent().map(Path::to_path_buf);

        // Start with the module context at the cursor position (e.g. if
        // cursor is inside `mod a { ... }`, container = ["a"]).
        let mod_ctx = nameres::module_context(analysis.source_file(), offset);

        // (current_file_path, remaining_container_for_this_file)
        let mut ctx_file = file_canon.clone();
        let mut ctx_container: Vec<String> = mod_ctx.clone();

        // Walk prefix segments (0 .. cursor_idx), resolving each.
        for (seg_name, _seg_range) in &segments[..cursor_idx] {
            // Try to resolve this segment in the current context.
            let ctx_analysis = self.analysis_of(&ctx_file)?;
            let ctx_symbols = ctx_analysis.symbols();

            let sym = find_symbol_in(seg_name, &ctx_container, ctx_symbols)?;

            if sym.kind == crate::symbols::SymbolKind::Module {
                // Check if this module is a file module (body in another file).
                if is_file_module(ctx_analysis.source_file(), seg_name, &ctx_container) {
                    // Resolve and load the file module.
                    let dir = if ctx_container.is_empty() {
                        current_dir.clone()?
                    } else {
                        // Build directory from container path.
                        let mut d = current_dir.clone()?;
                        for c in &ctx_container {
                            d = d.join(c);
                        }
                        d
                    };
                    let (mod_file, _is_decl) = resolve_mod_file_loader(&self.loader, &dir, seg_name)?;
                    ctx_file = mod_file;
                    ctx_container = Vec::new(); // Reset — top level of loaded file.
                    // Ensure the module file is loaded.
                    self.analysis_of(&ctx_file)?;
                } else {
                    // Inline module: push to container and continue in same file.
                    ctx_container.push(seg_name.clone());
                }
            } else {
                // Not a module — continue resolution in same file with updated container.
                ctx_container.push(seg_name.clone());
            }
        }

        // 5. Resolve the cursor's segment in the final context.
        let cursor_name = &segments[cursor_idx].0;
        let final_analysis = self.analysis_of(&ctx_file)?;
        let final_symbols = final_analysis.symbols();

        if let Some(sym) = find_symbol_in(cursor_name, &ctx_container, final_symbols) {
            return Some((
                ctx_file,
                sym.name_range,
                RefKind::Item,
                sym.detail.clone(),
            ));
        }

        // 6. If not found, also try in the parent container (walking up).
        let mut parent_ctx = ctx_container.clone();
        while !parent_ctx.is_empty() {
            parent_ctx.pop();
            if let Some(sym) =
                find_symbol_in(cursor_name, &parent_ctx, final_symbols)
            {
                return Some((
                    ctx_file.clone(),
                    sym.name_range,
                    RefKind::Item,
                    sym.detail.clone(),
                ));
            }
        }

        None
    }
}

// --- helpers ------------------------------------------------------------------

/// Look up a symbol by name and container path in a slice.
fn find_symbol_in(
    name: &str,
    container: &[String],
    symbols: &[crate::symbols::Symbol],
) -> Option<crate::symbols::Symbol> {
    symbols
        .iter()
        .find(|s| s.name == name && s.container == container)
        .cloned()
}

/// Check whether a module name in the given container path is a file module
/// (`mod name;` as opposed to `mod name { ... }`).
fn is_file_module(
    file: &SourceFile,
    name: &str,
    container: &[String],
) -> bool {
    let items = items_in_container(file, container);
    items.into_iter().any(|item| {
        if let Item::Mod(m) = item {
            m.is_file() && m.name_text().as_deref() == Some(name)
        } else {
            false
        }
    })
}

/// Collect all items visible in a specific container path within `file`.
fn items_in_container(file: &SourceFile, container: &[String]) -> Vec<Item> {
    if container.is_empty() {
        return file.items().collect();
    }

    fn descend(items: Vec<Item>, path: &[String]) -> Vec<Item> {
        if path.is_empty() {
            return items;
        }
        let target = &path[0];
        for item in &items {
            if let Item::Mod(m) = item
                && m.name_text().as_deref() == Some(target.as_str())
                && !m.is_file()
            {
                let child_items: Vec<Item> = m.items().collect();
                return descend(child_items, &path[1..]);
            }
        }
        Vec::new()
    }

    let top: Vec<Item> = file.items().collect();
    descend(top, container)
}

/// File-module resolution following the same rules as
/// `ruac/src/resolve.rs::resolve_mod_file`, but using the [`FileLoader`]
/// so in-memory tests work without real files.
///
/// Returns `(file_path, is_decl)` on success.
fn resolve_mod_file_loader<L: FileLoader>(
    loader: &L,
    dir: &Path,
    name: &str,
) -> Option<(PathBuf, bool)> {
    let candidates = mod_file_candidates(dir, name);
    for (path, is_decl) in &candidates {
        // Try disk existence first (fast path for real files).
        if path.exists()
            && let Some(canon) = canonicalize_existing(path)
        {
            return Some((canon, *is_decl));
        }
        // Try the loader (for in-memory / virtual files).
        if loader.load(path).is_some() {
            return Some((normalize_path(path), *is_decl));
        }
    }
    None
}

/// Ordered candidate paths for `mod <name>;` in directory `dir`.
fn mod_file_candidates(dir: &Path, name: &str) -> [(PathBuf, bool); 4] {
    let child_dir = dir.join(name);
    [
        (dir.join(format!("{name}.rua")), false),
        (child_dir.join("mod.rua"), false),
        (dir.join(format!("{name}.ruai")), true),
        (child_dir.join("mod.ruai"), true),
    ]
}

/// Normalize a path: canonicalize on Unix, handling non-existent paths
/// by manually normalizing `.` and `..` components.
///
/// Public so callers (e.g. the LSP) can compare a workspace-returned path
/// against a document key under the same normalization rules.
pub fn normalize_path(path: &Path) -> PathBuf {
    // Try real canonicalization first.
    if let Ok(canon) = std::fs::canonicalize(path) {
        return clean_path(canon);
    }
    // Fallback for non-existent paths (tests / uncreated files).
    let mut components = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    let mut result = PathBuf::new();
    for c in components {
        result.push(c);
    }
    clean_path(result)
}

/// Canonicalize an existing path.
fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(path).ok().map(clean_path)
}

/// Strip trailing slashes so `foo/` and `foo` compare equal.
fn clean_path(p: PathBuf) -> PathBuf {
    // On Unix, canonical paths never have trailing slashes.
    // But for in-memory paths, strip trailing separator.
    let s = p.to_string_lossy();
    if s.ends_with(std::path::MAIN_SEPARATOR) && s.len() > 1 {
        PathBuf::from(&s[..s.len() - 1])
    } else {
        p
    }
}

/// Check if a path has a `.ruai` extension.
fn is_ruai_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == "ruai")
}

// --- tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An in-memory [`FileLoader`] backed by a `HashMap<PathBuf, String>`.
    struct MapLoader {
        files: HashMap<PathBuf, String>,
    }

    impl MapLoader {
        fn new(files: HashMap<PathBuf, String>) -> Self {
            MapLoader { files }
        }
    }

    impl FileLoader for MapLoader {
        fn load(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }

        fn list_sources(&self, root: &Path) -> Vec<PathBuf> {
            // In-memory loader: every key under `root` (prefix match, or all
            // keys when `root` is empty/`.`) counts as discoverable.
            self.files
                .keys()
                .filter(|p| {
                    root.as_os_str().is_empty()
                        || root == Path::new(".")
                        || p.starts_with(root)
                })
                .cloned()
                .collect()
        }
    }

    /// Build a workspace from a set of in-memory files.
    fn workspace_with(files: &[(&str, &str)]) -> Workspace<MapLoader> {
        let map: HashMap<PathBuf, String> = files
            .iter()
            .map(|(p, s)| (PathBuf::from(p), s.to_string()))
            .collect();
        Workspace::new(MapLoader::new(map))
    }

    /// Shortcut: goto_definition with string paths (for tests).
    fn goto_def(
        ws: &mut Workspace<MapLoader>,
        file: &str,
        offset: usize,
    ) -> Option<(PathBuf, (usize, usize), RefKind, String)> {
        ws.goto_definition(&PathBuf::from(file), offset)
    }

    // --- helper: find identifier by nth occurrence in source ------------------

    fn is_ident_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn nth_ident(src: &str, name: &str, n: usize) -> usize {
        let bytes = src.as_bytes();
        let mut remaining = n;
        let mut pos = 0;
        loop {
            let found = src[pos..].find(name).expect("identifier not found");
            let abs = pos + found;
            let after = abs + name.len();
            let left_ok = abs == 0 || !is_ident_char(bytes[abs - 1] as char);
            let right_ok =
                after >= bytes.len() || !is_ident_char(bytes[after] as char);
            if left_ok && right_ok {
                if remaining == 0 {
                    return abs;
                }
                remaining -= 1;
            }
            pos = abs + 1;
        }
    }

    fn ident_at(src: &str, name: &str) -> usize {
        nth_ident(src, name, 0)
    }

    fn ident_at2(src: &str, name: &str) -> usize {
        nth_ident(src, name, 1)
    }

    // --- W1: cross-file go-to-def tests ---------------------------------------

    #[test]
    fn single_file_go_to_def_still_works() {
        let mut ws = workspace_with(&[("main.rua", "fn hello() {}\nfn main() { hello() }")]);
        let use_off = ident_at2("fn hello() {}\nfn main() { hello() }", "hello");
        let result = goto_def(&mut ws, "main.rua", use_off).expect("should resolve");
        assert_eq!(result.0, PathBuf::from("main.rua")); // same file
        assert_eq!(result.2, RefKind::Item);
        assert!(result.3.contains("hello"));
    }

    #[test]
    fn cross_file_go_to_def_top_level_file_module() {
        // main.rua: `mod geo;` + use `geo::area()`
        // geo.rua: `pub fn area() -> f64 { 0.0 }`
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod geo;\nfn main() -> f64 { geo::area() }",
            ),
            (
                "geo.rua",
                "pub fn area() -> f64 { 0.0 }",
            ),
        ]);

        // main.rua: cursor on "area" in `geo::area()` (1st/only occurrence).
        let src = "mod geo;\nfn main() -> f64 { geo::area() }";
        let cursor = ident_at(src, "area");
        let result = goto_def(&mut ws, "main.rua", cursor)
            .expect("should cross-file resolve geo::area");

        // Should resolve to geo.rua's area function.
        assert!(result.0.to_string_lossy().ends_with("geo.rua"), "target file: {:?}", result.0);
        assert_eq!(result.2, RefKind::Item);
        assert!(result.3.contains("area"));
    }

    #[test]
    fn cross_file_go_to_def_nested_module() {
        // main.rua: inline mod a contains file mod b; b.rua has fn foo
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod a {\n    mod b;\n    pub fn use_it() -> i64 { b::foo() }\n}",
            ),
            (
                "a/b.rua",
                "pub fn foo() -> i64 { 42 }",
            ),
        ]);

        // Cursor on "foo" in `b::foo()`.
        let src = "mod a {\n    mod b;\n    pub fn use_it() -> i64 { b::foo() }\n}";
        let cursor = ident_at(src, "foo");
        let result = goto_def(&mut ws, "main.rua", cursor);
        assert!(result.is_some(), "should resolve nested file module fn");
        let (file, _range, kind, detail) = result.unwrap();
        assert!(file.to_string_lossy().contains("b.rua"), "target: {:?}", file);
        assert_eq!(kind, RefKind::Item);
        assert!(detail.contains("foo"));
    }

    #[test]
    fn cross_file_go_to_def_nested_dir_style_module() {
        // main.rua: `mod shapes;` → shapes/mod.rua: `pub fn circle() {}`
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod shapes;\nfn main() { shapes::circle() }",
            ),
            (
                "shapes/mod.rua",
                "pub fn circle() -> f64 { 1.0 }",
            ),
        ]);

        let src = "mod shapes;\nfn main() { shapes::circle() }";
        let cursor = ident_at(src, "circle");
        let result = goto_def(&mut ws, "main.rua", cursor)
            .expect("should resolve dir-style module");
        assert!(result.0.to_string_lossy().contains("mod.rua"), "target: {:?}", result.0);
    }

    #[test]
    fn cross_file_go_to_def_ruai_declaration() {
        // .ruai files should be loadable for go-to-def.
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod moon;\nfn main() { moon::log(\"hi\") }",
            ),
            (
                "moon.ruai",
                "extern \"lua\" { pub fn log(s: &str); }",
            ),
        ]);

        let src = "mod moon;\nfn main() { moon::log(\"hi\") }";
        let cursor = ident_at(src, "log");
        let result = goto_def(&mut ws, "main.rua", cursor);
        assert!(result.is_some(), "should resolve into .ruai file");
    }

    #[test]
    fn cursor_on_first_segment_resolves_locally() {
        // Cursor on "geo" in `geo::area()` — should resolve to `mod geo;`
        // in the same file (single-file resolution).
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod geo;\nfn main() -> f64 { geo::area() }",
            ),
            (
                "geo.rua",
                "pub fn area() -> f64 { 0.0 }",
            ),
        ]);

        let _cursor = ident_at("mod geo;\nfn main() -> f64 { geo::area() }", "geo");
        // second "geo" — the one in `geo::area()`
        let cursor2 = ident_at2("mod geo;\nfn main() -> f64 { geo::area() }", "geo");
        let result = goto_def(&mut ws, "main.rua", cursor2)
            .expect("first segment should resolve to mod decl");
        assert_eq!(result.0, PathBuf::from("main.rua"));
        assert_eq!(result.2, RefKind::Item);
        assert!(result.3.contains("mod geo"));
    }

    #[test]
    fn hover_cross_file() {
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod geo;\nfn main() -> f64 { geo::area() }",
            ),
            (
                "geo.rua",
                "pub fn area() -> f64 { 0.0 }",
            ),
        ]);

        let src = "mod geo;\nfn main() -> f64 { geo::area() }";
        let cursor = ident_at(src, "area");
        let hover = ws.hover(&PathBuf::from("main.rua"), cursor);
        assert!(hover.is_some());
        assert!(hover.unwrap().contains("area"));
    }

    #[test]
    fn non_existent_module_returns_none() {
        let mut ws = workspace_with(&[(
            "main.rua",
            "fn main() { nonexistent::foo() }",
        )]);
        let cursor = ident_at("fn main() { nonexistent::foo() }", "foo");
        let result = goto_def(&mut ws, "main.rua", cursor);
        assert!(result.is_none(), "nonexistent module should return None");
    }

    // --- W2: cross-file references tests --------------------------------------

    #[test]
    fn local_refs_stay_in_same_file() {
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "fn f() { let x = 1; x + x }",
            ),
            (
                "other.rua",
                "fn g() { let x = 2; x }",
            ),
        ]);

        let src = "fn f() { let x = 1; x + x }";
        let cursor = ident_at2(src, "x"); // first use
        let refs = ws.references(&PathBuf::from("main.rua"), cursor, true);
        assert_eq!(refs.len(), 3); // def + 2 uses in main.rua only
        for (file, _) in &refs {
            assert_eq!(file, &PathBuf::from("main.rua"));
        }
    }

    #[test]
    fn item_refs_cross_files() {
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod geo;\nfn main() { geo::area(); geo::area() }",
            ),
            (
                "geo.rua",
                "pub fn area() -> f64 { 0.0 }",
            ),
        ]);

        let src = "mod geo;\nfn main() { geo::area(); geo::area() }";
        let cursor = ident_at2(src, "area"); // first use
        let refs = ws.references(&PathBuf::from("main.rua"), cursor, true);

        // Should find: def in geo.rua + 2 uses in main.rua
        assert!(
            refs.len() >= 3,
            "should have def in geo.rua + 2 uses in main.rua, got {}: {:?}",
            refs.len(),
            refs
        );

        let in_main: Vec<_> = refs.iter().filter(|(f, _)| f == &PathBuf::from("main.rua")).collect();
        let in_geo: Vec<_> = refs.iter().filter(|(f, _)| f.to_string_lossy().contains("geo.rua")).collect();
        assert_eq!(in_main.len(), 2, "2 uses in main.rua");
        assert_eq!(in_geo.len(), 1, "1 def in geo.rua");
    }

    #[test]
    fn index_root_finds_refs_in_unopened_file() {
        // consumer.rua references geo::area but is never opened/queried; only
        // eager indexing makes its reference visible to find-references.
        let mut ws = workspace_with(&[
            ("geo.rua", "pub fn area() -> f64 { 0.0 }"),
            ("main.rua", "mod geo;\nfn main() { geo::area() }"),
            ("consumer.rua", "mod geo;\nfn use_it() { geo::area() }"),
        ]);

        // Without eager indexing, only files reached by resolution are scanned.
        // Index the whole (virtual) root first.
        let n = ws.index_root(Path::new(""));
        assert_eq!(n, 3, "all three sources should be indexed");

        // Find references starting from geo.rua's definition.
        let geo_src = "pub fn area() -> f64 { 0.0 }";
        let def = ident_at(geo_src, "area");
        let refs = ws.references(&PathBuf::from("geo.rua"), def, true);

        let in_consumer: Vec<_> = refs
            .iter()
            .filter(|(f, _)| f == &PathBuf::from("consumer.rua"))
            .collect();
        assert_eq!(
            in_consumer.len(),
            1,
            "eager index should surface the reference in the unopened consumer.rua: {refs:?}"
        );
    }

    #[test]
    fn index_root_loads_all_sources() {
        let mut ws = workspace_with(&[
            ("a.rua", "fn a() {}"),
            ("b.rua", "fn b() {}"),
            ("sub/c.rua", "fn c() {}"),
        ]);
        let n = ws.index_root(Path::new(""));
        assert_eq!(n, 3);
        assert_eq!(ws.len(), 3);
    }

    // --- B4: member access through the workspace (LSP path) -------------------

    #[test]
    fn workspace_goto_definition_on_member_field() {
        let src = "struct Point { x: f64, y: f64 }\nfn main() { let p = Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
        let mut ws = workspace_with(&[("main.rua", src)]);
        // `x` in `p.x` is the 3rd occurrence (field def, struct-lit field, use).
        let use_off = nth_ident(src, "x", 2);
        let (file, range, kind, detail) = ws
            .goto_definition(&PathBuf::from("main.rua"), use_off)
            .expect("p.x should resolve through the workspace");
        assert_eq!(file, PathBuf::from("main.rua"));
        assert_eq!(kind, RefKind::Item);
        assert_eq!(&src[range.0..range.1], "x");
        assert_eq!(detail, "x: f64");
    }

    #[test]
    fn workspace_hover_on_member_method() {
        let src = "struct P { v: i64 }\nimpl P {\n    fn get(&self) -> i64 { self.v }\n}\nfn main() { let p = P { v: 1 }; let _ = p.get(); }";
        let mut ws = workspace_with(&[("main.rua", src)]);
        let use_off = nth_ident(src, "get", 1); // `get` in `p.get()`
        let hover = ws
            .hover(&PathBuf::from("main.rua"), use_off)
            .expect("p.get() hover should resolve");
        assert_eq!(hover, "fn get(&self) -> i64");
    }

    // --- B5: cross-file member access (real disk files) -----------------------

    /// A unique temp directory for a cross-file test (ruac loads `mod`
    /// children from disk, so these can't use the in-memory `MapLoader`).
    fn unique_tmp_dir(tag: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut d = std::env::temp_dir();
        d.push(format!("rua_b5_{}_{}_{}", tag, std::process::id(), nanos));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cross_file_member_field_goto_and_hover() {
        let dir = unique_tmp_dir("field");
        let geo = dir.join("geo.rua");
        let main = dir.join("main.rua");
        std::fs::write(&geo, "pub struct Point { pub x: f64, pub y: f64 }").unwrap();
        let main_src =
            "mod geo;\nfn main() { let p = geo::Point { x: 1.0, y: 2.0 }; let _ = p.x; }";
        std::fs::write(&main, main_src).unwrap();

        let mut ws = Workspace::new(DiskLoader);
        // Cursor on `x` in `p.x` — the 2nd whole-word `x` (1st is the struct-lit field).
        let use_off = nth_ident(main_src, "x", 1);
        let (tf, range, kind, detail) = ws
            .goto_definition(&main, use_off)
            .expect("cross-file p.x should resolve");
        assert!(tf.to_string_lossy().ends_with("geo.rua"), "target: {tf:?}");
        assert_eq!(kind, RefKind::Item);
        assert_eq!(detail, "x: f64");
        // Target range points at the field def `x` in geo.rua.
        let geo_src = std::fs::read_to_string(&geo).unwrap();
        assert_eq!(&geo_src[range.0..range.1], "x");

        let hover = ws.hover(&main, use_off).expect("hover should resolve");
        assert_eq!(hover, "x: f64");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_file_member_method_goto_and_hover() {
        let dir = unique_tmp_dir("method");
        let shapes = dir.join("shapes.rua");
        let main = dir.join("main.rua");
        std::fs::write(
            &shapes,
            "pub struct Circle { pub r: f64 }\nimpl Circle {\n    pub fn area(&self) -> f64 { self.r }\n}",
        )
        .unwrap();
        let main_src =
            "mod shapes;\nfn main() -> f64 { let c = shapes::Circle { r: 1.0 }; c.area() }";
        std::fs::write(&main, main_src).unwrap();

        let mut ws = Workspace::new(DiskLoader);
        // Cursor on `area` in `c.area()` — 2nd `area` (1st is the def in shapes.rua,
        // but that's a different file, so within main_src it's the only one).
        let use_off = nth_ident(main_src, "area", 0);
        let (tf, _range, kind, detail) = ws
            .goto_definition(&main, use_off)
            .expect("cross-file c.area() should resolve");
        assert!(tf.to_string_lossy().ends_with("shapes.rua"), "target: {tf:?}");
        assert_eq!(kind, RefKind::Item);
        assert!(detail.contains("area"), "detail: {detail}");

        let hover = ws.hover(&main, use_off).expect("hover should resolve");
        assert!(hover.contains("fn area"), "hover: {hover}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cross_file_member_unknown_receiver_is_none() {
        // `q.z` where `q`'s type is not resolvable → no member hit.
        let dir = unique_tmp_dir("unknown");
        let main = dir.join("main.rua");
        let main_src = "fn main() { let q = bogus(); let _ = q.z; }";
        std::fs::write(&main, main_src).unwrap();

        let mut ws = Workspace::new(DiskLoader);
        let use_off = nth_ident(main_src, "z", 0);
        assert!(ws.member_at(&main, use_off).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- C2: cross-file member completions -----------------------------------

    #[test]
    fn cross_file_member_completions() {
        let dir = unique_tmp_dir("complete");
        std::fs::write(
            dir.join("geo.rua"),
            "pub struct Point { pub x: f64, pub y: f64 }\nimpl Point { pub fn norm(&self) -> f64 { self.x } }",
        ).unwrap();
        let main = dir.join("main.rua");
        let main_src = "mod geo;\nfn main() { let p = geo::Point { x: 1.0, y: 2.0 }; p. }";
        std::fs::write(&main, main_src).unwrap();
        let mut ws = Workspace::new(DiskLoader);
        let items = ws
            .member_completions(&main, main_src.rfind('.').unwrap() + 1)
            .expect("member slot");
        let names: Vec<&str> = items.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"norm"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn references_empty_for_unknown() {
        let mut ws = workspace_with(&[("main.rua", "fn f() { foobar }")]);
        let cursor = ident_at("fn f() { foobar }", "foobar");
        let refs = ws.references(&PathBuf::from("main.rua"), cursor, true);
        assert!(refs.is_empty());
    }

    // --- W2: cross-file rename tests ------------------------------------------

    #[test]
    fn cross_file_rename_item() {
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod geo;\nfn main() { geo::area() }",
            ),
            (
                "geo.rua",
                "pub fn area() -> f64 { 0.0 }",
            ),
        ]);

        let src = "mod geo;\nfn main() { geo::area() }";
        let cursor = ident_at(src, "area");
        let edits = ws
            .rename_edits(&PathBuf::from("main.rua"), cursor, "surface")
            .expect("rename should succeed");

        // Should have edits in both files.
        assert!(edits.contains_key(&PathBuf::from("main.rua")));
        assert!(edits.keys().any(|k| k.to_string_lossy().contains("geo.rua")));
        for file_edits in edits.values() {
            for (_s, _e, text) in file_edits {
                assert_eq!(text, "surface");
            }
        }
    }

    #[test]
    fn rename_rejects_ruai_file() {
        let mut ws = workspace_with(&[
            (
                "main.rua",
                "mod moon;\nfn main() { moon::log(\"hi\") }",
            ),
            (
                "moon.ruai",
                "extern \"lua\" { pub fn log(s: &str); }",
            ),
        ]);

        let src = "mod moon;\nfn main() { moon::log(\"hi\") }";
        let cursor = ident_at(src, "log");
        let result = ws.rename_edits(&PathBuf::from("main.rua"), cursor, "debug");
        assert!(result.is_err(), ".ruai rename should be rejected");
    }

    #[test]
    fn rename_invalid_name() {
        let mut ws = workspace_with(&[
            ("main.rua", "fn f() { let x = 1; x }"),
        ]);

        let src = "fn f() { let x = 1; x }";
        let cursor = ident_at2(src, "x");
        let err = ws
            .rename_edits(&PathBuf::from("main.rua"), cursor, "1x")
            .unwrap_err();
        assert_eq!(err, RenameError::InvalidName);
    }

    // --- path normalization tests ---------------------------------------------

    #[test]
    fn normalize_removes_dot_components() {
        let p = normalize_path(Path::new("/foo/./bar"));
        assert!(!p.to_string_lossy().contains("/./"));
    }

    #[test]
    fn normalize_resolves_dotdot() {
        let p = normalize_path(Path::new("/foo/baz/../bar"));
        let s = p.to_string_lossy();
        assert!(!s.contains("baz"), "dotdot not resolved: {s}");
        assert!(s.contains("bar"));
    }

    #[test]
    fn normalize_trailing_slash_cleaned() {
        let p = normalize_path(Path::new("/foo/bar/"));
        let s = p.to_string_lossy();
        assert!(!s.ends_with('/'), "trailing slash not cleaned: {s}");
    }

    // --- module candidate tests -----------------------------------------------

    #[test]
    fn mod_candidates_flat_first() {
        let dir = Path::new("/src");
        let cand = mod_file_candidates(dir, "foo");
        assert_eq!(cand[0].0, PathBuf::from("/src/foo.rua"));
        assert!(!cand[0].1); // not decl
    }

    #[test]
    fn mod_candidates_includes_dir_style() {
        let dir = Path::new("/src");
        let cand = mod_file_candidates(dir, "foo");
        assert_eq!(cand[1].0, PathBuf::from("/src/foo/mod.rua"));
    }

    #[test]
    fn mod_candidates_includes_ruai_forms() {
        let dir = Path::new("/src");
        let cand = mod_file_candidates(dir, "foo");
        assert_eq!(cand[2].0, PathBuf::from("/src/foo.ruai"));
        assert!(cand[2].1); // is decl
        assert_eq!(cand[3].0, PathBuf::from("/src/foo/mod.ruai"));
        assert!(cand[3].1); // is decl
    }

    // --- workspace lifecycle tests --------------------------------------------

    #[test]
    fn add_file_overrides_loader() {
        let mut ws = workspace_with(&[("main.rua", "fn original() {}")]);
        ws.add_file(&PathBuf::from("main.rua"), "fn updated() {}");
        let analysis = ws.analysis_of(&PathBuf::from("main.rua")).unwrap();
        let syms = analysis.symbols();
        assert!(syms.iter().any(|s| s.name == "updated"));
        assert!(!syms.iter().any(|s| s.name == "original"));
    }

    #[test]
    fn remove_file_falls_back_to_loader() {
        let mut ws = workspace_with(&[("main.rua", "fn from_disk() {}")]);
        ws.add_file(&PathBuf::from("main.rua"), "fn from_buffer() {}");
        // Parse the open buffer so it is cached, then close it.
        assert!(
            ws.analysis_of(&PathBuf::from("main.rua"))
                .unwrap()
                .symbols()
                .iter()
                .any(|s| s.name == "from_buffer")
        );
        // remove_file must drop both the buffer AND its cached analysis, so the
        // next access transparently re-reads the on-disk content (no explicit
        // invalidate needed).
        ws.remove_file(&PathBuf::from("main.rua"));
        let analysis = ws.analysis_of(&PathBuf::from("main.rua")).unwrap();
        let syms = analysis.symbols();
        assert!(syms.iter().any(|s| s.name == "from_disk"));
        assert!(!syms.iter().any(|s| s.name == "from_buffer"));
    }

    #[test]
    fn invalidate_forces_reload() {
        let mut ws = workspace_with(&[("main.rua", "fn first() {}")]);
        let a1 = ws.analysis_of(&PathBuf::from("main.rua")).unwrap();
        assert!(a1.symbols().iter().any(|s| s.name == "first"));

        // Change the underlying source and invalidate.
        ws.add_file(&PathBuf::from("main.rua"), "fn second() {}");
        let a2 = ws.analysis_of(&PathBuf::from("main.rua")).unwrap();
        assert!(a2.symbols().iter().any(|s| s.name == "second"));
    }

    #[test]
    fn is_empty_and_len() {
        let mut ws = workspace_with(&[]);
        assert!(ws.is_empty());
        assert_eq!(ws.len(), 0);

        // Accessing a file lazily loads it.
        ws.add_file(&PathBuf::from("a.rua"), "fn f() {}");
        ws.analysis_of(&PathBuf::from("a.rua")).unwrap();
        assert_eq!(ws.len(), 1);
        assert!(!ws.is_empty());
    }

    // --- edge cases -----------------------------------------------------------

    #[test]
    fn goto_definition_empty_source() {
        let mut ws = workspace_with(&[("empty.rua", "")]);
        let result = goto_def(&mut ws, "empty.rua", 0);
        assert!(result.is_none());
    }

    #[test]
    fn goto_definition_past_end() {
        let mut ws = workspace_with(&[("main.rua", "fn f() {}")]);
        let result = goto_def(&mut ws, "main.rua", 999);
        assert!(result.is_none());
    }

    #[test]
    fn references_decl_filter_respects_include_decl() {
        let mut ws = workspace_with(&[("main.rua", "fn foo() {}\nfn main() { foo() }")]);
        let cursor = ident_at2("fn foo() {}\nfn main() { foo() }", "foo");
        let with_decl = ws.references(&PathBuf::from("main.rua"), cursor, true);
        let without_decl = ws.references(&PathBuf::from("main.rua"), cursor, false);
        assert_eq!(with_decl.len(), without_decl.len() + 1,
            "with_decl should have one more (the definition)");
    }
}
