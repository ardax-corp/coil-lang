//! Source pretty-printer for coil `.hy` files.
//!
//! Comments are parser trivia, so the formatter reads them from
//! [`crate::comments::collect`] and reattaches them by byte position:
//! a comment on its own line leads the next item, a comment after code on
//! the same line trails it. Single blank lines between items are kept.
//! `///` docs are part of the AST and are emitted with their declaration.

use crate::ast::{
    AdjustOp, AssignOp, Attribute, QuotePart, EnumConstructPayload, EnumVariantPayload, Expression,
    ExternFunction, ExternStructDecl, FieldModifier, LetPattern, Output, Pattern, RecordFieldDecl,
    RecordFieldValue, TypeParam, Visibility, WhereConstraint,
};
use crate::comments::{self, Comment};
use crate::Pratt;
use chumsky::span::SimpleSpan;
use reporting::Message;

const INDENT: &str = "    ";
/// Soft line-wrap budget (characters from start of line).
const MAX_WIDTH: usize = 100;

pub fn format_source(src: &str) -> Result<String, Message> {
    let ast = Pratt::default().parse(src)?;
    let comments = comments::collect(src);
    let expected = comments.len();
    let mut f = Formatter::new(src, comments);
    f.fmt_expression(ast.1.as_ref());
    f.flush_comments(usize::MAX);
    let out = f.finish();
    // Never hand back code that lost a comment or no longer parses.
    let kept = comments::collect(&out).len();
    let reparse = Pratt::default().parse(&out).err();
    if kept != expected || reparse.is_some() {
        let why = match reparse {
            Some(err) => {
                let line = out[..err.range().start.min(out.len())].matches('\n').count() + 1;
                format!("output line {line} does not parse: {}", err.message())
            }
            None => format!("{expected} comments became {kept}"),
        };
        return Err(Message::error(
            reporting::ErrorCode::ParseError,
            format!("coil fmt bug, file left unchanged: {why}"),
            0..0,
        ));
    }
    Ok(out)
}

/// Format a source range while preserving the formatter's whole-file parse
/// guarantees. The current formatter returns the complete formatted document;
/// callers can diff it against the requested range.
pub fn format_range(src: &str, _range: std::ops::Range<usize>) -> Result<String, Message> {
    format_source(src)
}

/// Format an AST without its source text (no comments, no blank-line
/// preservation). Prefer [`format_source`].
pub fn format_program(expr: &Expression<'_>) -> String {
    let mut f = Formatter::new("", Vec::new());
    f.fmt_expression(expr);
    f.finish()
}

struct Formatter<'s> {
    indent: usize,
    out: String,
    /// When true, never insert soft wraps (used to measure flat width).
    flat: bool,
    src: &'s str,
    comments: Vec<Comment<'s>>,
    /// First comment not yet emitted.
    next_comment: usize,
    /// End of the last source element emitted (blank-line detection).
    last_end: usize,
    /// Inside a type annotation (`[T; N]` vs the array literal `[a, b]`).
    type_ctx: bool,
}

impl<'s> Formatter<'s> {
    fn new(src: &'s str, comments: Vec<Comment<'s>>) -> Self {
        Self {
            indent: 0,
            out: String::new(),
            flat: false,
            src,
            comments,
            next_comment: 0,
            last_end: 0,
            type_ctx: false,
        }
    }

    /// Scratch formatter for measuring flat width. It has no comments, so
    /// callers must not choose a flat layout that spans pending comments
    /// (see [`Self::has_comment_in`]).
    fn measure(indent: usize) -> Formatter<'static> {
        Formatter {
            indent,
            flat: true,
            ..Formatter::new("", Vec::new())
        }
    }

    fn finish(mut self) -> String {
        while self.out.ends_with("\n\n") {
            self.out.pop();
        }
        if !self.out.ends_with('\n') {
            self.out.push('\n');
        }
        self.out
    }

    // ---- comments and blank lines -------------------------------------

    /// Comments are sorted and disjoint, so both starts and ends are sorted.
    fn comment_starting_at(&self, pos: usize) -> Option<&Comment<'s>> {
        let i = self.comments.binary_search_by_key(&pos, |c| c.span.start).ok()?;
        self.comments.get(i)
    }

    fn comment_ending_at(&self, pos: usize) -> Option<&Comment<'s>> {
        let i = self.comments.binary_search_by_key(&pos, |c| c.span.end).ok()?;
        self.comments.get(i)
    }

    fn pending(&self) -> Option<&Comment<'s>> {
        self.comments.get(self.next_comment)
    }

    /// First byte of real code in `span`: node spans can start or end with
    /// the whitespace and comments the parser skipped as padding.
    fn content_start(&self, span: SimpleSpan) -> usize {
        let bytes = self.src.as_bytes();
        let mut pos = span.start.min(bytes.len());
        loop {
            while pos < span.end && bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
                pos += 1;
            }
            match self.comment_starting_at(pos) {
                Some(c) if pos < span.end => pos = c.span.end,
                _ => return pos,
            }
        }
    }

    /// End of real code in `span` (see [`Self::content_start`]).
    fn content_end(&self, span: SimpleSpan) -> usize {
        let bytes = self.src.as_bytes();
        let mut pos = span.end.min(bytes.len());
        loop {
            while pos > span.start && bytes.get(pos - 1).is_some_and(u8::is_ascii_whitespace) {
                pos -= 1;
            }
            match self.comment_ending_at(pos) {
                Some(c) if pos > span.start => pos = c.span.start,
                _ => return pos,
            }
        }
    }

    /// Byte offset of a `&str` borrowed from the source.
    fn offset_of(&self, text: &str) -> Option<usize> {
        let base = self.src.as_ptr() as usize;
        let ptr = text.as_ptr() as usize;
        (ptr >= base && ptr + text.len() <= base + self.src.len()).then(|| ptr - base)
    }

    /// Whether `name` (borrowed from the source) is followed by `()`. The
    /// parser folds `Vec::new()` and `Vec::new` into one node; the source
    /// keeps the spelling.
    fn empty_parens_after(&self, name: &str) -> bool {
        let Some(start) = self.offset_of(name) else {
            return false;
        };
        let rest = self.src[start + name.len()..].trim_start();
        rest.strip_prefix('(')
            .is_some_and(|inner| inner.trim_start().starts_with(')'))
    }

    /// Position of the `close` delimiter after `from`, skipping whitespace,
    /// comments and a trailing comma.
    fn closing_after(&self, from: usize, close: u8) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let mut pos = from;
        loop {
            match bytes.get(pos) {
                Some(b) if *b == close => return Some(pos),
                Some(b) if b.is_ascii_whitespace() || *b == b',' || *b == b';' => pos += 1,
                Some(_) => pos = self.comment_starting_at(pos)?.span.end,
                None => return None,
            }
        }
    }

    /// Position of the `open` delimiter before `to`, skipping whitespace and
    /// comments.
    fn opening_before(&self, to: usize, open: u8) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let mut pos = to;
        while pos > 0 {
            let b = bytes[pos - 1];
            if b.is_ascii_whitespace() {
                pos -= 1;
            } else if b == open {
                return Some(pos - 1);
            } else {
                pos = self.comment_ending_at(pos)?.span.start;
            }
        }
        None
    }

    /// End of a node, including the closing brace of a bare block (block
    /// spans cover only their statements).
    fn node_end(&self, node: &Output<'_>) -> usize {
        let end = self.content_end(node.0);
        match node.1.as_ref() {
            Expression::Block(_) => self.closing_after(end, b'}').map_or(end, |close| close + 1),
            _ => end,
        }
    }

    fn has_comment_in(&self, start: usize, end: usize) -> bool {
        self.comments[self.next_comment..]
            .iter()
            .any(|c| c.span.start >= start && c.span.start < end)
    }

    /// Whether the output is at the start of a block / list (no blank line
    /// belongs there).
    fn at_open(&self) -> bool {
        let trimmed = self.out.trim_end_matches(' ');
        trimmed.is_empty()
            || trimmed.ends_with("\n\n")
            || trimmed.ends_with("{\n")
            || trimmed.ends_with("(\n")
            || trimmed.ends_with("[\n")
            || trimmed.ends_with("<\n")
    }

    /// Keep one blank line where the source had at least one between the last
    /// emitted element and `start`.
    fn blank_line_before(&mut self, start: usize) {
        let gap = self.src.get(self.last_end..start).unwrap_or("");
        if gap.matches('\n').count() >= 2 && !self.at_open() {
            self.out.push('\n');
        }
    }

    /// Emit every pending comment that starts before `pos`, each on its own
    /// line at the current indent. Call at the start of a line.
    fn leading_comments(&mut self, pos: usize) {
        while let Some(c) = self.pending() {
            if c.span.start >= pos {
                break;
            }
            let (start, end, text) = (c.span.start, c.span.end, c.text);
            if !self.out.is_empty() && !self.out.ends_with('\n') {
                self.out.push('\n');
            }
            self.blank_line_before(start);
            self.write_indent();
            self.push_str(text);
            self.newline();
            self.last_end = end;
            self.next_comment += 1;
        }
    }

    /// Append the comment that follows `end` on the same source line.
    fn trailing_comment(&mut self, end: usize) {
        let Some(c) = self.pending() else {
            return;
        };
        // Same source line, and not past a closer: `return 1; }, // c`
        // belongs to the arm, not to the statement inside its block.
        let same_line = c.span.start >= end
            && !self
                .src
                .get(end..c.span.start)
                .unwrap_or("\n")
                .contains(['\n', '}', ')', ']']);
        if same_line {
            let (end, text) = (c.span.end, c.text);
            self.push_str(" ");
            self.push_str(text);
            self.last_end = end;
            self.next_comment += 1;
        }
    }

    /// Emit all remaining comments before `pos` (end of a body or the file).
    fn flush_comments(&mut self, pos: usize) {
        self.leading_comments(pos);
    }

    /// One element of a multi-line body: leading comments and blank line,
    /// indent, `emit`, trailing comment, newline.
    fn body_item(&mut self, span: SimpleSpan, emit: impl FnOnce(&mut Self)) {
        let start = self.content_start(span);
        let end = self.content_end(span);
        self.leading_comments(start);
        self.blank_line_before(start);
        self.write_indent();
        emit(self);
        self.last_end = end.max(self.last_end);
        self.trailing_comment(end);
        self.newline();
    }

    /// Comments between the last body element and its closing delimiter.
    fn body_close(&mut self, last: Option<SimpleSpan>, close: u8) {
        if let Some(last) = last
            && let Some(pos) = self.closing_after(self.content_end(last), close)
        {
            self.leading_comments(pos);
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
        let mut f = Formatter::measure(self.indent);
        f.fmt_expression(expr);
        f.out
    }

    fn fits_flat(&self, flat: &str) -> bool {
        self.flat || self.current_col().saturating_add(flat.len()) <= MAX_WIDTH
    }

    /// A type annotation.
    fn fmt_type(&mut self, ty: &Output<'_>) {
        let was = std::mem::replace(&mut self.type_ctx, true);
        self.fmt_output(ty);
        self.type_ctx = was;
    }

    /// An expression in a slot that holds a whole expression (initializer,
    /// return value, argument, element): outer parens are redundant.
    fn fmt_value(&mut self, value: &Output<'_>) {
        self.fmt_output(strip_groups(value));
    }

    /// `if` / `while` / `match` heads: outer parens are redundant unless the
    /// inner expression is a `{` record, which would read as the body.
    fn fmt_condition(&mut self, cond: &Output<'_>) {
        let inner = strip_groups(cond);
        if matches!(inner.1.as_ref(), Expression::Dict(_)) {
            self.fmt_output(cond);
        } else {
            self.fmt_output(inner);
        }
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
            let mut f = Formatter::measure(self.indent);
            f.push_str(open);
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                f.fmt_value(item);
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
        let first = self.content_start(items[0].0);
        let close_at = self
            .closing_after(self.content_end(items[items.len() - 1].0), close.as_bytes()[0])
            .unwrap_or(first);
        let has_comments = self.has_comment_in(first, close_at);

        if !has_docs && !has_comments && self.fits_flat(&flat) {
            self.push_str(open);
            let was = self.flat;
            self.flat = true;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    self.push_str(", ");
                }
                self.fmt_value(item);
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
        if !has_comments && !has_docs && items.iter().all(|item| is_short_literal(item.1.as_ref())) {
            // Many short literals (`[0, 1, 2, …]`): fill lines instead of one
            // item per line.
            self.with_indent(|f| {
                f.write_indent();
                for (i, item) in items.iter().enumerate() {
                    let text = f.render_flat(strip_groups(item).1.as_ref());
                    if i > 0 {
                        if f.current_col() + 1 + text.len() + 1 > MAX_WIDTH {
                            f.newline();
                            f.write_indent();
                        } else {
                            f.push_str(" ");
                        }
                    }
                    f.push_str(&text);
                    f.push_str(",");
                }
                f.newline();
            });
            self.write_indent();
            self.push_str(close);
            return;
        }
        self.with_indent(|f| {
            for item in items {
                f.body_item(item.0, |f| {
                    f.fmt_value(item);
                    f.push_str(",");
                });
            }
            f.body_close(items.last().map(|i| i.0), close.as_bytes()[0]);
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
            let mut f = Formatter::measure(self.indent);
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
        let spans: Vec<SimpleSpan> = items
            .iter()
            .map(|field| self.field_span(field.name, field.value.0))
            .collect();
        let has_comments = self.has_comment_in(
            self.content_start(spans[0]),
            self.closing_after(self.content_end(spans[spans.len() - 1]), b'}')
                .unwrap_or(0),
        );
        if !has_comments && self.fits_flat(&flat) {
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
            for (field, span) in items.iter().zip(&spans) {
                f.body_item(*span, |f| {
                    f.push_str(field.name);
                    f.push_str(": ");
                    f.fmt_output(&field.value);
                    f.push_str(",");
                });
            }
            f.body_close(spans.last().copied(), b'}');
        });
        self.write_indent();
        self.push_str("}");
    }

    /// Span of `name: value` from the name's source slice (falls back to the
    /// value alone for synthetic names).
    fn field_span(&self, name: &str, value: SimpleSpan) -> SimpleSpan {
        let start = self.offset_of(name).map_or(value.start, |o| o.min(value.start));
        SimpleSpan::from(start..value.end)
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
                f.body_item(item.0, |f| {
                    f.fmt_expression(item.1.as_ref());
                    f.push_str(",");
                });
            }
            f.body_close(items.last().map(|i| i.0), b'}');
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
            // Parens around an atom never change the parse.
            Expression::Group(g) if is_atom(strip_groups(g).1.as_ref()) => {
                self.fmt_output(strip_groups(g))
            }
            Expression::Group(g) => {
                self.push_str("(");
                self.fmt_output(g);
                self.push_str(")");
            }
            Expression::ExprStatement(e) => {
                self.fmt_value(e);
                // A statement `match` ends at its `}`; no `;`.
                if !matches!(strip_groups(e).1.as_ref(), Expression::Match { .. }) {
                    self.push_str(";");
                }
            }
            Expression::Statement(s) => self.fmt_statement_line(s),

            Expression::Fragment(items) => self.fmt_fragment(items),
            Expression::Block(items) => self.fmt_block_braced(items),
            Expression::Program(items) => self.fmt_program(items),

            Expression::If(branches) => self.fmt_if(branches),
            Expression::Branch(cond, body) => {
                if let Some(c) = cond {
                    self.push_str("if ");
                    self.fmt_condition(c);
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
                    self.fmt_value(e);
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
                self.fmt_type(ty);
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
                self.fmt_value(rhs);
            }

            // `[T; N]` in a type position shares the value array node.
            Expression::Array(items) if self.type_ctx && items.len() == 2 => {
                self.push_str("[");
                self.fmt_output(&items[0]);
                self.push_str("; ");
                self.fmt_output(&items[1]);
                self.push_str("]");
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
            Expression::Access(_, _)
            | Expression::OptionalAccess(_, _)
            | Expression::Call { .. } => {
                if let Some(parts) = collect_member_chain(expr) {
                    self.fmt_member_chain(&parts);
                } else {
                    self.fmt_member_or_call_atom(expr);
                }
            }
            Expression::QualifiedAccess { owner, member } => {
                self.push_str(owner);
                self.push_str("::");
                // `m::f()` parses as `Call`, which prints its own parens.
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
                            self.fmt_type(t);
                            self.push_str("... ");
                            self.push_str(name);
                        }
                    }
                } else {
                    self.fmt_type(ty.as_ref().expect("fixed param"));
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
                    self.fmt_type(t);
                }
            }
            Expression::Constant(name, ty) => {
                self.push_str("const ");
                self.fmt_output(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_type(t);
                }
            }
            Expression::LetDestructure { pattern, rhs } => {
                self.push_str("let ");
                self.fmt_let_pattern(pattern);
                self.push_str(" = ");
                self.fmt_value(rhs);
            }
            Expression::StaticDecl {
                is_const,
                name,
                ty,
                init,
            } => {
                if *is_const {
                    self.push_str("static const ");
                } else {
                    self.push_str("static let ");
                }
                self.push_str(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_type(t);
                }
                self.push_str(" = ");
                self.fmt_value(init);
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
                pattern,
                iterable,
                body,
            } => {
                if let Some(ident) = identifier {
                    self.push_str("for ");
                    self.fmt_output(ident);
                    self.push_str(" in ");
                    self.fmt_condition(iterable);
                    self.push_str(" ");
                    self.fmt_block_or_inline(body);
                } else if let Some(pat) = pattern {
                    self.push_str("for ");
                    self.fmt_let_pattern(pat);
                    self.push_str(" in ");
                    self.fmt_condition(iterable);
                    self.push_str(" ");
                    self.fmt_block_or_inline(body);
                } else {
                    self.push_str("while ");
                    self.fmt_condition(iterable);
                    self.push_str(" ");
                    self.fmt_block_or_inline(body);
                }
            }

            Expression::IfLet {
                scrutinee,
                then_arm,
                else_arm,
            } => {
                self.push_str("if let ");
                self.fmt_pattern(&then_arm.pattern);
                self.push_str(" = ");
                self.fmt_condition(scrutinee);
                self.push_str(" ");
                self.fmt_block_or_inline(&then_arm.body);
                if !matches!(else_arm.body.1.as_ref(), Expression::Block(items) if items.is_empty())
                {
                    self.push_str(" else ");
                    match else_arm.body.1.as_ref() {
                        Expression::IfLet { .. } | Expression::If(_) => {
                            self.fmt_output(&else_arm.body);
                        }
                        _ => self.fmt_block_or_inline(&else_arm.body),
                    }
                }
            }

            Expression::WhileLet {
                scrutinee,
                then_arm,
                ..
            } => {
                self.push_str("while let ");
                self.fmt_pattern(&then_arm.pattern);
                self.push_str(" = ");
                self.fmt_condition(scrutinee);
                self.push_str(" ");
                self.fmt_block_or_inline(&then_arm.body);
            }

            Expression::Match { scrutinee, arms } => {
                self.push_str("match ");
                self.fmt_condition(scrutinee);
                self.push_str(" {");
                self.newline();
                let spans: Vec<SimpleSpan> = arms
                    .iter()
                    .map(|arm| SimpleSpan::from(arm.pattern.0.start..self.node_end(&arm.body)))
                    .collect();
                self.with_indent(|f| {
                    for (arm, span) in arms.iter().zip(&spans) {
                        f.body_item(*span, |f| {
                            f.fmt_pattern(&arm.pattern);
                            f.push_str(" => ");
                            f.fmt_match_arm_body(&arm.body);
                            f.push_str(",");
                        });
                    }
                    f.body_close(spans.last().copied(), b'}');
                });
                self.write_indent();
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
                if matches!(fields, EnumConstructPayload::Unit) && self.empty_parens_after(variant_name) {
                    self.push_str("()");
                }
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
                self.fmt_type(ty);
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
                self.fmt_type(ty);
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
                    self.fmt_type(ret);
                }
                self.fmt_where(where_constraints);
                self.push_str(" ");
                self.fmt_block_or_inline(body);
            }

            Expression::DeriveDecl {
                docs,
                name,
                args,
                returns,
                helpers,
                body,
            } => {
                self.fmt_docs(docs);
                self.push_str("derive ");
                self.push_str(name);
                self.fmt_paren_arg_list(args);
                if let Some(ret) = returns {
                    self.push_str(" -> ");
                    self.fmt_type(ret);
                }
                if !helpers.is_empty() {
                    self.push_str(" attrs(");
                    self.push_str(&helpers.join(", "));
                    self.push_str(")");
                }
                self.push_str(" ");
                self.fmt_block_or_inline(body);
            }

            // Template text is coil source the user laid out by hand: keep it
            // verbatim and only format the spliced expressions.
            Expression::Quote { kind, parts } => {
                self.push_str("quote ");
                self.push_str(kind.as_str());
                self.push_str(" {");
                for part in parts {
                    match part {
                        QuotePart::Lit(text) => self.push_str(text),
                        QuotePart::Splice(e) => {
                            self.push_str("${");
                            self.fmt_output(e);
                            self.push_str("}");
                        }
                        QuotePart::Repeat { list, sep } => {
                            self.push_str("$(");
                            self.fmt_output(list);
                            self.push_str(")");
                            self.push_str(sep);
                            self.push_str("*");
                        }
                    }
                }
                self.push_str("}");
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
            Expression::EnumVariant {
                docs,
                attrs,
                name,
                payload,
                discriminant,
            } => {
                self.fmt_docs(docs);
                self.fmt_member_attrs(attrs);
                self.push_str(name);
                self.fmt_enum_variant_payload(payload);
                if let Some(disc) = discriminant {
                    self.push_str(" = ");
                    self.fmt_output(disc);
                }
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
                attrs,
                visibility,
                modifier,
                name,
                ty,
                init,
            } => {
                self.fmt_docs(docs);
                self.fmt_member_attrs(attrs);
                self.fmt_visibility(*visibility);
                self.fmt_field_modifier(*modifier);
                self.fmt_output(name);
                self.push_str(": ");
                self.fmt_type(ty);
                if let Some(i) = init {
                    self.push_str(" = ");
                    self.fmt_value(i);
                }
            }
            Expression::Method(visibility, func) => {
                // Docs and attributes go before `pub`: `#[a]\npub fn f()`.
                let mut bare = func.clone();
                if let Expression::Function { docs, attrs, .. } = bare.1.as_mut() {
                    self.fmt_docs(docs);
                    self.fmt_member_attrs(attrs);
                    attrs.clear();
                }
                self.fmt_visibility(*visibility);
                self.fmt_function(&bare, false);
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
                        let was = std::mem::replace(&mut self.type_ctx, true);
                        self.fmt_delimited_outputs("<", ">", rest, false);
                        self.type_ctx = was;
                    }
                    self.push_str(" for ");
                    self.fmt_type(for_ty);
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
                self.fmt_type(ty);
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
            Expression::ExprStatement(_) => self.fmt_expression(s.1.as_ref()),
            other => {
                self.fmt_expression(other);
                if stmt_needs_semicolon(other) {
                    self.push_str(";");
                }
            }
        }
    }

    /// A block statement without indent / newline (see [`Self::body_item`]).
    ///
    /// `is_last`: the value-producing tail of a match-arm / lambda body
    /// (`{ let a = x + 1; a }`) has no `;` in the source and must not gain
    /// one; that would turn the block's value into `unit`.
    fn fmt_block_stmt(&mut self, item: &Output<'_>, is_last: bool) {
        match item.1.as_ref() {
            Expression::Statement(s) => self.fmt_statement_line(s),
            other => {
                self.fmt_expression(other);
                let tail = is_last && !self.src.is_empty() && !self.semicolon_after(item.0);
                if stmt_needs_semicolon(other) && !tail {
                    self.push_str(";");
                }
            }
        }
    }

    /// Whether the source has a `;` ending the node at `span`.
    fn semicolon_after(&self, span: SimpleSpan) -> bool {
        let end = self.content_end(span);
        if end > 0 && self.src.as_bytes().get(end - 1) == Some(&b';') {
            return true;
        }
        self.closing_after(end, b';').is_some()
    }

    fn fmt_block_braced(&mut self, items: &[Output<'_>]) {
        let open = items
            .first()
            .and_then(|first| self.opening_before(self.content_start(first.0), b'{'));
        let comments_inside = open.is_some_and(|open| {
            self.closing_after(self.content_end(items[items.len() - 1].0), b'}')
                .is_some_and(|close| self.has_comment_in(open, close))
        });
        if items.is_empty() && !comments_inside {
            self.push_str("{}");
            return;
        }
        self.push_str("{");
        if let Some(open) = open {
            // `fn f() { // why` keeps its comment on the brace line.
            self.trailing_comment(open + 1);
        }
        self.newline();
        self.with_indent(|f| {
            for (i, item) in items.iter().enumerate() {
                f.body_item(item.0, |f| f.fmt_block_stmt(item, i + 1 == items.len()));
            }
            f.body_close(items.last().map(|i| i.0), b'}');
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

    /// Top level: one blank line between items (a run of `use`s is one
    /// item). A comment after an item is separated by a blank line; after a
    /// comment the source spacing is kept, so a comment directly above an
    /// item stays attached and a header block stays one block.
    fn fmt_program(&mut self, items: &[Output<'_>]) {
        // `None` before the first element, else whether it was a comment.
        let mut prev: Option<bool> = None;
        let mut i = 0;
        while i < items.len() {
            let start = self.content_start(items[i].0);
            self.top_level_comments(start, &mut prev);
            self.top_level_separator(prev, start);
            let last = if is_use_item(items[i].1.as_ref()) {
                let run_start = i;
                i += 1;
                // A comment or a blank line between two `use`s ends the run:
                // import groups the author separated stay separated.
                while i < items.len()
                    && is_use_item(items[i].1.as_ref())
                    && !self
                        .pending()
                        .is_some_and(|c| c.span.start < self.content_start(items[i].0))
                    && !self
                        .src
                        .get(self.content_end(items[i - 1].0)..self.content_start(items[i].0))
                        .is_some_and(|gap| gap.matches('\n').count() >= 2)
                {
                    i += 1;
                }
                // `use a::{b, c};` is a fragment of `use`s: group them with
                // their plain neighbours.
                let mut uses = Vec::new();
                for item in &items[run_start..i] {
                    match item.1.as_ref() {
                        Expression::Fragment(parts) => uses.extend(parts.iter().cloned()),
                        _ => uses.push(item.clone()),
                    }
                }
                self.fmt_use_group(&uses);
                &items[i - 1]
            } else {
                self.fmt_expression(items[i].1.as_ref());
                i += 1;
                &items[i - 1]
            };
            let end = self.content_end(last.0);
            self.last_end = end;
            self.trailing_comment(end);
            self.newline();
            prev = Some(false);
        }
        self.top_level_comments(usize::MAX, &mut prev);
    }

    fn top_level_comments(&mut self, before: usize, prev: &mut Option<bool>) {
        while let Some(c) = self.pending() {
            if c.span.start >= before {
                break;
            }
            let (start, end, text) = (c.span.start, c.span.end, c.text);
            self.top_level_separator(*prev, start);
            self.push_str(text);
            self.newline();
            self.last_end = end;
            self.next_comment += 1;
            *prev = Some(true);
        }
    }

    fn top_level_separator(&mut self, prev: Option<bool>, start: usize) {
        let source_blank = self
            .src
            .get(self.last_end..start)
            .is_some_and(|gap| gap.matches('\n').count() >= 2);
        let blank = match prev {
            None => false,
            Some(false) => true,
            Some(true) => source_blank,
        };
        if blank {
            self.out.push('\n');
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
        let same_namespace = items
            .iter()
            .all(|item| use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path == first_path));
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
                    self.fmt_condition(c);
                    self.push_str(" ");
                }
            } else if cond.is_some() {
                self.push_str("else if ");
                self.fmt_condition(cond.as_ref().unwrap());
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
                    self.fmt_type(t);
                }
                if let Some(val) = items.get(1) {
                    self.push_str(" = ");
                    self.fmt_value(val);
                }
            }
            Expression::Constant(name, ty) => {
                self.push_str("const ");
                self.fmt_output(name);
                if let Some(t) = ty {
                    self.push_str(": ");
                    self.fmt_type(t);
                }
                if let Some(val) = items.get(1) {
                    self.push_str(" = ");
                    self.fmt_value(val);
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
                f.body_item(item.0, |f| f.fmt_expression(item.1.as_ref()));
            }
            f.body_close(items.last().map(|i| i.0), b'}');
        });
        self.write_indent();
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
                self.fmt_delimited_outputs("(", ")", args, false);
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
                let was = std::mem::replace(&mut self.type_ctx, true);
                self.fmt_delimited_outputs("(", ")", parts, false);
                self.type_ctx = was;
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
            let mut f = Formatter::measure(self.indent);
            f.push_str("{ ");
            for (i, rf) in fields.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                f.push_str(rf.name);
                f.push_str(": ");
                f.fmt_type(&rf.value);
            }
            f.push_str(" }");
            f.out
        };
        let spans: Vec<SimpleSpan> = fields
            .iter()
            .map(|rf| self.field_span(rf.name, rf.value.0))
            .collect();
        let has_comments = self.has_comment_in(
            self.content_start(spans[0]),
            self.closing_after(self.content_end(spans[spans.len() - 1]), b'}')
                .unwrap_or(0),
        );
        if !has_comments && self.fits_flat(&flat) {
            self.push_str("{ ");
            let was = self.flat;
            self.flat = true;
            for (i, rf) in fields.iter().enumerate() {
                if i > 0 {
                    self.push_str(", ");
                }
                self.push_str(rf.name);
                self.push_str(": ");
                self.fmt_type(&rf.value);
            }
            self.flat = was;
            self.push_str(" }");
            return;
        }
        self.push_str("{");
        self.newline();
        self.with_indent(|f| {
            for (rf, span) in fields.iter().zip(&spans) {
                f.body_item(*span, |f| {
                    f.push_str(rf.name);
                    f.push_str(": ");
                    f.fmt_type(&rf.value);
                    f.push_str(",");
                });
            }
            f.body_close(spans.last().copied(), b'}');
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
                            let mut f = Formatter::measure(self.indent);
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
            self.fmt_type(ret);
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
                f.fmt_type(ty);
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
            let mut f = Formatter::measure(self.indent);
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
            self.fmt_type(ret);
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

    /// Attributes on a field / variant: one per line at the member's indent.
    fn fmt_member_attrs(&mut self, attrs: &[Attribute<'_>]) {
        for attr in attrs {
            self.push_str(&attr.to_string());
            self.newline();
            self.write_indent();
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

/// Numbers and short strings: list items worth packing several per line.
fn is_short_literal(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Integer(_) | Expression::Float(_) | Expression::Bool(_) => true,
        Expression::String(s) => s.len() <= 8,
        Expression::Negate(inner) | Expression::Expr(inner) => is_short_literal(inner.1.as_ref()),
        _ => false,
    }
}

/// A top-level `use`, or a brace group `use a::{b, c};` (a fragment of them).
fn is_use_item(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Use { .. } => true,
        Expression::Fragment(parts) => {
            !parts.is_empty() && parts.iter().all(|p| matches!(p.1.as_ref(), Expression::Use { .. }))
        }
        _ => false,
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
    let same_namespace = items
        .iter()
        .all(|item| use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path == first_path));
    same_namespace
        || items
            .iter()
            .all(|item| use_parts(item.1.as_ref()).is_some_and(|(path, _, _)| path.len() + 1 > 3))
}

fn is_bare_return(expr: &Expression<'_>) -> bool {
    match expr {
        Expression::Noop(_) => true,
        Expression::Tuple(items) => items.is_empty(),
        Expression::Expr(inner) => is_bare_return(inner.1.as_ref()),
        _ => false,
    }
}

/// `output` without any enclosing parens. `(e)` parses as
/// `Group(Fragment([e]))`; the parser's [`Expression::Expr`] wrapper is
/// transparent too.
fn strip_groups<'a, 'e>(mut output: &'a Output<'e>) -> &'a Output<'e> {
    loop {
        match output.1.as_ref() {
            Expression::Group(inner) | Expression::Expr(inner) => output = inner,
            Expression::Fragment(items) if items.len() == 1 => output = &items[0],
            _ => return output,
        }
    }
}

/// Operands whose parens are always redundant. Numeric literals are left
/// out: `(1).f()` must not become `1.f()`, which reads as a float.
fn is_atom(expr: &Expression<'_>) -> bool {
    matches!(
        expr,
        Expression::Identifier(_)
            | Expression::String(_)
            | Expression::Bool(_)
            | Expression::Call { .. }
            | Expression::Access(..)
            | Expression::Index(..)
            | Expression::QualifiedAccess { .. }
            | Expression::Group(_)
            | Expression::List(_)
            | Expression::Array(_)
            | Expression::Tuple(_)
    )
}

fn stmt_needs_semicolon(expr: &Expression<'_>) -> bool {
    !matches!(
        expr,
        Expression::ExprStatement(_)
            | Expression::If(_)
            | Expression::Block(_)
            | Expression::Loop { .. }
            | Expression::IfLet { .. }
            | Expression::WhileLet { .. }
            | Expression::Defer { .. }
            // These print their own `;`.
            | Expression::TypeAlias { .. }
            | Expression::StaticDecl { .. }
            | Expression::Module(..)
            | Expression::Use { .. }
            | Expression::AssocTypeDecl { .. }
            | Expression::AssocTypeDef { .. }
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
    fn keeps_empty_call_parens_on_paths() {
        let src = "fn main() {\n    let v: Vec<int> = Vec::new();\n    let n = Option::None;\n    let t = clock::mono_nanos();\n    let f = Vec::new;\n}\n";
        let formatted = format_source(src).unwrap();
        assert_eq!(formatted, src);
    }

    #[test]
    fn groups_use_statements_by_namespace() {
        let src = "use io::stdout;\nuse io::open;\nfn main() { return; }\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("use io::{stdout, open};"));
        assert!(formatted.contains("};\n\nfn main"));
        assert!(!formatted.contains("stdout;\n\nuse"));
        Pratt::default()
            .parse(&formatted)
            .expect("grouped use parses");
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
        Pratt::default()
            .parse(&formatted)
            .expect("deep grouped use parses");

        let shallow = "use a::b::one;\nuse a::c::two;\nfn main() { return; }\n";
        let formatted = format_source(shallow).unwrap();
        assert!(!formatted.contains("use a::{"));
        assert!(formatted.contains("use a::b::one;\nuse a::c::two;"));
        Pratt::default()
            .parse(&formatted)
            .expect("shallow use parses");
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
        assert_eq!(construct.code(), Some(reporting::ErrorCode::DuplicateField));
        assert!(
            construct.message().contains("Duplicate field `x`"),
            "got {}",
            construct.message()
        );

        let enum_decl = format_source("enum E { Foo { x: int, x: int } }\n")
            .expect_err("duplicate enum field decls must not format");
        assert_eq!(enum_decl.code(), Some(reporting::ErrorCode::DuplicateField));
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

    /// Already-formatted input must come back byte-identical.
    fn stable(src: &str) {
        let formatted = format_source(src).expect("format failed");
        assert_eq!(formatted, src, "formatted:\n{formatted}");
    }

    #[test]
    fn header_comment_block_stays_one_block() {
        stable("// one\n// two\n//\n// four\n\nuse io::stdout;\n");
    }

    #[test]
    fn trailing_comments_stay_on_their_line() {
        stable(
            "fn f(int a) -> int { // why\n    let x = a + 1; // step\n    return x; // done\n}\n",
        );
    }

    #[test]
    fn single_blank_lines_are_kept_and_runs_collapse() {
        let src = "fn main() {\n    let a = 1;\n\n\n\n    let b = 2;\n    let c = 3;\n}\n";
        assert_eq!(
            format_source(src).unwrap(),
            "fn main() {\n    let a = 1;\n\n    let b = 2;\n    let c = 3;\n}\n"
        );
    }

    #[test]
    fn comments_in_bodies_and_lists() {
        stable(concat!(
            "class P {\n",
            "    // lead\n",
            "    pub x: int, // trail\n",
            "}\n",
            "\n",
            "fn main() {\n",
            "    let xs = [\n",
            "        1, // one\n",
            "        // before two\n",
            "        2,\n",
            "    ];\n",
            "    /* closing */\n",
            "}\n",
        ));
    }

    #[test]
    fn comment_forces_list_to_break() {
        let formatted = format_source("fn main() {\n    let xs = [1, /* one */ 2];\n}\n").unwrap();
        assert!(formatted.contains("[\n"), "list must break around its comment:\n{formatted}");
        assert!(formatted.contains("/* one */"));
    }

    #[test]
    fn arm_trailing_comment_stays_on_the_arm() {
        let src = "fn f(int y) -> int {\n    return match y {\n        1 => {\n            return 1;\n        }, // arm\n        _ => y,\n    };\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("}, // arm"), "{formatted}");
    }

    #[test]
    fn block_tail_value_gets_no_semicolon() {
        let src = "fn f(int y) -> int {\n    return match y {\n        _ => {\n            let a = y + 1;\n            a\n        }\n    };\n}\n";
        let formatted = format_source(src).unwrap();
        assert!(formatted.contains("            a\n"), "tail `a` must stay bare:\n{formatted}");
    }

    #[test]
    fn static_decl_and_fixed_array_types_round_trip() {
        stable("static let hits: int = 0;\n\nfn sum3([int; 3] xs) -> int {\n    return xs[0];\n}\n");
    }

    #[test]
    fn empty_block_is_compact() {
        stable("fn g() {}\n");
    }

    #[test]
    fn statement_match_drops_semicolon_and_every_arm_has_a_comma() {
        let src = "fn f(int d) {\n    match d {\n        1 => {},\n        default => {}\n    };\n}\n";
        assert_eq!(
            format_source(src).unwrap(),
            "fn f(int d) {\n    match d {\n        1 => {},\n        default => {},\n    }\n}\n"
        );
    }

    #[test]
    fn value_match_keeps_its_semicolon() {
        stable("fn f(int d) -> int {\n    return match d {\n        default => 1,\n    };\n}\n");
    }

    #[test]
    fn redundant_parens_are_removed() {
        let src = "fn main() {\n    let w = ((1 + 2));\n    let a = (x);\n    if (a > 1) {\n        return;\n    }\n    f((2 * 2 + 3), (y).z);\n    return (a);\n}\n";
        assert_eq!(
            format_source(src).unwrap(),
            "fn main() {\n    let w = 1 + 2;\n    let a = x;\n    if a > 1 {\n        return;\n    }\n    f(2 * 2 + 3, y.z);\n    return a;\n}\n"
        );
    }

    #[test]
    fn meaningful_parens_are_kept() {
        stable("fn main() {\n    let z = (1 + 2) * 3;\n    let n = (1).to_string();\n    let t = (1, 2);\n    let u = (1,);\n}\n");
    }

    #[test]
    fn brace_group_uses_join_their_run() {
        let src = "use io::{stdout};\nuse io::sync::{write_all};\nuse string::{format, to_bytes};\nfn main() {}\n";
        assert_eq!(
            format_source(src).unwrap(),
            "use io::stdout;\nuse io::sync::write_all;\nuse string::{format, to_bytes};\n\nfn main() {}\n"
        );
    }

    #[test]
    fn blank_line_between_use_groups_is_kept() {
        stable("use io::stdout;\n\nuse string::format;\n\nfn main() {}\n");
    }

    #[test]
    fn long_literal_lists_fill_lines() {
        let items: Vec<String> = (0..60).map(|n| n.to_string()).collect();
        let src = format!("fn main() {{\n    let a = [{}];\n}}\n", items.join(", "));
        let formatted = format_source(&src).unwrap();
        let lines: Vec<&str> = formatted.lines().collect();
        assert!(lines.len() < 10, "packed, not one per line:\n{formatted}");
        assert!(lines.iter().all(|l| l.len() <= MAX_WIDTH), "{formatted}");
        stable(&formatted);
    }
}
