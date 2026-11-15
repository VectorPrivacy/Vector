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
const NAME: u32 = 0x7ee787;
const SECTION: u32 = 0x1f6feb;
const ADDITION: u32 = 0xaff5b4;
const DELETION: u32 = 0xffdcd7;

struct Lang {
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    /// Keywords whose next identifier is a definition's name.
    definers: &'static [&'static str],
    line_comment: &'static [&'static str],
    /// A block comment's opening and closing marks.
    block: Option<(&'static str, &'static str)>,
    /// Keywords match in any case (SQL).
    ci: bool,
    /// A sigil that starts a variable (`$x` in PHP, Perl and shell).
    sigil: Option<char>,
    /// `name!` is a macro (Rust).
    macros: bool,
    /// `name(` is a call.
    calls: bool,
}

static RUST: Lang = Lang {
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
        "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "type",
        "unsafe", "use", "where", "while", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16",
        "u32", "u64", "u128", "usize", "f32", "f64", "str", "char", "bool",
    ],
    literals: &["true", "false", "Some", "None", "Ok", "Err"],
    definers: &["fn"],
    line_comment: &["//"],
    block: Some(("/*", "*/")),
    ci: false,
    sigil: None,
    macros: true,
    calls: true,
};

static JS: Lang = Lang {
    keywords: &[
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "of",
        "return",
        "static",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "yield",
    ],
    literals: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    definers: &["function", "class"],
    line_comment: &["//"],
    block: Some(("/*", "*/")),
    ci: false,
    sigil: None,
    macros: false,
    calls: true,
};

static PYTHON: Lang = Lang {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is",
        "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with",
        "yield",
    ],
    literals: &["True", "False", "None"],
    definers: &["def", "class"],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: None,
    macros: false,
    calls: false,
};

static C_LIKE: Lang = Lang {
    keywords: &[
        "auto",
        "break",
        "case",
        "char",
        "class",
        "const",
        "continue",
        "default",
        "do",
        "double",
        "else",
        "enum",
        "extends",
        "extern",
        "final",
        "float",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "implements",
        "import",
        "int",
        "interface",
        "long",
        "map",
        "namespace",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "catch",
        "typedef",
        "union",
        "unsigned",
        "using",
        "var",
        "void",
        "volatile",
        "while",
        "chan",
        "defer",
        "range",
        "select",
        "fun",
        "val",
        "let",
        "guard",
    ],
    literals: &["true", "false", "null", "nil", "nullptr", "NULL"],
    definers: &["func", "fun", "class", "struct"],
    line_comment: &["//"],
    block: Some(("/*", "*/")),
    ci: false,
    sigil: None,
    macros: false,
    calls: true,
};

static SHELL: Lang = Lang {
    keywords: &[
        "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case", "esac", "in",
        "function", "return", "export", "local",
    ],
    literals: &["true", "false"],
    definers: &["function"],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: false,
};

static DATA: Lang = Lang {
    keywords: &[],
    literals: &["true", "false", "null"],
    definers: &[],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: None,
    macros: false,
    calls: false,
};

static CSS: Lang = Lang {
    keywords: &["important", "media", "import", "keyframes", "font-face", "supports", "include", "mixin", "extend", "use", "if", "else", "each", "for", "return", "function"],
    literals: &["none", "auto", "inherit", "initial", "unset", "transparent"],
    definers: &["mixin", "function"],
    line_comment: &["//"],
    block: Some(("/*", "*/")),
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: true,
};

static SQL: Lang = Lang {
    keywords: &[
        "select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set",
        "delete", "create", "table", "drop", "alter", "add", "index", "primary", "key", "foreign",
        "references", "join", "inner", "left", "right", "outer", "full", "on", "as", "group", "by",
        "order", "having", "limit", "offset", "union", "all", "distinct", "case", "when", "then",
        "else", "end", "in", "is", "like", "between", "exists", "with", "default", "unique", "view",
        "begin", "commit", "rollback", "transaction", "returning", "asc", "desc", "integer", "int",
        "text", "varchar", "boolean", "real", "blob", "if", "pragma", "trigger", "conflict", "replace",
    ],
    literals: &["null", "true", "false"],
    definers: &["table", "view", "index", "trigger"],
    line_comment: &["--"],
    block: Some(("/*", "*/")),
    ci: true,
    sigil: None,
    macros: false,
    calls: true,
};

static PHP: Lang = Lang {
    keywords: &[
        "abstract", "and", "as", "break", "case", "catch", "class", "clone", "const", "continue",
        "declare", "default", "do", "echo", "else", "elseif", "enum", "extends", "final", "finally",
        "fn", "for", "foreach", "function", "global", "if", "implements", "include", "instanceof",
        "interface", "match", "namespace", "new", "or", "print", "private", "protected", "public",
        "readonly", "require", "require_once", "include_once", "return", "static", "switch", "throw",
        "trait", "try", "use", "var", "while", "yield",
    ],
    literals: &["true", "false", "null", "TRUE", "FALSE", "NULL"],
    definers: &["function", "class", "interface", "trait", "enum"],
    line_comment: &["//", "#"],
    block: Some(("/*", "*/")),
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: true,
};

static RUBY: Lang = Lang {
    keywords: &[
        "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
        "end", "ensure", "for", "if", "in", "module", "next", "not", "or", "redo", "rescue", "retry",
        "return", "self", "super", "then", "undef", "unless", "until", "when", "while", "yield",
        "require", "attr_accessor", "attr_reader", "puts", "lambda", "proc",
    ],
    literals: &["true", "false", "nil"],
    definers: &["def", "class", "module"],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: Some('@'),
    macros: false,
    calls: false,
};

static LUA: Lang = Lang {
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local",
        "not", "or", "repeat", "return", "then", "until", "while",
    ],
    literals: &["true", "false", "nil"],
    definers: &["function"],
    line_comment: &["--"],
    block: Some(("--[[", "]]")),
    ci: false,
    sigil: None,
    macros: false,
    calls: true,
};

static PERL: Lang = Lang {
    keywords: &[
        "my", "our", "local", "sub", "if", "elsif", "else", "unless", "while", "until", "for",
        "foreach", "return", "use", "package", "require", "last", "next", "redo", "die", "print",
        "printf", "eq", "ne", "lt", "gt", "le", "ge", "and", "or", "not", "qw", "undef",
    ],
    literals: &[],
    definers: &["sub", "package"],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: true,
};

static R: Lang = Lang {
    keywords: &["function", "if", "else", "for", "while", "repeat", "break", "next", "return", "in", "library", "require"],
    literals: &["TRUE", "FALSE", "NULL", "NA", "Inf", "NaN", "T", "F"],
    definers: &[],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: None,
    macros: false,
    calls: true,
};

static GRAPHQL: Lang = Lang {
    keywords: &["query", "mutation", "subscription", "fragment", "on", "type", "input", "enum", "interface", "union", "scalar", "schema", "extend", "implements", "directive"],
    literals: &["true", "false", "null"],
    definers: &["query", "mutation", "subscription", "fragment", "type", "input", "enum", "interface", "union", "scalar"],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: false,
};

static INI: Lang = Lang {
    keywords: &[],
    literals: &["true", "false", "on", "off", "yes", "no"],
    definers: &[],
    line_comment: &[";", "#"],
    block: None,
    ci: false,
    sigil: None,
    macros: false,
    calls: false,
};

static MAKE: Lang = Lang {
    keywords: &["include", "define", "endef", "ifdef", "ifndef", "ifeq", "ifneq", "else", "endif", "export", "override", ".PHONY"],
    literals: &[],
    definers: &[],
    line_comment: &["#"],
    block: None,
    ci: false,
    sigil: Some('$'),
    macros: false,
    calls: false,
};

enum Mode {
    Lex(&'static Lang),
    Markup,
    Diff,
    Ansi,
}

fn mode(name: &str) -> Option<Mode> {
    let name = name.trim().to_ascii_lowercase();
    Some(Mode::Lex(match name.as_str() {
        "rust" | "rs" => &RUST,
        "js" | "javascript" | "jsx" | "ts" | "typescript" | "tsx" | "mjs" | "cjs" | "node" => &JS,
        "py" | "python" | "py3" | "gyp" => &PYTHON,
        "c" | "h" | "cpp" | "c++" | "cc" | "hpp" | "cxx" | "go" | "golang" | "java" | "kotlin" | "kt"
        | "swift" | "cs" | "csharp" | "c#" | "dart" | "objc" | "objectivec" | "obj-c" | "m" | "mm"
        | "scala" | "groovy" | "zig" => &C_LIKE,
        "sh" | "bash" | "zsh" | "shell" | "console" | "shellsession" | "fish" => &SHELL,
        "json" | "jsonc" | "toml" | "yaml" | "yml" => &DATA,
        "css" | "scss" | "sass" | "less" | "stylus" => &CSS,
        "sql" | "sqlite" | "mysql" | "postgres" | "postgresql" | "plsql" => &SQL,
        "php" => &PHP,
        "rb" | "ruby" | "gemspec" | "podspec" => &RUBY,
        "lua" => &LUA,
        "pl" | "perl" | "pm" => &PERL,
        "r" => &R,
        "graphql" | "gql" => &GRAPHQL,
        "ini" | "cfg" | "conf" | "properties" | "env" | "dotenv" => &INI,
        "make" | "makefile" | "mk" | "mak" => &MAKE,
        "xml" | "html" | "htm" | "xhtml" | "svg" | "rss" | "atom" | "plist" | "vue" | "svelte" => return Some(Mode::Markup),
        "diff" | "patch" => return Some(Mode::Diff),
        "ansi" => return Some(Mode::Ansi),
        _ => return None,
    }))
}

/// `hljs.highlightAuto` for a fence with no language: the family whose tell-tale marks the
/// code carries most, or none (plain) when nothing stands out.
fn guess(code: &str) -> Option<Mode> {
    let t = code.trim_start();
    let lines: Vec<&str> = code.lines().take(60).collect();
    let has = |s: &str| code.contains(s);
    if t.contains('\u{1b}') {
        return Some(Mode::Ansi);
    }
    let diffish = lines.iter().filter(|l| l.starts_with("@@") || l.starts_with("+++") || l.starts_with("---")).count();
    if diffish >= 2 || (lines.iter().any(|l| l.starts_with("@@ ")) && lines.iter().any(|l| l.starts_with('+') || l.starts_with('-'))) {
        return Some(Mode::Diff);
    }
    if t.starts_with("<?php") {
        return mode("php");
    }
    if t.starts_with('<') && (t.contains("</") || t.contains("/>")) {
        return Some(Mode::Markup);
    }
    if (t.starts_with('{') || t.starts_with('[')) && t.contains("\":") {
        return mode("json");
    }
    if t.starts_with("#!") {
        return mode(if t.lines().next().is_some_and(|l| l.contains("python")) { "python" } else { "bash" });
    }
    let upper = code.to_ascii_uppercase();
    if (upper.contains("SELECT ") && upper.contains(" FROM ")) || upper.contains("CREATE TABLE") || upper.contains("INSERT INTO") {
        return mode("sql");
    }
    let score = |marks: &[&str]| marks.iter().filter(|m| has(m)).count();
    let scores = [
        ("rust", score(&["fn ", "let mut ", "impl ", "::", "pub fn", "-> ", "println!", "match ", "&self", "Vec<"])),
        ("python", score(&["def ", "import ", "self.", "elif ", "print(", "None", "    return", "__init__", "from "])),
        ("javascript", score(&["function", "const ", "let ", "=> ", "console.log", "===", "require(", "export ", "document."])),
        ("c", score(&["#include", "int main", "printf(", "std::", "->", "void ", "nullptr", "public static"])),
        ("bash", score(&["echo ", "$ ", "fi\n", "then\n", "sudo ", "apt ", "cd ", "export ", "${"])),
        ("css", score(&["{\n", "px;", "color:", "margin:", "display:", "@media", "!important"])),
        ("ruby", score(&["def ", "end\n", "puts ", "require '", "do |", "@"])),
        ("lua", score(&["local ", "function ", "end\n", "then\n", "~=", "nil"])),
        ("yaml", score(&[": ", "- ", "---\n"])),
    ];
    let (best, n) = scores.iter().max_by_key(|(_, n)| *n).copied().unwrap_or(("", 0));
    (n >= 2).then(|| mode(best)).flatten()
}

/// Each line of `code` as `(text, colour)` pieces: lexed by the fence's language, or the one
/// guessed when the fence names none. An ```ansi block shows its colours, its escapes gone.
pub fn colored(code: &str, language: &str) -> Vec<Vec<(String, Hsla)>> {
    let m = if language.trim().is_empty() { guess(code) } else { mode(language) };
    let runs = match m {
        Some(Mode::Ansi) => return ansi(code),
        Some(Mode::Lex(l)) => lex(code, l),
        Some(Mode::Markup) => markup(code),
        Some(Mode::Diff) => diff(code),
        None => code.split('\n').map(|line| vec![(line.len(), PLAIN)]).collect(),
    };
    code.split('\n')
        .zip(runs)
        .map(|(line, runs)| {
            let mut at = 0;
            runs.into_iter()
                .filter_map(|(len, c)| {
                    let piece = line.get(at..at + len)?;
                    at += len;
                    Some((piece.to_string(), rgb(c).into()))
                })
                .collect()
        })
        .collect()
}

/// The text a code block shows and copies: an ANSI block without its escapes.
pub fn shown(code: &str, language: &str) -> String {
    match if language.trim().is_empty() { guess(code) } else { mode(language) } {
        Some(Mode::Ansi) => colored(code, "ansi").iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n"),
        _ => code.to_string(),
    }
}

fn push(runs: &mut Vec<(usize, u32)>, len: usize, color: u32) {
    if len == 0 {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.1 == color => last.0 += len,
        _ => runs.push((len, color)),
    }
}

fn lex(code: &str, l: &Lang) -> Vec<Vec<(usize, u32)>> {
    let mut lines = Vec::new();
    let mut in_block = false;
    let mut def_next = false;
    let is_kw = |w: &str, list: &[&str]| if l.ci { list.iter().any(|k| k.eq_ignore_ascii_case(w)) } else { list.contains(&w) };
    for line in code.split('\n') {
        let mut runs: Vec<(usize, u32)> = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            if in_block {
                let close = l.block.map_or("*/", |b| b.1);
                let end = rest.find(close).map(|e| e + close.len()).unwrap_or(rest.len());
                in_block = !rest[..end].ends_with(close);
                push(&mut runs, end, COMMENT);
                i += end;
                continue;
            }
            if let Some((open, _)) = l.block
                && rest.starts_with(open)
            {
                in_block = true;
                push(&mut runs, open.len(), COMMENT);
                i += open.len();
                continue;
            }
            if l.line_comment.iter().any(|c| rest.starts_with(c)) {
                push(&mut runs, rest.len(), COMMENT);
                break;
            }
            let ch = rest.chars().next().unwrap();
            if ch == '"' || ch == '\'' || ch == '`' {
                // A Rust lifetime is no string.
                if ch == '\''
                    && l.macros
                    && !rest[1..].chars().nth(1).is_some_and(|c| c == '\'')
                    && !rest[1..].starts_with('\\')
                {
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
            if l.sigil == Some(ch) {
                let body = &rest[ch.len_utf8()..];
                let n = body.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '{' || c == '}')).unwrap_or(body.len());
                if n > 0 {
                    push(&mut runs, ch.len_utf8() + n, BUILT_IN);
                    i += ch.len_utf8() + n;
                    continue;
                }
            }
            if ch.is_ascii_digit() {
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_'))
                    .unwrap_or(rest.len());
                push(&mut runs, end, LITERAL);
                i += end;
                continue;
            }
            if ch.is_alphabetic() || ch == '_' {
                let end = rest
                    .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                let word = &rest[..end];
                let after = &rest[end..];
                let color = if def_next {
                    def_next = false;
                    TITLE
                } else if is_kw(word, l.keywords) {
                    def_next = is_kw(word, l.definers);
                    KEYWORD
                } else if is_kw(word, l.literals) {
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
        lines.push(runs);
    }
    lines
}

/// HTML and XML: tag names, attribute names, quoted values and comments.
fn markup(code: &str) -> Vec<Vec<(usize, u32)>> {
    let mut lines = Vec::new();
    let mut in_comment = false;
    let mut in_tag = false;
    for line in code.split('\n') {
        let mut runs = Vec::new();
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            if in_comment {
                let end = rest.find("-->").map(|e| e + 3).unwrap_or(rest.len());
                in_comment = !rest[..end].ends_with("-->");
                push(&mut runs, end, COMMENT);
                i += end;
                continue;
            }
            if rest.starts_with("<!--") {
                in_comment = true;
                push(&mut runs, 4, COMMENT);
                i += 4;
                continue;
            }
            let ch = rest.chars().next().unwrap();
            if !in_tag && ch == '<' {
                let open = if rest.starts_with("</") || rest.starts_with("<?") || rest.starts_with("<!") { 2 } else { 1 };
                push(&mut runs, open, PLAIN);
                let name = rest[open..].find(|c: char| !(c.is_alphanumeric() || c == '-' || c == ':' || c == '_')).unwrap_or(rest.len() - open);
                push(&mut runs, name, NAME);
                in_tag = true;
                i += open + name;
                continue;
            }
            if in_tag {
                if ch == '>' {
                    in_tag = false;
                    push(&mut runs, 1, PLAIN);
                    i += 1;
                    continue;
                }
                if ch == '"' || ch == '\'' {
                    let end = rest[1..].find(ch).map(|e| e + 2).unwrap_or(rest.len());
                    push(&mut runs, end, STRING);
                    i += end;
                    continue;
                }
                if ch.is_alphabetic() {
                    let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '-' || c == ':' || c == '_')).unwrap_or(rest.len());
                    push(&mut runs, end, LITERAL);
                    i += end;
                    continue;
                }
            }
            push(&mut runs, ch.len_utf8(), PLAIN);
            i += ch.len_utf8();
        }
        lines.push(runs);
    }
    lines
}

/// A unified diff: additions, deletions, hunk headers and file headers.
fn diff(code: &str) -> Vec<Vec<(usize, u32)>> {
    code.split('\n')
        .map(|line| {
            let color = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("diff ") || line.starts_with("index ") {
                KEYWORD
            } else if line.starts_with("@@") {
                SECTION
            } else if line.starts_with('+') || line.starts_with('>') {
                ADDITION
            } else if line.starts_with('-') || line.starts_with('<') {
                DELETION
            } else {
                PLAIN
            };
            vec![(line.len(), color)]
        })
        .collect()
}

/// `renderAnsiInto`: SGR colour codes applied, every escape sequence taken out of the text.
fn ansi(code: &str) -> Vec<Vec<(String, Hsla)>> {
    // The 16 colours as GitHub Dark's terminal draws them.
    const BASE: [u32; 16] = [
        0x484f58, 0xff7b72, 0x3fb950, 0xd29922, 0x58a6ff, 0xbc8cff, 0x39c5cf, 0xb1bac4,
        0x6e7681, 0xffa198, 0x56d364, 0xe3b341, 0x79c0ff, 0xd2a8ff, 0x56d4dd, 0xffffff,
    ];
    let cube = |n: u32| -> u32 {
        match n {
            0..=15 => BASE[n as usize],
            16..=231 => {
                let n = n - 16;
                let lvl = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
                (lvl(n / 36) << 16) | (lvl(n / 6 % 6) << 8) | lvl(n % 6)
            }
            _ => {
                let g = 8 + (n.min(255) - 232) * 10;
                (g << 16) | (g << 8) | g
            }
        }
    };
    let mut fg: Option<u32> = None;
    let mut bold = false;
    let mut out = Vec::new();
    for line in code.split('\n') {
        let mut pieces: Vec<(String, Hsla)> = Vec::new();
        let mut text = String::new();
        let color = |fg: Option<u32>, bold: bool| -> Hsla {
            let c = fg.map(|c| if bold && BASE[..8].contains(&c) { BASE[BASE.iter().position(|b| *b == c).unwrap_or(0) + 8] } else { c }).unwrap_or(PLAIN);
            rgb(c).into()
        };
        let mut chars = line.char_indices().peekable();
        while let Some((_, ch)) = chars.next() {
            if ch != '\u{1b}' {
                text.push(ch);
                continue;
            }
            // CSI: ESC [ params final; anything else after ESC is dropped with it.
            if chars.peek().map(|p| p.1) != Some('[') {
                chars.next();
                continue;
            }
            chars.next();
            let mut params = String::new();
            let mut fin = 'm';
            for (_, c) in chars.by_ref() {
                if c.is_ascii_alphabetic() || c == '~' {
                    fin = c;
                    break;
                }
                params.push(c);
            }
            if fin != 'm' {
                continue;
            }
            if !text.is_empty() {
                pieces.push((std::mem::take(&mut text), color(fg, bold)));
            }
            let codes: Vec<u32> = if params.is_empty() { vec![0] } else { params.split(';').map(|p| p.parse().unwrap_or(0)).collect() };
            let mut k = 0;
            while k < codes.len() {
                match codes[k] {
                    0 => {
                        fg = None;
                        bold = false;
                    }
                    1 => bold = true,
                    22 => bold = false,
                    39 => fg = None,
                    c @ 30..=37 => fg = Some(BASE[(c - 30) as usize]),
                    c @ 90..=97 => fg = Some(BASE[(c - 90 + 8) as usize]),
                    38 if codes.get(k + 1) == Some(&5) => {
                        fg = codes.get(k + 2).map(|n| cube(*n));
                        k += 2;
                    }
                    38 if codes.get(k + 1) == Some(&2) => {
                        if let (Some(r), Some(g), Some(b)) = (codes.get(k + 2), codes.get(k + 3), codes.get(k + 4)) {
                            fg = Some((r.min(&255) << 16) | (g.min(&255) << 8) | b.min(&255));
                        }
                        k += 4;
                    }
                    _ => {}
                }
                k += 1;
            }
        }
        if !text.is_empty() || pieces.is_empty() {
            pieces.push((text, color(fg, bold)));
        }
        out.push(pieces);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Vec<(String, Hsla)>]) -> String {
        lines.iter().map(|l| l.iter().map(|(t, _)| t.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn ansi_escapes_never_reach_the_text() {
        let code = "\u{1b}[1;32mok\u{1b}[0m done\n\u{1b}[38;5;196mred\u{1b}[39m plain\u{1b}[K";
        let lines = colored(code, "ansi");
        assert_eq!(text(&lines), "ok done\nred plain");
        assert_eq!(lines[0][0].1, rgb(0x56d364).into());
        assert_eq!(shown(code, "ansi"), "ok done\nred plain");
    }

    #[test]
    fn every_language_keeps_the_text() {
        let samples = [
            ("sql", "SELECT id FROM users WHERE x = 'a' -- c"),
            ("html", "<div class=\"a\">hi <!-- x --></div>"),
            ("diff", "@@ -1 +1 @@\n-old\n+new"),
            ("php", "<?php $x = foo('y'); // c"),
            ("lua", "local x = 1 --[[ c ]] print(x)"),
            ("", "fn main() {\n    let mut x = 1;\n    println!(\"{x}\");\n}"),
            ("", "def f(self):\n    import os\n    return None"),
            ("", "plain words only"),
        ];
        for (lang, code) in samples {
            assert_eq!(text(&colored(code, lang)), code, "{lang}");
        }
    }

    #[test]
    fn guesses_the_obvious() {
        assert!(matches!(guess("fn main() {\n    let mut x = Vec::new();\n}"), Some(Mode::Lex(l)) if std::ptr::eq(l, &RUST)));
        assert!(matches!(guess("def f():\n    import os\n    return None"), Some(Mode::Lex(l)) if std::ptr::eq(l, &PYTHON)));
        assert!(matches!(guess("<html><body></body></html>"), Some(Mode::Markup)));
        assert!(matches!(guess("SELECT a FROM b"), Some(Mode::Lex(l)) if std::ptr::eq(l, &SQL)));
        assert!(guess("just some words").is_none());
    }
}
