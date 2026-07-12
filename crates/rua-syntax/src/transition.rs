//! Temporary compiler bridge for the pre-`rua-analysis` IDE implementation.
//!
//! This module is gated behind `#[cfg(feature = "legacy")]` and only survives
//! for integration tests. All production code now uses `rua-analysis`.
//!
//! Removal plan: once the integration tests are migrated to native analysis,
//! delete this module and its callers (analysis, workspace, nameres, completion).

#![allow(dead_code)]

use std::path::Path;

use crate::kind::SyntaxKind;
use crate::lexer::LexToken;

// --- Crate-owned IDE data ----------------------------------------------------

/// Kind of a resolved or completable member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Field,
    Method,
}

/// One resolved member access, expressed only in file ids and byte ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberTarget {
    pub member_file: u32,
    pub member_start: usize,
    pub member_len: usize,
    pub target_file: u32,
    pub target_start: usize,
    pub target_len: usize,
    pub detail: String,
    pub kind: MemberKind,
}

/// Member-access resolutions produced by the temporary compiler bridge.
#[derive(Debug, Clone, Default)]
pub struct MemberIndex {
    hits: Vec<MemberTarget>,
}

impl MemberIndex {
    pub fn at(&self, file: u32, offset: usize) -> Option<&MemberTarget> {
        self.hits.iter().find(|hit| {
            hit.member_file == file
                && offset >= hit.member_start
                && offset < hit.member_start + hit.member_len
        })
    }

    pub fn hits(&self) -> &[MemberTarget] {
        &self.hits
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }
}

/// One field or method offered by member completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMember {
    pub name: String,
    pub kind: MemberKind,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct BindingType {
    file: u32,
    name_start: usize,
    name_len: usize,
    pub display: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BindingTypes {
    hits: Vec<BindingType>,
}

impl BindingTypes {
    pub(crate) fn display_at(&self, file: u32, offset: usize) -> Option<&str> {
        self.hits
            .iter()
            .find(|binding| {
                binding.file == file
                    && offset >= binding.name_start
                    && offset < binding.name_start + binding.name_len
            })
            .map(|binding| binding.display.as_str())
    }
}

// --- Semantic bridge ---------------------------------------------------------

pub(crate) fn member_index(src: &str) -> MemberIndex {
    convert_member_index(ruac::member_index(src))
}

pub(crate) fn binding_types(src: &str) -> BindingTypes {
    let bindings = ruac::binding_types(src);
    BindingTypes {
        hits: bindings
            .hits()
            .iter()
            .map(|binding| BindingType {
                file: binding.file,
                name_start: binding.name_start,
                name_len: binding.name_len,
                display: binding.display.clone(),
            })
            .collect(),
    }
}

pub(crate) fn member_completions(src: &str, receiver_end: usize) -> Vec<CompletionMember> {
    let (members, receivers) = ruac::member_completion(src);
    let Some(receiver) = receivers.at_end(0, receiver_end) else {
        return Vec::new();
    };
    members
        .get(&receiver.type_name)
        .iter()
        .map(convert_completion_member)
        .collect()
}

pub(crate) fn member_completions_src(
    src: &str,
    path: &Path,
    receiver_end: usize,
) -> Vec<CompletionMember> {
    let (members, receivers, _) = ruac::member_completion_src(src, path);
    let Some(receiver) = receivers.at_end(0, receiver_end) else {
        return Vec::new();
    };
    members
        .get(&receiver.type_name)
        .iter()
        .map(convert_completion_member)
        .collect()
}

pub(crate) fn member_index_src(src: &str, path: &Path) -> (MemberIndex, Vec<String>) {
    let (index, files) = ruac::member_index_src(src, path);
    (convert_member_index(index), files)
}

fn convert_member_index(index: ruac::typeck::MemberIndex) -> MemberIndex {
    MemberIndex {
        hits: index
            .hits()
            .iter()
            .map(|hit| MemberTarget {
                member_file: hit.member_file,
                member_start: hit.member_start,
                member_len: hit.member_len,
                target_file: hit.target_file,
                target_start: hit.target_start,
                target_len: hit.target_len,
                detail: hit.detail.clone(),
                kind: convert_member_kind(hit.kind),
            })
            .collect(),
    }
}

fn convert_completion_member(member: &ruac::typeck::CompletionMember) -> CompletionMember {
    CompletionMember {
        name: member.name.clone(),
        kind: convert_member_kind(member.kind),
        detail: member.detail.clone(),
    }
}

fn convert_member_kind(kind: ruac::typeck::MemberKind) -> MemberKind {
    match kind {
        ruac::typeck::MemberKind::Field => MemberKind::Field,
        ruac::typeck::MemberKind::Method => MemberKind::Method,
    }
}

// --- Lexer bridge ------------------------------------------------------------

/// Losslessly lex with the compiler tokenizer while keeping its token types
/// behind this boundary. The public lexer exposes only [`SyntaxKind`].
pub(crate) fn lex(text: &str) -> Vec<LexToken> {
    let mut out = Vec::new();
    let mut tokenizer = ruac::tokenize::RuaTokenize::new(text);
    let mut pos = 0usize;

    loop {
        match tokenizer.next_token() {
            Ok(token) => {
                let start = token.range.start;
                if start > pos {
                    push_trivia(&mut out, text, pos, start);
                }
                if token.kind == ruac::token::RuaTokenKind::Eof {
                    break;
                }
                out.push(LexToken::new(
                    syntax_kind(token.kind),
                    start,
                    token.range.len,
                ));
                pos = token.range.end();
            }
            Err(_) => {
                if pos < text.len() {
                    out.push(LexToken::new(SyntaxKind::Error, pos, text.len() - pos));
                }
                break;
            }
        }
    }

    out
}

fn push_trivia(out: &mut Vec<LexToken>, text: &str, from: usize, to: usize) {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < to {
        let c = bytes[i];
        if c == b'/' && i + 1 < to && bytes[i + 1] == b'/' {
            let mut j = i + 2;
            while j < to && bytes[j] != b'\n' && bytes[j] != b'\r' {
                j += 1;
            }
            out.push(LexToken::new(SyntaxKind::LineComment, i, j - i));
            i = j;
        } else if c == b'/' && i + 1 < to && bytes[i + 1] == b'*' {
            let mut j = i + 2;
            let mut depth = 1usize;
            while j < to && depth > 0 {
                if bytes[j] == b'/' && j + 1 < to && bytes[j + 1] == b'*' {
                    depth += 1;
                    j += 2;
                } else if bytes[j] == b'*' && j + 1 < to && bytes[j + 1] == b'/' {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.push(LexToken::new(SyntaxKind::BlockComment, i, j - i));
            i = j;
        } else {
            let mut j = i;
            while j < to {
                if bytes[j] == b'/' && j + 1 < to && (bytes[j + 1] == b'/' || bytes[j + 1] == b'*')
                {
                    break;
                }
                j += 1;
            }
            out.push(LexToken::new(SyntaxKind::Whitespace, i, j - i));
            i = j;
        }
    }
}

fn syntax_kind(kind: ruac::token::RuaTokenKind) -> SyntaxKind {
    use ruac::token::RuaTokenKind as Token;

    match kind {
        Token::KwFn => SyntaxKind::KwFn,
        Token::KwLet => SyntaxKind::KwLet,
        Token::KwMut => SyntaxKind::KwMut,
        Token::KwIf => SyntaxKind::KwIf,
        Token::KwElse => SyntaxKind::KwElse,
        Token::KwWhile => SyntaxKind::KwWhile,
        Token::KwLoop => SyntaxKind::KwLoop,
        Token::KwFor => SyntaxKind::KwFor,
        Token::KwIn => SyntaxKind::KwIn,
        Token::KwReturn => SyntaxKind::KwReturn,
        Token::KwBreak => SyntaxKind::KwBreak,
        Token::KwContinue => SyntaxKind::KwContinue,
        Token::KwTrue => SyntaxKind::KwTrue,
        Token::KwFalse => SyntaxKind::KwFalse,
        Token::KwStruct => SyntaxKind::KwStruct,
        Token::KwEnum => SyntaxKind::KwEnum,
        Token::KwTrait => SyntaxKind::KwTrait,
        Token::KwImpl => SyntaxKind::KwImpl,
        Token::KwPub => SyntaxKind::KwPub,
        Token::KwUse => SyntaxKind::KwUse,
        Token::KwMod => SyntaxKind::KwMod,
        Token::KwAs => SyntaxKind::KwAs,
        Token::KwMatch => SyntaxKind::KwMatch,
        Token::KwSelf => SyntaxKind::KwSelf,
        Token::KwExtern => SyntaxKind::KwExtern,
        Token::KwDyn => SyntaxKind::KwDyn,
        Token::Ident => SyntaxKind::Ident,
        Token::Int => SyntaxKind::Int,
        Token::Float => SyntaxKind::Float,
        Token::Str => SyntaxKind::Str,
        Token::Plus => SyntaxKind::Plus,
        Token::Minus => SyntaxKind::Minus,
        Token::Star => SyntaxKind::Star,
        Token::Slash => SyntaxKind::Slash,
        Token::Percent => SyntaxKind::Percent,
        Token::Eq => SyntaxKind::Eq,
        Token::EqEq => SyntaxKind::EqEq,
        Token::Ne => SyntaxKind::Ne,
        Token::Lt => SyntaxKind::Lt,
        Token::Le => SyntaxKind::Le,
        Token::Gt => SyntaxKind::Gt,
        Token::Ge => SyntaxKind::Ge,
        Token::AndAnd => SyntaxKind::AndAnd,
        Token::OrOr => SyntaxKind::OrOr,
        Token::Not => SyntaxKind::Not,
        Token::Amp => SyntaxKind::Amp,
        Token::Pipe => SyntaxKind::Pipe,
        Token::Question => SyntaxKind::Question,
        Token::Arrow => SyntaxKind::Arrow,
        Token::FatArrow => SyntaxKind::FatArrow,
        Token::ColonColon => SyntaxKind::ColonColon,
        Token::Colon => SyntaxKind::Colon,
        Token::Semi => SyntaxKind::Semi,
        Token::Comma => SyntaxKind::Comma,
        Token::Dot => SyntaxKind::Dot,
        Token::DotDot => SyntaxKind::DotDot,
        Token::DotDotEq => SyntaxKind::DotDotEq,
        Token::LParen => SyntaxKind::LParen,
        Token::RParen => SyntaxKind::RParen,
        Token::LBrace => SyntaxKind::LBrace,
        Token::RBrace => SyntaxKind::RBrace,
        Token::LBracket => SyntaxKind::LBracket,
        Token::RBracket => SyntaxKind::RBracket,
        Token::Eof => SyntaxKind::Eof,
        Token::Unknown => SyntaxKind::Error,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    #[test]
    fn compiler_access_is_isolated_to_transition_module() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let transition = src_dir.join("transition.rs");
        assert_no_compiler_access_outside(&src_dir, &transition);
    }

    fn assert_no_compiler_access_outside(dir: &Path, transition: &Path) {
        for entry in fs::read_dir(dir).expect("read rua-syntax source directory") {
            let path = entry.expect("read source directory entry").path();
            if path.is_dir() {
                assert_no_compiler_access_outside(&path, transition);
                continue;
            }
            if path == transition || path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("read Rust source file");
            assert!(
                !source.contains("ruac::"),
                "{} bypasses the transition-only compiler boundary",
                path.display()
            );
        }
    }
}
