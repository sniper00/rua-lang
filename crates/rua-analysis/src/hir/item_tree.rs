//! Per-file summaries of declarations relevant to name resolution.

use rua_syntax::{
    Named, SyntaxNode, SyntaxToken,
    ast::{Item, SourceFile},
};

/// Byte range in a source file, independent of rowan and LSP protocol types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextRange {
    start: u32,
    end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    pub const fn start(self) -> u32 {
        self.start
    }

    pub const fn end(self) -> u32 {
        self.end
    }

    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    /// Reserved for `type Name = ...` once both language parsers accept it.
    TypeAlias,
}

/// Phase 3 visibility model. More granular visibility can be added without
/// changing ItemTree ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTreeItem {
    name: String,
    kind: ItemKind,
    range: TextRange,
    name_range: TextRange,
    visibility: Visibility,
}

impl ItemTreeItem {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn kind(&self) -> ItemKind {
        self.kind
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub const fn name_range(&self) -> TextRange {
        self.name_range
    }

    pub const fn visibility(&self) -> Visibility {
        self.visibility
    }
}

/// Compact declaration-only representation of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemTree {
    items: Vec<ItemTreeItem>,
}

impl ItemTree {
    pub fn lower(file: &SourceFile) -> Self {
        let mut tree = Self::default();
        for item in file.items() {
            tree.lower_item(item);
        }
        tree
    }

    pub fn items(&self) -> &[ItemTreeItem] {
        &self.items
    }

    fn lower_item(&mut self, item: Item) {
        match item {
            Item::Fn(item) => {
                self.push_named(&item, ItemKind::Function, item.is_pub());
            }
            Item::Struct(item) => {
                self.push_named(&item, ItemKind::Struct, item.is_pub());
            }
            Item::Enum(item) => {
                self.push_named(&item, ItemKind::Enum, item.is_pub());
            }
            Item::Trait(item) => {
                self.push_named(&item, ItemKind::Trait, item.is_pub());
            }
            Item::Mod(item) => {
                self.push_named(&item, ItemKind::Module, item.is_pub());
            }
            Item::Extern(block) => {
                for function in block.fns() {
                    self.push_named(&function, ItemKind::Function, function.is_pub());
                }
            }
            Item::Impl(_) | Item::Use(_) => {}
        }
    }

    fn push_named(&mut self, item: &impl Named, kind: ItemKind, is_public: bool) {
        let Some(name) = item.name() else {
            return;
        };
        self.items.push(ItemTreeItem {
            name: name.text().to_string(),
            kind,
            range: node_range(item.syntax()),
            name_range: token_range(&name),
            visibility: if is_public {
                Visibility::Public
            } else {
                Visibility::Private
            },
        });
    }
}

fn node_range(node: &SyntaxNode) -> TextRange {
    let range = node.text_range();
    TextRange::new(range.start().into(), range.end().into())
}

fn token_range(token: &SyntaxToken) -> TextRange {
    let range = token.text_range();
    TextRange::new(range.start().into(), range.end().into())
}

#[cfg(test)]
mod tests {
    use rua_syntax::parse_source_file;

    use super::{ItemKind, ItemTree, Visibility};

    #[test]
    fn item_tree_lowers_top_level_declaration_summaries() {
        let source = concat!(
            "pub fn run() { let body_local = 1; }\n",
            "struct Record { value: i64 }\n",
            "pub enum State { Ready }\n",
            "trait Service { fn call(&self); }\n",
            "pub mod nested { fn hidden_inside_module() {} }\n",
            "mod external;\n",
            "extern \"lua\" { pub fn clock() -> i64; }\n",
        );
        let parse = parse_source_file(source);
        assert!(parse.errors().is_empty());

        let tree = ItemTree::lower(parse.tree());
        let summaries: Vec<_> = tree
            .items()
            .iter()
            .map(|item| (item.name(), item.kind(), item.visibility()))
            .collect();

        assert_eq!(
            summaries,
            [
                ("run", ItemKind::Function, Visibility::Public),
                ("Record", ItemKind::Struct, Visibility::Private),
                ("State", ItemKind::Enum, Visibility::Public),
                ("Service", ItemKind::Trait, Visibility::Private),
                ("nested", ItemKind::Module, Visibility::Public),
                ("external", ItemKind::Module, Visibility::Private),
                ("clock", ItemKind::Function, Visibility::Public),
            ]
        );
        assert!(tree.items().iter().all(|item| {
            &source[item.name_range().start() as usize..item.name_range().end() as usize]
                == item.name()
        }));
        assert!(
            tree.items()
                .iter()
                .all(|item| item.range().start() <= item.name_range().start()
                    && item.name_range().end() <= item.range().end())
        );
        assert!(
            tree.items()
                .iter()
                .all(|item| item.name() != "hidden_inside_module" && item.name() != "body_local")
        );
    }

    #[test]
    fn item_tree_skips_recovered_items_without_names() {
        let parse = parse_source_file("fn () {}\npub struct {}\nfn valid() {}");
        let tree = ItemTree::lower(parse.tree());

        assert_eq!(tree.items().len(), 1);
        assert_eq!(tree.items()[0].name(), "valid");
    }
}
