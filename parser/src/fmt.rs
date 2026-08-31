//! Source pretty-printer for coil `.hy` files.
//!
//! Preserves `//` comments and emits attached `///` docs on declarations.

use chumsky::span::SimpleSpan;
use crate::ast::{
    AdjustOp, AssignOp, Attribute, EnumConstructPayload, EnumVariantPayload, ExternFunction,
    ExternStructDecl, Expression, FieldModifier, LetPattern, Output, Pattern, RecordFieldDecl,
    RecordFieldValue, TypeParam, Visibility, WhereConstraint,
};
use crate::Pratt;
use reporting::Message;

const INDENT: &str = "    ";
/// Soft line-wrap budget (characters from start of line).
const MAX_WIDTH: usize = 100;

pub fn format_source(src: &str) -> Result<String, Message> {
    let ast = Pratt::default().parse(src)?;
    Ok(format_program(ast.1.as_ref()))
}

/// Format a source range while preserving the formatter's whole-file parse
/// guarantees. The current formatter returns the complete formatted document;
/// callers can diff it against the requested range.
pub fn format_range(src: &str, _range: std::ops::Range<usize>) -> Result<String, Message> {
    format_source(src)
}

pub fn format_program(expr: &Expression<'_>) -> String {
    let mut f = Formatter::new();
    f.fmt_expression(expr);
    if !f.out.ends_with('\n') {
        f.out.push('\n');
    }
    f.out
}

struct Formatter {
    indent: usize,
    out: String,
    /// When true, never insert soft wraps (used to measure flat width).
    flat: bool,
}

impl Formatter {
    fn new() -> Self {
        Self {
            indent: 0,
            out: String::new(),
            flat: false,
        }
    }

    fn push_str(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn newline(&mut self) {
        self.out.push('\n');
    }

    fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
    }

    fn current_col(&self) -> usize {
        match self.out.rfind('\n') {
            Some(i) => self.out.len() - i - 1,
            None => self.out.len(),
        }
    }

    fn pad_to_col(&mut self, col: usize) {
        let cur = self.current_col();
        if col > cur {
            for _ in 0..(col - cur) {
                self.out.push(' ');
            }
        }
    }

    fn with_indent(&mut self, f: impl FnOnce(&mut Self)) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    /// Render `expr` with soft wraps disabled (single-line preference).
    fn render_flat(&self, expr: &Expression<'_>) -> String {
        let mut f = Formatter {
            indent: self.indent,
            out: String::new(),
            flat: true,
        };
        f.fmt_expression(expr);
        f.out
    }

    fn fits_flat(&self, flat: &str) -> bool {
        self.flat || self.current_col().saturating_add(flat.len()) <= MAX_WIDTH
    }

    fn fmt_output(&mut self, output: &Output<'_>) {
        self.fmt_expression(output.1.as_ref());
    }

    /// Comma-separated list inside `open`/`close`, soft-wrapping with trailing commas.
    ///
    /// - Flat when the whole list fits in [`MAX_WIDTH`].
    /// - Broken form puts each item on its own indented line with a trailing `,`
    ///   (including after the last item) for cleaner diffs.
    /// - `single_item_trailing`: always keep a trailing comma when there is exactly
    ///   one item (needed for 1-tuples: `(x,)`).
    fn fmt_delimited_outputs(
        &mut self,
        open: &str,
        close: &str,
        items: &[Output<'_>],
        single_item_trailing: bool,
    ) {
        if items.is_empty() {
            self.push_str(open);
            self.push_str(close);
            return;
        }

        let flat = {
            let mut f = Formatter {
                indent: self.indent,
                out: String::new(),
                flat: true,
            };
            f.push_str(open);
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                f.fmt_output(item);
            }
            if single_item_trailing && items.len() == 1 {
                f.push_str(",");
            }
            f.push_str(close);
            f.out
        };

        let has_docs = items.iter().any(|item| {
            matches!(
                item.1.as_ref(),
                Expression::Argument { docs, .. } if !docs.is_empty()
            )
        });

        if !has_docs && self.fits_flat(&flat) {
            self.push_str(open);
            let was = self.flat;
            self.flat = true;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    self.push_str(", ");
                }
                self.fmt_output(item);
            }
            if single_item_trailing && items.len() == 1 {
                self.push_str(",");
            }
            self.flat = was;
            self.push_str(close);
            return;
        }

        self.push_str(open);
        self.newline();
        self.with_indent(|f| {
            for item in items {
                f.write_indent();
                f.fmt_output(item);
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
        self.push_str(close);
    }

    fn fmt_delimited_strings(
        &mut self,
        open: &str,
        close: &str,
        items: &[String],
        single_item_trailing: bool,
    ) {
        if items.is_empty() {
            self.push_str(open);
            self.push_str(close);
            return;
        }
        let mut flat = String::from(open);
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                flat.push_str(", ");
            }
            flat.push_str(item);
        }
        if single_item_trailing && items.len() == 1 {
            flat.push(',');
        }
        flat.push_str(close);
        if self.fits_flat(&flat) {
            self.push_str(&flat);
            return;
        }
        self.push_str(open);
        self.newline();
        self.with_indent(|f| {
            for item in items {
                f.write_indent();
                f.push_str(item);
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
        self.push_str(close);
    }

    fn fmt_record_list(&mut self, items: &[RecordFieldValue<'_>]) {
        if items.is_empty() {
            self.push_str("{}");
            return;
        }
        let flat = {
            let mut f = Formatter {
                indent: self.indent,
                out: String::new(),
                flat: true,
            };
            f.push_str("{ ");
            for (i, field) in items.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                f.push_str(field.name);
                f.push_str(": ");
                f.fmt_output(&field.value);
            }
            f.push_str(" }");
            f.out
        };
        if self.fits_flat(&flat) {
            self.push_str("{ ");
            let was = self.flat;
            self.flat = true;
            for (i, field) in items.iter().enumerate() {
                if i > 0 {
                    self.push_str(", ");
                }
                self.push_str(field.name);
                self.push_str(": ");
                self.fmt_output(&field.value);
            }
            self.flat = was;
            self.push_str(" }");
            return;
        }
        self.push_str("{");
        self.newline();
        self.with_indent(|f| {
            for field in items {
                f.write_indent();
                f.push_str(field.name);
                f.push_str(": ");
                f.fmt_output(&field.value);
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
        self.push_str("}");
    }

    /// Class / enum-style body: always multiline when non-empty, trailing commas.
    fn fmt_comma_body(&mut self, items: &[Output<'_>]) {
        if items.is_empty() {
            self.push_str(" {}");
            return;
        }
        self.push_str(" {");
        self.newline();
        self.with_indent(|f| {
            for item in items {
                f.write_indent();
                f.fmt_expression(item.1.as_ref());
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
        self.push_str("}");
    }

    fn fmt_paren_arg_list(&mut self, args: &Output<'_>) {
        match args.1.as_ref() {
            Expression::Fragment(items) => {
                self.fmt_delimited_outputs("(", ")", items, false);
            }
            other => {
                self.push_str("(");
                self.fmt_expression(other);
                self.push_str(")");
            }
        }
    }

    fn fmt_expression(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::Comment(text) => self.fmt_comment_line(text),

            Expression::Integer(n) => self.push_str(&n.to_string()),
            Expression::Float(n) => self.push_str(&format!("{n:?}")),
            Expression::Bool(b) => self.push_str(if *b { "true" } else { "false" }),
            Expression::String(s) => {
                self.push_str("\"");
                self.push_str(s);
                self.push_str("\"");
            }
            Expression::Identifier(id) => self.push_str(id),
            Expression::Type(n) => self.push_str(n),
            Expression::Break => self.push_str("break"),
            Expression::Continue => self.push_str("continue"),
            Expression::Noop(n) => {
                self.push_str("@{ ");
                self.fmt_output(n);
                self.push_str(" }@");
            }
            Expression::Default(name) => self.push_str(name),
            Expression::Module(name, _) => {
                self.push_str("mod ");
                self.push_str(name);
                self.push_str(";");
            }

            Expression::Expr(inner) | Expression::ImplicitReturn(inner) => self.fmt_output(inner),
            Expression::Group(g) => {
                self.push_str("(");
                self.fmt_output(g);
                self.push_str(")");
            }
            Expression::ExprStatement(e) => {
                self.fmt_output(e);
                self.push_str(";");
            }
            Expression::Statement(s) => self.fmt_statement_line(s),

            Expression::Fragment(items) => self.fmt_fragment(items),
            Expression::Block(items) => self.fmt_block_braced(items),
            Expression::Program(items) => self.fmt_program(items),

            Expression::If(branches) => self.fmt_if(branches),
            Expression::Branch(cond, body) => {
                if let Some(c) = cond {
                    self.push_str("if ");
                    self.fmt_output(c);
                    self.push_str(" ");
                } else {
                    self.push_str("else ");
                }
                self.fmt_block_or_inline(body);
            }

            Expression::Return(e) => {
                self.push_str("return");
                if !is_bare_return(e.1.as_ref()) {
                    self.push_str(" ");
                    self.fmt_output(e);
                }
            }
            Expression::Raise(inner) => {
                self.push_str("raise ");
                self.fmt_output(inner);
            }
            Expression::Panic(inner) => {
                self.push_str("panic ");
                self.fmt_output(inner);
            }
            Expression::Yield(inner) => {
                self.push_str("yield ");
                self.fmt_output(inner);
            }
            Expression::YieldFrom(inner) => {
                self.push_str("yield from ");
                self.fmt_output(inner);
            }
            Expression::Resume(target, arg) => {
                self.push_str("resume ");
                self.fmt_output(target);
                if let Some(a) = arg {
                    self.push_str(" with ");
                    self.fmt_output(a);
                }
            }

            Expression::Negate(n) => {
                self.push_str("-");
                self.fmt_output(n);
            }
            Expression::Positive(n) => {
                self.push_str("+");
                self.fmt_output(n);
            }
            Expression::Not(n) => {
                self.push_str("~");
                self.fmt_output(n);
            }
            Expression::LogicalNot(n) => {
                self.push_str("!");
                self.fmt_output(n);
            }
            Expression::Try(inner) => {
                self.fmt_output(inner);
                self.push_str("?");
            }
            Expression::Readonly(inner) => {
                self.push_str("readonly ");
                self.fmt_output(inner);
            }
            Expression::TypeOf(inner) => {
                self.push_str("typeof ");
                self.fmt_output(inner);
            }

            Expression::And(lhs, rhs) => self.fmt_logic_chain(expr, lhs, rhs, "&&"),
            Expression::Or(lhs, rhs) => self.fmt_logic_chain(expr, lhs, rhs, "||"),
            Expression::Coalesce(lhs, rhs) => self.fmt_logic_chain(expr, lhs, rhs, "??"),

            Expression::Add(lhs, rhs)
            | Expression::Sub(lhs, rhs)
            | Expression::Mul(lhs, rhs)
            | Expression::Div(lhs, rhs)
            | Expression::Mod(lhs, rhs)
            | Expression::Pow(lhs, rhs)
            | Expression::Shl(lhs, rhs)
            | Expression::Shr(lhs, rhs)
            | Expression::Xor(lhs, rhs)
            | Expression::BitAnd(lhs, rhs)
            | Expression::BitOr(lhs, rhs)
            | Expression::Eq(lhs, rhs)
            | Expression::Neq(lhs, rhs)
            | Expression::Le(lhs, rhs)
            | Expression::Gt(lhs, rhs)
            | Expression::Leq(lhs, rhs)
            | Expression::Geq(lhs, rhs) => {
                self.fmt_output(lhs);
                self.push_str(" ");
                self.push_str(binary_op(expr));
                self.push_str(" ");
                self.fmt_output(rhs);
            }
            Expression::Cast(expr, ty) => {
                self.fmt_output(expr);
                self.push_str(" as ");
                self.fmt_output(ty);
            }
            Expression::CompoundAssign(lhs, op, rhs) => {
                self.fmt_output(lhs);
                self.push_str(" ");
                self.push_str(compound_op(*op));
                self.push_str(" ");
                self.fmt_output(rhs);
            }
            Expression::Adjust { op, prefix, target } => {
                let sym = match op {
                    AdjustOp::Inc => "++",
                    AdjustOp::Dec => "--",
                };
                if *prefix {
                    self.push_str(sym);
                    self.fmt_output(target);
                } else {
                    self.fmt_output(target);
                    self.push_str(sym);
                }
            }
            Expression::Range {
                start,
                end,
                inclusive,
            } => {
                self.fmt_output(start);
                if *inclusive {
                    self.push_str("..=");
                } else {
                    self.push_str("..");
                }
                self.fmt_output(end);
            }
            Expression::Assignment(lhs, rhs) => {
                self.fmt_output(lhs);
                self.push_str(" = ");
                self.fmt_output(rhs);
            }

            Expression::List(items) | Expression::Array(items) => {
                self.fmt_delimited_outputs("[", "]", items, false);
            }
            Expression::Tuple(items) => {
                self.fmt_delimited_outputs("(", ")", items, items.len() == 1);
            }
            Expression::Dict(items) => self.fmt_record_list(items),
            Expression::Index(target, index) => {
                self.fmt_output(target);
                self.push_str("[");
                if let Some(idx) = index {
                    self.fmt_output(idx);
                }
                self.push_str("]");
            }
            Expression::Access(_, _) | Expression::OptionalAccess(_, _) | Expression::Call { .. } => {
                if let Some(parts) = collect_member_chain(expr) {
                    self.fmt_member_chain(&parts);
                } else {
                    self.fmt_member_or_call_atom(expr);
                }
            }
            Expression::QualifiedAccess { owner, member } => {
                self.push_str(owner);
                self.push_str("::");
                self.push_str(member);
            }
            Expression::Member(inner) => self.fmt_output(inner),

            Expression::NamedArg(name, value) => {
                self.push_str(name);
                self.push_str(": ");
                self.fmt_output(value);
            }
            Expression::Spread(inner) => {
                self.push_str("...");
                self.fmt_output(inner);
            }
            Expression::Argument {
                docs,
                ty,
                name,
                is_rest,
            } => {
                self.fmt_docs(docs);
                if *is_rest {
                    match ty {
                        None => {
                            self.push_str("... ");
                            self.push_str(name);
                        }
                        Some(t) => {
                            self.fmt_output(t);
                            self.push_str("... ");
                            self.push_str(name);
                        }
                    }
                } else {
                    self.fmt_output(ty.as_ref().expect("fixed param"));
                    self.push_str(" ");
                    self.push_str(name);
                }
            }

            Expression::Instantiate(class, args) => {
                self.push_str("new ");
                self.fmt_output(class);
                self.fmt_delimited_outputs("(", ")", args.as_deref().unwrap_or(&[]), false);
            }

            Expression::Dload(path) => {
                self.push_str("dload(");
                self.fmt_output(path);
                self.push_str(")");
            }
            Expression::Done(handle) => {
                self.push_str("done(");
                self.fmt_output(handle);
                self.push_str(")");
            }
            Expression::Declare(args) | Expression::Invoke(args) => {
                let kw = if matches!(expr, Expression::Declare(_)) {
                    "declare"
                } else {
                    "invoke"
                };
                self.push_str(kw);
                self.fmt_delimited_outputs("(", ")", args, false);
            }

            Expression::Use { path, name, alias } => {
                self.fmt_use(path, name, alias.as_ref());
            }

            Expression::Variable(name, ty) => {
                self.push_str("let ");
                self.push_str(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_output(t);
                }
            }
            Expression::Constant(name, ty) => {
                self.push_str("const ");
                self.fmt_output(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_output(t);
                }
            }
            Expression::LetDestructure { pattern, rhs } => {
                self.push_str("let ");
                self.fmt_let_pattern(pattern);
                self.push_str(" = ");
                self.fmt_output(rhs);
            }
            Expression::StaticDecl {
                is_const,
                name,
                ty,
                init,
            } => {
                if *is_const {
                    self.push_str("static const");
                } else {
                    self.push_str("static let");
                }
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_output(t);
                }
                self.push_str(" ");
                self.push_str(name);
                self.push_str(" = ");
                self.fmt_output(init);
                self.push_str(";");
            }

            Expression::Defer { captures, body } => {
                self.push_str("defer");
                if !captures.is_empty() {
                    self.push_str(" use (");
                    for (i, c) in captures.iter().enumerate() {
                        if i > 0 {
                            self.push_str(", ");
                        }
                        self.push_str(c);
                    }
                    self.push_str(")");
                }
                self.push_str(" ");
                self.fmt_block_or_inline(body);
            }

            Expression::Function { .. } => self.fmt_function_expr(expr, true),

            Expression::Loop {
                identifier,
                iterable,
                body,
            } => {
                if let Some(ident) = identifier {
                    self.push_str("for ");
                    self.fmt_output(ident);
                    self.push_str(" in ");
                    self.fmt_output(iterable);
                    self.push_str(" ");
                    self.fmt_block_or_inline(body);
                } else {
                    self.push_str("while ");
                    self.fmt_output(iterable);
                    self.push_str(" ");
                    self.fmt_block_or_inline(body);
                }
            }

            Expression::Match { scrutinee, arms } => {
                self.push_str("match ");
                self.fmt_output(scrutinee);
                self.push_str(" {");
                self.newline();
                self.with_indent(|f| {
                    for (i, arm) in arms.iter().enumerate() {
                        f.write_indent();
                        f.fmt_pattern(&arm.pattern);
                        f.push_str(" => ");
                        f.fmt_match_arm_body(&arm.body);
                        if i + 1 < arms.len() {
                            f.push_str(",");
                        }
                        f.newline();
                    }
                });
                self.push_str("}");
            }

            Expression::Construct {
                enum_name,
                variant_name,
                fields,
            } => {
                self.push_str(enum_name);
                self.push_str("::");
                self.push_str(variant_name);
                self.fmt_construct_payload(fields);
            }

            Expression::Lambda {
                args,
                captures,
                body,
            } => {
                self.push_str("fn ");
                self.fmt_paren_arg_list(args);
                if !captures.is_empty() {
                    let caps: Vec<String> = captures.iter().map(|c| (*c).to_string()).collect();
                    self.push_str(" use ");
                    self.fmt_delimited_strings("(", ")", &caps, false);
                }
                match body.1.as_ref() {
                    Expression::Block(_) => {
                        self.push_str(" ");
                        self.fmt_block_or_inline(body);
                    }
                    _ => {
                        self.push_str(" => ");
                        self.fmt_output(body);
                    }
                }
            }

            Expression::TypeAlias {
                docs,
                name,
                type_params,
                ty,
            } => {
                self.fmt_docs(docs);
                self.push_str("type ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.push_str(" = ");
                self.fmt_output(ty);
                self.push_str(";");
            }
            Expression::TypeApp { name, args } => {
                self.push_str(name);
                self.fmt_delimited_outputs("<", ">", args, false);
            }
            Expression::TypeProjection { owner, name, args } => {
                self.push_str(owner);
                self.push_str("::");
                self.push_str(name);
                if !args.is_empty() {
                    self.fmt_delimited_outputs("<", ">", args, false);
                }
            }
            Expression::TypeFun(arg, ret) => {
                self.fmt_output(arg);
                self.push_str(" -> ");
                self.fmt_output(ret);
            }
            Expression::TypeFnSig { params, ret } => {
                self.push_str("fn");
                self.fmt_paren_arg_list(params);
                self.push_str(" -> ");
                self.fmt_output(ret);
            }
            Expression::Forall { params, ty } => {
                self.push_str("forall ");
                self.fmt_type_params_list(params);
                self.push_str(". ");
                self.fmt_output(ty);
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
                self.fmt_docs(docs);
                self.push_str("attr ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.fmt_paren_arg_list(args);
                if let Some(ret) = returns {
                    self.push_str(" -> ");
                    self.fmt_output(ret);
                }
                self.fmt_where(where_constraints);
                self.push_str(" ");
                self.fmt_block_or_inline(body);
            }

            Expression::TestCase { name, body } => {
                self.push_str("test(");
                self.fmt_output(name);
                self.push_str(") ");
                self.fmt_block_or_inline(body);
            }

            Expression::EnumDecl {
                docs,
                attrs,
                name,
                type_params,
                variants,
            } => {
                self.fmt_docs(docs);
                self.fmt_attrs(attrs);
                self.push_str("enum ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.fmt_comma_body(variants);
            }
            Expression::EnumVariant { docs, name, payload } => {
                self.fmt_docs(docs);
                self.push_str(name);
                self.fmt_enum_variant_payload(payload);
            }

            Expression::Class {
                docs,
                attrs,
                name,
                type_params,
                fields,
            } => {
                self.fmt_docs(docs);
                self.fmt_attrs(attrs);
                self.push_str("class ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.fmt_comma_body(fields);
            }
            Expression::Field {
                docs,
                visibility,
                modifier,
                name,
                ty,
                init,
            } => {
                self.fmt_docs(docs);
                self.fmt_visibility(*visibility);
                self.fmt_field_modifier(*modifier);
                self.fmt_output(name);
                self.push_str(": ");
                self.fmt_output(ty);
                if let Some(i) = init {
                    self.push_str(" = ");
                    self.fmt_output(i);
                }
            }
            Expression::Method(visibility, func) => {
                if let Expression::Function { docs, .. } = func.1.as_ref() {
                    self.fmt_docs(docs);
                }
                self.fmt_visibility(*visibility);
                self.fmt_function(func, false);
            }
            Expression::Implementation {
                what,
                owner,
                type_params,
                methods,
            } => {
                self.push_str("impl ");
                if !what.is_empty() {
                    self.push_str(what);
                    self.push_str(" for ");
                }
                self.push_str(owner);
                self.fmt_type_params(type_params);
                self.fmt_braced_items(methods);
            }
            Expression::TypeClass {
                docs,
                name,
                type_params,
                methods,
            } => {
                self.fmt_docs(docs);
                self.push_str("trait ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.fmt_braced_items(methods);
            }
            Expression::TypeClassImpl {
                class,
                args,
                methods,
            } => {
                self.push_str("impl ");
                self.push_str(class);
                if let Some((for_ty, rest)) = args.split_first() {
                    if !rest.is_empty() {
                        self.fmt_delimited_outputs("<", ">", rest, false);
                    }
                    self.push_str(" for ");
                    self.fmt_output(for_ty);
                }
                self.fmt_braced_items(methods);
            }
            Expression::AssocTypeDecl { name, type_params } => {
                self.push_str("type ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.push_str(";");
            }
            Expression::AssocTypeDef {
                name,
                type_params,
                ty,
            } => {
                self.push_str("type ");
                self.push_str(name);
                self.fmt_type_params(type_params);
                self.push_str(" = ");
                self.fmt_output(ty);
                self.push_str(";");
            }

            Expression::ExternBlock {
                library,
                declarations,
            } => {
                self.push_str("extern \"");
                self.push_str(library);
                self.push_str("\" {");
                self.newline();
                self.with_indent(|f| {
                    for decl in declarations {
                        f.write_indent();
                        f.fmt_extern_function(decl);
                        f.newline();
                    }
                });
                self.push_str("}");
            }
            Expression::ExternStruct(decl) => self.fmt_extern_struct(decl),
        }
    }

    fn fmt_statement_line(&mut self, s: &Output<'_>) {
        match s.1.as_ref() {
            Expression::Comment(text) => self.fmt_comment_line(text),
            Expression::ExprStatement(_) => self.fmt_expression(s.1.as_ref()),
            other => {
                self.fmt_expression(other);
                if stmt_needs_semicolon(other) {
                    self.push_str(";");
                }
            }
        }
    }

    fn fmt_block_stmt(&mut self, item: &Output<'_>) {
        match item.1.as_ref() {
            Expression::Comment(text) => {
                self.write_indent();
                self.fmt_comment_line(text);
                self.newline();
            }
            Expression::Statement(s) => {
                self.write_indent();
                self.fmt_statement_line(s);
                self.newline();
            }
            other => {
                self.write_indent();
                self.fmt_expression(other);
                if stmt_needs_semicolon(other) {
                    self.push_str(";");
                }
                self.newline();
            }
        }
    }

    fn fmt_block_braced(&mut self, items: &[Output<'_>]) {
        self.push_str("{");
        self.newline();
        self.with_indent(|f| {
            for item in items {
                f.fmt_block_stmt(item);
            }
        });
        self.write_indent();
        self.push_str("}");
    }

    fn fmt_block_or_inline(&mut self, body: &Output<'_>) {
        match body.1.as_ref() {
            Expression::Block(items) => self.fmt_block_braced(items),
            other => self.fmt_expression(other),
        }
    }

    fn fmt_program(&mut self, items: &[Output<'_>]) {
        let mut prev_comment = false;
        let mut i = 0;
        while i < items.len() {
            let item = &items[i];
            let is_comment = matches!(item.1.as_ref(), Expression::Comment(_));
            if i > 0 {
                self.newline();
                if !(prev_comment && is_comment) {
                    self.newline();
                }
            }
            if matches!(item.1.as_ref(), Expression::Use { .. }) {
                let start = i;
                while i < items.len() && matches!(items[i].1.as_ref(), Expression::Use { .. }) {
                    i += 1;
                }
                self.fmt_use_group(&items[start..i]);
            } else {
                self.fmt_expression(item.1.as_ref());
                i += 1;
            }
            prev_comment = is_comment;
        }
    }

    fn fmt_use(&mut self, path: &[String], name: &str, alias: Option<&String>) {
        self.push_str("use ");
        for (i, segment) in path.iter().enumerate() {
            if i > 0 {
                self.push_str("::");
            }
            self.push_str(segment);
        }
        if !path.is_empty() {
            self.push_str("::");
        }
        self.push_str(name);
        if let Some(alias) = alias {
            self.push_str(" as ");
            self.push_str(alias);
        }
        self.push_str(";");
    }

    fn fmt_use_group(&mut self, items: &[Output<'_>]) {
        let mut start = 0;
        while start < items.len() {
            let Some((root, _, _)) = use_parts(items[start].1.as_ref()) else {
                start += 1;
                continue;
            };
            let mut end = start + 1;
            while end < items.len()
                && use_parts(items[end].1.as_ref())
                    .is_some_and(|(candidate, _, _)| candidate.first() == root.first())
            {
                end += 1;
            }
            let group = &items[start..end];
            if group.len() < 2 {
                if let Some((path, name, alias)) = use_parts(group[0].1.as_ref()) {
                    self.fmt_use(path, name, alias);
                }
            } else if can_group_uses(group) {
                self.fmt_grouped_use(group);
            } else {
                for (index, item) in group.iter().enumerate() {
                    if index > 0 {
                        self.newline();
                    }
                    if let Some((path, name, alias)) = use_parts(item.1.as_ref()) {
                        self.fmt_use(path, name, alias);
                    }
                }
            }
            start = end;
            if start < items.len() {
                self.newline();
            }
        }
    }

    fn fmt_grouped_use(&mut self, items: &[Output<'_>]) {
        let Some((first_path, _, _)) = use_parts(items[0].1.as_ref()) else {
            return;
        };
        let same_namespace = items.iter().all(|item| {
            use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path == first_path)
        });
        let root_len = if same_namespace { first_path.len() } else { 1 };
        let root = &first_path[..root_len];

        self.push_str("use ");
        self.push_str(&root.join("::"));
        self.push_str("::{");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.push_str(", ");
            }
            let Some((path, name, alias)) = use_parts(item.1.as_ref()) else {
                continue;
            };
            if !same_namespace {
                let suffix = &path[root_len..];
                if !suffix.is_empty() {
                    self.push_str(&suffix.join("::"));
                    self.push_str("::");
                }
            }
            self.push_str(name);
            if let Some(alias) = alias {
                self.push_str(" as ");
                self.push_str(alias);
            }
        }
        self.push_str("};");
    }

    fn fmt_if(&mut self, branches: &[Output<'_>]) {
        for (i, branch) in branches.iter().enumerate() {
            let Expression::Branch(cond, body) = branch.1.as_ref() else {
                self.fmt_output(branch);
                continue;
            };
            if i > 0 {
                self.push_str(" ");
            }
            if i == 0 {
                self.push_str("if ");
                if let Some(c) = cond {
                    self.fmt_output(c);
                    self.push_str(" ");
                }
            } else if cond.is_some() {
                self.push_str("else if ");
                self.fmt_output(cond.as_ref().unwrap());
                self.push_str(" ");
            } else {
                self.push_str("else ");
            }
            self.fmt_block_or_inline(body);
        }
    }

    fn fmt_fragment(&mut self, items: &[Output<'_>]) {
        if items.is_empty() {
            return;
        }
        match items[0].1.as_ref() {
            Expression::Variable(name, ty) => {
                self.push_str("let ");
                self.push_str(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_output(t);
                }
                if let Some(val) = items.get(1) {
                    self.push_str(" = ");
                    self.fmt_output(val);
                }
            }
            Expression::Constant(name, ty) => {
                self.push_str("const ");
                self.fmt_output(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_output(t);
                }
                if let Some(val) = items.get(1) {
                    self.push_str(" = ");
                    self.fmt_output(val);
                }
            }
            // Brace-group `use path::{a, b}` parses as Fragment([Use, Use, …]).
            Expression::Use { .. }
                if items
                    .iter()
                    .all(|item| matches!(item.1.as_ref(), Expression::Use { .. })) =>
            {
                self.fmt_use_group(items);
            }
            _ => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.push_str(", ");
                    }
                    self.fmt_output(item);
                }
            }
        }
    }

    fn fmt_braced_items(&mut self, items: &[Output<'_>]) {
        self.push_str(" {");
        self.newline();
        self.with_indent(|f| {
            for item in items {
                f.write_indent();
                f.fmt_expression(item.1.as_ref());
                f.newline();
            }
        });
        self.push_str("}");
    }

    fn fmt_match_arm_body(&mut self, body: &Output<'_>) {
        match body.1.as_ref() {
            Expression::Block(items) => self.fmt_block_braced(items),
            other => self.fmt_expression(other),
        }
    }

    fn fmt_construct_payload(&mut self, fields: &EnumConstructPayload<'_>) {
        match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                self.fmt_delimited_outputs("(", ")", args, args.len() == 1);
            }
            EnumConstructPayload::Record(parts) => self.fmt_record_list(parts),
        }
    }

    fn fmt_enum_variant_payload(&mut self, payload: &EnumVariantPayload<'_>) {
        match payload {
            EnumVariantPayload::Unit => {}
            EnumVariantPayload::Tuple(parts) => {
                if parts.is_empty() {
                    return;
                }
                self.fmt_delimited_outputs("(", ")", parts, parts.len() == 1);
            }
            EnumVariantPayload::Record(fields) => {
                self.push_str(" ");
                self.fmt_record_decls(fields);
            }
        }
    }

    fn fmt_record_decls(&mut self, fields: &[RecordFieldDecl<'_>]) {
        if fields.is_empty() {
            self.push_str("{}");
            return;
        }
        let flat = {
            let mut f = Formatter {
                indent: self.indent,
                out: String::new(),
                flat: true,
            };
            f.push_str("{ ");
            for (i, rf) in fields.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                f.push_str(rf.name);
                f.push_str(": ");
                f.fmt_output(&rf.value);
            }
            f.push_str(" }");
            f.out
        };
        if self.fits_flat(&flat) {
            self.push_str("{ ");
            let was = self.flat;
            self.flat = true;
            for (i, rf) in fields.iter().enumerate() {
                if i > 0 {
                    self.push_str(", ");
                }
                self.push_str(rf.name);
                self.push_str(": ");
                self.fmt_output(&rf.value);
            }
            self.flat = was;
            self.push_str(" }");
            return;
        }
        self.push_str("{");
        self.newline();
        self.with_indent(|f| {
            for rf in fields {
                f.write_indent();
                f.push_str(rf.name);
                f.push_str(": ");
                f.fmt_output(&rf.value);
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
        self.push_str("}");
    }

    fn fmt_extern_function(&mut self, decl: &ExternFunction<'_>) {
        self.push_str("fn ");
        self.push_str(decl.name);
        match decl.args.1.as_ref() {
            Expression::Fragment(items) => {
                // Variadic FFI uses a synthetic trailing `...` after args.
                if decl.variadic {
                    self.push_str("(");
                    if items.is_empty() {
                        self.push_str("...");
                    } else {
                        let flat = {
                            let mut f = Formatter {
                                indent: self.indent,
                                out: String::new(),
                                flat: true,
                            };
                            for (i, item) in items.iter().enumerate() {
                                if i > 0 {
                                    f.push_str(", ");
                                }
                                f.fmt_output(item);
                            }
                            f.push_str(", ...");
                            f.out
                        };
                        if self.fits_flat(&format!("({flat})")) {
                            let was = self.flat;
                            self.flat = true;
                            for (i, item) in items.iter().enumerate() {
                                if i > 0 {
                                    self.push_str(", ");
                                }
                                self.fmt_output(item);
                            }
                            self.push_str(", ...");
                            self.flat = was;
                        } else {
                            self.newline();
                            self.with_indent(|f| {
                                for item in items {
                                    f.write_indent();
                                    f.fmt_output(item);
                                    f.push_str(",");
                                    f.newline();
                                }
                                f.write_indent();
                                f.push_str("...,");
                                f.newline();
                            });
                            self.write_indent();
                        }
                    }
                    self.push_str(")");
                } else {
                    self.fmt_delimited_outputs("(", ")", items, false);
                }
            }
            other => {
                self.push_str("(");
                self.fmt_expression(other);
                if decl.variadic {
                    self.push_str(", ...");
                }
                self.push_str(")");
            }
        }
        if let Some(ret) = &decl.returns {
            self.push_str(" -> ");
            self.fmt_output(ret);
        }
        self.push_str(";");
    }

    fn fmt_extern_struct(&mut self, decl: &ExternStructDecl<'_>) {
        self.push_str("extern struct ");
        self.push_str(decl.name);
        if decl.fields.is_empty() {
            self.push_str(" {};");
            return;
        }
        self.push_str(" {");
        self.newline();
        self.with_indent(|f| {
            for (name, ty) in &decl.fields {
                f.write_indent();
                f.push_str(name);
                f.push_str(": ");
                f.fmt_output(ty);
                f.push_str(",");
                f.newline();
            }
        });
        self.push_str("};");
    }

    fn fmt_visibility(&mut self, visibility: Visibility) {
        if visibility == Visibility::Public {
            self.push_str("pub ");
        }
    }

    fn fmt_field_modifier(&mut self, modifier: FieldModifier) {
        match modifier {
            FieldModifier::Const => self.push_str("const "),
            FieldModifier::Static => self.push_str("static "),
            FieldModifier::Instance => {}
        }
    }

    fn fmt_logic_chain(
        &mut self,
        root: &Expression<'_>,
        _lhs: &Output<'_>,
        _rhs: &Output<'_>,
        op: &str,
    ) {
        let operands = flatten_logic(root, op);
        if operands.len() <= 1 {
            if let Some(e) = operands.first() {
                self.fmt_expression(e);
            }
            return;
        }

        let mut flat = String::new();
        for (i, operand) in operands.iter().enumerate() {
            if i > 0 {
                flat.push(' ');
                flat.push_str(op);
                flat.push(' ');
            }
            flat.push_str(&self.render_flat(operand));
        }

        if self.fits_flat(&flat) {
            for (i, operand) in operands.iter().enumerate() {
                if i > 0 {
                    self.push_str(" ");
                    self.push_str(op);
                    self.push_str(" ");
                }
                let was_flat = self.flat;
                self.flat = true;
                self.fmt_expression(operand);
                self.flat = was_flat;
            }
            return;
        }

        let hang = self.current_col();
        for (i, operand) in operands.iter().enumerate() {
            if i > 0 {
                self.push_str(" ");
                self.push_str(op);
                self.newline();
                self.pad_to_col(hang);
            }
            self.fmt_expression(operand);
        }
    }

    fn fmt_member_or_call_atom(&mut self, expr: &Expression<'_>) {
        match expr {
            Expression::Access(receiver, field) => {
                self.fmt_output(receiver);
                self.push_str(".");
                self.push_str(field);
            }
            Expression::OptionalAccess(receiver, field) => {
                self.fmt_output(receiver);
                self.push_str("?.");
                self.push_str(field);
            }
            Expression::Call { name, args } => {
                self.fmt_output(name);
                self.fmt_delimited_outputs("(", ")", args.as_deref().unwrap_or(&[]), false);
            }
            other => self.fmt_expression(other),
        }
    }

    fn fmt_member_chain(&mut self, parts: &[ChainPart<'_>]) {
        let flat = {
            let mut f = Formatter {
                indent: self.indent,
                out: String::new(),
                flat: true,
            };
            f.emit_member_chain_flat(parts);
            f.out
        };
        if self.fits_flat(&flat) {
            self.emit_member_chain_flat(parts);
            return;
        }

        match &parts[0] {
            ChainPart::Root(expr) => self.fmt_expression(expr),
            ChainPart::Field { .. } => unreachable!("member chain must start with root"),
        }
        self.with_indent(|f| {
            for part in &parts[1..] {
                f.newline();
                f.write_indent();
                f.emit_chain_field(part);
            }
        });
    }

    fn emit_chain_field(&mut self, part: &ChainPart<'_>) {
        match part {
            ChainPart::Root(_) => unreachable!(),
            ChainPart::Field {
                optional,
                name,
                call_args,
            } => {
                if *optional {
                    self.push_str("?.");
                } else {
                    self.push_str(".");
                }
                self.push_str(name);
                if let Some(args) = call_args {
                    self.fmt_delimited_outputs("(", ")", args, false);
                }
            }
        }
    }

    fn emit_member_chain_flat(&mut self, parts: &[ChainPart<'_>]) {
        match &parts[0] {
            ChainPart::Root(expr) => {
                let was = self.flat;
                self.flat = true;
                self.fmt_expression(expr);
                self.flat = was;
            }
            ChainPart::Field { .. } => unreachable!("member chain must start with root"),
        }
        for part in &parts[1..] {
            let was = self.flat;
            self.flat = true;
            self.emit_chain_field(part);
            self.flat = was;
        }
    }

    fn fmt_docs(&mut self, docs: &[&str]) {
        if docs.is_empty() {
            return;
        }
        for (i, line) in docs.iter().enumerate() {
            if i > 0 {
                self.write_indent();
            }
            self.push_str("///");
            if !line.is_empty() {
                self.push_str(" ");
                self.push_str(line);
            }
            self.newline();
            self.write_indent();
        }
    }

    fn fmt_comment_line(&mut self, text: &str) {
        self.push_str("//");
        if !text.is_empty() {
            self.push_str(" ");
            self.push_str(text);
        }
    }

    /// Pretty-print a [`Expression::Function`], optionally emitting attached docs.
    fn fmt_function(&mut self, func: &Output<'_>, emit_docs: bool) {
        self.fmt_function_expr(func.1.as_ref(), emit_docs);
    }

    fn fmt_function_expr(&mut self, expr: &Expression<'_>, emit_docs: bool) {
        let Expression::Function {
            docs,
            attrs,
            name,
            is_coro,
            is_static,
            type_params,
            args,
            returns,
            where_constraints,
            body,
        } = expr
        else {
            self.fmt_expression(expr);
            return;
        };
        if emit_docs {
            self.fmt_docs(docs);
        }
        self.fmt_attrs(attrs);
        if *is_coro {
            self.push_str("async ");
        }
        if *is_static {
            self.push_str("static ");
        }
        self.push_str("fn ");
        self.push_str(name);
        self.fmt_type_params(type_params);
        self.fmt_paren_arg_list(args);
        if let Some(ret) = returns {
            self.push_str(" -> ");
            self.fmt_output(ret);
        }
        self.fmt_where(where_constraints);
        match body {
            Some(b) => {
                self.push_str(" ");
                self.fmt_block_or_inline(b);
            }
            None => self.push_str(";"),
        }
    }

    fn fmt_attrs(&mut self, attrs: &[Attribute<'_>]) {
        for attr in attrs {
            self.push_str(&attr.to_string());
            self.newline();
        }
    }

    fn fmt_type_params(&mut self, params: &[TypeParam<'_>]) {
        if params.is_empty() {
            return;
        }
        let items: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        self.fmt_delimited_strings("<", ">", &items, false);
    }

    fn fmt_type_params_list(&mut self, params: &[TypeParam<'_>]) {
        let items: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        // Bare list without brackets (forall …).
        if items.is_empty() {
            return;
        }
        let flat = items.join(", ");
        if self.fits_flat(&flat) {
            self.push_str(&flat);
            return;
        }
        self.newline();
        self.with_indent(|f| {
            for item in &items {
                f.write_indent();
                f.push_str(item);
                f.push_str(",");
                f.newline();
            }
        });
        self.write_indent();
    }

    fn fmt_where(&mut self, constraints: &[WhereConstraint<'_>]) {
        if constraints.is_empty() {
            return;
        }
        self.push_str(" where ");
        for (i, c) in constraints.iter().enumerate() {
            if i > 0 {
                self.push_str(", ");
            }
            self.push_str(&c.to_string());
        }
    }

    fn fmt_pattern(&mut self, pattern: &(SimpleSpan, Pattern<'_>)) {
        self.push_str(&pattern.1.to_string());
    }

    fn fmt_let_pattern(&mut self, pattern: &LetPattern<'_>) {
        self.push_str(&pattern.to_string());
    }
}

enum ChainPart<'a> {
    Root(&'a Expression<'a>),
    Field {
        optional: bool,
        name: &'a str,
        call_args: Option<&'a [Output<'a>]>,
    },
}

/// Flatten a left-associative `&&` / `||` / `??` tree into operand expressions.
fn flatten_logic<'a>(expr: &'a Expression<'a>, op: &str) -> Vec<&'a Expression<'a>> {
    match (expr, op) {
        (Expression::And(lhs, rhs), "&&") => {
            let mut out = flatten_logic(lhs.1.as_ref(), op);
            out.extend(flatten_logic(rhs.1.as_ref(), op));
            out
        }
        (Expression::Or(lhs, rhs), "||") => {
            let mut out = flatten_logic(lhs.1.as_ref(), op);
            out.extend(flatten_logic(rhs.1.as_ref(), op));
            out
        }
        (Expression::Coalesce(lhs, rhs), "??") => {
            let mut out = flatten_logic(lhs.1.as_ref(), op);
            out.extend(flatten_logic(rhs.1.as_ref(), op));
            out
        }
        (other, _) => vec![other],
    }
}

/// Collect `recv.field`, `recv?.field`, and `recv.method(args)` into a chain.
///
/// Returns `None` when `expr` is not a multi-part member/call chain.
fn collect_member_chain<'a>(expr: &'a Expression<'a>) -> Option<Vec<ChainPart<'a>>> {
    let mut rev: Vec<ChainPart<'a>> = Vec::new();
    let mut cur = expr;
    loop {
        match cur {
            Expression::Call { name, args } => match name.1.as_ref() {
                Expression::Access(recv, field) => {
                    rev.push(ChainPart::Field {
                        optional: false,
                        name: field,
                        call_args: args.as_deref(),
                    });
                    cur = recv.1.as_ref();
                }
                Expression::OptionalAccess(recv, field) => {
                    rev.push(ChainPart::Field {
                        optional: true,
                        name: field,
                        call_args: args.as_deref(),
                    });
                    cur = recv.1.as_ref();
                }
                _ => {
                    if rev.is_empty() {
                        return None;
                    }
                    rev.push(ChainPart::Root(cur));
                    break;
                }
            },
            Expression::Access(recv, field) => {
                rev.push(ChainPart::Field {
                    optional: false,
                    name: field,
                    call_args: None,
                });
                cur = recv.1.as_ref();
            }
            Expression::OptionalAccess(recv, field) => {
                rev.push(ChainPart::Field {
                    optional: true,
                    name: field,
                    call_args: None,
                });
                cur = recv.1.as_ref();
            }
            other => {
                if rev.is_empty() {
                    return None;
                }
                rev.push(ChainPart::Root(other));
                break;
            }
        }
    }
    rev.reverse();
    if rev.len() < 2 {
        return None;
    }
    Some(rev)
}

fn binary_op(expr: &Expression<'_>) -> &'static str {
    match expr {
        Expression::Add(_, _) => "+",
        Expression::Sub(_, _) => "-",
        Expression::Mul(_, _) => "*",
        Expression::Div(_, _) => "/",
        Expression::Mod(_, _) => "%",
        Expression::Pow(_, _) => "**",
        Expression::Shl(_, _) => "<<",
        Expression::Shr(_, _) => ">>",
        Expression::Xor(_, _) => "^",
        Expression::And(_, _) => "&&",
        Expression::BitAnd(_, _) => "&",
        Expression::Or(_, _) => "||",
        Expression::BitOr(_, _) => "|",
        Expression::Eq(_, _) => "==",
        Expression::Neq(_, _) => "!=",
        Expression::Le(_, _) => "<",
        Expression::Gt(_, _) => ">",
        Expression::Leq(_, _) => "<=",
        Expression::Geq(_, _) => ">=",
        _ => "?",
    }
}

fn use_parts<'a, 'expr>(
    expr: &'a Expression<'expr>,
) -> Option<(&'a [String], &'a str, Option<&'a String>)> {
    match expr {
        Expression::Use { path, name, alias } => Some((path, name, alias.as_ref())),
        _ => None,
    }
}

fn can_group_uses(items: &[Output<'_>]) -> bool {
    let Some((first_path, _, _)) = use_parts(items[0].1.as_ref()) else {
        return false;
    };
    if first_path.is_empty() {
        return false;
    }
    let same_namespace = items.iter().all(|item| {
        use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path == first_path)
    });
    same_namespace
        || items.iter().all(|item| {
            use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path.len() + 1 > 3)
        })
}

fn is_bare_return(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Noop(_) => true,
        Expression::Tuple(items) => items.is_empty(),
        Expression::Expr(inner) => is_bare_return(inner.1.as_ref()),
        _ => false,
    }
}

fn stmt_needs_semicolon(expr: &Expression<'_>) -> bool {
    !matches!(
        expr,
        Expression::ExprStatement(_)
            | Expression::If(_)
            | Expression::Block(_)
            |             Expression::Loop { .. }
            | Expression::Defer { .. }
    )
}

fn compound_op(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Mod => "%=",
        AssignOp::Pow => "**=",
        AssignOp::Shl => "<<=",
        AssignOp::Shr => ">>=",
        AssignOp::BitAnd => "&=",
        AssignOp::BitOr => "|=",
        AssignOp::BitXor => "^=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expression;
    use crate::Pratt;

    fn parse_program(src: &str) -> Expression<'_> {
        Pratt::default()
            .parse(src)
            .expect("parse failed")
            .1
            .as_ref()
            .clone()
    }

    fn parse_exprs(src: &str) -> Vec<Expression<'_>> {
        match parse_program(src) {
            Expression::Program(items) => items.iter().map(|(_, e)| e.as_ref().clone()).collect(),
            other => vec![other],
        }
    }

    fn round_trip(src: &str) {
        let ast1 = parse_exprs(src);
        let formatted = format_source(src).expect("format failed");
        let ast2 = parse_exprs(&formatted);
        assert_eq!(ast1, ast2, "formatted:\n{formatted}");
    }

    #[test]
    fn format_fib_like_function() {
        let src = r#"fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}"#;
        round_trip(src);
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("if n <= 2"));
        assert!(formatted.contains("return fib(n - 1) + fib(n - 2);"));
    }

    #[test]
    fn format_simple_main_with_calls() {
        let src = r#"fn main() {
    write_all(stdout(), to_bytes(format("%i", fib(32))));
    return;
}"#;
        round_trip(src);
    }

    #[test]
    fn format_is_idempotent() {
        let src = "fn main() { return; }\n";
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn comments_are_preserved() {
        let src = "fn main() {\n    // hello\n    return;\n}\n";
        round_trip(src);
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("// hello"));
    }

    #[test]
    fn doc_comments_attach_and_round_trip() {
        let src = "/// Adds one.\n/// More detail.\nfn add(int x) -> int {\n    return x + 1;\n}\n";
        round_trip(src);
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("/// Adds one."));
        assert!(formatted.contains("/// More detail."));
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn parameter_docs_force_multiline_and_round_trip() {
        let src = "fn add(\n/// Left operand.\nint left,\n/// Right operand.\nint right,\n) -> int {\n    return left + right;\n}\n";
        let once = format_source(src).unwrap();
        assert!(once.contains("/// Left operand."));
        assert!(once.contains("/// Right operand."));
        assert!(once.contains("int left,"));
        assert_eq!(once, format_source(&once).unwrap());
    }

    #[test]
    fn groups_use_statements_by_namespace() {
        let src = "use io::stdout;\nuse io::open;\nfn main() { return; }\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("use io::{stdout, open};"));
        assert!(formatted.contains("};\n\nfn main"));
        assert!(!formatted.contains("stdout;\n\nuse"));
        Pratt::default().parse(&formatted).expect("grouped use parses");
    }

    #[test]
    fn brace_group_use_formats_without_comma_after_semicolon() {
        let src = "use io::{stdout, open};\nfn main() { return; }\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("use io::{stdout, open};"),
            "expected regrouped brace import, got:\n{formatted}"
        );
        assert!(
            !formatted.contains(";,"),
            "must not emit comma after semicolon:\n{formatted}"
        );
        Pratt::default()
            .parse(&formatted)
            .expect("brace-group format must reparse");
        assert_eq!(formatted, format_source(&formatted).unwrap());
    }

    #[test]
    fn brace_group_use_with_aliases_round_trips() {
        let src = "use io::{stdout as out, open as o};\nfn main() { return; }\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("use io::{stdout as out, open as o};"),
            "expected aliased brace import, got:\n{formatted}"
        );
        assert!(
            !formatted.contains(";,"),
            "must not emit comma after semicolon:\n{formatted}"
        );
        Pratt::default()
            .parse(&formatted)
            .expect("aliased brace-group format must reparse");
        assert_eq!(formatted, format_source(&formatted).unwrap());
    }

    #[test]
    fn groups_deep_use_statements_only_past_three_segments() {
        let deep = "use a::b::c::one;\nuse a::b::d::two;\nfn main() { return; }\n";
        let formatted = format_source(deep).unwrap();
        assert!(formatted.contains("use a::{b::c::one, b::d::two};"));
        Pratt::default().parse(&formatted).expect("deep grouped use parses");

        let shallow = "use a::b::one;\nuse a::c::two;\nfn main() { return; }\n";
        let formatted = format_source(shallow).unwrap();
        assert!(!formatted.contains("use a::{"));
        assert!(formatted.contains("use a::b::one;\nuse a::c::two;"));
        Pratt::default().parse(&formatted).expect("shallow use parses");
    }

    #[test]
    fn orphan_doc_comment_is_error() {
        let err = Pratt::default()
            .parse("/// orphan\n")
            .expect_err("should fail");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("doc comment")
                || msg.contains("Parse error")
                || msg.contains("unexpected")
                || err.code() == Some(reporting::ErrorCode::ParseError),
            "{msg}"
        );
    }

    #[test]
    fn duplicate_record_fields_are_not_formatted() {
        let err = format_source("fn main() { let x = { foo: 1, foo: 2 }; }\n")
            .expect_err("duplicate fields must not format");
        assert_eq!(err.code(), Some(reporting::ErrorCode::DuplicateField));
        assert!(
            err.message().contains("Duplicate field `foo`"),
            "got {}",
            err.message()
        );
    }

    #[test]
    fn duplicate_construct_and_enum_fields_are_not_formatted() {
        let construct = format_source(
            "enum E { Foo { x: int, y: int } }\nfn main() { E::Foo { x: 1, x: 2 }; }\n",
        )
        .expect_err("duplicate construct fields must not format");
        assert_eq!(
            construct.code(),
            Some(reporting::ErrorCode::DuplicateField)
        );
        assert!(
            construct.message().contains("Duplicate field `x`"),
            "got {}",
            construct.message()
        );

        let enum_decl = format_source("enum E { Foo { x: int, x: int } }\n")
            .expect_err("duplicate enum field decls must not format");
        assert_eq!(
            enum_decl.code(),
            Some(reporting::ErrorCode::DuplicateField)
        );
        assert!(
            enum_decl.message().contains("Duplicate field `x`"),
            "got {}",
            enum_decl.message()
        );
    }

    #[test]
    fn item_docs_reads_attached_lines() {
        use crate::ast::item_docs;
        let src = "/// Hello\n/// World\nfn f() { return; }\n";
        let ast = Pratt::default().parse(src).unwrap();
        let Expression::Program(items) = ast.1.as_ref() else {
            panic!("expected program");
        };
        let docs = item_docs(items[0].1.as_ref()).expect("docs");
        assert_eq!(docs, ["Hello", "World"]);
    }

    #[test]
    fn wraps_long_and_chain_with_hanging_indent() {
        let src = "\
fn main() {
    if (object.veryLongPropertyName == other.notSoLongName && object.shortName == other.somewhatLongerNameButStillGrowing && object.extraFlag == other.anotherFlag) {
        return;
    }
}
";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("&&\n"),
            "expected soft wrap before continuation:\n{formatted}"
        );
        assert!(
            formatted.contains("object.veryLongPropertyName == other.notSoLongName &&"),
            "&& should trail the previous line:\n{formatted}"
        );
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn wraps_long_method_chain() {
        let src = "\
fn main() {
    let x = builder.withVeryLongConfigurationOption(1).withAnotherQuiteLongOption(2).withYetAnotherOption(3).build();
    return;
}
";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("\n") && formatted.contains(".with"),
            "expected wrapped method chain:\n{formatted}"
        );
        assert!(
            formatted.lines().any(|l| l.trim_start().starts_with('.')),
            "continuation lines should start with '.':\n{formatted}"
        );
        let once = format_source(src).unwrap();
        let twice = format_source(&once).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn keeps_short_and_on_one_line() {
        let src = "fn main() {\n    if (a == 1 && b == 2) {\n        return;\n    }\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("a == 1 && b == 2"),
            "short condition should stay flat:\n{formatted}"
        );
        assert!(
            !formatted.contains("&&\n"),
            "should not wrap short &&:\n{formatted}"
        );
    }

    #[test]
    fn wraps_long_or_and_null_coalesce_chains() {
        let or_src = "\
fn main() {
    if (object.veryLongPropertyName == other.notSoLongName || object.shortName == other.somewhatLongerNameButStillGrowing || object.extraFlag == other.anotherFlag) {
        return;
    }
}
";
        let or_fmt = format_source(or_src).unwrap();
        assert!(
            or_fmt.contains("||\n"),
            "expected soft wrap before || continuation:\n{or_fmt}"
        );
        assert_eq!(or_fmt, format_source(&or_fmt).unwrap());

        let coalesce_src = "\
fn main() {
    let x = object.veryLongOptionalProperty ?? other.alsoQuiteLongFallbackValue ?? yetAnotherFallbackValue;
    return;
}
";
        let coalesce_fmt = format_source(coalesce_src).unwrap();
        assert!(
            coalesce_fmt.contains("??\n"),
            "expected soft wrap before ?? continuation:\n{coalesce_fmt}"
        );
        assert_eq!(coalesce_fmt, format_source(&coalesce_fmt).unwrap());
    }

    #[test]
    fn wraps_long_call_args_with_trailing_commas() {
        let src = "\
fn main() {
    write_all(stdout(), to_bytes(format(\"%s %s %s %s\", \"alpha-alpha-alpha\", \"beta-beta-beta-beta\", \"gamma-gamma-gamma\", \"delta-delta-delta-delta\")));
    return;
}
";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains(",\n"),
            "expected wrapped args with commas:\n{formatted}"
        );
        // Last wrapped arg should still have a trailing comma before the closing paren.
        let broken = formatted
            .lines()
            .filter(|l| l.contains('"') && l.trim_end().ends_with(','))
            .count();
        assert!(
            broken >= 2,
            "expected multiple trailing-comma argument lines:\n{formatted}"
        );
        round_trip(&formatted);
    }

    #[test]
    fn wraps_long_array_and_dict_with_trailing_commas() {
        let src = "\
fn main() {
    let a = [\"one-long-string-value\", \"two-long-string-value\", \"three-long-string-value\", \"four-long-string-value\"];
    let d = { alpha: 1, beta: 2, gamma: 3, delta: 4, epsilon: 5, zeta: 6, eta: 7, theta: 8, iota: 9, kappa: 10 };
    return;
}
";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("[\n") || formatted.contains("{\n"),
            "expected wrapped collection:\n{formatted}"
        );
        assert!(
            formatted.contains(",\n"),
            "expected trailing commas on wrapped items:\n{formatted}"
        );
        round_trip(&formatted);
    }

    #[test]
    fn class_fields_keep_trailing_commas() {
        let src = "\
class Point {
    pub x: int,
    pub y: int
}
fn main() { return; }
";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("pub x: int,") && formatted.contains("pub y: int,"),
            "class fields need trailing commas:\n{formatted}"
        );
        round_trip(&formatted);
    }

    #[test]
    fn one_tuple_keeps_trailing_comma() {
        let src = "fn main() {\n    let t = (1,);\n    return;\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(
            formatted.contains("(1,)"),
            "1-tuple must keep trailing comma:\n{formatted}"
        );
        round_trip(src);
    }
}
