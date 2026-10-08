//! Effects over HIR (E1): which bodies are pure, and which of their
//! function-typed parameters they call.
//!
//! The AST walk ([`crate::typechecking::purity`]) keys effects by name and
//! gives any call through a function value unknown effects, so every
//! higher-order function (`map`, `filter`, `fold`) and every caller of one
//! is impure. Here a body's [`Summary`] keeps the effects it has of its own
//! apart from the parameters it calls (`latent`). A call site fills those in
//! from its arguments, so `map(xs, fn (int x) => x * 2)` costs the lambda's
//! effects, which are none: each instance of a generic function gets the
//! effects of the functions it is passed, without a copy per instance.
//!
//! Summaries are solved per module to a fixed point and kept for the whole
//! program in [`ProgramEffects`] (modules compile in dependency order), so
//! a call into an earlier module sees the callee's summary.

use std::collections::{HashMap, HashSet};

use common::EffectFlags;

use super::lower::children;
use super::{BodyKind, Builtin, BinOp, Callee, HirBody, HirId, HirKind, HirModule, LocalId, LocalKind, MakeKind};
use crate::typechecking::def_id::DefId;
use crate::typechecking::infer::{Checker, ForInKind};
use crate::typechecking::purity::classify_host_name;
use crate::typechecking::ty::Ty;

const UNKNOWN: EffectFlags =
    EffectFlags::from_bits(EffectFlags::UNKNOWN | EffectFlags::HOST | EffectFlags::RESIZE);

/// `Vec` methods that change the receiver.
const VEC_MUTATORS: &[&str] = &["push", "pop", "insert", "remove", "clear", "reserve"];

/// What calling a body costs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Effects of the body itself and of every call it makes whose callee
    /// is known.
    pub flags: EffectFlags,
    /// Bit `i`: the body calls its parameter `i` (a function value), so a
    /// call also has the effects of the function passed there.
    pub latent: u64,
}

impl Summary {
    /// Pure whatever it is passed.
    pub fn is_pure(self) -> bool {
        self.flags.is_pure() && self.latent == 0
    }

    fn of(flags: EffectFlags) -> Self {
        Self { flags, latent: 0 }
    }
}

/// Summaries of every body compiled so far, by full body name
/// (`module::f`, `module::Owner::m`) and by [`DefId`] for functions.
#[derive(Debug, Default)]
pub struct ProgramEffects {
    fns: HashMap<DefId, Summary>,
    by_name: HashMap<String, Summary>,
}

impl ProgramEffects {
    /// Keep `module`'s summaries for the modules compiled after it.
    pub fn record(&mut self, module: &HirModule, checker: &Checker, module_path: &str, summaries: &[Summary]) {
        for (body, &s) in module.bodies.iter().zip(summaries) {
            if !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
                continue;
            }
            self.by_name.insert(body.name.clone(), s);
            if body.kind == BodyKind::Function
                && let Some(def) = def_of(checker, module_path, &body.name)
            {
                self.fns.insert(def, s);
            }
        }
    }
}

/// Names of `module`'s pure functions and methods, keyed as
/// [`crate::typechecking::purity::record_fn_effects`] keys them: `f`,
/// `Owner::m` and `module::Owner::m`. A bare `m` is listed when every body
/// of that short name is pure, here or in `known` (the AST walk's set).
pub fn pure_names(module: &HirModule, module_path: &str, summaries: &[Summary], known: &HashSet<String>) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut by_short: HashMap<&str, bool> = HashMap::new();
    for (body, s) in module.bodies.iter().zip(summaries) {
        let Some(local) = local_name(module_path, &body.name) else { continue };
        let (owner, short) = match local.rsplit_once("::") {
            Some((owner, short)) => (Some(owner), short),
            None => (None, local),
        };
        let pure = s.is_pure() || known.contains(local);
        match (body.kind, owner) {
            (BodyKind::Function, None) => {
                if s.is_pure() {
                    out.insert(local.to_string());
                }
            }
            // Trait impls (`Show for P::m`) and nested functions keep the
            // AST walk's verdict.
            (BodyKind::Method, Some(owner)) if !owner.contains(' ') && !owner.contains("::") => {
                if s.is_pure() {
                    out.insert(local.to_string());
                    if !module_path.is_empty() {
                        out.insert(body.name.clone());
                    }
                }
            }
            _ => continue,
        }
        let all = by_short.entry(short).or_insert(true);
        *all &= pure;
    }
    for (short, pure) in by_short {
        if pure {
            out.insert(short.to_string());
        }
    }
    out
}

/// `name` without the module prefix, or `None` when it is not this module's.
fn local_name<'a>(module_path: &str, name: &'a str) -> Option<&'a str> {
    if module_path.is_empty() {
        return Some(name);
    }
    name.strip_prefix(module_path)?.strip_prefix("::")
}

fn def_of(checker: &Checker, module_path: &str, name: &str) -> Option<DefId> {
    let local = local_name(module_path, name)?;
    checker
        .interned_def(module_path, local)
        .or_else(|| (checker.current_module_name() == module_path).then(|| checker.def_id_of(local)).flatten())
}

/// Solve the summary of every body in `module`, in body order.
pub fn analyze(module: &HirModule, checker: &Checker, module_path: &str, program: &ProgramEffects) -> Vec<Summary> {
    let mut by_def = HashMap::new();
    let mut by_name: HashMap<&str, Option<usize>> = HashMap::new();
    for (i, body) in module.bodies.iter().enumerate() {
        if !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
            continue;
        }
        if body.kind == BodyKind::Function
            && let Some(def) = def_of(checker, module_path, &body.name)
        {
            by_def.insert(def, i);
        }
        let mut names = vec![body.name.as_str()];
        if let Some(local) = local_name(module_path, &body.name)
            && local != body.name
        {
            names.push(local);
        }
        for n in names {
            by_name.entry(n).and_modify(|seen| *seen = None).or_insert(Some(i));
        }
    }
    let facts: Vec<BodyFacts> = module.bodies.iter().map(|b| BodyFacts::new(module, b)).collect();
    let cx = Cx {
        module,
        checker,
        module_path,
        program,
        by_def,
        by_name,
        facts,
    };
    let mut out = vec![Summary::default(); module.bodies.len()];
    loop {
        let mut changed = false;
        for i in 0..module.bodies.len() {
            let s = cx.body(i, &out);
            if s != out[i] {
                out[i] = s;
                changed = true;
            }
        }
        if !changed {
            return out;
        }
    }
}

/// Per-body facts that do not change while solving.
struct BodyFacts {
    /// Locals holding an object made in this body that never leaves it but
    /// by `return`: a write through one is invisible to callers.
    private: HashSet<LocalId>,
    /// `let f = <lambda or fn>` never reassigned: a call through `f` is a
    /// call of that function.
    fn_values: HashMap<LocalId, HirId>,
}

impl BodyFacts {
    fn new(module: &HirModule, body: &HirBody) -> Self {
        let mut fresh = HashSet::new();
        let mut fn_values = HashMap::new();
        let mut assigned = HashSet::new();
        for e in &body.exprs {
            match &e.kind {
                HirKind::Let { local, init: Some(init) } if body.local(*local).kind == LocalKind::Let => {
                    match &body.expr(*init).kind {
                        HirKind::Make {
                            kind: MakeKind::Array | MakeKind::List | MakeKind::Tuple | MakeKind::Class(_) | MakeKind::Record(_),
                            ..
                        } => {
                            fresh.insert(*local);
                        }
                        HirKind::Call { callee: Callee::Named { name, .. }, .. } if is_vec_ctor(name) => {
                            fresh.insert(*local);
                        }
                        HirKind::Lambda { .. } | HirKind::Global { .. } => {
                            fn_values.insert(*local, *init);
                        }
                        _ => {}
                    }
                }
                HirKind::Assign { place, .. } => {
                    if let HirKind::Local(l) = body.expr(*place).kind {
                        assigned.insert(l);
                    }
                }
                _ => {}
            }
        }
        fn_values.retain(|l, _| !assigned.contains(l));
        // Where a fresh local may appear: the base of `x.f` / `x[i]`, the
        // receiver of a `Vec` method, and `return x`.
        let mut allowed = HashSet::new();
        let mut escaped = assigned;
        for e in &body.exprs {
            match &e.kind {
                HirKind::Field { base, .. } | HirKind::Index { base, .. } | HirKind::Append { base, .. } => {
                    allowed.insert(*base);
                }
                HirKind::Call { callee: Callee::Method { name }, args }
                    if VEC_MUTATORS.contains(&name.as_str()) || matches!(name.as_str(), "len" | "capacity") =>
                {
                    if let Some(&recv) = args.first()
                        && is_vec(body.expr(recv).ty.as_ref())
                    {
                        allowed.insert(recv);
                    }
                }
                HirKind::Return(Some(v)) => {
                    allowed.insert(*v);
                }
                HirKind::Lambda { body: inner } => {
                    escaped.extend(module.bodies[*inner].captures.iter().map(|(outer, _)| *outer));
                }
                HirKind::Defer { captures, .. } => escaped.extend(captures.iter().flatten()),
                _ => {}
            }
        }
        for (i, e) in body.exprs.iter().enumerate() {
            if let HirKind::Local(l) = e.kind
                && fresh.contains(&l)
                && !allowed.contains(&HirId(i as u32))
            {
                escaped.insert(l);
            }
        }
        fresh.retain(|l| !escaped.contains(l));
        Self { private: fresh, fn_values }
    }
}

fn is_vec_ctor(name: &str) -> bool {
    matches!(name, "Vec::new" | "Vec::with_capacity")
}

fn is_vec(ty: Option<&Ty>) -> bool {
    match ty {
        Some(Ty::Readonly(inner)) => is_vec(Some(inner)),
        Some(Ty::App(head, _)) => matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE),
        _ => false,
    }
}

/// A class name for a method receiver's type.
fn class_of(ty: Option<&Ty>) -> Option<&str> {
    match ty? {
        Ty::Readonly(inner) => class_of(Some(inner)),
        Ty::Con(n) => Some(n),
        Ty::App(head, _) => class_of(Some(head)),
        _ => None,
    }
}

struct Cx<'a> {
    module: &'a HirModule,
    checker: &'a Checker,
    module_path: &'a str,
    program: &'a ProgramEffects,
    by_def: HashMap<DefId, usize>,
    by_name: HashMap<&'a str, Option<usize>>,
    facts: Vec<BodyFacts>,
}

/// One body's walk: the effects so far and the parameters it calls.
struct Walk<'s> {
    index: usize,
    summaries: &'s [Summary],
    out: Summary,
}

impl Cx<'_> {
    fn body(&self, index: usize, summaries: &[Summary]) -> Summary {
        let body = &self.module.bodies[index];
        let mut w = Walk {
            index,
            summaries,
            out: Summary::default(),
        };
        if let Some(root) = body.root {
            self.expr(&mut w, root);
        }
        w.out
    }

    fn add(w: &mut Walk<'_>, bits: u16) {
        w.out.flags.insert(bits);
    }

    fn expr(&self, w: &mut Walk<'_>, id: HirId) {
        let body = &self.module.bodies[w.index];
        let e = body.expr(id);
        match &e.kind {
            HirKind::Call { callee, args } => self.call(w, callee, args),
            HirKind::Assign { place, .. } => match &body.expr(*place).kind {
                HirKind::Field { base, .. } | HirKind::Index { base, .. } if !self.is_private(w, *base) => {
                    Self::add(w, EffectFlags::HEAP_MUT)
                }
                HirKind::Local(l) if body.local(*l).kind == LocalKind::Capture => Self::add(w, EffectFlags::HEAP_MUT),
                HirKind::Global { .. } => Self::add(w, EffectFlags::HEAP_MUT),
                _ => {}
            },
            HirKind::Append { base, .. } if !self.is_private(w, *base) => {
                Self::add(w, EffectFlags::HEAP_MUT | EffectFlags::RESIZE)
            }
            HirKind::Yield { .. } | HirKind::Resume { .. } => Self::add(w, EffectFlags::YIELD | EffectFlags::RESIZE),
            HirKind::ForIn { kind, .. } => {
                if !matches!(
                    kind,
                    Some(ForInKind::Array | ForInKind::Tuple { .. } | ForInKind::Dict | ForInKind::Range { .. })
                ) {
                    Self::add(w, EffectFlags::UNKNOWN | EffectFlags::RESIZE);
                }
            }
            // As the AST walk: the deferred body is still walked.
            HirKind::Defer { .. } => Self::add(w, EffectFlags::UNKNOWN),
            HirKind::Builtin { op, .. } => match op {
                Builtin::Panic => Self::add(w, EffectFlags::UNKNOWN),
                Builtin::Declare | Builtin::Invoke | Builtin::Dload => Self::add(w, EffectFlags::FFI | EffectFlags::RESIZE),
                Builtin::TypeOf | Builtin::Done | Builtin::Readonly | Builtin::Default => {}
            },
            // A trait operator runs user code this pass does not resolve.
            HirKind::Bin { op: BinOp::Overloaded(_), .. } => Self::add(w, EffectFlags::UNKNOWN),
            HirKind::Unsupported(_) => w.out.flags = w.out.flags.union(UNKNOWN),
            _ => {}
        }
        for k in children(body, id) {
            self.expr(w, k);
        }
    }

    fn is_private(&self, w: &Walk<'_>, base: HirId) -> bool {
        let body = &self.module.bodies[w.index];
        matches!(body.expr(base).kind, HirKind::Local(l) if self.facts[w.index].private.contains(&l))
    }

    fn call(&self, w: &mut Walk<'_>, callee: &Callee, args: &[HirId]) {
        let body = &self.module.bodies[w.index];
        match callee {
            // The host table keeps `len` unknown for any receiver; a
            // length read has no effect.
            Callee::Named { name, .. } if name == "len" && args.len() == 1 => {}
            Callee::Named { name, def, .. } => {
                let s = self.named(w, name, *def);
                self.apply(w, s, args);
            }
            Callee::Method { name } => {
                let Some(&recv) = args.first() else { return };
                let recv_ty = body.expr(recv).ty.as_ref();
                if is_vec(recv_ty) {
                    if matches!(name.as_str(), "len" | "capacity") {
                        return;
                    }
                    if VEC_MUTATORS.contains(&name.as_str()) {
                        if !self.is_private(w, recv) {
                            Self::add(w, EffectFlags::HEAP_MUT | EffectFlags::RESIZE);
                        }
                        return;
                    }
                }
                match self.method(w, recv, name) {
                    Some(s) => self.apply(w, s, args),
                    // As the AST walk: an unresolved method is unknown code,
                    // but `len` / `capacity` cannot resize.
                    None if matches!(name.as_str(), "len" | "capacity") => Self::add(w, EffectFlags::UNKNOWN),
                    None => Self::add(w, EffectFlags::UNKNOWN | EffectFlags::RESIZE),
                }
            }
            Callee::Value(f) => {
                if let Some(i) = self.param_index(w.index, *f) {
                    w.out.latent |= 1 << i;
                    return;
                }
                match self.fn_value(w, *f) {
                    Some(s) => self.apply(w, s, args),
                    None => w.out.flags = w.out.flags.union(UNKNOWN),
                }
            }
        }
    }

    /// The parameter index of `id` when it reads one of `index`'s
    /// parameters (below 64).
    fn param_index(&self, index: usize, id: HirId) -> Option<u32> {
        let body = &self.module.bodies[index];
        let HirKind::Local(l) = body.expr(id).kind else { return None };
        let i = body.params.iter().position(|p| *p == l)?;
        (i < 64).then_some(i as u32)
    }

    /// The summary of calling `callee` with `args`, folded into `w`: its
    /// own effects, plus for each parameter it calls, the effects of the
    /// function passed there.
    fn apply(&self, w: &mut Walk<'_>, callee: Summary, args: &[HirId]) {
        w.out.flags = w.out.flags.union(callee.flags);
        if callee.latent == 0 {
            return;
        }
        let body = &self.module.bodies[w.index];
        let positional = args
            .iter()
            .all(|a| !matches!(body.expr(*a).kind, HirKind::Named { .. } | HirKind::Spread(_)));
        for i in 0..64 {
            if callee.latent & (1 << i) == 0 {
                continue;
            }
            let arg = args.get(i).copied().filter(|_| positional);
            let Some(arg) = arg else {
                w.out.flags = w.out.flags.union(UNKNOWN);
                continue;
            };
            if let Some(j) = self.param_index(w.index, arg) {
                w.out.latent |= 1 << j;
                continue;
            }
            match self.fn_value(w, arg) {
                // A function that calls its own function parameters gets
                // arguments this call site does not see.
                Some(s) if s.latent == 0 => w.out.flags = w.out.flags.union(s.flags),
                _ => w.out.flags = w.out.flags.union(UNKNOWN),
            }
        }
    }

    /// The summary of the function value `id` evaluates to, when known.
    fn fn_value(&self, w: &Walk<'_>, id: HirId) -> Option<Summary> {
        let body = &self.module.bodies[w.index];
        match &body.expr(id).kind {
            HirKind::Lambda { body: inner } => Some(w.summaries[*inner]),
            HirKind::Global { name, def } => self.known_named(w, name, *def),
            HirKind::Local(l) => {
                let init = *self.facts[w.index].fn_values.get(l)?;
                self.fn_value(w, init)
            }
            _ => None,
        }
    }

    fn named(&self, w: &Walk<'_>, name: &str, def: Option<DefId>) -> Summary {
        if let Some(s) = self.known_named(w, name, def) {
            return s;
        }
        if is_vec_ctor(name) || name == "Vec::from" {
            return Summary::default();
        }
        if let Some((owner, member)) = name.rsplit_once("::")
            && self.checker.tag_for(owner, member).is_some()
        {
            return Summary::default();
        }
        Summary::of(classify_host_name(name))
    }

    /// A user function's summary: this module's, an earlier module's, or
    /// the AST walk's flags for a module compiled without HIR.
    fn known_named(&self, w: &Walk<'_>, name: &str, def: Option<DefId>) -> Option<Summary> {
        // A `use`d name has no def on the call; the module's def table has it.
        let def = def.or_else(|| {
            (self.checker.current_module_name() == self.module_path)
                .then(|| self.checker.def_id_of(name))
                .flatten()
        });
        if let Some(def) = def {
            if let Some(&i) = self.by_def.get(&def) {
                return Some(w.summaries[i]);
            }
            if let Some(&s) = self.program.fns.get(&def) {
                return Some(s);
            }
        }
        if let Some(Some(i)) = self.by_name.get(name) {
            return Some(w.summaries[*i]);
        }
        // An imported function: its def names the module it is in.
        let interner = self.checker.def_interner();
        let declared = def
            .and_then(|d| interner.info(d))
            .and_then(|info| Some((interner.module_path(info.module)?, info.name.as_str())))
            .map(|(module, short)| if module.is_empty() { short.to_string() } else { format!("{module}::{short}") });
        let qualified = format!("{}::{name}", self.module_path);
        for key in [Some(name), declared.as_deref(), Some(qualified.as_str())].into_iter().flatten() {
            if let Some(&s) = self.program.by_name.get(key) {
                return Some(s);
            }
        }
        if let Some(fx) = def.and_then(|d| self.checker.program_fn_effects.get(&d)) {
            return Some(Summary::of(*fx));
        }
        self.checker.program_method_effects.get(name).map(|fx| Summary::of(*fx))
    }

    /// The summary of `recv.name(..)` when the receiver is a user class.
    fn method(&self, w: &Walk<'_>, recv: HirId, name: &str) -> Option<Summary> {
        let body = &self.module.bodies[w.index];
        let e = body.expr(recv);
        let owner = self
            .checker
            .class_owner_at_span(e.span)
            .or_else(|| class_of(e.ty.as_ref()).map(str::to_string))?;
        let key = format!("{owner}::{name}");
        if let Some(Some(i)) = self.by_name.get(key.as_str()) {
            return Some(w.summaries[*i]);
        }
        let qualified = format!("{}::{key}", self.module_path);
        for k in [key.as_str(), qualified.as_str()] {
            if let Some(&s) = self.program.by_name.get(k) {
                return Some(s);
            }
            if let Some(fx) = self.checker.program_method_effects.get(k) {
                return Some(Summary::of(*fx));
            }
        }
        None
    }
}

#[cfg(test)]
#[path = "effects.tests.rs"]
mod tests;
