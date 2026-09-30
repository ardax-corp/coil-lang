//! Lower macro items to ordinary coil, before attribute expansion.
//!
//! - `derive Name(TypeDecl t) -> Code { … }` → `fn __derive_Name(TypeDecl t) -> Code { … }`
//! - `attr name(FnDecl f, string msg) -> Code { … }` → `fn __attr_name(…)`
//!   (an `attr` whose first parameter is not `FnDecl` / `TypeDecl` is a
//!   legacy runtime decorator and is left for `attrs::expand_program`)
//! - `quote kind { text ${e} $(xs) sep * }` →
//!   `new Code("text" + e.src() + join(xs, "sep"))`
//!
//! Hygiene: a name bound with `let` / `for` inside any quote of one item is
//! renamed to `name__m` in all of that item's quotes, so generated locals
//! never capture or shadow names in code spliced from the user. A bare name
//! for one of the module's own items (`JsonValue`, `helper()`) is written as
//! its fully qualified path (`json::JsonValue`), so expansions never depend
//! on what the using module imports.

use std::collections::HashSet;

use parser::ast::{Expression, Output, QuotePart};
use parser::SimpleSpan;
use reporting::{ErrorCode, Message};

use super::{attr_fn_name, derive_fn_name, MacroDecl, MacroInput, MacroKind, MACRO_MODULE};
use crate::attrs::{fresh_span, leak};

/// Suffix hygienic renaming appends to quote-local names.
pub const HYGIENE_SUFFIX: &str = "__m";

#[derive(Default)]
pub struct Lowered {
    pub decls: Vec<MacroDecl>,
    pub messages: Vec<Message>,
}

/// Lower every macro item and quote in `ast` (a `Program`).
///
/// `module` is the file's module path; the embedded `macro` module itself
/// gets no implicit import of its own names.
pub fn lower_program(ast: &mut Output<'_>, module: &str) -> Lowered {
    let mut out = Lowered::default();
    let Expression::Program(children) = ast.1.as_mut() else {
        return out;
    };
    let mut uses_quote = false;
    let mut uses_join = false;
    let own_items = if module.is_empty() || module == MACRO_MODULE {
        HashSet::new()
    } else {
        module_item_names(children)
    };
    for child in children.iter_mut() {
        lower_item(child, &mut out);
        let names = Names {
            bound: quote_bound_names(child),
            own_items: &own_items,
            module,
        };
        lower_quotes(child, &names, &mut uses_quote, &mut uses_join, &mut out.messages);
    }
    if module != MACRO_MODULE && (uses_quote || !out.decls.is_empty()) {
        let mut needed = vec!["Code"];
        if uses_join {
            needed.push("join");
        }
        inject_macro_uses(children, &needed);
    }
    out
}

/// How template text is rewritten: hygienic locals and module-qualified items.
struct Names<'n> {
    bound: HashSet<String>,
    own_items: &'n HashSet<String>,
    module: &'n str,
}

/// Top-level types, traits, functions and statics a module declares.
fn module_item_names(children: &[Output<'_>]) -> HashSet<String> {
    let mut out = HashSet::new();
    for c in children {
        let name = match c.1.as_ref() {
            Expression::Class { name, .. }
            | Expression::EnumDecl { name, .. }
            | Expression::TypeClass { name, .. }
            | Expression::TypeAlias { name, .. }
            | Expression::Function { name, .. } => *name,
            Expression::StaticDecl { name, .. } => *name,
            _ => continue,
        };
        out.insert(name.to_string());
    }
    out
}

fn lower_item<'a>(item: &mut Output<'a>, out: &mut Lowered) {
    let span = item.0;
    match item.1.as_mut() {
        Expression::DeriveDecl {
            docs,
            name,
            args,
            returns,
            helpers,
            body,
        } => {
            let params = param_list(args);
            if params.len() != 1 || !is_model_type(&params[0].1, "TypeDecl") {
                out.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("derive `{name}` must take exactly one `TypeDecl` parameter"),
                    span.into_range(),
                ));
            }
            if returns.is_none() {
                out.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("derive `{name}` must return `Code`"),
                    span.into_range(),
                ));
            }
            out.decls.push(MacroDecl {
                kind: MacroKind::Derive,
                name: name.to_string(),
                fn_name: derive_fn_name(name),
                helpers: helpers.iter().map(|h| h.to_string()).collect(),
                input: MacroInput::TypeDecl,
                params: Vec::new(),
            });
            let func = Expression::Function {
                docs: std::mem::take(docs),
                attrs: Vec::new(),
                name: leak(derive_fn_name(name)),
                is_coro: false,
                is_static: false,
                type_params: Vec::new(),
                args: args.clone(),
                returns: returns.clone(),
                where_constraints: Vec::new(),
                body: Some(body.clone()),
            };
            *item.1 = func;
        }
        Expression::AttrDecl {
            docs,
            name,
            type_params,
            args,
            returns,
            where_constraints,
            body,
        } => {
            let params = param_list(args);
            let input = match params.first() {
                Some((_, ty, _)) if is_model_type(ty, "FnDecl") => MacroInput::FnDecl,
                Some((_, ty, _)) if is_model_type(ty, "TypeDecl") => MacroInput::TypeDecl,
                // Legacy `target(...args)` decorator.
                _ => return,
            };
            if params.iter().skip(1).any(|(_, _, rest)| *rest) {
                out.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("attribute macro `{name}` cannot take a rest parameter"),
                    span.into_range(),
                ));
            }
            if returns.is_none() {
                out.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("attribute macro `{name}` must return `Code`"),
                    span.into_range(),
                ));
            }
            out.decls.push(MacroDecl {
                kind: MacroKind::Attr,
                name: name.to_string(),
                fn_name: attr_fn_name(name),
                helpers: Vec::new(),
                input,
                params: params
                    .iter()
                    .skip(1)
                    .map(|(n, t, _)| (n.clone(), t.clone()))
                    .collect(),
            });
            let func = Expression::Function {
                docs: std::mem::take(docs),
                attrs: Vec::new(),
                name: leak(attr_fn_name(name)),
                is_coro: false,
                is_static: false,
                type_params: type_params.clone(),
                args: args.clone(),
                returns: returns.clone(),
                where_constraints: where_constraints.clone(),
                body: Some(body.clone()),
            };
            *item.1 = func;
        }
        _ => {}
    }
}

/// `(name, type as written, is_rest)` for each parameter of a `Fragment` arg list.
fn param_list(args: &Output<'_>) -> Vec<(String, String, bool)> {
    let Expression::Fragment(items) = args.1.as_ref() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|a| match a.1.as_ref() {
            Expression::Argument {
                ty, name, is_rest, ..
            } => Some((
                name.to_string(),
                ty.as_ref().map(|t| t.1.to_string()).unwrap_or_default(),
                *is_rest,
            )),
            _ => None,
        })
        .collect()
}

fn is_model_type(ty: &str, name: &str) -> bool {
    ty == name || ty == format!("{MACRO_MODULE}::{name}")
}

fn inject_macro_uses(children: &mut Vec<Output<'_>>, needed: &[&str]) {
    let mut have: HashSet<String> = HashSet::new();
    let mut glob = false;
    fn scan(node: &Output<'_>, have: &mut HashSet<String>, glob: &mut bool) {
        match node.1.as_ref() {
            Expression::Use { path, name, alias } if path.first().map(String::as_str) == Some(MACRO_MODULE) => {
                if name == "*" {
                    *glob = true;
                } else {
                    have.insert(alias.clone().unwrap_or_else(|| name.clone()));
                }
            }
            Expression::Fragment(items) => items.iter().for_each(|i| scan(i, have, glob)),
            _ => {}
        }
    }
    for child in children.iter() {
        scan(child, &mut have, &mut glob);
    }
    if glob {
        return;
    }
    for name in needed.iter().rev() {
        if have.contains(*name) {
            continue;
        }
        children.insert(
            0,
            (
                fresh_span(),
                Box::new(Expression::Use {
                    path: vec![MACRO_MODULE.to_string()],
                    name: name.to_string(),
                    alias: None,
                }),
            ),
        );
    }
}

/// Names bound by `let` / `for` in the template text of every quote under `node`.
fn quote_bound_names(node: &Output<'_>) -> HashSet<String> {
    fn walk(node: &Output<'_>, out: &mut HashSet<String>) {
        if let Expression::Quote { parts, .. } = node.1.as_ref() {
            let text: String = parts
                .iter()
                .map(|p| match p {
                    QuotePart::Lit(t) => *t,
                    // Keep token boundaries across holes.
                    _ => " ",
                })
                .collect();
            let toks = tokens(&text);
            for w in toks.windows(2) {
                if let (Tok::Ident(kw), Tok::Ident(name)) = (&w[0], &w[1])
                    && (*kw == "let" || *kw == "for")
                {
                    out.insert(name.to_string());
                }
            }
        }
        node.1.for_each_child(&mut |c| walk(c, out));
    }
    let mut out = HashSet::new();
    walk(node, &mut out);
    out
}

#[derive(Debug, PartialEq)]
enum Tok<'s> {
    Ident(&'s str),
    Punct(char),
    Str,
}

/// Coarse coil tokens: identifiers, string literals, single punctuation.
fn tokens(text: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    for (tok, _) in token_spans(text) {
        out.push(tok);
    }
    out
}

fn token_spans(text: &str) -> Vec<(Tok<'_>, std::ops::Range<usize>)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(bytes.len());
            out.push((Tok::Str, start..i));
        } else if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((Tok::Ident(&text[start..i]), start..i));
        } else if c.is_ascii_digit() {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if c.is_ascii_whitespace() {
            i += 1;
        } else {
            out.push((Tok::Punct(c as char), i..i + 1));
            i += 1;
        }
    }
    out
}

/// Rewrite one piece of template text: bound locals get [`HYGIENE_SUFFIX`],
/// the module's own items get their module path. Names after `.` or `::`
/// (members, already-qualified paths) are left alone.
fn rewrite_names(text: &str, names: &Names<'_>) -> String {
    if names.bound.is_empty() && names.own_items.is_empty() {
        return text.to_string();
    }
    let spans = token_spans(text);
    let mut out = String::with_capacity(text.len() + 8);
    let mut last = 0;
    for (k, (tok, range)) in spans.iter().enumerate() {
        let Tok::Ident(name) = tok else { continue };
        let prev = k.checked_sub(1).map(|p| &spans[p].0);
        let after_member = matches!(prev, Some(Tok::Punct('.')))
            || (matches!(prev, Some(Tok::Punct(':')))
                && k >= 2
                && matches!(spans[k - 2].0, Tok::Punct(':'))
                && spans[k - 2].1.end == spans[k - 1].1.start);
        if after_member {
            continue;
        }
        if names.bound.contains(*name) {
            out.push_str(&text[last..range.end]);
            out.push_str(HYGIENE_SUFFIX);
            last = range.end;
        } else if names.own_items.contains(*name) {
            out.push_str(&text[last..range.start]);
            out.push_str(names.module);
            out.push_str("::");
            out.push_str(name);
            last = range.end;
        }
    }
    out.push_str(&text[last..]);
    out
}

fn lower_quotes(
    node: &mut Output<'_>,
    names: &Names<'_>,
    uses_quote: &mut bool,
    uses_join: &mut bool,
    messages: &mut Vec<Message>,
) {
    // Holes first: a hole may itself contain a quote.
    node.1
        .for_each_child_mut(&mut |c| lower_quotes(c, names, uses_quote, uses_join, messages));
    let span = node.0;
    let Expression::Quote { parts, .. } = node.1.as_mut() else {
        return;
    };
    *uses_quote = true;
    let parts = std::mem::take(parts);
    let mut pieces: Vec<Output<'_>> = Vec::new();
    for part in parts {
        match part {
            QuotePart::Lit(text) => {
                let renamed = rewrite_names(text, names);
                if !renamed.is_empty() {
                    pieces.push(node_at(Expression::String(leak(super::escape_string_lit(&renamed)))));
                }
            }
            QuotePart::Splice(e) => {
                let access = node_at(Expression::Access(e, "src"));
                pieces.push(node_at(Expression::Call {
                    name: access,
                    args: Some(Vec::new()),
                }));
            }
            QuotePart::Repeat { list, sep } => {
                *uses_join = true;
                let sep = node_at(Expression::String(leak(super::escape_string_lit(sep))));
                pieces.push(node_at(Expression::Call {
                    name: node_at(Expression::Identifier("join")),
                    args: Some(vec![list, sep]),
                }));
            }
        }
    }
    let mut concat = pieces
        .into_iter()
        .reduce(|lhs, rhs| node_at(Expression::Add(lhs, rhs)))
        .unwrap_or_else(|| node_at(Expression::String("")));
    // A lone splice is already a string; `+ ""` keeps every quote a concat.
    if !matches!(concat.1.as_ref(), Expression::Add(..) | Expression::String(_)) {
        concat = node_at(Expression::Add(concat, node_at(Expression::String(""))));
    }
    *node = (
        span,
        Box::new(Expression::Instantiate(
            node_at(Expression::Identifier("Code")),
            Some(vec![concat]),
        )),
    );
}

fn node_at(e: Expression<'_>) -> Output<'_> {
    (fresh_span(), Box::new(e))
}

/// True when `span` came from a quote-lowered node (fresh synthetic span).
pub fn is_synthetic(span: SimpleSpan) -> bool {
    span.start >= 0x4000_0000
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    #[test]
    fn rename_skips_members_and_strings() {
        let own = HashSet::new();
        let names = Names {
            bound: ["obj".to_string()].into_iter().collect(),
            own_items: &own,
            module: "m",
        };
        let out = rewrite_names("let obj = x.obj; \"obj\"; obj.set(obj)", &names);
        assert_eq!(out, "let obj__m = x.obj; \"obj\"; obj__m.set(obj__m)");
    }

    #[test]
    fn own_items_become_qualified() {
        let own: HashSet<String> = ["Value".to_string(), "helper".to_string()].into_iter().collect();
        let names = Names {
            bound: HashSet::new(),
            own_items: &own,
            module: "json",
        };
        let out = rewrite_names("let v: Value = helper(Value::make(), json::Value, o.helper, x: int)", &names);
        assert_eq!(
            out,
            "let v: json::Value = json::helper(json::Value::make(), json::Value, o.helper, x: int)"
        );
    }

    #[test]
    fn bound_names_come_from_let_and_for() {
        let src = "fn f() -> Code { return quote stmts { let a = 1; for b in xs { } c = 2; }; }";
        let ast = Pratt::default().parse(src).unwrap();
        let names = quote_bound_names(&ast);
        assert!(names.contains("a") && names.contains("b") && !names.contains("c"));
    }

}
