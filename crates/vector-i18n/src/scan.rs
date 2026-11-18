//! What each message in a `.ftl` source asks of the code: its values and how they are used.
//! Shared by the build script (typed functions from en-GB) and the checker (translations
//! against English).

use std::collections::{BTreeMap, HashMap};

use fluent_syntax::ast;
use fluent_syntax::parser;

/// How a value is used, which decides the type a generated function takes for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Shown as is: text or a number.
    Any,
    /// Selected on or passed to a number function, so it must be a number for plurals to work.
    Num,
    /// Passed to `LIST`.
    List,
}

#[derive(Clone, Debug)]
pub struct Unit {
    /// `id` or `id.attribute`.
    pub id: String,
    pub vars: BTreeMap<String, Kind>,
    /// Messages and attributes this one places, whose values it passes through.
    pub refs: Vec<String>,
    /// Tags of the `<tag>…</tag>` rich text it uses, in any variant.
    pub tags: Vec<String>,
    pub comment: Option<String>,
    /// The pattern, roughly as written, for generated docs.
    pub text: String,
    /// The line it starts on, for messages pointing at it.
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub units: Vec<Unit>,
    pub errors: Vec<String>,
}

pub fn scan(file: &str, source: &str) -> Scan {
    let mut out = Scan::default();
    let res = match parser::parse(source) {
        Ok(r) => r,
        Err((r, errs)) => {
            for e in errs {
                let line = source[..e.pos.start.min(source.len())].matches('\n').count() + 1;
                out.errors.push(format!("{file}:{line}: {:?}", e.kind));
            }
            r
        }
    };
    let mut seen: HashMap<String, usize> = HashMap::new();
    for entry in &res.body {
        let ast::Entry::Message(m) = entry else {
            if let ast::Entry::Junk { content } = entry {
                let line = line_of(source, content);
                out.errors.push(format!("{file}:{line}: unparsed text"));
            }
            continue;
        };
        let line = line_of(source, m.id.name);
        let comment = m.comment.as_ref().map(|c| c.content.join("\n"));
        let mut push = |id: String, pattern: &ast::Pattern<&str>, comment: Option<String>| {
            if let Some(prev) = seen.insert(id.clone(), line) {
                out.errors.push(format!("{file}:{line}: `{id}` is already defined on line {prev}"));
            }
            let mut u = Unit { id, vars: BTreeMap::new(), refs: vec![], tags: vec![], comment, text: String::new(), line };
            walk_pattern(pattern, &mut u);
            u.text = u.text.trim().to_string();
            out.units.push(u);
        };
        if let Some(v) = &m.value {
            push(m.id.name.to_string(), v, comment.clone());
        }
        for a in &m.attributes {
            push(format!("{}.{}", m.id.name, a.id.name), &a.value, comment.clone());
        }
    }
    out
}

fn line_of(source: &str, slice: &str) -> usize {
    let start = (slice.as_ptr() as usize).saturating_sub(source.as_ptr() as usize).min(source.len());
    source[..start].matches('\n').count() + 1
}

fn walk_pattern(p: &ast::Pattern<&str>, u: &mut Unit) {
    for el in &p.elements {
        match el {
            ast::PatternElement::TextElement { value } => {
                u.text.push_str(value);
                for t in tags_of(value) {
                    if !u.tags.contains(&t) {
                        u.tags.push(t);
                    }
                }
            }
            ast::PatternElement::Placeable { expression } => walk_expr(expression, u),
        }
    }
}

fn walk_expr(e: &ast::Expression<&str>, u: &mut Unit) {
    match e {
        ast::Expression::Inline(i) => {
            u.text.push_str("{ ");
            walk_inline(i, u, Kind::Any);
            u.text.push_str(" }");
        }
        ast::Expression::Select { selector, variants } => {
            let mark = u.text.len();
            walk_inline(selector, u, Kind::Num);
            u.text.truncate(mark);
            // The docs show the default variant: what English says in the general case.
            let default = variants.iter().position(|v| v.default).unwrap_or(0);
            for (i, v) in variants.iter().enumerate() {
                let mark = u.text.len();
                walk_pattern(&v.value, u);
                if i != default {
                    u.text.truncate(mark);
                }
            }
        }
    }
}

fn walk_inline(i: &ast::InlineExpression<&str>, u: &mut Unit, kind: Kind) {
    match i {
        ast::InlineExpression::VariableReference { id } => {
            u.text.push('$');
            u.text.push_str(id.name);
            let k = u.vars.entry(id.name.to_string()).or_insert(kind);
            *k = (*k).max(kind);
        }
        ast::InlineExpression::FunctionReference { id, arguments } => {
            u.text.push_str(id.name);
            let k = match id.name {
                "NUMBER" | "BYTES" | "DURATION" | "PERCENT" => Kind::Num,
                "LIST" => Kind::List,
                _ => Kind::Any,
            };
            for a in &arguments.positional {
                walk_inline(a, u, k);
            }
            for a in &arguments.named {
                walk_inline(&a.value, u, Kind::Any);
            }
        }
        ast::InlineExpression::MessageReference { id, attribute } => {
            let r = match attribute {
                Some(a) => format!("{}.{}", id.name, a.name),
                None => id.name.to_string(),
            };
            u.text.push_str(&r);
            u.refs.push(r);
        }
        ast::InlineExpression::TermReference { id, .. } => {
            u.text.push('-');
            u.text.push_str(id.name);
        }
        ast::InlineExpression::StringLiteral { value } => u.text.push_str(value),
        ast::InlineExpression::NumberLiteral { value } => u.text.push_str(value),
        ast::InlineExpression::Placeable { expression } => walk_expr(expression, u),
    }
}

/// The opening tags of `<name>…</name>` rich text, in order of first use.
pub fn tags_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
        if end > 0 && rest[end..].starts_with('>') {
            let name = &rest[..end];
            if !out.iter().any(|t| t == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Every unit's values, with the values of the messages it places folded in.
pub fn resolve(units: &mut [Unit]) {
    let index: HashMap<String, usize> = units.iter().enumerate().map(|(i, u)| (u.id.clone(), i)).collect();
    for i in 0..units.len() {
        let mut vars = units[i].vars.clone();
        let mut stack = units[i].refs.clone();
        let mut seen = vec![units[i].id.clone()];
        while let Some(r) = stack.pop() {
            if seen.contains(&r) {
                continue;
            }
            seen.push(r.clone());
            if let Some(&j) = index.get(&r) {
                for (k, v) in &units[j].vars {
                    let e = vars.entry(k.clone()).or_insert(*v);
                    *e = (*e).max(*v);
                }
                stack.extend(units[j].refs.iter().cloned());
            }
        }
        units[i].vars = vars;
    }
}

/// `unread-jump.tip` → `unread_jump_tip`.
pub fn fn_name(id: &str) -> String {
    let s: String = id.chars().map(|c| if c == '-' || c == '.' { '_' } else { c }).collect();
    if RESERVED.contains(&s.as_str()) { format!("{s}_") } else { s }
}

const RESERVED: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for",
    "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct",
    "super", "trait", "true", "try", "type", "unsafe", "use", "where", "while", "yield",
];
