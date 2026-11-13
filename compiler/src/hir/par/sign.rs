//! Ranges of `int` values, for the sign rule of a loop's fold
//! ([coil-lang#2423](https://github.com/ardax-corp/coil-lang/issues/2423)).
//!
//! A chunk of a parallel loop folds from the operator's identity, so it
//! cannot see an overflow the running fold only reaches part way through
//! the chunk. A fold whose every term has one sign has nothing to see:
//! the running fold only moves one way, so it leaves the `int` range iff
//! the whole fold does, and the fork-join's own tests find that.
//! [`Signs`] proves the sign.
//!
//! The analysis is an interval evaluation of HIR as built. `int`
//! arithmetic traps on overflow, so an operator's range is the exact
//! arithmetic on its operands' ranges, cut to the `int` range. It reads
//! literals, locals (a `let` nothing writes again holds its initializer's
//! range; a `let` assigned later holds the join of everything written to
//! it, widened to a sign), `byte` values, structural `len()`, the
//! checker's own non-negative proofs ([`HirFlags::NONNEG`]), branch
//! conditions over locals nothing writes (a contract check's too), the
//! module's `const`s, the elements of a `readonly` array literal, and
//! calls of the module's functions: the range of what the body returns
//! for the ranges of the call's arguments. Anything else is the whole
//! range of its type: unknown means not proved. A call of a function
//! already being worked out (a recursive one) is unknown.
//!
//! A sign that depends on values the loop never writes (a capture it
//! scales by, a start read at run time) is proved for narrowed ranges of
//! them, which the fork-join then tests as the loop starts
//! ([`super::loop_par::SignTest`]).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use super::super::{BinOp, BodyKind, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, HirPat, IndexKind, Lit, LocalId, LocalKind, MakeKind, UnOp, classify, cold, query};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty as coil_ty;

/// Every value an `int` expression may take: `lo..=hi`, never empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Interval {
    pub lo: i64,
    pub hi: i64,
}

/// A range, or `None` for an expression that yields no value at all (it
/// always traps or leaves, or nothing reaches it).
pub type Range = Option<Interval>;

/// What is known of a body's locals at one point.
pub type Env = HashMap<LocalId, Range>;

const MIN: i128 = i64::MIN as i128;
const MAX: i128 = i64::MAX as i128;

impl Interval {
    pub const FULL: Self = Self { lo: i64::MIN, hi: i64::MAX };
    pub const BYTE: Self = Self { lo: 0, hi: 255 };
    pub const NONNEG: Self = Self { lo: 0, hi: i64::MAX };

    pub fn point(k: i64) -> Self {
        Self { lo: k, hi: k }
    }

    /// The range of a local or value by its type alone.
    pub fn of_type(ty: Option<&coil_ty::Ty>) -> Self {
        match ty.and_then(classify::primitive) {
            Some(coil_ty::BYTE) => Self::BYTE,
            _ => Self::FULL,
        }
    }

    /// The `int` values of `lo..=hi`: an operator whose exact result is
    /// outside the `int` range traps, and yields no value.
    pub fn cut(lo: i128, hi: i128) -> Range {
        let (lo, hi) = (lo.max(MIN), hi.min(MAX));
        (lo <= hi).then_some(Self { lo: lo as i64, hi: hi as i64 })
    }

    /// The least and greatest of `corners`, cut to the `int` range.
    fn hull(corners: [i128; 4]) -> Range {
        Self::cut(*corners.iter().min().expect("four corners"), *corners.iter().max().expect("four corners"))
    }

    pub fn join(self, other: Self) -> Self {
        Self { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }

    pub fn meet(self, other: Self) -> Range {
        Self::cut(self.lo.max(other.lo) as i128, self.hi.min(other.hi) as i128)
    }

    fn wide(self) -> (i128, i128) {
        (self.lo as i128, self.hi as i128)
    }

    /// The greatest magnitude of a value.
    fn magnitude(self) -> i128 {
        let (lo, hi) = self.wide();
        lo.abs().max(hi.abs())
    }

    fn add(self, o: Self) -> Range {
        Self::cut(self.lo as i128 + o.lo as i128, self.hi as i128 + o.hi as i128)
    }

    fn sub(self, o: Self) -> Range {
        Self::cut(self.lo as i128 - o.hi as i128, self.hi as i128 - o.lo as i128)
    }

    fn mul(self, o: Self) -> Range {
        let ((a, b), (c, d)) = (self.wide(), o.wide());
        Self::hull([a * c, a * d, b * c, b * d])
    }

    /// `x * x` for one `x` of this range.
    fn square(self) -> Range {
        let (a, b) = self.wide();
        let least = if a >= 0 {
            a * a
        } else if b <= 0 {
            b * b
        } else {
            0
        };
        Self::cut(least, (a * a).max(b * b))
    }

    fn neg(self) -> Range {
        Self::cut(-(self.hi as i128), -(self.lo as i128))
    }

    /// `!x` is `-x - 1`.
    fn bit_not(self) -> Range {
        Self::cut(-(self.hi as i128) - 1, -(self.lo as i128) - 1)
    }

    /// Truncating division; a zero divisor traps.
    fn div(self, o: Self) -> Range {
        let ((a, b), (c, d)) = (self.wide(), o.wide());
        if c >= 1 || d <= -1 {
            // One sign of divisor: the quotient is monotone in each operand.
            return Self::hull([a / c, a / d, b / c, b / d]);
        }
        if c == 0 && d == 0 {
            return None;
        }
        // A quotient is no larger than its dividend.
        let m = self.magnitude();
        Self::cut(-m, m)
    }

    /// The remainder takes its dividend's sign and is smaller than the
    /// divisor; a zero divisor traps.
    fn rem(self, o: Self) -> Range {
        let most = o.magnitude();
        if most == 0 {
            return None;
        }
        let m = self.magnitude().min(most - 1);
        Self::cut(if self.lo >= 0 { 0 } else { -m }, if self.hi <= 0 { 0 } else { m })
    }

    /// The least `2^k - 1` at or above non-negative `x`.
    fn mask(x: i64) -> i128 {
        (1i128 << (64 - (x as u64).leading_zeros())) - 1
    }

    fn bit_and(self, o: Self) -> Range {
        match (self.lo >= 0, o.lo >= 0) {
            (true, true) => Self::cut(0, self.hi.min(o.hi) as i128),
            (true, false) => Self::cut(0, self.hi as i128),
            (false, true) => Self::cut(0, o.hi as i128),
            (false, false) if self.hi < 0 && o.hi < 0 => Self::cut(MIN, -1),
            (false, false) => Some(Self::FULL),
        }
    }

    fn bit_or(self, o: Self) -> Range {
        if self.lo >= 0 && o.lo >= 0 {
            return Self::cut(self.lo.max(o.lo) as i128, Self::mask(self.hi.max(o.hi)));
        }
        if self.hi < 0 || o.hi < 0 {
            return Self::cut(MIN, -1);
        }
        Some(Self::FULL)
    }

    fn bit_xor(self, o: Self) -> Range {
        if self.lo >= 0 && o.lo >= 0 {
            return Self::cut(0, Self::mask(self.hi.max(o.hi)));
        }
        if self.hi < 0 && o.hi < 0 {
            return Some(Self::NONNEG);
        }
        if (self.hi < 0 && o.lo >= 0) || (self.lo >= 0 && o.hi < 0) {
            return Self::cut(MIN, -1);
        }
        Some(Self::FULL)
    }

    /// A shift's count is in `0..=63`, or it traps.
    fn count(self) -> Option<(u32, u32)> {
        let c = self.meet(Self { lo: 0, hi: 63 })?;
        Some((c.lo as u32, c.hi as u32))
    }

    /// `x << n` drops the bits shifted out: known only where none are.
    fn shl(self, o: Self) -> Range {
        let (c, d) = o.count()?;
        let (a, b) = self.wide();
        let corners = [a << c, a << d, b << c, b << d];
        if corners.iter().any(|x| !(MIN..=MAX).contains(x)) {
            return Some(Self::FULL);
        }
        Self::hull(corners)
    }

    fn shr(self, o: Self) -> Range {
        let (c, d) = o.count()?;
        let (a, b) = self.wide();
        Self::hull([a >> c, a >> d, b >> c, b >> d])
    }

    /// `x ** n`; a negative exponent traps.
    fn pow(self, o: Self) -> Range {
        let exponent = o.meet(Self::NONNEG)?;
        if self.lo >= 1 {
            Self::cut(1, MAX)
        } else if self.lo >= 0 || (exponent.lo == exponent.hi && exponent.lo % 2 == 0) {
            Some(Self::NONNEG)
        } else {
            Some(Self::FULL)
        }
    }

    /// Widen `self`, which grew from `old`, so a chain of growing ranges
    /// ends: a bound that moved goes to the next of `1`, `0`, `-1` and the
    /// end of the `int` range.
    fn widen(self, old: Self) -> Self {
        let lo = if self.lo < old.lo {
            [1, 0, -1].into_iter().find(|&t| t <= self.lo).unwrap_or(i64::MIN)
        } else {
            self.lo
        };
        let hi = if self.hi > old.hi {
            [-1, 0, 1].into_iter().find(|&t| t >= self.hi).unwrap_or(i64::MAX)
        } else {
            self.hi
        };
        Self { lo, hi }
    }
}

fn join(a: Range, b: Range) -> Range {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.join(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn of_type(ty: Option<&coil_ty::Ty>) -> Interval {
    Interval::of_type(ty)
}

fn is_int(ty: Option<&coil_ty::Ty>) -> bool {
    matches!(ty.and_then(classify::primitive), Some(coil_ty::INT | coil_ty::BYTE))
}

/// The ranges of a module's `int` values.
pub struct Signs<'a> {
    module: &'a HirModule,
    checker: &'a Checker,
    module_path: &'a str,
    /// What each function returns, by name and the ranges of its
    /// arguments.
    results: RefCell<HashMap<(String, Vec<Interval>), Interval>>,
    /// The functions and `const`s being worked out.
    stack: RefCell<Vec<String>>,
    /// Bodies evaluated so far: past [`BUDGET`], a call is unknown.
    evaluated: Cell<usize>,
    /// What each module `const` holds: an `int`'s range, or the range of
    /// the elements of an array literal.
    consts: RefCell<HashMap<String, Range>>,
}

/// Function bodies one module's analysis evaluates.
const BUDGET: usize = 4096;

impl<'a> Signs<'a> {
    pub fn new(module: &'a HirModule, checker: &'a Checker, module_path: &'a str) -> Self {
        Self {
            module,
            checker,
            module_path,
            results: RefCell::new(HashMap::new()),
            stack: RefCell::new(Vec::new()),
            evaluated: Cell::new(0),
            consts: RefCell::new(HashMap::new()),
        }
    }

    /// The range of expression `id` of `body`, where its locals hold what
    /// `env` says, and one not in `env` anything of its type. `id` writes
    /// no local (a loop's term, a bound, an initializer).
    pub fn range(&self, body: &HirBody, id: HirId, env: &Env) -> Range {
        let mut eval = Eval::new(self, body, false);
        eval.run(id, &mut env.clone())
    }

    /// Narrow `env` to where condition `cond` of `body` is `truth`, for
    /// the locals of `env` and any other `int` local: the caller knows
    /// nothing writes them while the fact is used. `false` when no values
    /// satisfy it.
    pub fn assume(&self, body: &HirBody, cond: HirId, truth: bool, env: &mut Env) -> bool {
        let mut eval = Eval::new(self, body, false);
        eval.assume(cond, truth, env)
    }

    /// What a call of the module's function `name` returns when its
    /// arguments are in `args` (one range per parameter).
    pub fn result_of(&self, name: &str, args: &[Interval]) -> Interval {
        let key = (name.to_string(), args.to_vec());
        if let Some(&known) = self.results.borrow().get(&key) {
            return known;
        }
        if self.stack.borrow().iter().any(|n| n == name) || self.evaluated.get() >= BUDGET {
            return Interval::FULL;
        }
        self.evaluated.set(self.evaluated.get() + 1);
        self.stack.borrow_mut().push(name.to_string());
        let result = self.returned(name, args);
        self.stack.borrow_mut().pop();
        self.results.borrow_mut().insert(key, result);
        result
    }

    fn local(&self, body: &HirBody, name: &str) -> bool {
        crate::hir::effects::local_name(self.module_path, &body.name) == Some(name)
    }

    fn returned(&self, name: &str, args: &[Interval]) -> Interval {
        // A function at the top of this module, by its one body: a nested
        // `fn` or an overload is not known by its bare name.
        let mut named = self.module.bodies.iter().filter(|b| b.kind == BodyKind::Function && self.local(b, name));
        let (Some(body), None) = (named.next(), named.next()) else { return Interval::FULL };
        let Some(root) = body.root else { return Interval::FULL };
        let floor = of_type(body.ret.as_ref());
        if body.is_coro || body.result_mode || !is_int(body.ret.as_ref()) {
            return floor;
        }
        let mut eval = Eval::new(self, body, true);
        let mut env = Env::new();
        if args.len() == body.params.len() {
            for (&p, &arg) in body.params.iter().zip(args) {
                if eval.steady(p) {
                    env.insert(p, arg.meet(eval.local_floor(p)));
                }
            }
        }
        // A function that never returns is not one to reason from.
        eval.returns(root, &env).and_then(|r| r.meet(floor)).unwrap_or(floor)
    }

    /// The initializer of module `const` `name`, in the body that holds it.
    fn const_init(&self, name: &str) -> Option<(&'a HirBody, HirId)> {
        // A `static let` shares a `const`'s form here, and may be written.
        if name.contains("::") || self.checker.is_mutable_static(name) {
            return None;
        }
        let top = self.module.bodies.iter().find(|b| b.kind == BodyKind::TopLevel)?;
        let mut found = top.exprs.iter().filter_map(|e| match e.kind {
            HirKind::Let { local, init: Some(init) } if top.local(local).kind == LocalKind::Const && top.local(local).name == name => Some(init),
            _ => None,
        });
        match (found.next(), found.next()) {
            (Some(init), None) => Some((top, init)),
            _ => None,
        }
    }

    /// What module `const` `name` holds, or with `elements`, what the
    /// elements of its array literal hold. A `static let` may be written;
    /// it is unknown.
    fn const_range(&self, name: &str, elements: bool) -> Range {
        let key = format!("{name}{}", if elements { "[]" } else { "" });
        if let Some(&known) = self.consts.borrow().get(&key) {
            return known;
        }
        if self.stack.borrow().contains(&key) {
            return Some(Interval::FULL);
        }
        let Some((top, init)) = self.const_init(name) else { return Some(Interval::FULL) };
        self.stack.borrow_mut().push(key.clone());
        let mut eval = Eval::new(self, top, false);
        let held = if elements { eval.elements(init, &Env::new()) } else { eval.run(init, &mut Env::new()) };
        self.stack.borrow_mut().pop();
        // A `const` set at startup is a slot, which reads as `0` in code
        // that runs ahead of its initializer (another `const`'s, through
        // a function declared above both). Which ones fold instead is
        // not this analysis's to know, so every one may read `0`.
        let held = join(held, Some(Interval::point(0)));
        self.consts.borrow_mut().insert(key, held);
        held
    }
}

/// One body's evaluation.
struct Eval<'a, 'm> {
    signs: &'a Signs<'m>,
    body: &'a HirBody,
    /// The locals something assigns after their `let` (a lambda's writes
    /// of its captures included), when the body's statements are
    /// evaluated. Nothing is learned of one from a `let` or a condition.
    written: HashSet<LocalId>,
    /// Those of `written` that are plain `int` `let`s: each holds the join
    /// of every value written to it, over the whole body.
    tracked: HashMap<LocalId, Range>,
    /// What this pass's writes add to `tracked`.
    writes: HashMap<LocalId, Range>,
    /// What this pass's `return`s yield.
    returned: Range,
}

/// Passes over a body before its written locals are given up as unknown.
const PASSES: usize = 24;

impl<'a, 'm> Eval<'a, 'm> {
    /// `statements`: the whole body is evaluated, so its writes are found
    /// and followed. Otherwise the caller hands over expressions that
    /// write nothing and an `env` that holds while they run.
    fn new(signs: &'a Signs<'m>, body: &'a HirBody, statements: bool) -> Self {
        let mut written = HashSet::new();
        let mut tracked = HashMap::new();
        if statements && let Some(root) = body.root {
            super::loop_par::assigned_in(signs.module, body, root, &mut written);
            for &l in &written {
                let local = body.local(l);
                if local.kind == LocalKind::Let && !local.borrow && !local.captured && is_int(local.ty.as_ref()) {
                    tracked.insert(l, None);
                }
            }
        }
        Self { signs, body, written, tracked, writes: HashMap::new(), returned: None }
    }

    /// The range of everything the body returns: its `return`s, and its
    /// last expression's value.
    fn returns(&mut self, root: HirId, env: &Env) -> Range {
        for pass in 0..PASSES {
            self.writes.clear();
            self.returned = None;
            let value = self.run(root, &mut env.clone());
            let mut grew = false;
            for (l, new) in std::mem::take(&mut self.writes) {
                let old = self.tracked[&l];
                let mut all = join(old, new);
                if let (Some(a), Some(o)) = (all, old)
                    && pass >= 2
                {
                    all = Some(a.widen(o));
                }
                grew |= all != old;
                self.tracked.insert(l, all);
            }
            if !grew {
                // The body's own value, when its type is not `never`.
                let falls_out = self.body.expr(root).ty != Some(coil_ty::never());
                return if falls_out { join(self.returned, value) } else { self.returned };
            }
        }
        Some(Interval::FULL)
    }

    fn local_floor(&self, l: LocalId) -> Interval {
        of_type(self.body.local(l).ty.as_ref())
    }

    fn read(&self, l: LocalId, env: &Env) -> Range {
        if let Some(&r) = self.tracked.get(&l) {
            return r;
        }
        if self.written.contains(&l) {
            return Some(self.local_floor(l));
        }
        match env.get(&l) {
            Some(&r) => r,
            None => Some(self.local_floor(l)),
        }
    }

    /// Whether a fact about `l` may be kept in an `Env`.
    fn steady(&self, l: LocalId) -> bool {
        let local = self.body.local(l);
        !self.written.contains(&l) && !local.borrow && is_int(local.ty.as_ref())
    }

    /// Evaluate `id`: its value's range, with every write and `return`
    /// under it noted, and `env` holding what its `let`s bind.
    fn run(&mut self, id: HirId, env: &mut Env) -> Range {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.run_level(id, env))
    }

    fn run_level(&mut self, id: HirId, env: &mut Env) -> Range {
        let body = self.body;
        let e = body.expr(id);
        let typed = Some(of_type(e.ty.as_ref()));
        let value = match &e.kind {
            HirKind::Lit(Lit::Int(k)) => Some(Interval::point(*k)),
            HirKind::Local(l) => self.read(*l, env),
            HirKind::Un { op, operand } => {
                let v = self.run(*operand, env);
                if !is_int(e.ty.as_ref()) {
                    v.and(typed)
                } else {
                    match op {
                        UnOp::Neg => v.and_then(Interval::neg),
                        UnOp::BitNot => v.and_then(Interval::bit_not),
                        UnOp::Not => v.and(typed),
                    }
                }
            }
            HirKind::Bin { op, lhs, rhs } => {
                let l = self.run(*lhs, env);
                let r = self.run(*rhs, env);
                let same = matches!((&body.expr(*lhs).kind, &body.expr(*rhs).kind), (HirKind::Local(a), HirKind::Local(b)) if a == b);
                match (l, r) {
                    (Some(l), Some(r)) => match op {
                        BinOp::IntAdd => l.add(r),
                        BinOp::IntSub => l.sub(r),
                        BinOp::IntMul if same => l.square(),
                        BinOp::IntMul => l.mul(r),
                        BinOp::IntDiv => l.div(r),
                        BinOp::IntRem => l.rem(r),
                        BinOp::IntPow => l.pow(r),
                        // A bitwise operator on `bool`s has no range.
                        _ if !is_int(e.ty.as_ref()) => Some(Interval::FULL),
                        BinOp::Shl => l.shl(r),
                        BinOp::Shr => l.shr(r),
                        BinOp::BitAnd => l.bit_and(r),
                        BinOp::BitOr => l.bit_or(r),
                        BinOp::BitXor => l.bit_xor(r),
                        _ => Some(Interval::FULL),
                    },
                    _ => None,
                }
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.run(*lhs, env);
                self.run(*rhs, &mut env.clone());
                typed
            }
            HirKind::Cast { value } => {
                let v = self.run(*value, env);
                let from = body.expr(*value).ty.as_ref();
                // A `byte` is its own value as an `int`; anything else is
                // converted.
                match v {
                    Some(v) if is_int(from) && is_int(e.ty.as_ref()) => v.meet(of_type(e.ty.as_ref())).or(typed),
                    Some(_) => typed,
                    None => None,
                }
            }
            HirKind::Call { callee, args } => {
                if let Callee::Value(f) = callee {
                    self.run(*f, env);
                }
                let mut given = Vec::with_capacity(args.len());
                // One range per parameter: an argument written `name: v`
                // or `...v` is not one in its place.
                let mut placed = true;
                for &a in args {
                    placed &= !matches!(body.expr(a).kind, HirKind::Named { .. } | HirKind::Spread(_));
                    given.push(self.run(a, env));
                }
                // An argument with no value: the call is never made.
                let given = given.into_iter().collect::<Option<Vec<Interval>>>()?;
                let known = match callee {
                    Callee::Method { name } if name == "len" && args.len() == 1 && query::structural_len(body, self.signs.checker, args[0]) => {
                        Some(Interval::NONNEG)
                    }
                    Callee::Named { .. } => super::par_profit::bare_callee(self.signs.checker, body, id)
                        .filter(|_| is_int(e.ty.as_ref()))
                        .map(|name| self.signs.result_of(name, if placed { &given } else { &[] })),
                    _ => None,
                };
                known.and_then(|k| k.meet(of_type(e.ty.as_ref()))).or(typed)
            }
            HirKind::Global { name, .. } if is_int(e.ty.as_ref()) => self.signs.const_range(name, false).and_then(|r| r.meet(of_type(e.ty.as_ref()))).or(typed),
            HirKind::Index { base, index, kind: IndexKind::Array } => {
                let b = self.run(*base, env);
                let i = self.run(*index, env);
                // The elements of a `readonly` array literal a `const`
                // names: nothing writes one.
                let readonly = body.expr(*base).ty.as_ref().is_some_and(|t| coil_ty::strip_readonly(t) != t);
                let elements = match &body.expr(*base).kind {
                    _ if !readonly || !is_int(e.ty.as_ref()) => None,
                    HirKind::Global { name, .. } => Some(self.signs.const_range(name, true)),
                    HirKind::Local(l) if body.local(*l).kind == LocalKind::Const => self.let_init(*l).map(|init| self.elements(init, env)),
                    _ => None,
                };
                match (b, i) {
                    (Some(_), Some(_)) => elements.map_or(typed, |r| r.and_then(|r| r.meet(of_type(e.ty.as_ref())))),
                    _ => None,
                }
            }
            HirKind::Block { stmts, tail } => {
                let mut inner = env.clone();
                let mut value = typed;
                for &s in stmts.iter().chain(tail) {
                    value = self.run(s, &mut inner);
                    self.after(s, &mut inner);
                }
                if tail.is_some() { value } else { typed }
            }
            HirKind::Let { local, init } => {
                match init {
                    Some(init) => {
                        let v = self.run(*init, env);
                        self.bind(*local, v, env);
                    }
                    // Declared without a value: whatever its type holds.
                    None => {
                        let floor = Some(self.local_floor(*local));
                        self.bind(*local, floor, env);
                    }
                }
                typed
            }
            HirKind::Assign { place, value } => {
                let v = self.run(*value, env);
                match body.expr(*place).kind {
                    HirKind::Local(l) => {
                        if self.tracked.contains_key(&l) {
                            let noted = self.writes.entry(l).or_insert(None);
                            *noted = join(*noted, v);
                        }
                        typed
                    }
                    _ => {
                        self.run(*place, env);
                        typed
                    }
                }
            }
            HirKind::If { cond, then, els } => {
                self.run(*cond, &mut env.clone());
                let mut on = env.clone();
                let then_value = if self.assume(*cond, true, &mut on) { self.run(*then, &mut on) } else { None };
                match els {
                    Some(els) => {
                        let mut off = env.clone();
                        let else_value = if self.assume(*cond, false, &mut off) { self.run(*els, &mut off) } else { None };
                        join(then_value, else_value)
                    }
                    None => typed,
                }
            }
            HirKind::Match { scrutinee, arms } => {
                let s = self.run(*scrutinee, env);
                let subject = match body.expr(*scrutinee).kind {
                    HirKind::Local(l) if self.steady(l) => Some(l),
                    _ => None,
                };
                let mut value = None;
                for arm in arms {
                    let mut inner = env.clone();
                    match arm.pat {
                        HirPat::Int(k) => {
                            let Some(hit) = s.and_then(|s| s.meet(Interval::point(k))) else { continue };
                            if let Some(l) = subject {
                                inner.insert(l, Some(hit));
                            }
                        }
                        HirPat::Bind(l) if self.steady(l) => {
                            inner.insert(l, s);
                        }
                        _ => {}
                    }
                    value = join(value, self.run(arm.body, &mut inner));
                }
                value
            }
            HirKind::Loop { body: inner } => {
                self.run(*inner, &mut env.clone());
                typed
            }
            HirKind::ForIn { pat, iterable, body: inner, .. } => {
                self.run(*iterable, env);
                let mut scope = env.clone();
                // A range, or a local nothing writes that was bound to one:
                // its bounds are read as they are now, which is what a
                // local nothing writes held then too.
                let range = match body.expr(*iterable).kind {
                    HirKind::Local(r) if !self.written.contains(&r) && !body.local(r).borrow => self.let_init(r).unwrap_or(*iterable),
                    _ => *iterable,
                };
                if let HirPat::Bind(l) = *pat
                    && self.steady(l)
                    && let HirKind::Make { kind: MakeKind::Range { inclusive }, args } = &body.expr(range).kind
                    && let [start, end] = args.as_slice()
                {
                    let first = self.run(*start, &mut env.clone());
                    let last = self.run(*end, &mut env.clone());
                    let over = match (first, last) {
                        (Some(a), Some(b)) => Interval::cut(a.lo as i128, b.hi as i128 - i128::from(!*inclusive)),
                        _ => None,
                    };
                    scope.insert(l, over.and_then(|r| r.meet(self.local_floor(l))));
                }
                self.run(*inner, &mut scope);
                typed
            }
            HirKind::Return(value) => {
                if let Some(v) = value {
                    let r = self.run(*v, env);
                    self.returned = join(self.returned, r);
                }
                None
            }
            HirKind::Break | HirKind::Continue => None,
            // Its body is another body; its writes are in `written`.
            HirKind::Lambda { .. } => typed,
            _ => {
                for k in body.children(id) {
                    self.run(k, &mut env.clone());
                }
                typed
            }
        };
        // The checker's proof that the value is not negative.
        if e.flags.contains(HirFlags::NONNEG) && is_int(e.ty.as_ref()) {
            return value.and_then(|v| v.meet(Interval::NONNEG));
        }
        value
    }

    /// The initializer of the one `let` that binds `local`.
    fn let_init(&self, local: LocalId) -> Option<HirId> {
        let mut found = self.body.exprs.iter().filter_map(|e| match e.kind {
            HirKind::Let { local: l, init } if l == local => Some(init),
            _ => None,
        });
        match (found.next(), found.next()) {
            (Some(init), None) => init,
            _ => None,
        }
    }

    /// The range of the elements of array literal `id`; unknown for any
    /// other expression.
    fn elements(&mut self, id: HirId, env: &Env) -> Range {
        let HirKind::Make { kind: MakeKind::Array, args } = &self.body.expr(id).kind else {
            return Some(Interval::FULL);
        };
        let mut all = None;
        for &a in args {
            if !is_int(self.body.expr(a).ty.as_ref()) || matches!(self.body.expr(a).kind, HirKind::Spread(_)) {
                return Some(Interval::FULL);
            }
            // An element with no value: the literal is never built.
            all = Some(join(all, Some(self.run(a, &mut env.clone())?))?);
        }
        all.or(Some(Interval::FULL))
    }

    /// `let local = v`.
    fn bind(&mut self, local: LocalId, v: Range, env: &mut Env) {
        if self.tracked.contains_key(&local) {
            let noted = self.writes.entry(local).or_insert(None);
            *noted = join(*noted, v);
        } else if self.steady(local) {
            env.insert(local, v.and_then(|v| v.meet(self.local_floor(local))));
        }
    }

    /// What statement `s` leaves known for the statements after it: an
    /// `if` one of whose branches always returns was not taken that way.
    fn after(&mut self, s: HirId, env: &mut Env) {
        let HirKind::If { cond, then, els } = self.body.expr(s).kind else { return };
        // A branch that panics is a contract check, or one written by hand.
        if query::exits(self.body, then) || cold::fails(self.body, then, &|_| false) {
            self.assume(cond, false, env);
        } else if els.is_some_and(|els| query::exits(self.body, els)) {
            self.assume(cond, true, env);
        }
    }

    /// Narrow `env` to where `cond` is `truth`. `false`: nowhere.
    fn assume(&mut self, cond: HirId, truth: bool, env: &mut Env) -> bool {
        let body = self.body;
        match body.expr(cond).kind {
            HirKind::Un { op: UnOp::Not, operand } => self.assume(operand, !truth, env),
            // `a && b` holds, or `a || b` fails: both sides are decided.
            HirKind::Logic { and, lhs, rhs } if and == truth => self.assume(lhs, truth, env) && self.assume(rhs, truth, env),
            HirKind::Bin { op, lhs, rhs } if is_int(body.expr(lhs).ty.as_ref()) && is_int(body.expr(rhs).ty.as_ref()) => {
                let op = match (op, truth) {
                    (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne, true) => op,
                    (BinOp::Lt, false) => BinOp::Ge,
                    (BinOp::Le, false) => BinOp::Gt,
                    (BinOp::Gt, false) => BinOp::Le,
                    (BinOp::Ge, false) => BinOp::Lt,
                    (BinOp::Eq, false) => BinOp::Ne,
                    (BinOp::Ne, false) => BinOp::Eq,
                    _ => return true,
                };
                // The operands write nothing the first evaluation did not note.
                let (Some(l), Some(r)) = (self.run(lhs, &mut env.clone()), self.run(rhs, &mut env.clone())) else {
                    return true;
                };
                let flipped = match op {
                    BinOp::Lt => BinOp::Gt,
                    BinOp::Le => BinOp::Ge,
                    BinOp::Gt => BinOp::Lt,
                    BinOp::Ge => BinOp::Le,
                    same => same,
                };
                self.narrow(lhs, l, op, r, env) && self.narrow(rhs, r, flipped, l, env)
            }
            _ => true,
        }
    }

    /// `x op other` holds for expression `x` of range `now`: narrow `x`
    /// when it is a local a fact may be kept of.
    fn narrow(&self, x: HirId, now: Interval, op: BinOp, other: Interval, env: &mut Env) -> bool {
        let (lo, hi) = other.wide();
        let narrowed = match op {
            BinOp::Lt => now.meet_wide(MIN, hi - 1),
            BinOp::Le => now.meet_wide(MIN, hi),
            BinOp::Gt => now.meet_wide(lo + 1, MAX),
            BinOp::Ge => now.meet_wide(lo, MAX),
            BinOp::Eq => now.meet(other),
            // Only an end of the range can be cut off.
            BinOp::Ne if lo == hi => {
                let (a, b) = now.wide();
                Interval::cut(a + i128::from(a == lo), b - i128::from(b == lo))
            }
            _ => Some(now),
        };
        let Some(narrowed) = narrowed else { return false };
        if let HirKind::Local(l) = self.body.expr(x).kind
            && self.steady(l)
        {
            env.insert(l, Some(narrowed));
        }
        true
    }
}

impl Interval {
    fn meet_wide(self, lo: i128, hi: i128) -> Range {
        Self::cut(lo.max(self.lo as i128), hi.min(self.hi as i128))
    }
}

#[cfg(test)]
#[path = "sign.tests.rs"]
mod tests;
