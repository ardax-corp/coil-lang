//! Text form of HIR for `coil dissect --hir` and golden tests.
//!
//! One node per line, children indented two spaces. A node prints its kind,
//! then `: type`, then its layout when it is not a plain word, then its
//! sidecar flags. Locals print as `name%N`.

use std::fmt::Write;

use super::layout::{Layout, PairKind};
use super::{
    BinOp, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, HirPatFields, Lit,
    LocalId, LocalKind, MakeKind,
};

pub fn body_to_string(module: &HirModule, body: &HirBody) -> String {
    let mut p = Printer {
        module,
        body,
        out: String::new(),
    };
    p.header();
    if let Some(root) = body.root {
        p.node(root, 1);
    }
    p.out
}

struct Printer<'a> {
    module: &'a HirModule,
    body: &'a HirBody,
    out: String,
}

fn layout_str(layout: &Layout) -> Option<String> {
    Some(match layout {
        Layout::Word => return None,
        Layout::Pair(kind) => format!(
            "pair {}",
            match kind {
                PairKind::Option => "Option".to_string(),
                PairKind::Result => "Result".to_string(),
                PairKind::Product => "product".to_string(),
                PairKind::Range { inclusive: false } => "range".to_string(),
                PairKind::Range { inclusive: true } => "range_inclusive".to_string(),
                PairKind::Enum(name) => name.clone(),
            }
        ),
        Layout::NicheOption => "niche Option".to_string(),
        Layout::NicheUnitResult => "niche unit Result".to_string(),
        Layout::NicheResult => "niche Result".to_string(),
    })
}

fn bin_str(op: BinOp) -> String {
    match op {
        BinOp::Overloaded(sym) => format!("overloaded `{sym}`"),
        other => format!("{other:?}"),
    }
}

impl Printer<'_> {
    fn local(&self, id: LocalId) -> String {
        format!("{}%{}", self.body.local(id).name, id.0)
    }

    fn header(&mut self) {
        let b = self.body;
        let params: Vec<String> = b
            .params
            .iter()
            .map(|p| match &b.local(*p).ty {
                Some(ty) => format!("{}: {ty}", self.local(*p)),
                None => self.local(*p),
            })
            .collect();
        let _ = write!(self.out, "{:?} {}({})", b.kind, b.name, params.join(", "));
        if let Some(ret) = &b.ret {
            let _ = write!(self.out, " -> {ret}");
        }
        if let Some(l) = layout_str(&b.ret_layout) {
            let _ = write!(self.out, " [{l}]");
        }
        let mut tags = Vec::new();
        if b.result_mode {
            tags.push("result-mode");
        }
        if b.is_coro {
            tags.push("coro");
        }
        if b.is_generic {
            tags.push("generic");
        }
        if !tags.is_empty() {
            let _ = write!(self.out, " {}", tags.join(" "));
        }
        self.out.push('\n');
        if !b.captures.is_empty() {
            let caps: Vec<String> = b.captures.iter().map(|(_, inner)| self.local(*inner)).collect();
            let _ = writeln!(self.out, "  captures {}", caps.join(", "));
        }
    }

    fn line(&mut self, depth: usize, id: HirId, head: String) {
        let e = self.body.expr(id);
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(&head);
        if let Some(ty) = &e.ty {
            let _ = write!(self.out, " : {ty}");
        }
        if let Some(l) = layout_str(&e.layout) {
            let _ = write!(self.out, " [{l}]");
        }
        let mut flags = Vec::new();
        for (flag, name) in [
            (HirFlags::FRAME_LOCAL, "frame-local"),
            (HirFlags::LAST_USE, "last-use"),
            (HirFlags::IN_BOUNDS, "in-bounds"),
            (HirFlags::NONNEG, "nonneg"),
        ] {
            if e.flags.contains(flag) {
                flags.push(name);
            }
        }
        if !flags.is_empty() {
            let _ = write!(self.out, " {{{}}}", flags.join(", "));
        }
        self.out.push('\n');
    }

    fn pat(&self, pat: &HirPat) -> String {
        match pat {
            HirPat::Wild => "_".to_string(),
            HirPat::Bind(l) => self.local(*l),
            HirPat::Int(n) => n.to_string(),
            HirPat::Variant {
                enum_name,
                variant,
                tag,
                fields,
            } => {
                let tag = tag.map_or_else(String::new, |t| format!("#{t}"));
                let fields = match fields {
                    HirPatFields::Unit => String::new(),
                    HirPatFields::Tuple(items) => format!(
                        "({})",
                        items.iter().map(|p| self.pat(p)).collect::<Vec<_>>().join(", ")
                    ),
                    HirPatFields::Record(items) => format!(
                        " {{ {} }}",
                        items
                            .iter()
                            .map(|(n, p)| format!("{n}: {}", self.pat(p)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                format!("{enum_name}::{variant}{tag}{fields}")
            }
            HirPat::Tuple(items) => format!(
                "({})",
                items.iter().map(|p| self.pat(p)).collect::<Vec<_>>().join(", ")
            ),
            HirPat::Record(items) => format!(
                "{{ {} }}",
                items
                    .iter()
                    .map(|(n, p)| format!("{n}: {}", self.pat(p)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn node(&mut self, id: HirId, depth: usize) {
        let kind = self.body.expr(id).kind.clone();
        let d = depth + 1;
        match kind {
            HirKind::Lit(lit) => {
                let s = match lit {
                    Lit::Int(n) => n.to_string(),
                    Lit::Float(f) => format!("{f:?}"),
                    Lit::Str(s) => format!("{s:?}"),
                    Lit::Bool(v) => v.to_string(),
                    Lit::Unit => "()".to_string(),
                };
                self.line(depth, id, format!("lit {s}"));
            }
            HirKind::Local(l) => {
                let head = format!("local {}", self.local(l));
                self.line(depth, id, head);
            }
            HirKind::Global { name, def } => {
                let def = def.map_or_else(String::new, |d| format!(" {d:?}"));
                self.line(depth, id, format!("global {name}{def}"));
            }
            HirKind::Field { base, name } => {
                self.line(depth, id, format!("field .{name}"));
                self.node(base, d);
            }
            HirKind::Index { base, index, kind } => {
                self.line(depth, id, format!("index {kind:?}"));
                self.node(base, d);
                self.node(index, d);
            }
            HirKind::Bin { op, lhs, rhs } => {
                self.line(depth, id, format!("bin {}", bin_str(op)));
                self.node(lhs, d);
                self.node(rhs, d);
            }
            HirKind::Logic { and, lhs, rhs } => {
                self.line(depth, id, (if and { "and" } else { "or" }).to_string());
                self.node(lhs, d);
                self.node(rhs, d);
            }
            HirKind::Un { op, operand } => {
                self.line(depth, id, format!("un {op:?}"));
                self.node(operand, d);
            }
            HirKind::Cast { value } => {
                self.line(depth, id, "cast".to_string());
                self.node(value, d);
            }
            HirKind::Call { callee, args } => {
                match &callee {
                    Callee::Named { name, overload, .. } => {
                        let ov = overload.map_or_else(String::new, |o| format!(" overload#{o}"));
                        self.line(depth, id, format!("call {name}{ov}"));
                    }
                    Callee::Method { name } => self.line(depth, id, format!("call .{name}")),
                    Callee::Value(f) => {
                        self.line(depth, id, "call value".to_string());
                        self.node(*f, d);
                    }
                }
                for a in args {
                    self.node(a, d);
                }
            }
            HirKind::Named { name, value } => {
                self.line(depth, id, format!("named {name}"));
                self.node(value, d);
            }
            HirKind::Spread(value) => {
                self.line(depth, id, "spread".to_string());
                self.node(value, d);
            }
            HirKind::Make { kind, args } => {
                let head = match kind {
                    MakeKind::Variant {
                        enum_name,
                        variant,
                        tag,
                        fields,
                    } => {
                        let tag = tag.map_or_else(String::new, |t| format!("#{t}"));
                        let fields =
                            fields.map_or_else(String::new, |f| format!(" {{{}}}", f.join(", ")));
                        format!("make {enum_name}::{variant}{tag}{fields}")
                    }
                    MakeKind::Class(name) => format!("make class {name}"),
                    MakeKind::Tuple => "make tuple".to_string(),
                    MakeKind::Array => "make array".to_string(),
                    MakeKind::List => "make list".to_string(),
                    MakeKind::Record(names) => format!("make record {{{}}}", names.join(", ")),
                    MakeKind::Range { inclusive } => {
                        format!("make range{}", if inclusive { " inclusive" } else { "" })
                    }
                };
                self.line(depth, id, head);
                for a in args {
                    self.node(a, d);
                }
            }
            HirKind::Block { stmts, tail } => {
                self.line(depth, id, "block".to_string());
                for s in stmts {
                    self.node(s, d);
                }
                if let Some(t) = tail {
                    for _ in 0..d {
                        self.out.push_str("  ");
                    }
                    self.out.push_str("=>\n");
                    self.node(t, d + 1);
                }
            }
            HirKind::Let { local, init } => {
                let l = self.body.local(local);
                let kw = if l.kind == LocalKind::Const { "const" } else { "let" };
                let ty = l.ty.as_ref().map_or_else(String::new, |t| format!(": {t}"));
                let head = format!("{kw} {}{ty}", self.local(local));
                self.line(depth, id, head);
                if let Some(init) = init {
                    self.node(init, d);
                }
            }
            HirKind::LetPat { pat, init } => {
                let head = format!("let {}", self.pat(&pat));
                self.line(depth, id, head);
                self.node(init, d);
            }
            HirKind::Assign { place, value } => {
                self.line(depth, id, "assign".to_string());
                self.node(place, d);
                self.node(value, d);
            }
            HirKind::Append { base, value } => {
                self.line(depth, id, "append".to_string());
                self.node(base, d);
                self.node(value, d);
            }
            HirKind::If { cond, then, els } => {
                self.line(depth, id, "if".to_string());
                self.node(cond, d);
                self.node(then, d);
                if let Some(e) = els {
                    self.node(e, d);
                }
            }
            HirKind::Loop { body } => {
                self.line(depth, id, "loop".to_string());
                self.node(body, d);
            }
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind,
            } => {
                let kind = kind.map_or_else(|| "?".to_string(), |k| format!("{k:?}"));
                let head = format!("for {} in ({kind})", self.pat(&pat));
                self.line(depth, id, head);
                self.node(iterable, d);
                self.node(body, d);
            }
            HirKind::Break => self.line(depth, id, "break".to_string()),
            HirKind::Continue => self.line(depth, id, "continue".to_string()),
            HirKind::Return(value) => {
                self.line(depth, id, "return".to_string());
                if let Some(v) = value {
                    self.node(v, d);
                }
            }
            HirKind::Match { scrutinee, arms } => {
                self.line(depth, id, "match".to_string());
                self.node(scrutinee, d);
                for arm in arms {
                    for _ in 0..d {
                        self.out.push_str("  ");
                    }
                    let _ = writeln!(self.out, "{} =>", self.pat(&arm.pat));
                    self.node(arm.body, d + 1);
                }
            }
            HirKind::Lambda { body } => {
                let name = self
                    .module
                    .bodies
                    .get(body)
                    .map_or("?", |b| b.name.as_str());
                self.line(depth, id, format!("lambda {name}"));
            }
            HirKind::Yield { value, from } => {
                self.line(depth, id, (if from { "yield from" } else { "yield" }).to_string());
                self.node(value, d);
            }
            HirKind::Resume { handle, value } => {
                self.line(depth, id, "resume".to_string());
                self.node(handle, d);
                if let Some(v) = value {
                    self.node(v, d);
                }
            }
            HirKind::Defer { body, .. } => {
                self.line(depth, id, "defer".to_string());
                self.node(body, d);
            }
            HirKind::Builtin { op, args } => {
                self.line(depth, id, format!("builtin {op:?}"));
                for a in args {
                    self.node(a, d);
                }
            }
            HirKind::Unsupported(what) => {
                self.line(depth, id, format!("UNSUPPORTED {what}"));
            }
        }
    }
}
