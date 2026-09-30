//! Describe a declaration as coil source that builds the `macro` model.
//!
//! The expansion program passes each macro its input as a constructor
//! expression (`new TypeDecl(new Ident("Config"), "class", …)`), so no heap
//! layout is shared between the compiler and the VM.

use parser::ast::{AttrArgs, AttrLit, Attribute, EnumVariantPayload, Expression, Output};

use super::{string_lit, MacroArg};

/// Statements that build the model, one `let` per object, so no constructor
/// call is nested in another's arguments (the expansion entry runs them in
/// order, then passes the last binding to the macro).
#[derive(Default)]
pub struct Hoist {
    stmts: Vec<String>,
}

impl Hoist {
    /// Bind `expr` to a fresh local and return its name.
    fn bind(&mut self, expr: String) -> String {
        let name = format!("__e{}", self.stmts.len());
        self.stmts.push(format!("let {name} = {expr};"));
        name
    }

    fn bind_typed(&mut self, ty: &str, expr: String) -> String {
        let name = format!("__e{}", self.stmts.len());
        self.stmts.push(format!("let {name}: {ty} = {expr};"));
        name
    }

    /// The `let` statements, in order.
    pub fn statements(&self) -> &[String] {
        &self.stmts
    }
}

/// Which attributes to leave out of the model and of `source`.
pub struct Strip<'s> {
    /// Drop `#[derive(...)]`.
    pub derive: bool,
    /// Drop the attribute macro's own `#[name(...)]` (its first occurrence).
    pub attr: Option<&'s str>,
}

fn vec_of(h: &mut Hoist, ty: &str, items: Vec<String>) -> String {
    if items.is_empty() {
        return h.bind_typed(&format!("Vec<{ty}>"), "Vec::new()".to_string());
    }
    let arr = h.bind(format!("[{}]", items.join(", ")));
    h.bind(format!("Vec::from({arr})"))
}

fn ident(h: &mut Hoist, name: &str) -> String {
    h.bind(format!("new Ident({})", string_lit(name)))
}

fn strings(h: &mut Hoist, items: &[&str]) -> String {
    let lits = items.iter().map(|s| string_lit(s)).collect();
    vec_of(h, "string", lits)
}

/// `new TypeRef(text, head, args)` for a type annotation as written.
pub fn type_ref(h: &mut Hoist, ty: &Output<'_>) -> String {
    let text = ty.1.to_string();
    let (head, args): (String, Vec<String>) = match ty.1.as_ref() {
        Expression::TypeApp { name, args } => {
            (name.to_string(), args.iter().map(|a| type_ref(h, a)).collect())
        }
        Expression::Type(n) | Expression::Identifier(n) => (n.to_string(), Vec::new()),
        _ => (text.clone(), Vec::new()),
    };
    let args = vec_of(h, "TypeRef", args);
    h.bind(format!(
        "new TypeRef({}, {}, {args})",
        string_lit(&text),
        string_lit(&head),
    ))
}

fn named_type_ref(h: &mut Hoist, text: &str) -> String {
    let args = vec_of(h, "TypeRef", Vec::new());
    h.bind(format!("new TypeRef({0}, {0}, {args})", string_lit(text)))
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

fn attr(h: &mut Hoist, a: &Attribute<'_>) -> String {
    let args: Vec<String> = attr_args(&a.args)
        .iter()
        .map(|m| {
            h.bind(format!(
                "new AttrArg({}, {}, {})",
                string_lit(&m.key),
                string_lit(&m.value),
                string_lit(m.kind)
            ))
        })
        .collect();
    let args = vec_of(h, "AttrArg", args);
    h.bind(format!("new Attr({}, {args})", string_lit(a.name)))
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

fn attrs_model(h: &mut Hoist, attrs: &[Attribute<'_>], strip: &Strip<'_>) -> String {
    let items = kept(attrs, strip).into_iter().map(|a| attr(h, a)).collect();
    vec_of(h, "Attr", items)
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

fn field(
    h: &mut Hoist,
    name: &str,
    ty: &Output<'_>,
    is_pub: bool,
    attrs: &[Attribute<'_>],
    docs: &[&str],
) -> String {
    let none = Strip {
        derive: false,
        attr: None,
    };
    let name = ident(h, name);
    let ty = type_ref(h, ty);
    let attrs = attrs_model(h, attrs, &none);
    let docs = strings(h, docs);
    h.bind(format!("new Field({name}, {ty}, {is_pub}, {attrs}, {docs})"))
}

fn class_fields(h: &mut Hoist, fields: &[Output<'_>]) -> Vec<String> {
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
                Some(field(
                    h,
                    n,
                    ty,
                    *visibility == parser::ast::Visibility::Public,
                    attrs,
                    docs,
                ))
            }
            _ => None,
        })
        .collect()
}

fn variants(h: &mut Hoist, variants: &[Output<'_>]) -> Vec<String> {
    let none = Strip {
        derive: false,
        attr: None,
    };
    variants
        .iter()
        .filter_map(|v| {
            let Expression::EnumVariant {
                docs,
                attrs,
                name,
                payload,
                discriminant,
            } = v.1.as_ref()
            else {
                return None;
            };
            let (shape, tuple, fields) = match payload {
                EnumVariantPayload::Unit => ("unit", Vec::new(), Vec::new()),
                EnumVariantPayload::Tuple(tys) => {
                    ("tuple", tys.iter().map(|t| type_ref(h, t)).collect(), Vec::new())
                }
                EnumVariantPayload::Record(fs) => (
                    "record",
                    Vec::new(),
                    fs.iter()
                        .map(|f| field(h, f.name, &f.value, true, &[], &[]))
                        .collect(),
                ),
            };
            let value = discriminant.as_ref().map(|d| d.1.to_string()).unwrap_or_default();
            let name = ident(h, name);
            let tuple = vec_of(h, "TypeRef", tuple);
            let fields = vec_of(h, "Field", fields);
            let attrs = attrs_model(h, attrs, &none);
            let docs = strings(h, docs);
            Some(h.bind(format!(
                "new Variant({name}, {}, {tuple}, {fields}, {}, {attrs}, {docs})",
                string_lit(shape),
                string_lit(&value),
            )))
        })
        .collect()
}

/// `new TypeDecl(…)` for a class or enum node, or `None` for anything else.
/// Returns the local holding it; the statements are in `h`.
pub fn type_decl(
    h: &mut Hoist,
    node: &Output<'_>,
    source: &str,
    module: &str,
    strip: &Strip<'_>,
) -> Option<String> {
    let (kind, name, type_params, attrs, docs, field_list, variant_list, repr) = match node.1.as_ref() {
        Expression::Class {
            docs,
            attrs,
            name,
            type_params,
            fields,
        } => ("class", name, type_params, attrs, docs, class_fields(h, fields), Vec::new(), String::new()),
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
            variants(h, vs),
            crate::attrs::scalar_backing_ty_name(attrs, vs)
                .unwrap_or_default()
                .to_string(),
        ),
        _ => return None,
    };
    let generics: Vec<String> = type_params.iter().map(|p| ident(h, p.name)).collect();
    let name = ident(h, name);
    let generics = vec_of(h, "Ident", generics);
    let field_list = vec_of(h, "Field", field_list);
    let variant_list = vec_of(h, "Variant", variant_list);
    let attrs_v = attrs_model(h, attrs, strip);
    let docs = strings(h, docs);
    Some(h.bind(format!(
        "new TypeDecl({name}, {}, {generics}, {field_list}, {variant_list}, {attrs_v}, {}, {}, {docs}, {})",
        string_lit(kind),
        string_lit(&repr),
        string_lit(module),
        string_lit(&source_without_attrs(node, source, attrs, strip)),
    )))
}

/// `new FnDecl(…)` for a function node, or `None` for anything else.
/// Returns the local holding it; the statements are in `h`.
pub fn fn_decl(
    h: &mut Hoist,
    node: &Output<'_>,
    source: &str,
    owner: Option<&str>,
    is_pub: bool,
    strip: &Strip<'_>,
) -> Option<String> {
    let Expression::Function {
        docs,
        attrs,
        name,
        is_coro,
        is_static,
        type_params,
        args,
        returns,
        body,
        ..
    } = node.1.as_ref()
    else {
        return None;
    };
    let params: Vec<String> = match args.1.as_ref() {
        Expression::Fragment(items) => items
            .iter()
            .filter_map(|a| match a.1.as_ref() {
                Expression::Argument { ty: Some(ty), name, .. } => {
                    let name = ident(h, name);
                    let ty = type_ref(h, ty);
                    Some(h.bind(format!("new Param({name}, {ty})")))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let ret = match returns.as_ref() {
        Some(r) => type_ref(h, r),
        None => named_type_ref(h, "unit"),
    };
    // A block's span covers its statements; the braces are outside it.
    let body_text = body
        .as_ref()
        .and_then(|b| source.get(b.0.into_range()))
        .map(|inner| format!("{{{inner}}}"))
        .unwrap_or_else(|| body.as_ref().map(|b| b.1.to_string()).unwrap_or_default());
    let type_params: Vec<String> = type_params.iter().map(|p| ident(h, p.name)).collect();
    let name = ident(h, name);
    let params = vec_of(h, "Param", params);
    let type_params = vec_of(h, "Ident", type_params);
    let attrs_v = attrs_model(h, attrs, strip);
    let docs = strings(h, docs);
    Some(h.bind(format!(
        "new FnDecl({name}, {params}, {ret}, {type_params}, {attrs_v}, {}, {is_pub}, {is_static}, {is_coro}, {docs}, {}, {})",
        string_lit(owner.unwrap_or("")),
        string_lit(&body_text),
        string_lit(&source_without_attrs(node, source, attrs, strip)),
    )))
}

/// A coil expression for an attribute argument bound to a macro parameter.
pub fn arg_value(arg: &MacroArg) -> String {
    match arg.kind {
        "string" => string_lit(&arg.value),
        "ident" => string_lit(&arg.value),
        _ => arg.value.clone(),
    }
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
        let mut h = Hoist::default();
        let last = type_decl(&mut h, &items[0], src, "app", &strip).unwrap();
        let out = h.statements().join("\n");
        assert!(out.contains(&format!("let {last} = new TypeDecl(")), "{out}");
        assert!(out.contains("new AttrArg(\"rename\", \"p\", \"string\")"), "{out}");
        assert!(out.contains("new TypeRef(\"Vec<string>\", \"Vec\""), "{out}");
        assert!(out.contains("\"class Config {"), "{out}");
        assert!(!out.contains("derive"), "{out}");
        // No constructor is nested in another's arguments.
        for stmt in h.statements() {
            assert!(stmt.matches("new ").count() <= 1, "{stmt}");
        }
    }
}
