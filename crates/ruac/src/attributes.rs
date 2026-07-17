//! Attribute expansion and the active compiler view.

use rua_core::{Attribute, CfgOptions, expand_cfg_attributes};

use crate::ast::{Block, ChunkEntry, Field, Item, Program, VariantKind};

pub(crate) fn apply_cfg(program: &mut Program, options: &CfgOptions) -> Result<(), String> {
    filter_scope(
        &mut program.items,
        &mut program.chunk,
        &mut program.source_order,
        options,
    )
}

fn filter_scope(
    items: &mut Vec<Item>,
    chunk: &mut Block,
    source_order: &mut Vec<ChunkEntry>,
    options: &CfgOptions,
) -> Result<(), String> {
    let old_items = std::mem::take(items);
    let mut index_map = vec![None; old_items.len()];
    for (old_index, mut item) in old_items.into_iter().enumerate() {
        let expanded =
            expand_cfg_attributes(item.attributes(), options).map_err(|error| error.to_string())?;
        if !expanded.active {
            continue;
        }
        item.set_attributes(expanded.attributes);
        filter_item_members(&mut item, options)?;
        let new_index = items.len();
        index_map[old_index] = Some(new_index);
        items.push(item);
    }

    let old_order = std::mem::take(source_order);
    for entry in old_order {
        match entry {
            ChunkEntry::Item(index) => {
                if let Some(index) = index_map.get(index).copied().flatten() {
                    source_order.push(ChunkEntry::Item(index));
                }
            }
            ChunkEntry::Statement(index) => source_order.push(ChunkEntry::Statement(index)),
        }
    }
    debug_assert_eq!(chunk.stmts.len(), chunk.statement_blank_before.len());
    Ok(())
}

fn filter_item_members(item: &mut Item, options: &CfgOptions) -> Result<(), String> {
    match item {
        Item::Struct(structure) => filter_fields(&mut structure.fields, options),
        Item::Enum(enumeration) => {
            let old_variants = std::mem::take(&mut enumeration.variants);
            for mut variant in old_variants {
                if !expand_active(&mut variant.attributes, options)? {
                    continue;
                }
                if let VariantKind::Struct(fields) = &mut variant.kind {
                    filter_fields(fields, options)?;
                }
                enumeration.variants.push(variant);
            }
            Ok(())
        }
        Item::Impl(implementation) => filter_functions(&mut implementation.methods, options),
        Item::Trait(trait_decl) => {
            let old_methods = std::mem::take(&mut trait_decl.methods);
            for mut method in old_methods {
                if expand_active(&mut method.attributes, options)? {
                    trait_decl.methods.push(method);
                }
            }
            Ok(())
        }
        Item::Extern(block) => {
            let old_functions = std::mem::take(&mut block.fns);
            for mut function in old_functions {
                if expand_active(&mut function.attributes, options)? {
                    block.fns.push(function);
                }
            }
            Ok(())
        }
        Item::Mod(module) => filter_scope(
            &mut module.items,
            &mut module.chunk,
            &mut module.source_order,
            options,
        ),
        Item::Annotation(_) | Item::Fn(_) | Item::Use(_) => Ok(()),
    }
}

fn filter_fields(fields: &mut Vec<Field>, options: &CfgOptions) -> Result<(), String> {
    let old_fields = std::mem::take(fields);
    for mut field in old_fields {
        if expand_active(&mut field.attributes, options)? {
            fields.push(field);
        }
    }
    Ok(())
}

fn filter_functions(
    functions: &mut Vec<crate::ast::FnDecl>,
    options: &CfgOptions,
) -> Result<(), String> {
    let old_functions = std::mem::take(functions);
    for mut function in old_functions {
        if expand_active(&mut function.attributes, options)? {
            functions.push(function);
        }
    }
    Ok(())
}

fn expand_active(attributes: &mut Vec<Attribute>, options: &CfgOptions) -> Result<bool, String> {
    let expanded = expand_cfg_attributes(attributes, options).map_err(|error| error.to_string())?;
    *attributes = expanded.attributes;
    Ok(expanded.active)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;

    #[test]
    fn cfg_filters_items_and_preserves_chunk_order() {
        let mut program = parser::parse(
            r#"
            let before = 1;
            #[cfg(feature = "server")]
            fn server() -> i64 { 1 }
            let after = 2;
            "#,
        )
        .unwrap();
        apply_cfg(&mut program, &CfgOptions::default()).unwrap();
        assert!(program.items.is_empty());
        assert_eq!(program.chunk.stmts.len(), 2);
        assert_eq!(
            program.source_order,
            [ChunkEntry::Statement(0), ChunkEntry::Statement(1)]
        );
    }

    #[test]
    fn cfg_attr_can_enable_an_item() {
        let mut program = parser::parse(
            r#"
            #[cfg_attr(feature = "server", cfg(enabled))]
            fn server() -> i64 { 1 }
            "#,
        )
        .unwrap();
        let mut options = CfgOptions::default();
        options.insert_feature("server");
        options.insert_flag("enabled");
        apply_cfg(&mut program, &options).unwrap();
        assert_eq!(program.items.len(), 1);
    }
}
