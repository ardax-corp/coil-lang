//! Describe a declaration as data the `macro` model decodes.
//!
//! Each macro call's input is one string: length-prefixed fields
//! (`<len>:<bytes>`, lists as a count then their items) in the order the
//! model's constructors take them. `macro::Reader` rebuilds the objects, so
//! the compiled expansion program depends only on the providers and no heap
//! layout is shared between the compiler and the VM.

use parser::ast::{AttrArgs, AttrLit, Attribute, EnumVariantPayload, Expression, Output};

use super::MacroArg;

/// A macro input being written; see the module docs for the format.
#[derive(Default)]
pub struct Wire {
    buf: String,
}

impl Wire {
    pub fn str(&mut self, s: &str) {
        self.buf.push_str(&s.len().to_string());
        self.buf.push(':');
        self.buf.push_str(s);
    }

    pub fn bool(&mut self, b: bool) {
        self.str(if b { "1" } else { "0" });
    }

    pub fn count(&mut self, n: usize) {
        self.str(&n.to_string());
    }

    pub fn finish(self) -> String {
        self.buf
    }
}

/// Which attributes to leave out of the model and of `source`.
pub struct Strip<'s> {
    /// Drop `#[derive(...)]`.
    pub derive: bool,
    /// Drop the attribute macro's own `#[name(...)]` (its first occurrence).
    pub attr: Option<&'s str>,
}

fn ident(w: &mut Wire, name: &str) {
    w.str(name);
}

fn strings(w: &mut Wire, items: &[&str]) {
    w.count(items.len());
    for s in items {
        w.str(s);
    }
}

/// A `TypeRef`: text, head, then its generic arguments.
pub fn type_ref(w: &mut Wire, ty: &Output<'_>) {
    let text = ty.1.to_string();
    match ty.1.as_ref() {
        Expression::TypeApp { name, args } => {
            w.str(&text);
            w.str(name);
            w.count(args.len());
            for a in args {
                type_ref(w, a);
            }
        }
        Expression::Type(n) | Expression::Identifier(n) => {
            w.str(&text);
            w.str(n);
            w.count(0);
        }
        _ => {
            w.str(&text);
            w.str(&text);
            w.count(0);
        }
    }
}

fn named_type_ref(w: &mut Wire, text: &str) {
    w.str(text);
    w.str(text);
    w.count(0);
}

/// Attribute arguments in the order written.
pub fn attr_args(args: &AttrArgs<'_>) -> Vec<MacroArg> {
    fn lit(l: &AttrLit<'_>) -> (String, &'static str) {
        match l {
            AttrLit::String(s) => (crate::codegen::unescape_coil_string(s), "string"),
            AttrLit::Int(i) => (i.to_string(), "int"),
            AttrLit::Float(f) => (f.to_string(), "float"),
            AttrLit::Bool(b) => (b.to_string(), "bool"),
        }
    }
    match args {
        AttrArgs::Empty => Vec::new(),
        AttrArgs::Idents(ids) => ids
            .iter()
            .map(|i| MacroArg {
                key: String::new(),
                value: i.to_string(),
                kind: "ident",
            })
            .collect(),
        AttrArgs::KeyValues(kvs) => kvs
            .iter()
            .map(|(k, v)| {
                let (value, kind) = lit(v);
                MacroArg {
                    key: k.to_string(),
                    value,
                    kind,
                }
            })
            .collect(),
        AttrArgs::Positional(ls) => ls
            .iter()
            .map(|v| {
                let (value, kind) = lit(v);
                MacroArg {
                    key: String::new(),
                    value,
                    kind,
                }
            })
            .collect(),
        AttrArgs::String(s) => vec![MacroArg {
            key: String::new(),
            value: crate::codegen::unescape_coil_string(s),
            kind: "string",
        }],
    }
}

fn attr(w: &mut Wire, a: &Attribute<'_>) {
    w.str(a.name);
    let args = attr_args(&a.args);
    w.count(args.len());
    for m in &args {
        w.str(&m.key);
        w.str(&m.value);
        w.str(m.kind);
    }
}

/// Attributes that stay on the item: all but `#[derive]` (when stripped)
/// and the first `#[attr]` of the macro being expanded — a second one of the
/// same name expands in the next round.
fn kept<'a, 'b>(attrs: &'b [Attribute<'a>], strip: &Strip<'_>) -> Vec<&'b Attribute<'a>> {
    let own = strip
        .attr
        .and_then(|name| attrs.iter().position(|a| a.name == name));
    attrs
        .iter()
        .enumerate()
        .filter(|(i, a)| !(strip.derive && a.name == "derive") && Some(*i) != own)
        .map(|(_, a)| a)
        .collect()
}

fn attrs_model(w: &mut Wire, attrs: &[Attribute<'_>], strip: &Strip<'_>) {
    let items = kept(attrs, strip);
    w.count(items.len());
    for a in items {
        attr(w, a);
    }
}

/// Declaration text without its leading docs / attributes, prefixed by the
/// attributes that remain after `strip`.
fn source_without_attrs(node: &Output<'_>, source: &str, attrs: &[Attribute<'_>], strip: &Strip<'_>) -> String {
    let range = node.0.into_range();
    let text = source.get(range).unwrap_or_default();
    let body = skip_leading_attrs(text);
    let mut out = String::new();
    for a in kept(attrs, strip) {
        out.push_str(&a.to_string());
        out.push('\n');
    }
    out.push_str(body);
    out
}

/// Skip whitespace, `///` / `//` comment lines and `#[…]` groups.
pub fn skip_leading_attrs(text: &str) -> &str {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.starts_with("//") {
            rest = trimmed.split_once('\n').map(|(_, r)| r).unwrap_or("");
            continue;
        }
        if trimmed.starts_with("#[") {
            let mut depth = 0usize;
            let mut end = trimmed.len();
            let mut in_str = false;
            let mut escaped = false;
            for (i, c) in trimmed.char_indices() {
                if in_str {
                    match c {
                        _ if escaped => escaped = false,
                        '\\' => escaped = true,
                        '"' => in_str = false,
                        _ => {}
                    }
                    continue;
                }
                match c {
                    '"' => in_str = true,
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = &trimmed[end..];
            continue;
        }
        return trimmed;
    }
}

fn field(w: &mut Wire, name: &str, ty: &Output<'_>, is_pub: bool, attrs: &[Attribute<'_>], docs: &[&str]) {
    let none = Strip {
        derive: false,
        attr: None,
    };
    ident(w, name);
    type_ref(w, ty);
    w.bool(is_pub);
    attrs_model(w, attrs, &none);
    strings(w, docs);
}

/// An instance field of a class.
struct ClassField<'n, 'a> {
    name: &'a str,
    ty: &'n Output<'a>,
    is_pub: bool,
    attrs: &'n [Attribute<'a>],
    docs: &'n [&'a str],
}

/// Instance fields of a class.
fn class_fields<'n, 'a>(fields: &'n [Output<'a>]) -> Vec<ClassField<'n, 'a>> {
    fields
        .iter()
        .filter_map(|f| match f.1.as_ref() {
            Expression::Field {
                docs,
                attrs,
                visibility,
                modifier,
                name,
                ty,
                ..
            } if *modifier == parser::ast::FieldModifier::Instance => {
                let Expression::Identifier(n) = name.1.as_ref() else {
                    return None;
                };
                Some(ClassField {
                    name: n,
                    ty,
                    is_pub: *visibility == parser::ast::Visibility::Public,
                    attrs: attrs.as_slice(),
                    docs: docs.as_slice(),
                })
            }
            _ => None,
        })
        .collect()
}

fn variants(w: &mut Wire, variants: &[Output<'_>]) {
    let none = Strip {
        derive: false,
        attr: None,
    };
    let vs: Vec<_> = variants
        .iter()
        .filter_map(|v| match v.1.as_ref() {
            Expression::EnumVariant {
                docs,
                attrs,
                name,
                payload,
                discriminant,
            } => Some((docs, attrs, name, payload, discriminant)),
            _ => None,
        })
        .collect();
    w.count(vs.len());
    for (docs, attrs, name, payload, discriminant) in vs {
        ident(w, name);
        match payload {
            EnumVariantPayload::Unit => {
                w.str("unit");
                w.count(0);
                w.count(0);
            }
            EnumVariantPayload::Tuple(tys) => {
                w.str("tuple");
                w.count(tys.len());
                for t in tys {
                    type_ref(w, t);
                }
                w.count(0);
            }
            EnumVariantPayload::Record(fs) => {
                w.str("record");
                w.count(0);
                w.count(fs.len());
                for f in fs {
                    field(w, f.name, &f.value, true, &[], &[]);
                }
            }
        }
        w.str(&discriminant.as_ref().map(|d| d.1.to_string()).unwrap_or_default());
        attrs_model(w, attrs, &none);
        strings(w, docs);
    }
}

/// A `TypeDecl` for a class or enum node; `false` for anything else.
pub fn type_decl(w: &mut Wire, node: &Output<'_>, source: &str, module: &str, strip: &Strip<'_>) -> bool {
    let (kind, name, type_params, attrs, docs, fields, vs, repr) = match node.1.as_ref() {
        Expression::Class {
            docs,
            attrs,
            name,
            type_params,
            fields,
        } => ("class", name, type_params, attrs, docs, class_fields(fields), None, String::new()),
        Expression::EnumDecl {
            docs,
            attrs,
            name,
            type_params,
            variants: vs,
        } => (
            "enum",
            name,
            type_params,
            attrs,
            docs,
            Vec::new(),
            Some(vs),
            crate::attrs::scalar_backing_ty_name(attrs, vs)
                .unwrap_or_default()
                .to_string(),
        ),
        _ => return false,
    };
    ident(w, name);
    w.str(kind);
    w.count(type_params.len());
    for p in type_params {
        ident(w, p.name);
    }
    w.count(fields.len());
    for f in fields {
        field(w, f.name, f.ty, f.is_pub, f.attrs, f.docs);
    }
    match vs {
        Some(vs) => variants(w, vs),
        None => w.count(0),
    }
    attrs_model(w, attrs, strip);
    w.str(&repr);
    w.str(module);
    strings(w, docs);
    w.str(&source_without_attrs(node, source, attrs, strip));
    true
}

/// An `FnDecl` for a function node; `false` for anything else.
pub fn fn_decl(
    w: &mut Wire,
    node: &Output<'_>,
    source: &str,
    owner: Option<&str>,
    is_pub: bool,
    strip: &Strip<'_>,
) -> bool {
    let Expression::Function {
        docs,
        attrs,
        name,
        is_coro,
        is_static,
        type_params,
        args,
        returns,
        effects,
        body,
        ..
    } = node.1.as_ref()
    else {
        return false;
    };
    let params: Vec<(&str, &Output<'_>)> = match args.1.as_ref() {
        Expression::Fragment(items) => items
            .iter()
            .filter_map(|a| match a.1.as_ref() {
                Expression::Argument { ty: Some(ty), name, .. } => Some((*name, ty)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    // A block's span covers its statements; the braces are outside it.
    let body_text = body
        .as_ref()
        .and_then(|b| source.get(b.0.into_range()))
        .map(|inner| format!("{{{inner}}}"))
        .unwrap_or_else(|| body.as_ref().map(|b| b.1.to_string()).unwrap_or_default());
    ident(w, name);
    w.count(params.len());
    for (n, ty) in params {
        ident(w, n);
        type_ref(w, ty);
    }
    match returns.as_ref() {
        Some(r) => type_ref(w, r),
        None => named_type_ref(w, "unit"),
    }
    w.count(type_params.len());
    for p in type_params {
        ident(w, p.name);
    }
    attrs_model(w, attrs, strip);
    w.str(owner.unwrap_or(""));
    w.bool(is_pub);
    w.bool(*is_static);
    w.bool(*is_coro);
    w.bool(effects.is_some());
    w.bool(effects.as_ref().is_some_and(|e| e.pure));
    strings(w, effects.as_ref().map_or(&[][..], |e| &e.uses[..]));
    strings(w, docs);
    w.str(&body_text);
    w.str(&source_without_attrs(node, source, attrs, strip));
    true
}

/// Write an attribute argument bound to a macro parameter: strings and
/// identifiers as their text, numbers and booleans as written.
/// Coarse kind of a function-style macro argument (`Expr::kind()`).
pub fn expr_kind(e: &Expression<'_>) -> &'static str {
    match e {
        Expression::Integer(_) | Expression::Float(_) | Expression::String(_) | Expression::Bool(_) => "literal",
        Expression::Negate(inner) if matches!(inner.1.as_ref(), Expression::Integer(_) | Expression::Float(_)) => {
            "literal"
        }
        Expression::Identifier(_) => "ident",
        Expression::QualifiedAccess { .. } | Expression::Access(..) => "path",
        Expression::Call { .. } | Expression::Construct { .. } | Expression::Instantiate(..) => "call",
        Expression::Expr(inner) => expr_kind(inner.1.as_ref()),
        Expression::Group(inner) if expr_kind(inner.1.as_ref()) != "other" => expr_kind(inner.1.as_ref()),
        _ => "other",
    }
}

/// One `Expr`: kind, then the source text (`source` sliced by its span).
pub fn expr(w: &mut Wire, e: &Output<'_>, source: &str) {
    w.str(expr_kind(e.1.as_ref()));
    let text = source
        .get(e.0.start..e.0.end)
        .map(str::trim)
        .map(str::to_string)
        .unwrap_or_else(|| e.1.to_string());
    w.str(&text);
}

pub fn arg(w: &mut Wire, arg: &MacroArg) {
    w.str(&arg.value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    #[test]
    fn skip_attrs_and_docs() {
        assert_eq!(
            skip_leading_attrs("/// d\n#[derive(A)]\n#[x(y = \"]\")]\nclass C {}"),
            "class C {}"
        );
    }

    #[test]
    fn encodes_class_fields_and_attrs() {
        let src = "#[derive(ToJson)]\nclass Config {\n    #[json(rename = \"p\")]\n    pub port: int,\n    name: Vec<string>,\n}";
        let ast = Pratt::default().parse(src).unwrap();
        let Expression::Program(items) = ast.1.as_ref() else { panic!() };
        let strip = Strip { derive: true, attr: None };
        let mut w = Wire::default();
        assert!(type_decl(&mut w, &items[0], src, "app", &strip));
        let out = w.finish();
        assert!(out.starts_with("6:Config5:class1:01:24:port"), "{out}");
        assert!(out.contains("4:json1:16:rename1:p6:string"), "{out}");
        assert!(out.contains("11:Vec<string>3:Vec1:1"), "{out}");
        assert!(out.contains("class Config {"), "{out}");
        assert!(!out.contains("derive"), "{out}");
    }
}
