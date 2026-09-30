//! Mutation sites: parse a source file and list text patches, each a small
//! behaviour change a good test suite should notice.

use parser::Pratt;
use parser::ast::{Expression, Output};

/// Mutation operator families (`--operators`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Operator {
    /// `<` ↔ `<=`, `>` ↔ `>=`.
    Boundary,
    /// `==` ↔ `!=`.
    Negate,
    /// `+` ↔ `-`, `*` ↔ `/`, `%` → `*`.
    Arith,
    /// `&&` ↔ `||`.
    Logic,
    /// `if c` / `while c` → `!(c)`.
    Cond,
    /// `true` ↔ `false`.
    Bool,
    /// `0` ↔ `1`, `n` → `n + 1`.
    Int,
}

impl Operator {
    pub const ALL: [Operator; 7] = [
        Operator::Boundary,
        Operator::Negate,
        Operator::Arith,
        Operator::Logic,
        Operator::Cond,
        Operator::Bool,
        Operator::Int,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Operator::Boundary => "boundary",
            Operator::Negate => "negate",
            Operator::Arith => "arith",
            Operator::Logic => "logic",
            Operator::Cond => "cond",
            Operator::Bool => "bool",
            Operator::Int => "int",
        }
    }

    pub fn parse(name: &str) -> Option<Operator> {
        Self::ALL.into_iter().find(|op| op.name() == name)
    }
}

/// One mutant: replace `source[start..end]` with `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    pub operator: Operator,
    pub start: usize,
    pub end: usize,
    pub replacement: String,
    /// 1-based line of `start`.
    pub line: u32,
    /// 1-based line where the enclosing top-level declaration starts (the
    /// lowest line whose coverage can stand for this site).
    pub scope_line: u32,
}

impl Site {
    /// `source` with this mutant applied.
    pub fn apply(&self, source: &str) -> String {
        let mut out = String::with_capacity(source.len() + self.replacement.len());
        out.push_str(&source[..self.start]);
        out.push_str(&self.replacement);
        out.push_str(&source[self.end..]);
        out
    }

    /// `original → replacement` for reports.
    pub fn describe(&self, source: &str) -> String {
        format!(
            "`{}` → `{}`",
            &source[self.start..self.end],
            self.replacement
        )
    }
}

/// Marker that suppresses mutants on its line, or in a whole `fn` when on
/// the line of (or just above) its header.
pub const NO_MUTATE: &str = "coil:no-mutate";

/// Every site in `source` for `operators`, in source order. `Err` when the
/// file does not parse.
pub fn enumerate(source: &str, operators: &[Operator]) -> Result<Vec<Site>, String> {
    let ast = Pratt::default()
        .parse(source)
        .map_err(|e| format!("{e:?}"))?;
    let mut walk = Walk {
        source,
        operators,
        line_starts: line_starts(source),
        scope_line: 1,
        in_fn: false,
        sites: Vec::new(),
    };
    walk.node(&ast);
    let mut sites = walk.sites;
    sites.retain(|s| !walk_line(source, &walk.line_starts, s.line).contains(NO_MUTATE));
    sites.sort_by_key(|s| (s.start, s.end, s.operator));
    sites.dedup_by(|a, b| a.start == b.start && a.end == b.end && a.replacement == b.replacement);
    Ok(sites)
}

fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn walk_line<'s>(source: &'s str, starts: &[usize], line: u32) -> &'s str {
    let i = line as usize - 1;
    let end = starts.get(i + 1).copied().unwrap_or(source.len());
    &source[starts[i]..end]
}

struct Walk<'a> {
    source: &'a str,
    operators: &'a [Operator],
    line_starts: Vec<usize>,
    scope_line: u32,
    in_fn: bool,
    sites: Vec<Site>,
}

/// Characters that can continue an operator token.
const OP_CHARS: &[u8] = b"<>=!&|+-*/%.";

impl Walk<'_> {
    fn line_of(&self, byte: usize) -> u32 {
        self.line_starts.partition_point(|&s| s <= byte) as u32
    }

    fn push(&mut self, operator: Operator, start: usize, end: usize, replacement: String) {
        if !self.operators.contains(&operator) {
            return;
        }
        self.sites.push(Site {
            operator,
            start,
            end,
            replacement,
            line: self.line_of(start),
            // Outside a `fn` (statics, enum discriminants) only the site's
            // own line can carry its coverage.
            scope_line: if self.in_fn {
                self.scope_line
            } else {
                self.line_of(start)
            },
        });
    }

    /// `true` when a `fn` starting at `start` is marked [`NO_MUTATE`] on its
    /// first line or the line above.
    fn fn_suppressed(&self, start: usize) -> bool {
        let line = self.line_of(start);
        (line.saturating_sub(1).max(1)..=line)
            .any(|l| walk_line(self.source, &self.line_starts, l).contains(NO_MUTATE))
    }

    fn node(&mut self, (span, expr): &Output<'_>) {
        let (start, end) = (span.start, span.end);
        if end > self.source.len() || start > end {
            return;
        }
        let skip = match expr.as_ref() {
            // Test code is not a mutation target.
            Expression::TestCase { .. } => true,
            Expression::Function { attrs, .. } => {
                attrs.iter().any(|a| a.name == "test") || self.fn_suppressed(start)
            }
            _ => false,
        };
        if skip {
            return;
        }
        // The outermost `fn` bounds the coverage search for its sites.
        let saved = (self.scope_line, self.in_fn);
        if !self.in_fn && matches!(expr.as_ref(), Expression::Function { .. }) {
            self.scope_line = self.line_of(start);
            self.in_fn = true;
        }
        self.visit(start, end, expr);
        expr.for_each_child(&mut |child| self.node(child));
        (self.scope_line, self.in_fn) = saved;
    }

    fn visit(&mut self, start: usize, end: usize, expr: &Expression<'_>) {
        use Expression as E;
        use Operator as O;
        let text = self.source[start..end].trim_end();
        let end = start + text.len();
        match expr {
            E::Bool(b) if text == "true" || text == "false" => {
                self.push(O::Bool, start, end, (!b).to_string());
            }
            E::Integer(n) if text.starts_with(|c: char| c.is_ascii_digit()) => {
                let to = match n {
                    0 => 1,
                    1 => 0,
                    n => n.saturating_add(1),
                };
                if to != *n {
                    self.push(O::Int, start, end, to.to_string());
                }
            }
            E::Branch(Some(cond), _) => self.negate(cond),
            E::Loop {
                identifier: None,
                pattern: None,
                iterable,
                ..
            } => self.negate(iterable),
            E::Le(l, r) => self.binary(l, r, "<", &[(O::Boundary, "<=")]),
            E::Leq(l, r) => self.binary(l, r, "<=", &[(O::Boundary, "<")]),
            E::Gt(l, r) => self.binary(l, r, ">", &[(O::Boundary, ">=")]),
            E::Geq(l, r) => self.binary(l, r, ">=", &[(O::Boundary, ">")]),
            E::Eq(l, r) => self.binary(l, r, "==", &[(O::Negate, "!=")]),
            E::Neq(l, r) => self.binary(l, r, "!=", &[(O::Negate, "==")]),
            E::Add(l, r) => self.binary(l, r, "+", &[(O::Arith, "-")]),
            E::Sub(l, r) => self.binary(l, r, "-", &[(O::Arith, "+")]),
            E::Mul(l, r) => self.binary(l, r, "*", &[(O::Arith, "/")]),
            E::Div(l, r) => self.binary(l, r, "/", &[(O::Arith, "*")]),
            E::Mod(l, r) => self.binary(l, r, "%", &[(O::Arith, "*")]),
            E::And(l, r) => self.binary(l, r, "&&", &[(O::Logic, "||")]),
            E::Or(l, r) => self.binary(l, r, "||", &[(O::Logic, "&&")]),
            _ => {}
        }
    }

    /// `c` → `!(c)`.
    fn negate(&mut self, (span, _): &Output<'_>) {
        // Spans can include trailing whitespace; keep it outside the parens.
        let Some(text) = self.source.get(span.start..span.end).map(str::trim_end) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let end = span.start + text.len();
        self.push(Operator::Cond, span.start, end, format!("!({text})"));
    }

    /// Find `op` as the only operator token between the operands and emit
    /// one site per replacement.
    fn binary(&mut self, l: &Output<'_>, r: &Output<'_>, op: &str, to: &[(Operator, &str)]) {
        let (gap_start, gap_end) = (l.0.end, r.0.start);
        let Some(gap) = self.source.get(gap_start..gap_end) else {
            return;
        };
        let Some(at) = find_token(gap, op) else {
            return;
        };
        let start = gap_start + at;
        for &(operator, replacement) in to {
            self.push(operator, start, start + op.len(), replacement.to_string());
        }
    }
}

/// Byte offset of `op` in `gap` when it is the gap's only operator token
/// (the rest is whitespace, parentheses and comments).
fn find_token(gap: &str, op: &str) -> Option<usize> {
    let bytes = gap.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() || c == b'(' || c == b')' {
            i += 1;
        } else if gap[i..].starts_with("//") {
            i += gap[i..].find('\n').map_or(gap.len() - i, |n| n + 1);
        } else if gap[i..].starts_with("/*") {
            i += gap[i..].find("*/").map_or(gap.len() - i, |n| n + 2);
        } else if OP_CHARS.contains(&c) {
            let len = bytes[i..]
                .iter()
                .take_while(|b| OP_CHARS.contains(b))
                .count();
            if &gap[i..i + len] != op || found.is_some() {
                return None;
            }
            found = Some(i);
            i += len;
        } else {
            return None;
        }
    }
    found
}

#[cfg(test)]
#[path = "sites.tests.rs"]
mod tests;
