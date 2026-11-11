//! Code-block colouring in highlight.js's GitHub Dark palette, for the languages chats carry
//! most. A lexer per family rather than hljs's grammars: keywords, literals, strings,
//! comments, numbers, macros and called or defined function names, which is what the theme
//! distinguishes.

use gpui::{Hsla, rgb};

pub const PLAIN: u32 = 0xc9d1d9;
const KEYWORD: u32 = 0xff7b72;
const TITLE: u32 = 0xd2a8ff;
const LITERAL: u32 = 0x79c0ff;
const STRING: u32 = 0xa5d6ff;
const BUILT_IN: u32 = 0xffa657;
const COMMENT: u32 = 0x8b949e;

struct Lang {
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    /// Keywords whose next identifier is a definition's name.
    definers: &'static [&'static str],
    line_comment: &'static [&'static str],
    block_comment: bool,
    /// `name!` is a macro (Rust).
    macros: bool,
    /// `name(` is a call.
    calls: bool,
}

const RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for", "if", "impl",
        "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
        "trait", "type", "unsafe", "use", "where", "while", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
        "u128", "usize", "f32", "f64", "str", "char", "bool",
    ],
    literals: &["true", "false", "Some", "None", "Ok", "Err"],
    definers: &["fn"],
    line_comment: &["//"],
    block_comment: true,
    macros: true,
    calls: true,
};

const JS: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
        "else", "export", "extends", "finally", "for", "from", "function", "if", "import", "in", "instanceof", "interface", "let",
        "new", "of", "return", "static", "switch", "this", "throw", "try", "type", "typeof", "var", "void", "while", "yield",
    ],
    literals: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    definers: &["function", "class"],
    line_comment: &["//"],
    block_comment: true,
    macros: false,
    calls: true,
};

const PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "finally",
        "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try",
        "while", "with", "yield",
    ],
    literals: &["True", "False", "None"],
    definers: &["def", "class"],
    line_comment: &["#"],
    block_comment: false,
    macros: false,
    calls: false,
};

const C_LIKE: Lang = Lang {
    keywords: &[
        "auto", "break", "case", "char", "class", "const", "continue", "default", "do", "double", "else", "enum", "extends",
        "extern", "final", "float", "for", "func", "go", "goto", "if", "implements", "import", "int", "interface", "long", "map",
        "namespace", "new", "package", "private", "protected", "public", "return", "short", "signed", "sizeof", "static", "struct",
        "switch", "template", "this", "throw", "try", "catch", "typedef", "union", "unsigned", "using", "var", "void", "volatile",
        "while", "chan", "defer", "range", "select", "fun", "val", "let", "guard",
    ],
    literals: &["true", "false", "null", "nil", "nullptr", "NULL"],
    definers: &["func", "fun", "class", "struct"],
    line_comment: &["//"],
    block_comment: true,
    macros: false,
    calls: true,
};

const SHELL: Lang = Lang {
    keywords: &["if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in", "function", "return", "export", "local"],
    literals: &["true", "false"],
    definers: &["function"],
    line_comment: &["#"],
    block_comment: false,
    macros: false,
    calls: false,
};

const DATA: Lang = Lang {
    keywords: &[],
    literals: &["true", "false", "null"],
    definers: &[],
    line_comment: &["#"],
    block_comment: false,
    macros: false,
    calls: false,
};

fn lang(name: &str) -> Option<&'static Lang> {
    Some(match name {
        "rust" | "rs" => &RUST,
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" => &JS,
        "py" | "python" => &PYTHON,
        "c" | "h" | "cpp" | "c++" | "cc" | "go" | "golang" | "java" | "kotlin" | "kt" | "swift" | "cs" | "csharp" | "dart" => &C_LIKE,
        "sh" | "bash" | "zsh" | "shell" => &SHELL,
        "json" | "toml" | "yaml" | "yml" => &DATA,
        _ => return None,
    })
}

/// Colour runs `(byte_len, colour)` for each line of `code`.
pub fn highlight(code: &str, language: &str) -> Vec<Vec<(usize, Hsla)>> {
    let Some(l) = lang(language) else {
        return code.split('\n').map(|line| vec![(line.len(), rgb(PLAIN).into())]).collect();
    };
    let mut lines = Vec::new();
    let mut in_block = false;
    let mut def_next = false;
    for line in code.split('\n') {
        let mut runs: Vec<(usize, u32)> = Vec::new();
        let push = |runs: &mut Vec<(usize, u32)>, len: usize, color: u32| {
            if len == 0 {
                return;
            }
            match runs.last_mut() {
                Some(last) if last.1 == color => last.0 += len,
                _ => runs.push((len, color)),
            }
        };
        let b = line.as_bytes();
        let mut i = 0;
        while i < b.len() {
            let rest = &line[i..];
            if in_block {
                let end = rest.find("*/").map(|e| e + 2).unwrap_or(rest.len());
                in_block = !rest[..end].ends_with("*/");
                push(&mut runs, end, COMMENT);
                i += end;
                continue;
            }
            if l.block_comment && rest.starts_with("/*") {
                in_block = true;
                push(&mut runs, 2, COMMENT);
                i += 2;
                continue;
            }
            if l.line_comment.iter().any(|c| rest.starts_with(c)) {
                push(&mut runs, rest.len(), COMMENT);
                break;
            }
            let ch = rest.chars().next().unwrap();
            if ch == '"' || ch == '\'' || ch == '`' {
                // A Rust lifetime is no string.
                if ch == '\'' && l.macros && !rest[1..].chars().nth(1).is_some_and(|c| c == '\'') && !rest[1..].starts_with('\\') {
                    push(&mut runs, 1, PLAIN);
                    i += 1;
                    continue;
                }
                let mut j = 1;
                let rb = rest.as_bytes();
                while j < rb.len() && rb[j] != ch as u8 {
                    j += if rb[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(rest.len());
                push(&mut runs, end, STRING);
                i += end;
                continue;
            }
            if ch.is_ascii_digit() {
                let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_')).unwrap_or(rest.len());
                push(&mut runs, end, LITERAL);
                i += end;
                continue;
            }
            if ch.is_alphabetic() || ch == '_' {
                let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
                let word = &rest[..end];
                let after = &rest[end..];
                let color = if def_next {
                    def_next = false;
                    TITLE
                } else if l.keywords.contains(&word) {
                    def_next = l.definers.contains(&word);
                    KEYWORD
                } else if l.literals.contains(&word) {
                    LITERAL
                } else if l.macros && after.starts_with('!') && !after.starts_with("!=") {
                    push(&mut runs, end + 1, BUILT_IN);
                    i += end + 1;
                    continue;
                } else if l.calls && after.starts_with('(') {
                    TITLE
                } else {
                    PLAIN
                };
                push(&mut runs, end, color);
                i += end;
                continue;
            }
            push(&mut runs, ch.len_utf8(), PLAIN);
            i += ch.len_utf8();
        }
        lines.push(runs.into_iter().map(|(n, c)| (n, rgb(c).into())).collect());
    }
    lines
}
