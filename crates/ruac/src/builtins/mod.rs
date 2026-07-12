//! Builtin type declarations — loaded from `.ruai` files on disk.
//!
//! The `.ruai` files live in a directory (default: `builtins/` relative to the
//! current directory, overridable via `--builtins-dir`). They use the same
//! syntax as user-facing library `.ruai` files and are parsed by the same parser.
//!
//! Code generation rules that can't be expressed in .ruai syntax (e.g. `None`
//! compiles to `nil`) live separately in `CodegenRules`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Code generation rules
// ---------------------------------------------------------------------------

/// Describes how a builtin path or method generates Lua code.
#[derive(Clone, Debug)]
pub enum CodegenRule {
    /// Replace the path with a fixed Lua expression (e.g. `None` → `nil`).
    Literal(&'static str),
    /// Inline the first argument as a raw expression (e.g. `Some(v)` → `v`).
    InlineArg,
    /// Wrap the arguments in a Lua table constructor.
    TableCtor { tag: Option<&'static str> },
    /// Generate a call to the `rua_rt` runtime library.
    RtCall(&'static str),
}

/// Fully-qualified path → codegen rule.
#[derive(Clone, Debug)]
pub struct CodegenRules {
    rules: HashMap<String, CodegenRule>,
}

impl CodegenRules {
    pub fn get(&self, path: &str) -> Option<&CodegenRule> {
        self.rules.get(path)
    }
}

impl Default for CodegenRules {
    fn default() -> Self {
        let mut m = HashMap::new();

        // --- Option<T> ---
        // Some(x) wraps as { ok = x } so that ? can distinguish Some(val) from
        // None (nil).  This mirrors Result's Ok/Err table convention.
        m.insert("None".into(), CodegenRule::Literal("nil"));
        m.insert("Some".into(), CodegenRule::TableCtor { tag: Some("ok") });
        m.insert("Option::None".into(), CodegenRule::Literal("nil"));
        m.insert("Option::Some".into(), CodegenRule::TableCtor { tag: Some("ok") });

        // --- Result<T, E> ---
        m.insert("Ok".into(), CodegenRule::TableCtor { tag: Some("ok") });
        m.insert("Err".into(), CodegenRule::TableCtor { tag: Some("err") });
        m.insert("Result::Ok".into(), CodegenRule::TableCtor { tag: Some("ok") });
        m.insert("Result::Err".into(), CodegenRule::TableCtor { tag: Some("err") });

        // --- Vec<T> ---
        m.insert("Vec::new".into(), CodegenRule::RtCall("rt.vec({ n = 0 })"));

        // --- HashMap<K, V> ---
        m.insert("HashMap::new".into(), CodegenRule::RtCall("rt.map()"));

        Self { rules: m }
    }
}

// ---------------------------------------------------------------------------
// .ruai file loading from disk
// ---------------------------------------------------------------------------

/// Load all `.ruai` files from `dir`, parse them, and return their items.
/// Files are loaded in lexicographic order for deterministic behaviour.
pub fn load_builtins_dir(dir: &Path) -> Result<Vec<crate::ast::Item>, String> {
    let mut items = Vec::new();
    let mut paths: Vec<_> = match fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(e) => {
            // If the directory doesn't exist, that's fine — no builtins.
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(items);
            }
            return Err(format!("reading builtins dir {}: {}", dir.display(), e));
        }
    };
    paths.sort_by_key(|e| e.file_name());

    for entry in paths {
        let p = entry.path();
        if p.extension().is_none_or(|ext| ext != "ruai") {
            continue;
        }
        let src = fs::read_to_string(&p)
            .map_err(|e| format!("reading {}: {}", p.display(), e))?;
        let mut program = crate::parser::parse(&src)
            .map_err(|e| format!("{}: {}", p.display(), e))?;
        items.append(&mut program.items);
    }
    Ok(items)
}
