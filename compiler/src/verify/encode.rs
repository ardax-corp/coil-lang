//! Symbolic execution of a HIR body into SMT-LIB queries.
//!
//! A state maps locals to terms under a guard: the condition for execution
//! to be here. Branches run under `guard && c` / `guard && !c` and join with
//! `ite`; a `return`, `break` or panic ends its path. Every term is bound by
//! a `define-fun`, so a joined value shares its operands instead of copying
//! them.
//!
//! A reachable contract panic is a goal: the query asserts the guard at the
//! panic, so `unsat` proves the check never fails. The function's own
//! `requires` checks end their failing path without a goal, which makes
//! them assumptions for the rest of the body.
//!
//! A loop is cut at its head: the leading checks (`invariant`, `decreases`)
//! are goals on entry, every local the body assigns is replaced by a fresh
//! constant, the checks are re-run as assumptions, and the body runs once
//! from there. Each path back to the head then has to re-establish the
//! checks. A loop with no invariant still works; it only knows less.

use std::collections::{HashMap, HashSet};

use super::{FnCheck, Goal, Options, Param, ParamShape, Query};
use crate::hir::{
    BinOp, BodyKind, Builtin, Callee, HirBody, HirId, HirKind, HirModule, HirPat, HirPatFields, Lit,
    LocalId, MakeKind, Span, UnOp,
};
use crate::typechecking::ty::Ty;

const BV: &str = "(_ BitVec 64)";
const ARR: &str = "(Array (_ BitVec 64) (_ BitVec 64))";
const VIOLATED: &str = "contract violated: ";
/// How deep callee contracts are expanded inside each other.
const MAX_CALL_DEPTH: u32 = 3;

/// The goals of every function in `module` that has a contract or calls one.
pub fn verify_module(module: &HirModule, options: Options) -> Vec<FnCheck> {
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    for (i, b) in module.bodies.iter().enumerate() {
        if eligible(b) {
            by_name.entry(b.name.as_str()).or_insert(i);
        }
    }
    let mut out = Vec::new();
    for body in &module.bodies {
        if !eligible(body) {
            continue;
        }
        let mut enc = Enc::new(module, &by_name, options);
        if let Some(check) = enc.function(body)
            && !check.goals.is_empty()
        {
            out.push(check);
        }
    }
    out
}

fn eligible(b: &HirBody) -> bool {
    matches!(b.kind, BodyKind::Function | BodyKind::Method) && !b.is_generic && !b.is_coro && b.root.is_some()
}

/// A value: a term of the sort its type maps to.
#[derive(Debug, Clone, PartialEq)]
enum V {
    Bv(String),
    Bool(String),
    /// A `Vec<int>`, `Vec<byte>` or string: its length and, when known, its
    /// elements. `id` names the object, so a write through one local is seen
    /// through its aliases.
    Seq { len: String, data: Option<String>, id: u32 },
    Unit,
    Opaque,
}

#[derive(Debug, Clone)]
struct St {
    env: HashMap<LocalId, V>,
    guard: String,
}

type Out = Option<(V, St)>;

#[derive(Default)]
struct LoopCx {
    breaks: Vec<St>,
    conts: Vec<St>,
}

struct Ob {
    keyword: String,
    clause: String,
    callee: Option<String>,
    span: Span,
    guard: String,
    defs: usize,
    exact: bool,
}

/// The body whose locals an expression reads: the function itself, or a
/// callee whose clauses are evaluated at a call.
struct Frame<'m> {
    body: &'m HirBody,
    /// Own body: a contract panic is a goal (or, for `requires`, an
    /// assumption).
    own: bool,
}

struct Enc<'m> {
    module: &'m HirModule,
    by_name: &'m HashMap<&'m str, usize>,
    defs: Vec<String>,
    next: u32,
    obs: Vec<Ob>,
    loops: Vec<LoopCx>,
    /// A fresh value stands for something on the way here.
    exact: bool,
    /// > 0: contract panics record no goal (loop checks re-run as assumptions).
    quiet: u32,
    depth: u32,
    next_seq: u32,
    options: Options,
}

impl<'m> Enc<'m> {
    fn new(module: &'m HirModule, by_name: &'m HashMap<&'m str, usize>, options: Options) -> Self {
        Self {
            options,
            module,
            by_name,
            defs: Vec::new(),
            next: 0,
            obs: Vec::new(),
            loops: Vec::new(),
            exact: true,
            quiet: 0,
            depth: 0,
            next_seq: 0,
        }
    }

    fn function(&mut self, body: &'m HirBody) -> Option<FnCheck> {
        let root = body.root?;
        let mut st = St { env: HashMap::new(), guard: "true".into() };
        let mut params = Vec::new();
        for &p in &body.params {
            let local = body.local(p);
            let name = sanitize(&local.name);
            let v = self.fresh_named(local.ty.as_ref(), &format!("p_{name}"), &mut st);
            let shape = match &v {
                V::Bv(c) | V::Bool(c) => ParamShape::Scalar(c.clone()),
                V::Seq { len, .. } => ParamShape::Seq { len: len.clone() },
                _ => ParamShape::Opaque,
            };
            params.push(Param { name: local.name.clone(), shape });
            st.env.insert(p, v);
        }
        // A model only ever shows the parameters, so it is exact only while
        // nothing else was invented.
        self.exact = true;
        let frame = Frame { body, own: true };
        let _ = self.exec(&frame, root, st);
        Some(FnCheck { name: body.name.clone(), span: body.span, params, goals: self.goals(body) })
    }

    fn goals(&mut self, body: &HirBody) -> Vec<Goal> {
        let mut goals: Vec<Goal> = Vec::new();
        let mut index: HashMap<(Span, String, Option<String>), usize> = HashMap::new();
        let shown: Vec<String> = body
            .params
            .iter()
            .filter_map(|&p| {
                let name = sanitize(&body.local(p).name);
                self.defs
                    .iter()
                    .find_map(|d| {
                        let c = format!("p_{name}");
                        let len = format!("p_{name}_len");
                        if d.starts_with(&format!("(declare-const {c} ")) {
                            Some(c)
                        } else if d.starts_with(&format!("(declare-const {len} ")) {
                            Some(len)
                        } else {
                            None
                        }
                    })
            })
            .collect();
        for ob in &self.obs {
            let mut smt = String::from("(set-option :produce-models true)\n(set-logic ALL)\n");
            for d in &self.defs[..ob.defs] {
                smt.push_str(d);
                smt.push('\n');
            }
            smt.push_str(&format!("(assert {})\n(check-sat)\n", ob.guard));
            if !shown.is_empty() {
                smt.push_str(&format!("(get-value ({}))\n", shown.join(" ")));
            }
            let key = (ob.span, ob.clause.clone(), ob.callee.clone());
            let i = *index.entry(key).or_insert_with(|| {
                goals.push(Goal {
                    keyword: ob.keyword.clone(),
                    clause: ob.clause.clone(),
                    callee: ob.callee.clone(),
                    span: ob.span,
                    queries: Vec::new(),
                });
                goals.len() - 1
            });
            goals[i].queries.push(Query { smt, exact: ob.exact });
        }
        goals
    }

    // ---- terms ----

    fn def(&mut self, sort: &str, term: String) -> String {
        let name = format!("t{}", self.next);
        self.next += 1;
        self.defs.push(format!("(define-fun {name} () {sort} {term})"));
        name
    }

    fn declare(&mut self, sort: &str, hint: &str) -> String {
        let name = format!("{hint}{}", self.next);
        self.next += 1;
        self.defs.push(format!("(declare-const {name} {sort})"));
        name
    }

    fn and(&mut self, a: &str, b: &str) -> String {
        if a == "true" {
            return b.to_string();
        }
        if b == "true" {
            return a.to_string();
        }
        self.def("Bool", format!("(and {a} {b})"))
    }

    fn not(&mut self, a: &str) -> String {
        match a {
            "true" => "false".into(),
            "false" => "true".into(),
            _ => self.def("Bool", format!("(not {a})")),
        }
    }

    fn assume(&mut self, st: &mut St, c: &str) {
        st.guard = self.and(&st.guard, c);
    }

    /// A fresh value of `ty`; its type facts (a byte is below 256, a length
    /// is not negative) join the guard.
    fn fresh(&mut self, ty: Option<&Ty>, st: &mut St) -> V {
        self.fresh_named(ty, "v", st)
    }

    fn fresh_named(&mut self, ty: Option<&Ty>, hint: &str, st: &mut St) -> V {
        let named = hint.starts_with("p_");
        let decl = |s: &mut Self, sort: &str, suffix: &str| {
            if named {
                let name = format!("{hint}{suffix}");
                s.defs.push(format!("(declare-const {name} {sort})"));
                name
            } else {
                s.declare(sort, hint)
            }
        };
        match ty.map(shape) {
            Some(Shape::Int) => V::Bv(decl(self, BV, "")),
            Some(Shape::Byte) => {
                let c = decl(self, BV, "");
                let fact = self.def("Bool", format!("(bvule {c} #x00000000000000ff)"));
                self.assume(st, &fact);
                V::Bv(c)
            }
            Some(Shape::Bool) => V::Bool(decl(self, "Bool", "")),
            Some(Shape::Seq) => {
                let len = decl(self, BV, "_len");
                let data = decl(self, ARR, "_data");
                // No sequence comes near 2^48 items.
                let fact = self.def("Bool", format!("(and (bvsge {len} #x0000000000000000) (bvslt {len} #x0001000000000000))"));
                self.assume(st, &fact);
                self.next_seq += 1;
                V::Seq { len, data: Some(data), id: self.next_seq }
            }
            Some(Shape::Unit) => V::Unit,
            _ => V::Opaque,
        }
    }

    /// `fresh`, and the path now depends on a value the encoder made up.
    fn invent(&mut self, ty: Option<&Ty>, st: &mut St) -> V {
        self.exact = false;
        self.fresh(ty, st)
    }

    fn bool_of(&mut self, v: &V, st: &mut St) -> String {
        match v {
            V::Bool(t) => t.clone(),
            _ => {
                self.exact = false;
                let _ = st;
                self.declare("Bool", "b")
            }
        }
    }

    // ---- joins ----

    fn join(&mut self, a: Out, b: Out) -> Out {
        let (va, sa) = match a {
            None => return b,
            Some(x) => x,
        };
        let (vb, sb) = match b {
            None => return Some((va, sa)),
            Some(x) => x,
        };
        let c = sa.guard.clone();
        let guard = if sb.guard == "true" || sa.guard == "true" {
            "true".to_string()
        } else {
            self.def("Bool", format!("(or {} {})", sa.guard, sb.guard))
        };
        let mut env = HashMap::new();
        for (k, x) in &sa.env {
            match sb.env.get(k) {
                Some(y) => {
                    let m = self.merge(&c, x, y);
                    env.insert(*k, m);
                }
                None => {
                    env.insert(*k, x.clone());
                }
            }
        }
        for (k, y) in &sb.env {
            env.entry(*k).or_insert_with(|| y.clone());
        }
        let v = self.merge(&c, &va, &vb);
        Some((v, St { env, guard }))
    }

    fn merge(&mut self, c: &str, a: &V, b: &V) -> V {
        if a == b {
            return a.clone();
        }
        match (a, b) {
            (V::Bv(x), V::Bv(y)) => V::Bv(self.def(BV, format!("(ite {c} {x} {y})"))),
            (V::Bool(x), V::Bool(y)) => V::Bool(self.def("Bool", format!("(ite {c} {x} {y})"))),
            (V::Seq { len: l1, data: d1, id: i1 }, V::Seq { len: l2, data: d2, id: i2 }) => {
                let len = if l1 == l2 { l1.clone() } else { self.def(BV, format!("(ite {c} {l1} {l2})")) };
                let data = match (d1, d2) {
                    (Some(x), Some(y)) if x == y => Some(x.clone()),
                    (Some(x), Some(y)) => Some(self.def(ARR, format!("(ite {c} {x} {y})"))),
                    _ => None,
                };
                let id = if i1 == i2 {
                    *i1
                } else {
                    self.next_seq += 1;
                    self.next_seq
                };
                V::Seq { len, data, id }
            }
            (V::Unit, V::Unit) => V::Unit,
            _ => V::Opaque,
        }
    }

    // ---- goals ----

    fn goal(&mut self, keyword: &str, clause: String, callee: Option<String>, span: Span, guard: &str) {
        if self.quiet > 0 || guard == "false" {
            return;
        }
        self.obs.push(Ob {
            keyword: keyword.to_string(),
            clause,
            callee,
            span,
            guard: guard.to_string(),
            defs: self.defs.len(),
            exact: self.exact,
        });
    }

    // ---- execution ----

    fn exec(&mut self, f: &Frame<'m>, id: HirId, mut st: St) -> Out {
        let b = f.body;
        let e = b.expr(id);
        let ty = e.ty.as_ref();
        match &e.kind {
            HirKind::Lit(lit) => Some((
                match lit {
                    Lit::Int(i) => V::Bv(bv_lit(*i)),
                    Lit::Bool(x) => V::Bool(x.to_string()),
                    Lit::Unit => V::Unit,
                    Lit::Str(s) => {
                        self.next_seq += 1;
                        V::Seq { len: bv_lit(s.len() as i64), data: None, id: self.next_seq }
                    }
                    Lit::Float(_) => V::Opaque,
                },
                st,
            )),
            HirKind::Local(l) => {
                let v = match st.env.get(l) {
                    Some(v) => v.clone(),
                    None => self.invent(b.local(*l).ty.as_ref(), &mut st),
                };
                Some((v, st))
            }
            HirKind::Global { .. } | HirKind::Lambda { .. } | HirKind::Unsupported(_) => {
                let v = self.invent(ty, &mut st);
                Some((v, st))
            }
            HirKind::Field { base, .. } => {
                let (_, mut st) = self.exec(f, *base, st)?;
                let v = self.invent(ty, &mut st);
                Some((v, st))
            }
            HirKind::Index { base, index, .. } => {
                let (bv, st) = self.exec(f, *base, st)?;
                let (iv, mut st) = self.exec(f, *index, st)?;
                self.index(&bv, &iv, ty, &mut st).map(|v| (v, st))
            }
            HirKind::Bin { op, lhs, rhs } => {
                let (a, st) = self.exec(f, *lhs, st)?;
                let (c, mut st) = self.exec(f, *rhs, st)?;
                self.bin(*op, &a, &c, ty, &mut st).map(|v| (v, st))
            }
            HirKind::Logic { and, lhs, rhs } => {
                let (l, mut st) = self.exec(f, *lhs, st)?;
                let c = self.bool_of(&l, &mut st);
                let nc = self.not(&c);
                let (go, stop, short) = if *and { (c, nc, "false") } else { (nc, c, "true") };
                let mut on = st.clone();
                self.assume(&mut on, &go);
                let mut off = st;
                self.assume(&mut off, &stop);
                let a = self.exec(f, *rhs, on).map(|(v, mut s)| {
                    let t = self.bool_of(&v, &mut s);
                    (V::Bool(t), s)
                });
                self.join(a, Some((V::Bool(short.into()), off)))
            }
            HirKind::Un { op, operand } => {
                let (v, mut st) = self.exec(f, *operand, st)?;
                let r = match (op, &v) {
                    (UnOp::Not, V::Bool(t)) => V::Bool(self.not(t)),
                    (UnOp::Neg, V::Bv(t)) => {
                        if self.options.overflow_traps {
                            let fits = self.def("Bool", format!("(distinct {t} #x8000000000000000)"));
                            self.assume(&mut st, &fits);
                        }
                        V::Bv(self.def(BV, format!("(bvneg {t})")))
                    }
                    (UnOp::BitNot, V::Bv(t)) => V::Bv(self.def(BV, format!("(bvnot {t})"))),
                    _ => self.invent(ty, &mut st),
                };
                Some((r, st))
            }
            HirKind::Cast { value } => {
                let (v, mut st) = self.exec(f, *value, st)?;
                let from = b.expr(*value).ty.as_ref().map(shape);
                let r = match (from, ty.map(shape), &v) {
                    (Some(Shape::Byte), Some(Shape::Int), V::Bv(_)) => v.clone(),
                    (Some(Shape::Int | Shape::Byte), Some(Shape::Byte), V::Bv(_)) | (Some(Shape::Int), Some(Shape::Int), V::Bv(_)) => {
                        // `int as byte` keeps the low byte.
                        if matches!(ty.map(shape), Some(Shape::Byte)) {
                            let V::Bv(t) = &v else { unreachable!() };
                            V::Bv(self.def(BV, format!("(bvand {t} #x00000000000000ff)")))
                        } else {
                            v.clone()
                        }
                    }
                    _ => self.invent(ty, &mut st),
                };
                Some((r, st))
            }
            HirKind::Call { callee, args } => self.call(f, id, callee, args, st),
            HirKind::Named { value, .. } => self.exec(f, *value, st),
            HirKind::Spread(value) => {
                let (_, mut st) = self.exec(f, *value, st)?;
                let v = self.invent(ty, &mut st);
                Some((v, st))
            }
            HirKind::Make { kind, args } => {
                let mut vals = Vec::new();
                for &a in args {
                    let (v, s) = self.exec(f, a, st)?;
                    st = s;
                    vals.push(v);
                }
                let v = match kind {
                    MakeKind::Array | MakeKind::List if matches!(ty.map(shape), Some(Shape::Seq)) => {
                        let mut data = self.declare(ARR, "a");
                        for (i, v) in vals.iter().enumerate() {
                            match v {
                                V::Bv(t) => data = self.def(ARR, format!("(store {data} {} {t})", bv_lit(i as i64))),
                                _ => self.exact = false,
                            }
                        }
                        self.next_seq += 1;
                        V::Seq { len: bv_lit(vals.len() as i64), data: Some(data), id: self.next_seq }
                    }
                    _ => self.invent(ty, &mut st),
                };
                Some((v, st))
            }
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    let (_, next) = self.exec(f, s, st)?;
                    st = next;
                }
                match tail {
                    Some(t) => self.exec(f, *t, st),
                    None => Some((V::Unit, st)),
                }
            }
            HirKind::Let { local, init } => {
                let v = match init {
                    Some(i) => {
                        let (v, s) = self.exec(f, *i, st)?;
                        st = s;
                        v
                    }
                    None => self.fresh(b.local(*local).ty.as_ref(), &mut st),
                };
                st.env.insert(*local, v);
                Some((V::Unit, st))
            }
            HirKind::LetPat { pat, init } => {
                let (_, mut st) = self.exec(f, *init, st)?;
                self.bind_fresh(b, pat, &mut st);
                Some((V::Unit, st))
            }
            HirKind::Assign { place, value } => self.assign(f, id, *place, *value, st),
            HirKind::Append { base, value } => {
                let (bv, st) = self.exec(f, *base, st)?;
                let (v, mut st) = self.exec(f, *value, st)?;
                self.push(&bv, &v, &mut st);
                Some((V::Unit, st))
            }
            HirKind::If { cond, then, els } => {
                let (c, mut st) = self.exec(f, *cond, st)?;
                let c = self.bool_of(&c, &mut st);
                let nc = self.not(&c);
                let mut on = st.clone();
                self.assume(&mut on, &c);
                let mut off = st;
                self.assume(&mut off, &nc);
                let a = if on.guard == "false" { None } else { self.exec(f, *then, on) };
                let z = match els {
                    _ if off.guard == "false" => None,
                    Some(e) => self.exec(f, *e, off),
                    None => Some((V::Unit, off)),
                };
                self.join(a, z)
            }
            HirKind::Loop { body } => self.loop_(f, *body, None, st),
            HirKind::ForIn { pat, iterable, body, .. } => self.for_in(f, pat, *iterable, *body, st),
            HirKind::Break => {
                if let Some(cx) = self.loops.last_mut() {
                    cx.breaks.push(st);
                }
                None
            }
            HirKind::Continue => {
                if let Some(cx) = self.loops.last_mut() {
                    cx.conts.push(st);
                }
                None
            }
            HirKind::Return(value) => {
                if let Some(v) = value {
                    self.exec(f, *v, st)?;
                }
                None
            }
            HirKind::Match { scrutinee, arms } => {
                let (sv, st) = self.exec(f, *scrutinee, st)?;
                let mut rest = st;
                let mut out: Out = None;
                for arm in arms {
                    if rest.guard == "false" {
                        break;
                    }
                    let mut taken = rest.clone();
                    let c = self.pattern(b, &arm.pat, &sv, &mut taken);
                    let nc = self.not(&c);
                    self.assume(&mut taken, &c);
                    self.assume(&mut rest, &nc);
                    if taken.guard != "false" {
                        let r = self.exec(f, arm.body, taken);
                        out = self.join(out, r);
                    }
                }
                out
            }
            HirKind::Yield { .. } | HirKind::Resume { .. } => {
                let v = self.invent(ty, &mut st);
                Some((v, st))
            }
            HirKind::Defer { .. } => {
                // Runs at exit, after the `ensures` checks.
                self.exact = false;
                Some((V::Unit, st))
            }
            HirKind::Builtin { op: Builtin::Panic, args } => {
                self.panic(f, id, args, &st);
                None
            }
            HirKind::Builtin { args, .. } => {
                for &a in args {
                    let (_, s) = self.exec(f, a, st)?;
                    st = s;
                }
                let v = self.invent(ty, &mut st);
                Some((v, st))
            }
            HirKind::Clear(_) => Some((V::Unit, st)),
        }
    }

    fn panic(&mut self, f: &Frame<'m>, id: HirId, args: &[HirId], st: &St) {
        let b = f.body;
        let Some(HirKind::Lit(Lit::Str(msg))) = args.first().map(|&a| &b.expr(a).kind) else { return };
        let Some(text) = msg.strip_prefix(VIOLATED) else { return };
        let blames_caller = args.len() == 2;
        if !f.own || blames_caller {
            // The function's own `requires`: an assumption.
            return;
        }
        let clause = strip_in(text, &b.name);
        let keyword = clause.split_whitespace().next().unwrap_or("").to_string();
        let span = b.expr(id).span;
        self.goal(&keyword, clause, None, span, &st.guard);
    }

    fn index(&mut self, base: &V, index: &V, ty: Option<&Ty>, st: &mut St) -> Option<V> {
        match (base, index) {
            (V::Seq { len, data, .. }, V::Bv(i)) => {
                // An index outside `0..len` panics, so a path that goes on
                // has it inside.
                let ok = self.def("Bool", format!("(and (bvsge {i} #x0000000000000000) (bvslt {i} {len}))"));
                self.assume(st, &ok);
                if st.guard == "false" {
                    return None;
                }
                Some(match data {
                    Some(d) => V::Bv(self.def(BV, format!("(select {d} {i})"))),
                    None => self.invent(ty, st),
                })
            }
            _ => Some(self.invent(ty, st)),
        }
    }

    fn bin(&mut self, op: BinOp, a: &V, c: &V, ty: Option<&Ty>, st: &mut St) -> Option<V> {
        let byte = matches!(ty.map(shape), Some(Shape::Byte));
        let v = match (a, c) {
            (V::Bv(x), V::Bv(y)) if !byte => {
                let arith = |s: &mut Self, f: &str| V::Bv(s.def(BV, format!("({f} {x} {y})")));
                let cmp = |s: &mut Self, f: &str| V::Bool(s.def("Bool", format!("({f} {x} {y})")));
                if self.options.overflow_traps && matches!(op, BinOp::IntAdd | BinOp::IntSub | BinOp::IntMul) {
                    // The result fits: the exact value, computed wider, equals it.
                    let (w, f) = match op {
                        BinOp::IntAdd => (1, "bvadd"),
                        BinOp::IntSub => (1, "bvsub"),
                        _ => (64, "bvmul"),
                    };
                    let fits = self.def(
                        "Bool",
                        format!("(= ({f} ((_ sign_extend {w}) {x}) ((_ sign_extend {w}) {y})) ((_ sign_extend {w}) ({f} {x} {y})))"),
                    );
                    self.assume(st, &fits);
                    if st.guard == "false" {
                        return None;
                    }
                }
                match op {
                    BinOp::IntAdd => arith(self, "bvadd"),
                    BinOp::IntSub => arith(self, "bvsub"),
                    BinOp::IntMul => arith(self, "bvmul"),
                    BinOp::IntDiv | BinOp::IntRem => {
                        // Division by zero and `MIN / -1` panic.
                        let ok = self.def(
                            "Bool",
                            format!(
                                "(and (not (= {y} #x0000000000000000)) (not (and (= {x} #x8000000000000000) (= {y} #xffffffffffffffff))))"
                            ),
                        );
                        self.assume(st, &ok);
                        if st.guard == "false" {
                            return None;
                        }
                        arith(self, if op == BinOp::IntDiv { "bvsdiv" } else { "bvsrem" })
                    }
                    BinOp::BitAnd => arith(self, "bvand"),
                    BinOp::BitOr => arith(self, "bvor"),
                    BinOp::BitXor => arith(self, "bvxor"),
                    BinOp::Lt => cmp(self, "bvslt"),
                    BinOp::Le => cmp(self, "bvsle"),
                    BinOp::Gt => cmp(self, "bvsgt"),
                    BinOp::Ge => cmp(self, "bvsge"),
                    BinOp::Eq => cmp(self, "="),
                    BinOp::Ne => cmp(self, "distinct"),
                    _ => self.invent(ty, st),
                }
            }
            (V::Bv(x), V::Bv(y)) if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne) => {
                let f = match op {
                    BinOp::Lt => "bvslt",
                    BinOp::Le => "bvsle",
                    BinOp::Gt => "bvsgt",
                    BinOp::Ge => "bvsge",
                    BinOp::Eq => "=",
                    _ => "distinct",
                };
                V::Bool(self.def("Bool", format!("({f} {x} {y})")))
            }
            (V::Bool(x), V::Bool(y)) => match op {
                BinOp::Eq => V::Bool(self.def("Bool", format!("(= {x} {y})"))),
                BinOp::Ne | BinOp::BitXor => V::Bool(self.def("Bool", format!("(distinct {x} {y})"))),
                BinOp::BitAnd => V::Bool(self.def("Bool", format!("(and {x} {y})"))),
                BinOp::BitOr => V::Bool(self.def("Bool", format!("(or {x} {y})"))),
                _ => self.invent(ty, st),
            },
            (V::Seq { len: l1, .. }, V::Seq { len: l2, .. }) if op == BinOp::StrConcat => {
                let len = self.def(BV, format!("(bvadd {l1} {l2})"));
                self.next_seq += 1;
                V::Seq { len, data: None, id: self.next_seq }
            }
            _ => self.invent(ty, st),
        };
        Some(v)
    }

    /// The condition for `pat` to match `v`; binds the pattern's locals.
    fn pattern(&mut self, b: &HirBody, pat: &HirPat, v: &V, st: &mut St) -> String {
        match (pat, v) {
            (HirPat::Wild, _) => "true".into(),
            (HirPat::Bind(l), _) => {
                st.env.insert(*l, v.clone());
                "true".into()
            }
            (HirPat::Int(k), V::Bv(t)) => self.def("Bool", format!("(= {t} {})", bv_lit(*k))),
            _ => {
                self.bind_fresh(b, pat, st);
                self.exact = false;
                self.declare("Bool", "m")
            }
        }
    }

    fn bind_fresh(&mut self, b: &HirBody, pat: &HirPat, st: &mut St) {
        let mut locals = Vec::new();
        pat_locals(pat, &mut locals);
        for l in locals {
            let v = self.invent(b.local(l).ty.as_ref(), st);
            st.env.insert(l, v);
        }
    }

    fn assign(&mut self, f: &Frame<'m>, id: HirId, place: HirId, value: HirId, st: St) -> Out {
        let b = f.body;
        let flags = b.expr(id).flags;
        match &b.expr(place).kind {
            HirKind::Local(l) => {
                let old = st.env.get(l).cloned();
                let (v, mut st) = self.exec(f, value, st)?;
                st.env.insert(*l, v.clone());
                let shown = if flags.contains(crate::hir::HirFlags::ADJUST) && !flags.contains(crate::hir::HirFlags::PREFIX) {
                    old.unwrap_or(V::Opaque)
                } else {
                    v
                };
                Some((shown, st))
            }
            HirKind::Index { base, index, .. } => {
                let (bv, st) = self.exec(f, *base, st)?;
                let (iv, st) = self.exec(f, *index, st)?;
                let (v, mut st) = self.exec(f, value, st)?;
                self.index(&bv, &iv, None, &mut st)?;
                match (&bv, &iv, &v) {
                    (V::Seq { len, data: Some(d), id }, V::Bv(i), V::Bv(x)) => {
                        let data = self.def(ARR, format!("(store {d} {i} {x})"));
                        let new = V::Seq { len: len.clone(), data: Some(data), id: *id };
                        self.write_seq(*id, new, &mut st);
                    }
                    _ => self.havoc_seqs(&mut st),
                }
                Some((v, st))
            }
            _ => {
                let (_, st) = self.exec(f, place, st)?;
                let (v, st) = self.exec(f, value, st)?;
                self.exact = false;
                Some((v, st))
            }
        }
    }

    fn push(&mut self, base: &V, v: &V, st: &mut St) {
        match (base, v) {
            (V::Seq { len, data, id }, x) => {
                let data = match (data, x) {
                    (Some(d), V::Bv(x)) => Some(self.def(ARR, format!("(store {d} {len} {x})"))),
                    _ => None,
                };
                let len = self.def(BV, format!("(bvadd {len} #x0000000000000001)"));
                let new = V::Seq { len, data, id: *id };
                self.write_seq(*id, new, st);
            }
            _ => self.havoc_seqs(st),
        }
    }

    /// The object `id` now holds `new`: every local naming it sees the
    /// write; any other sequence may be the same object under another
    /// name, so it forgets what it held.
    fn write_seq(&mut self, id: u32, new: V, st: &mut St) {
        let seqs: Vec<(LocalId, u32)> = st
            .env
            .iter()
            .filter_map(|(k, v)| match v {
                V::Seq { id, .. } => Some((*k, *id)),
                _ => None,
            })
            .collect();
        let mut fresh: HashMap<u32, V> = HashMap::new();
        for (k, other) in seqs {
            let v = if other == id {
                new.clone()
            } else if let Some(v) = fresh.get(&other) {
                v.clone()
            } else {
                let v = self.fresh_seq(other, st);
                fresh.insert(other, v.clone());
                v
            };
            st.env.insert(k, v);
        }
    }

    /// A sequence `id` that may now hold anything.
    fn fresh_seq(&mut self, id: u32, st: &mut St) -> V {
        self.exact = false;
        match self.fresh(Some(&Ty::Con(crate::typechecking::ty::STRING.into())), st) {
            V::Seq { len, data, .. } => V::Seq { len, data, id },
            v => v,
        }
    }

    /// Every sequence may have changed.
    fn havoc_seqs(&mut self, st: &mut St) {
        let seqs: Vec<(LocalId, u32)> = st
            .env
            .iter()
            .filter_map(|(k, v)| match v {
                V::Seq { id, .. } => Some((*k, *id)),
                _ => None,
            })
            .collect();
        // One fresh object per id, so aliases stay aliases.
        let mut fresh: HashMap<u32, V> = HashMap::new();
        for (k, id) in seqs {
            let v = match fresh.get(&id) {
                Some(v) => v.clone(),
                None => {
                    let v = self.fresh_seq(id, st);
                    fresh.insert(id, v.clone());
                    v
                }
            };
            st.env.insert(k, v);
        }
    }

    fn call(&mut self, f: &Frame<'m>, id: HirId, callee: &Callee, args: &[HirId], mut st: St) -> Out {
        let b = f.body;
        let ty = b.expr(id).ty.as_ref();
        let mut vals = Vec::new();
        for &a in args {
            let (v, s) = self.exec(f, a, st)?;
            st = s;
            vals.push(v);
        }
        match callee {
            Callee::Named { name, .. } if name == "len" && vals.len() == 1 => {
                if let V::Seq { len, .. } = &vals[0] {
                    return Some((V::Bv(len.clone()), st));
                }
            }
            Callee::Method { name } if name == "len" && vals.len() == 1 => {
                if let V::Seq { len, .. } = &vals[0] {
                    return Some((V::Bv(len.clone()), st));
                }
            }
            Callee::Method { name } if name == "push" && vals.len() == 2 => {
                self.push(&vals[0].clone(), &vals[1].clone(), &mut st);
                return Some((V::Unit, st));
            }
            Callee::Named { name, .. } => {
                if let Some(&i) = self.by_name.get(name.as_str())
                    && self.depth < MAX_CALL_DEPTH
                {
                    let callee = &self.module.bodies[i];
                    return self.modular_call(callee, &vals, b.expr(id).span, st);
                }
            }
            _ => {}
        }
        // Unknown: any sequence it was handed, or any receiver, may change.
        if matches!(callee, Callee::Method { .. } | Callee::Value(_)) || vals.iter().any(|v| matches!(v, V::Seq { .. })) {
            self.havoc_seqs(&mut st);
        }
        let v = self.invent(ty, &mut st);
        Some((v, st))
    }

    /// A call to a function of this module: its `requires` are goals here,
    /// then its `ensures` hold of a fresh result.
    fn modular_call(&mut self, callee: &'m HirBody, args: &[V], span: Span, mut st: St) -> Out {
        let Some(root) = callee.root else { return None };
        let HirKind::Block { stmts, .. } = &callee.expr(root).kind else {
            let v = self.invent(callee.ret.as_ref(), &mut st);
            return Some((v, st));
        };
        self.depth += 1;
        let frame = Frame { body: callee, own: false };
        let mut env = HashMap::new();
        for (&p, v) in callee.params.iter().zip(args) {
            env.insert(p, v.clone());
        }
        let caller_env = std::mem::replace(&mut st.env, env);
        let mut out = Some(st);
        // Entry: `requires` checks, then `old(…)` lets.
        for &s in stmts {
            let Some(mut cur) = out.take() else { break };
            match &callee.expr(s).kind {
                HirKind::If { cond, then, els: None } if is_contract_panic(callee, *then).is_some_and(|(_, blame)| blame) => {
                    let (text, _) = is_contract_panic(callee, *then).unwrap();
                    let HirKind::Un { op: UnOp::Not, operand } = callee.expr(*cond).kind else {
                        out = Some(cur);
                        break;
                    };
                    self.quiet += 1;
                    let r = self.exec(&frame, operand, cur.clone());
                    self.quiet -= 1;
                    let Some((v, after)) = r else {
                        out = None;
                        break;
                    };
                    cur.guard = after.guard;
                    let c = self.bool_of(&v, &mut cur);
                    let fails = self.not(&c);
                    let g = self.and(&cur.guard, &fails);
                    if self.depth == 1 {
                        self.goal("requires", strip_in(&text, &callee.name), Some(callee.name.clone()), span, &g);
                    }
                    self.assume(&mut cur, &c);
                    out = Some(cur);
                }
                HirKind::Let { local, init: Some(init) } if callee.local(*local).name.starts_with("old$") => {
                    self.quiet += 1;
                    let r = self.exec(&frame, *init, cur.clone());
                    self.quiet -= 1;
                    match r {
                        Some((v, after)) => {
                            cur.guard = after.guard;
                            cur.env.insert(*local, v);
                        }
                        None => {
                            let v = self.invent(callee.local(*local).ty.as_ref(), &mut cur);
                            cur.env.insert(*local, v);
                        }
                    }
                    out = Some(cur);
                }
                _ => {
                    out = Some(cur);
                    break;
                }
            }
        }
        let Some(mut st) = out else {
            self.depth -= 1;
            return None;
        };
        // What the callee may write: the sequences it was handed.
        let pure = callee.declared.as_ref().is_some_and(|d| d.names.is_empty());
        if !pure && args.iter().any(|v| matches!(v, V::Seq { .. })) {
            let mut caller = St { env: caller_env, guard: st.guard.clone() };
            self.havoc_seqs(&mut caller);
            // The parameters name the objects as they are after the call.
            for (&p, v) in callee.params.iter().zip(args) {
                if let V::Seq { id, .. } = v
                    && let Some(now) = caller.env.values().find(|x| matches!(x, V::Seq { id: i, .. } if i == id))
                {
                    st.env.insert(p, now.clone());
                }
            }
            st.guard = caller.guard;
            return self.finish_call(callee, &frame, st, caller.env);
        }
        self.finish_call(callee, &frame, st, caller_env)
    }

    fn finish_call(&mut self, callee: &'m HirBody, frame: &Frame<'m>, mut st: St, caller_env: HashMap<LocalId, V>) -> Out {
        self.exact = false;
        let result = self.fresh(callee.ret.as_ref(), &mut st);
        if let Some((result_local, checks)) = ensures_block(callee) {
            st.env.insert(result_local, result.clone());
            for (cond, _) in checks {
                let HirKind::Un { op: UnOp::Not, operand } = callee.expr(cond).kind else { continue };
                self.quiet += 1;
                let r = self.exec(frame, operand, st.clone());
                self.quiet -= 1;
                if let Some((v, after)) = r {
                    st.guard = after.guard;
                    let c = self.bool_of(&v, &mut st);
                    self.assume(&mut st, &c);
                }
            }
        }
        self.depth -= 1;
        st.env = caller_env;
        Some((result, st))
    }

    // ---- loops ----

    /// The leading statements of a loop body that check its clauses.
    fn head_len(b: &HirBody, stmts: &[HirId]) -> usize {
        stmts
            .iter()
            .take_while(|&&s| match &b.expr(s).kind {
                HirKind::If { then, .. } => {
                    is_contract_panic(b, *then).is_some()
                        || matches!(&b.expr(*then).kind, HirKind::Block { stmts, .. } if stmts.iter().any(|&x| matches!(&b.expr(x).kind, HirKind::If { then, .. } if is_contract_panic(b, *then).is_some())))
                }
                HirKind::Let { local, .. } => b.local(*local).name.starts_with("decreases$"),
                HirKind::Assign { place, .. } => {
                    matches!(b.expr(*place).kind, HirKind::Local(l) if b.local(l).name.starts_with("decreases$"))
                }
                _ => false,
            })
            .count()
    }

    fn run_stmts(&mut self, f: &Frame<'m>, stmts: &[HirId], mut st: St) -> Option<St> {
        for &s in stmts {
            st = self.exec(f, s, st)?.1;
        }
        Some(st)
    }

    /// Every local the loop may change gets a fresh value.
    fn cut(&mut self, f: &Frame<'m>, body: HirId, st: &St) -> St {
        let b = f.body;
        let mut assigned = HashSet::new();
        let mut heap = false;
        walk(b, body, &mut |e| match &b.expr(e).kind {
            HirKind::Assign { place, .. } => match b.expr(*place).kind {
                HirKind::Local(l) => {
                    assigned.insert(l);
                }
                _ => heap = true,
            },
            HirKind::Append { .. } | HirKind::Call { .. } => heap = true,
            _ => {}
        });
        let mut out = st.clone();
        for l in assigned {
            if out.env.contains_key(&l) {
                let v = self.invent(b.local(l).ty.as_ref(), &mut out);
                out.env.insert(l, v);
            }
        }
        if heap {
            self.havoc_seqs(&mut out);
        }
        self.exact = false;
        out
    }

    /// `loop { head; rest }`, with `bind` placing the loop variable.
    fn loop_(&mut self, f: &Frame<'m>, body: HirId, bind: Option<&dyn Fn(&mut Self, &mut St)>, st: St) -> Out {
        let b = f.body;
        let (stmts, tail) = match &b.expr(body).kind {
            HirKind::Block { stmts, tail } => (stmts.clone(), *tail),
            _ => (vec![body], None),
        };
        let head = Self::head_len(b, &stmts);
        let (head_stmts, rest) = stmts.split_at(head);
        // The clauses hold on entry.
        let mut entry = st.clone();
        if let Some(bind) = bind {
            bind(self, &mut entry);
        }
        let _ = self.run_stmts(f, head_stmts, entry);
        // An arbitrary iteration where they hold.
        let mut cut = self.cut(f, body, &st);
        let exit_base = cut.clone();
        if let Some(bind) = bind {
            bind(self, &mut cut);
        }
        self.quiet += 1;
        let at_head = self.run_stmts(f, head_stmts, cut);
        self.quiet -= 1;
        let mut exits: Out = None;
        if let Some(at_head) = at_head {
            self.loops.push(LoopCx::default());
            let mut end = self.run_stmts(f, rest, at_head);
            if let Some(t) = tail
                && let Some(s) = end.take()
            {
                end = self.exec(f, t, s).map(|(_, s)| s);
            }
            let cx = self.loops.pop().unwrap_or_default();
            // Each way back to the head re-establishes the clauses.
            for back in end.into_iter().chain(cx.conts) {
                let mut back = back;
                if let Some(bind) = bind {
                    bind(self, &mut back);
                }
                let _ = self.run_stmts(f, head_stmts, back);
            }
            for s in cx.breaks {
                exits = self.join(exits, Some((V::Unit, s)));
            }
        }
        if bind.is_some() {
            // A `for` loop also ends when its items run out: in a state where
            // the clauses hold, as the checks after it then confirm.
            self.quiet += 1;
            let done = self.run_stmts(f, head_stmts, exit_base);
            self.quiet -= 1;
            if let Some(done) = done {
                exits = self.join(exits, Some((V::Unit, done)));
            }
        }
        exits
    }

    fn for_in(&mut self, f: &Frame<'m>, pat: &HirPat, iterable: HirId, body: HirId, st: St) -> Out {
        let b = f.body;
        // What the loop variable ranges over: `lo..hi`, or a sequence's items.
        let (items, st) = match &b.expr(iterable).kind {
            HirKind::Make { kind: MakeKind::Range { inclusive }, args } if args.len() == 2 => {
                let (lo, st) = self.exec(f, args[0], st)?;
                let (hi, st) = self.exec(f, args[1], st)?;
                match (lo, hi) {
                    (V::Bv(lo), V::Bv(hi)) => (Items::Range { lo, hi, inclusive: *inclusive }, st),
                    _ => (Items::Unknown, st),
                }
            }
            _ => {
                let (it, st) = self.exec(f, iterable, st)?;
                match it {
                    V::Seq { len, data: Some(data), .. } => (Items::Seq { len, data }, st),
                    _ => (Items::Unknown, st),
                }
            }
        };
        let pat = pat.clone();
        let bind = move |s: &mut Self, st: &mut St| match (&pat, &items) {
            (HirPat::Bind(l), Items::Range { lo, hi, inclusive }) => {
                let i = s.declare(BV, "i");
                let upper = if *inclusive { "bvsle" } else { "bvslt" };
                let fact = s.def("Bool", format!("(and (bvsle {lo} {i}) ({upper} {i} {hi}))"));
                s.assume(st, &fact);
                st.env.insert(*l, V::Bv(i));
            }
            (HirPat::Bind(l), Items::Seq { len, data }) if matches!(b.local(*l).ty.as_ref().map(shape), Some(Shape::Int | Shape::Byte)) => {
                let i = s.declare(BV, "k");
                let fact = s.def("Bool", format!("(and (bvsle #x0000000000000000 {i}) (bvslt {i} {len}))"));
                s.assume(st, &fact);
                let item = s.def(BV, format!("(select {data} {i})"));
                st.env.insert(*l, V::Bv(item));
            }
            _ => s.bind_fresh(b, &pat, st),
        };
        self.loop_(f, body, Some(&bind), st)
    }
}

/// What a `for` loop iterates.
enum Items {
    Range { lo: String, hi: String, inclusive: bool },
    Seq { len: String, data: String },
    Unknown,
}

// ---- helpers ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Int,
    Byte,
    Bool,
    Seq,
    Unit,
    Other,
}

fn shape(ty: &Ty) -> Shape {
    use crate::typechecking::ty::{BOOL, BYTE, INT, STRING};
    match ty {
        Ty::Con(n) if n == INT => Shape::Int,
        Ty::Con(n) if n == BYTE => Shape::Byte,
        Ty::Con(n) if n == BOOL => Shape::Bool,
        Ty::Con(n) if n == STRING => Shape::Seq,
        Ty::Con(n) if n == "unit" || n == "()" => Shape::Unit,
        Ty::Tuple(t) if t.is_empty() => Shape::Unit,
        Ty::App(head, args) if args.len() == 1 && matches!(head.as_ref(), Ty::Con(n) if n == "Vec" || n.ends_with("::Vec")) => {
            match shape(&args[0]) {
                Shape::Int | Shape::Byte => Shape::Seq,
                _ => Shape::Other,
            }
        }
        Ty::List(e) | Ty::Array { element: e, .. } => match shape(e) {
            Shape::Int | Shape::Byte => Shape::Seq,
            _ => Shape::Other,
        },
        Ty::Readonly(t) => shape(t),
        _ => Shape::Other,
    }
}

fn bv_lit(i: i64) -> String {
    format!("#x{:016x}", i as u64)
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// `ensures x >= 0 in f` → `ensures x >= 0`.
fn strip_in(text: &str, fname: &str) -> String {
    text.strip_suffix(&format!(" in {fname}")).unwrap_or(text).to_string()
}

/// `{ panic "contract violated: …" }`: the text after the prefix and
/// whether the panic blames the caller (`requires`).
fn is_contract_panic(b: &HirBody, then: HirId) -> Option<(String, bool)> {
    let HirKind::Block { stmts, tail: None } = &b.expr(then).kind else { return None };
    let [panic] = stmts.as_slice() else { return None };
    let HirKind::Builtin { op: Builtin::Panic, args } = &b.expr(*panic).kind else { return None };
    let HirKind::Lit(Lit::Str(msg)) = &b.expr(*args.first()?).kind else { return None };
    let text = msg.strip_prefix(VIOLATED)?;
    Some((text.to_string(), args.len() == 2))
}

/// The first `{ let result = v; ensures checks; result }` in `b`: the
/// `result` local and each check's condition.
fn ensures_block(b: &HirBody) -> Option<(LocalId, Vec<(HirId, String)>)> {
    for e in &b.exprs {
        let HirKind::Block { stmts, tail: Some(_) } = &e.kind else { continue };
        let Some((&first, checks)) = stmts.split_first() else { continue };
        let HirKind::Let { local, .. } = b.expr(first).kind else { continue };
        if b.local(local).name != "result" {
            continue;
        }
        let mut out = Vec::new();
        for &c in checks {
            if let HirKind::If { cond, then, els: None } = &b.expr(c).kind
                && let Some((text, false)) = is_contract_panic(b, *then)
            {
                out.push((*cond, text));
            }
        }
        return Some((local, out));
    }
    None
}

fn pat_locals(pat: &HirPat, out: &mut Vec<LocalId>) {
    match pat {
        HirPat::Bind(l) => out.push(*l),
        HirPat::Variant { fields, .. } => match fields {
            HirPatFields::Unit => {}
            HirPatFields::Tuple(ps) => ps.iter().for_each(|p| pat_locals(p, out)),
            HirPatFields::Record(ps) => ps.iter().for_each(|(_, p)| pat_locals(p, out)),
        },
        HirPat::Tuple(ps) => ps.iter().for_each(|p| pat_locals(p, out)),
        HirPat::Record(ps) => ps.iter().for_each(|(_, p)| pat_locals(p, out)),
        HirPat::Wild | HirPat::Int(_) => {}
    }
}

/// Visit `id` and every node under it in this body.
fn walk(b: &HirBody, id: HirId, f: &mut dyn FnMut(HirId)) {
    f(id);
    let mut kids: Vec<HirId> = Vec::new();
    match &b.expr(id).kind {
        HirKind::Field { base, .. } => kids.push(*base),
        HirKind::Index { base, index, .. } => kids.extend([*base, *index]),
        HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => kids.extend([*lhs, *rhs]),
        HirKind::Un { operand, .. } => kids.push(*operand),
        HirKind::Cast { value } | HirKind::Named { value, .. } | HirKind::Spread(value) => kids.push(*value),
        HirKind::Call { callee, args } => {
            if let Callee::Value(v) = callee {
                kids.push(*v);
            }
            kids.extend(args.iter().copied());
        }
        HirKind::Make { args, .. } | HirKind::Builtin { args, .. } => kids.extend(args.iter().copied()),
        HirKind::Block { stmts, tail } => {
            kids.extend(stmts.iter().copied());
            kids.extend(tail.iter().copied());
        }
        HirKind::Let { init, .. } => kids.extend(init.iter().copied()),
        HirKind::LetPat { init, .. } => kids.push(*init),
        HirKind::Assign { place, value } => kids.extend([*place, *value]),
        HirKind::Append { base, value } => kids.extend([*base, *value]),
        HirKind::If { cond, then, els } => {
            kids.extend([*cond, *then]);
            kids.extend(els.iter().copied());
        }
        HirKind::Loop { body } => kids.push(*body),
        HirKind::ForIn { iterable, body, .. } => kids.extend([*iterable, *body]),
        HirKind::Return(v) => kids.extend(v.iter().copied()),
        HirKind::Match { scrutinee, arms } => {
            kids.push(*scrutinee);
            kids.extend(arms.iter().map(|a| a.body));
        }
        HirKind::Yield { value, .. } => kids.push(*value),
        HirKind::Resume { handle, value } => {
            kids.push(*handle);
            kids.extend(value.iter().copied());
        }
        HirKind::Defer { body, .. } => kids.push(*body),
        HirKind::Lit(_)
        | HirKind::Local(_)
        | HirKind::Global { .. }
        | HirKind::Break
        | HirKind::Continue
        | HirKind::Lambda { .. }
        | HirKind::Clear(_)
        | HirKind::Unsupported(_) => {}
    }
    for k in kids {
        walk(b, k, f);
    }
}

#[cfg(test)]
#[path = "encode.tests.rs"]
mod tests;
