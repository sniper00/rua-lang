//! Builtin type declarations loaded from `.ruai` sources.
//!
//! The default sysroot is embedded in the compiler so library use never depends
//! on the process working directory. Hosts and the CLI may explicitly replace it
//! with a directory; explicit directories are validated strictly.
//!
//! Code generation rules that can't be expressed in .ruai syntax (e.g. `None`
//! compiles to `nil`) live separately in `CodegenRules`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

// ---------------------------------------------------------------------------
// Code generation rules
// ---------------------------------------------------------------------------

/// Describes how a builtin path or method generates Lua code.
#[derive(Clone, Debug)]
pub enum CodegenRule {
    /// Replace the path with Lua `nil` (`Option::None`).
    Nil,
    /// Inline the first argument as a raw expression (e.g. `Some(v)` → `v`).
    InlineArg,
    /// Wrap the arguments in a Lua table constructor.
    TableCtor { tag: Option<&'static str> },
    /// Construct Rua's first-class tagged Result value.
    TaggedResult { ok: bool },
}

/// Resolved builtin identity to codegen rule.
#[derive(Clone, Debug)]
pub struct CodegenRules {
    rules: HashMap<rua_core::BuiltinId, CodegenRule>,
}

impl CodegenRules {
    pub fn get(&self, builtin: rua_core::BuiltinId) -> Option<&CodegenRule> {
        self.rules.get(&builtin)
    }
}

impl Default for CodegenRules {
    fn default() -> Self {
        let mut m = HashMap::new();

        // --- Option<T> ---
        // Lua-idiomatic: Some(v) is the bare value, None is nil.
        m.insert(rua_core::BuiltinId::VariantOptionNone, CodegenRule::Nil);
        m.insert(
            rua_core::BuiltinId::VariantOptionSome,
            CodegenRule::InlineArg,
        );

        // --- Result<T, E> ---
        m.insert(
            rua_core::BuiltinId::VariantResultOk,
            CodegenRule::TaggedResult { ok: true },
        );
        m.insert(
            rua_core::BuiltinId::VariantResultErr,
            CodegenRule::TaggedResult { ok: false },
        );

        Self { rules: m }
    }
}

// ---------------------------------------------------------------------------
// .ruai source loading
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub(crate) struct RuntimeModule {
    pub runtime: String,
    pub export: Option<String>,
    pub alias: String,
    pub abi: Option<u32>,
    pub dispatch: rua_resources::StdDispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LanguageItem {
    Option,
    OptionSome,
    OptionNone,
    OptionMap,
    OptionUnwrap,
    OptionExpect,
    OptionUnwrapOr,
    OptionIsSome,
    OptionIsNone,
    Result,
    ResultOk,
    ResultErr,
    ResultMap,
    ResultUnwrap,
    ResultExpect,
    ResultUnwrapOr,
    ResultIsOk,
    ResultIsErr,
}

impl LanguageItem {
    pub const ALL: &[(Self, &str)] = &[
        (Self::Option, "option"),
        (Self::OptionSome, "option_some"),
        (Self::OptionNone, "option_none"),
        (Self::OptionMap, "option_map"),
        (Self::OptionUnwrap, "option_unwrap"),
        (Self::OptionExpect, "option_expect"),
        (Self::OptionUnwrapOr, "option_unwrap_or"),
        (Self::OptionIsSome, "option_is_some"),
        (Self::OptionIsNone, "option_is_none"),
        (Self::Result, "result"),
        (Self::ResultOk, "result_ok"),
        (Self::ResultErr, "result_err"),
        (Self::ResultMap, "result_map"),
        (Self::ResultUnwrap, "result_unwrap"),
        (Self::ResultExpect, "result_expect"),
        (Self::ResultUnwrapOr, "result_unwrap_or"),
        (Self::ResultIsOk, "result_is_ok"),
        (Self::ResultIsErr, "result_is_err"),
    ];
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StandardMetadata {
    runtime_modules: BTreeMap<u32, RuntimeModule>,
    named_runtime_modules: BTreeMap<String, RuntimeModule>,
    runtime_helpers: BTreeMap<String, RuntimeModule>,
    language_items: BTreeMap<String, LanguageItem>,
}

impl StandardMetadata {
    pub fn runtime_module(&self, semantic_file: u32) -> Option<&RuntimeModule> {
        self.runtime_modules.get(&semantic_file)
    }

    pub fn named_runtime_module(&self, name: &str) -> Option<&RuntimeModule> {
        self.named_runtime_modules.get(name)
    }

    pub fn runtime_helper(&self, name: &str) -> Option<&RuntimeModule> {
        self.runtime_helpers.get(name)
    }

    pub fn language_items(&self) -> impl Iterator<Item = (&str, LanguageItem)> {
        self.language_items
            .iter()
            .map(|(path, item)| (path.as_str(), *item))
    }
}

#[derive(Debug)]
pub(crate) struct LoadedStandardLibrary {
    pub items: Vec<crate::ast::Item>,
    pub metadata: StandardMetadata,
}

fn parse_standard_library(
    library: &rua_resources::StdLibrary,
) -> Result<LoadedStandardLibrary, String> {
    let mut items = Vec::new();
    let mut metadata = StandardMetadata::default();
    for (index, source) in library.declarations().iter().enumerate() {
        let semantic_file = u32::MAX
            .checked_sub(index as u32)
            .expect("builtin source count exceeds semantic file namespace");
        let mut program = crate::parser::parse_with_semantic_file(source.text(), semantic_file)
            .map_err(|error| format!("standard declaration {}: {error}", source.path()))?;
        crate::resolve::set_file_program(&mut program, semantic_file);
        crate::resolve::validate_declaration_program(&program, semantic_file)
            .map_err(|error| format!("standard declaration {}: {}", source.path(), error.msg))?;
        if let Some(module) = library
            .manifest()
            .modules
            .iter()
            .find(|module| module.declaration == source.path())
        {
            let runtime = RuntimeModule {
                runtime: module.runtime.clone(),
                export: module.export.clone(),
                alias: module.alias.clone().unwrap_or_else(|| {
                    module
                        .name
                        .rsplit("::")
                        .next()
                        .unwrap_or("std")
                        .to_ascii_lowercase()
                }),
                abi: module.abi,
                dispatch: module.dispatch,
            };
            metadata
                .runtime_modules
                .insert(semantic_file, runtime.clone());
            metadata
                .named_runtime_modules
                .insert(module.name.clone(), runtime);
        }
        items.append(&mut program.items);
    }
    for (name, helper) in &library.manifest().runtime_helpers {
        metadata.runtime_helpers.insert(
            name.clone(),
            RuntimeModule {
                runtime: helper.module.clone(),
                export: helper.export.clone(),
                alias: helper.alias.clone().unwrap_or_else(|| name.clone()),
                abi: helper.abi,
                dispatch: rua_resources::StdDispatch::Module,
            },
        );
    }
    for (item, key) in LanguageItem::ALL {
        let path = library
            .lang_item(key)
            .ok_or_else(|| format!("std.toml is missing language item `{key}`"))?;
        if metadata
            .language_items
            .insert(path.to_string(), *item)
            .is_some()
        {
            return Err(format!(
                "std.toml maps more than one language item to `{path}`"
            ));
        }
    }
    Ok(LoadedStandardLibrary { items, metadata })
}

/// Parse the compiler's versioned, embedded builtin declarations.
pub(crate) fn load_embedded_builtins() -> Result<LoadedStandardLibrary, String> {
    let library = rua_resources::embedded_std().map_err(ToString::to_string)?;
    parse_standard_library(library)
}

/// Load and validate an external standard-library root. The directory must
/// contain `std.toml`; unlisted `.ruai` files are intentionally ignored.
pub(crate) fn load_builtins_dir(dir: &Path) -> Result<LoadedStandardLibrary, String> {
    let library = rua_resources::load_std_dir(dir).map_err(|error| error.to_string())?;
    parse_standard_library(&library)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_sysroot_parses() {
        assert!(!load_embedded_builtins().unwrap().items.is_empty());
    }
}
