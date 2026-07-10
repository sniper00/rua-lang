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

    pub const fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModuleKind {
    Inline,
    File,
}

/// Phase 3 visibility model. More granular visibility can be added without
/// changing ItemTree ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Visibility {
    Private,
    Public,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    path: Vec<String>,
    alias: Option<String>,
}

impl Import {
    pub fn path(&self) -> &[String] {
        &self.path
    }

    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    pub fn binding_name(&self) -> Option<&str> {
        self.alias()
            .or_else(|| self.path.last().map(String::as_str))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemTreeItem {
    name: String,
    kind: ItemKind,
    range: TextRange,
    name_range: TextRange,
    visibility: Visibility,
    module_kind: Option<ModuleKind>,
    children: Vec<ItemTreeItem>,
    imports: Vec<Import>,
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

    pub const fn module_kind(&self) -> Option<ModuleKind> {
        self.module_kind
    }

    pub fn children(&self) -> &[ItemTreeItem] {
        &self.children
    }

    pub fn imports(&self) -> &[Import] {
        &self.imports
    }
}

/// Compact declaration-only representation of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemTree {
    items: Vec<ItemTreeItem>,
    imports: Vec<Import>,
}

impl ItemTree {
    pub fn lower(file: &SourceFile) -> Self {
        let (items, imports) = Self::lower_scope(file.items());
        Self { items, imports }
    }

    pub fn items(&self) -> &[ItemTreeItem] {
        &self.items
    }

    pub fn imports(&self) -> &[Import] {
        &self.imports
    }

    fn lower_scope(items: impl Iterator<Item = Item>) -> (Vec<ItemTreeItem>, Vec<Import>) {
        let mut summaries = Vec::new();
        let mut imports = Vec::new();
        for item in items {
            match item {
                Item::Use(item) => imports.extend(item.imports().map(|import| {
                    Import {
                        path: import
                            .path
                            .into_iter()
                            .map(|token| token.text().to_string())
                            .collect(),
                        alias: import.alias.map(|token| token.text().to_string()),
                    }
                })),
                Item::Mod(item) => {
                    let module_kind = if item.is_file() {
                        ModuleKind::File
                    } else {
                        ModuleKind::Inline
                    };
                    let (children, module_imports) = if module_kind == ModuleKind::Inline {
                        Self::lower_scope(item.items())
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    if let Some(mut summary) = Self::named(&item, ItemKind::Module, item.is_pub()) {
                        summary.module_kind = Some(module_kind);
                        summary.children = children;
                        summary.imports = module_imports;
                        summaries.push(summary);
                    }
                }
                item => summaries.extend(Self::lower_non_module_item(item)),
            }
        }
        (summaries, imports)
    }

    fn lower_non_module_item(item: Item) -> Vec<ItemTreeItem> {
        match item {
            Item::Fn(item) => Self::named(&item, ItemKind::Function, item.is_pub())
                .into_iter()
                .collect(),
            Item::Struct(item) => Self::named(&item, ItemKind::Struct, item.is_pub())
                .into_iter()
                .collect(),
            Item::Enum(item) => Self::named(&item, ItemKind::Enum, item.is_pub())
                .into_iter()
                .collect(),
            Item::Trait(item) => Self::named(&item, ItemKind::Trait, item.is_pub())
                .into_iter()
                .collect(),
            Item::Extern(block) => block
                .fns()
                .filter_map(|function| {
                    Self::named(&function, ItemKind::Function, function.is_pub())
                })
                .collect(),
            Item::Impl(_) | Item::Mod(_) | Item::Use(_) => Vec::new(),
        }
    }

    fn named(item: &impl Named, kind: ItemKind, is_public: bool) -> Option<ItemTreeItem> {
        let name = item.name()?;
        Some(ItemTreeItem {
            name: name.text().to_string(),
            kind,
            range: node_range(item.syntax()),
            name_range: token_range(&name),
            visibility: if is_public {
                Visibility::Public
            } else {
                Visibility::Private
            },
            module_kind: None,
            children: Vec::new(),
            imports: Vec::new(),
        })
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

    use super::{ItemKind, ItemTree, ModuleKind, Visibility};

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
        let nested = &tree.items()[4];
        assert_eq!(nested.module_kind(), Some(ModuleKind::Inline));
        assert_eq!(nested.children().len(), 1);
        assert_eq!(nested.children()[0].name(), "hidden_inside_module");
        assert_eq!(tree.items()[5].module_kind(), Some(ModuleKind::File));
        assert!(tree.items()[5].children().is_empty());
    }

    #[test]
    fn item_tree_skips_recovered_items_without_names() {
        let parse = parse_source_file("fn () {}\npub struct {}\nfn valid() {}");
        let tree = ItemTree::lower(parse.tree());

        assert_eq!(tree.items().len(), 1);
        assert_eq!(tree.items()[0].name(), "valid");
    }

    #[test]
    fn item_tree_lowers_imports_in_their_module_scope() {
        let parse = parse_source_file(concat!(
            "use math::{one, two as second};\n",
            "mod nested { use parent::value as local; fn call() {} }\n",
        ));
        let tree = ItemTree::lower(parse.tree());

        assert_eq!(tree.imports().len(), 2);
        assert_eq!(tree.imports()[0].path(), ["math", "one"]);
        assert_eq!(tree.imports()[0].binding_name(), Some("one"));
        assert_eq!(tree.imports()[1].path(), ["math", "two"]);
        assert_eq!(tree.imports()[1].binding_name(), Some("second"));
        assert_eq!(tree.items()[0].imports().len(), 1);
        assert_eq!(tree.items()[0].imports()[0].path(), ["parent", "value"]);
        assert_eq!(tree.items()[0].imports()[0].binding_name(), Some("local"));
    }
}
