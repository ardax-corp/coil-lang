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
//!
//! A summary also says which parameters a body mutates (`mutates`, `self`
//! is parameter 0 of a method): a write through a value reached from the
//! parameter, or passing such a value where the callee mutates it. A call
//! that passes a `readonly` value where the callee mutates it is E0107
//! ([`ModuleEffects::readonly_messages`], #1558).
//!
//! A write into an object the body made itself is invisible to callers.
//! That covers a call that mutates only such an object too (design §3.7,
//! #1648): [`Summary::param_only`] keeps apart the `mutate` a body has
//! only from changing what its parameters reach, and a caller passing
//! there only objects it owns ([`BodyFacts::owned`]) does not pay it.

use std::collections::{HashMap, HashSet};

use common::EffectFlags;

use super::lower::children;
use super::{BodyKind, Builtin, BinOp, Callee, HirBody, HirId, HirKind, HirModule, HirPat, HirPatFields, LocalId, LocalKind, MakeKind};
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
    /// What a user sees and declares (`uses {…}`): `flags` without panics,
    /// `defer`, GC, generator yields and resizes.
    pub visible: EffectFlags,
    /// Bit `i`: the body calls its parameter `i` (a function value), so a
    /// call also has the effects of the function passed there.
    pub latent: u64,
    /// Bit `i`: the body may change what its parameter `i` reaches (a
    /// field, an element, a `Vec`), itself or through a callee. A
    /// `readonly` argument there is E0107.
    pub mutates: u64,
    /// Reads or writes a mutable `static`, itself or through a callee: what
    /// compile-time code may never do, even with IO granted.
    pub statics: bool,
    /// The bits of `flags` among [`OWNED_BITS`] that come only from
    /// changing what its parameters reach, through a parameter or a field
    /// or element path from one. A caller that passes there only objects
    /// it owns ([`BodyFacts::owned`]) discharges them (design §3.7, #1648).
    pub param_only: u16,
}

/// Effects a write to an object can have that its owner hides.
const OWNED_BITS: u16 = EffectFlags::HEAP_MUT | EffectFlags::RESIZE;

/// `open(path, "r")` with a literal mode reads or writes only as the mode
/// says, as the capability check reads it (#886).
fn open_mode_summary(body: &HirBody, name: &str, args: &[HirId], s: Summary) -> Summary {
    if name.rsplit("::").next() != Some("open") {
        return s;
    }
    let Some(HirKind::Lit(super::Lit::Str(mode))) = args.get(1).map(|&a| &body.expr(a).kind) else {
        return s;
    };
    let caps = common::open_mode_caps(Some(mode));
    let mut drop = 0;
    if !caps.contains(common::Caps::READ) {
        drop |= EffectFlags::READ;
    }
    if !caps.contains(common::Caps::WRITE) {
        drop |= EffectFlags::WRITE;
    }
    let keep = |f: EffectFlags| EffectFlags::from_bits(f.bits() & !drop);
    Summary {
        flags: keep(s.flags),
        visible: keep(s.visible),
        ..s
    }
}

impl Summary {
    /// Pure whatever it is passed.
    pub fn is_pure(self) -> bool {
        self.flags.is_pure() && self.latent == 0
    }

    fn of(flags: EffectFlags) -> Self {
        // Host tables pair `unknown` with host state to be safe; unknown
        // already says it all.
        let hidden = if flags.contains(EffectFlags::UNKNOWN) { HIDDEN | EffectFlags::HOST } else { HIDDEN };
        Self {
            flags,
            visible: EffectFlags::from_bits(flags.bits() & !hidden),
            latent: 0,
            mutates: 0,
            statics: false,
            param_only: 0,
        }
    }
}

/// Bits that are never user-visible effects.
const HIDDEN: u16 = EffectFlags::GC | EffectFlags::YIELD | EffectFlags::RESIZE | EffectFlags::ALLOC;

/// The `uses {…}` vocabulary and the bits each name covers. `read` covers
/// the clock and mutable statics too: what a function reads besides its
/// arguments.
pub const VOCABULARY: &[(&str, u16)] = &[
    ("read", EffectFlags::READ | EffectFlags::HOST),
    ("write", EffectFlags::WRITE),
    ("net", EffectFlags::NET),
    ("env", EffectFlags::ENV),
    ("exec", EffectFlags::EXEC),
    ("ffi", EffectFlags::FFI),
    ("thread", EffectFlags::THREAD),
    ("suspend", EffectFlags::SUSPEND | EffectFlags::ATTACH_PARK),
    ("mutate", EffectFlags::HEAP_MUT),
];

/// The `uses {…}` names for `visible`, then `unknown` for effects this
/// analysis cannot see (no declaration covers those).
pub fn uses_names(visible: EffectFlags) -> Vec<&'static str> {
    let mut out: Vec<&str> = VOCABULARY
        .iter()
        .filter(|(_, bits)| visible.contains(*bits))
        .map(|(n, _)| *n)
        .collect();
    if visible.contains(EffectFlags::UNKNOWN) {
        out.push("unknown");
    }
    out
}

/// `uses {read, write}` for `visible`.
pub fn uses_clause(visible: EffectFlags) -> String {
    format!("uses {{{}}}", uses_names(visible).join(", "))
}

/// The bits a declaration's names allow.
pub fn allowed(names: &[String]) -> EffectFlags {
    let bits = names
        .iter()
        .filter_map(|n| VOCABULARY.iter().find(|(v, _)| v == n))
        .fold(0, |acc, (_, bits)| acc | bits);
    EffectFlags::from_bits(bits)
}

/// Why bodies mutate their parameters, by full body name:
/// `(parameter index, "writes field `x`")`.
pub type Mutations = HashMap<String, Vec<(u32, String)>>;

/// Summaries of every body compiled so far, by full body name
/// (`module::f`, `module::Owner::m`) and by [`DefId`] for functions.
#[derive(Debug, Default)]
pub struct ProgramEffects {
    fns: HashMap<DefId, Summary>,
    by_name: HashMap<String, Summary>,
    /// Trait methods of every module so far, with their declarations.
    traits: Vec<super::TraitEffects>,
    /// Why a body mutates each parameter it mutates, by full body name:
    /// `(parameter index, "writes field `x`")`.
    mutations: Mutations,
}

impl ProgramEffects {
    /// Why the body named `key` (or `module::key`, `.m()` for a method
    /// `Owner::m`) mutates its parameter `j`, when an earlier module said.
    fn mutation_reason(&self, key: &str, j: u32) -> Option<String> {
        let key = key.trim_start_matches('.').trim_end_matches("()");
        let suffix = format!("::{key}");
        let found = self.mutations.get(key).or_else(|| {
            self.mutations
                .iter()
                .find(|(name, _)| name.ends_with(&suffix))
                .map(|(_, r)| r)
        })?;
        found.iter().find(|(k, _)| *k == j).map(|(_, r)| r.clone())
    }

    /// The summary of the body named `name` (`module::f`, `module::Owner::m`).
    pub fn summary_named(&self, name: &str) -> Option<Summary> {
        self.by_name.get(name).copied()
    }

    /// Keep why a module's bodies mutate their parameters
    /// ([`ModuleEffects::mutations`]), so a later module's E0107 can say so.
    pub fn record_mutations(&mut self, mutations: Mutations) {
        self.mutations.extend(mutations);
    }

    /// Keep `module`'s summaries for the modules compiled after it.
    pub fn record(&mut self, module: &HirModule, checker: &Checker, module_path: &str, summaries: &[Summary]) {
        self.traits.extend(module.trait_effects.iter().cloned());
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
    names_where(module, module_path, summaries, known, Summary::is_pure)
}

/// Functions and methods that never change the length of an array or
/// `Vec` (no append, resize, yield or unknown code), whatever they are
/// passed. `known` are names already known to qualify.
pub fn steady_names(module: &HirModule, module_path: &str, summaries: &[Summary], known: &HashSet<String>) -> HashSet<String> {
    names_where(module, module_path, summaries, known, |s| {
        s.latent == 0 && !s.flags.contains(EffectFlags::RESIZE | EffectFlags::UNKNOWN | EffectFlags::YIELD)
    })
}

fn names_where(
    module: &HirModule,
    module_path: &str,
    summaries: &[Summary],
    known: &HashSet<String>,
    ok: impl Fn(Summary) -> bool,
) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut by_short: HashMap<&str, bool> = HashMap::new();
    for (body, s) in module.bodies.iter().zip(summaries) {
        let Some(local) = local_name(module_path, &body.name) else { continue };
        let (owner, short) = match local.rsplit_once("::") {
            Some((owner, short)) => (Some(owner), short),
            None => (None, local),
        };
        let pure = ok(*s) || known.contains(local);
        match (body.kind, owner) {
            (BodyKind::Function, None) => {
                if ok(*s) {
                    out.insert(local.to_string());
                }
            }
            // Trait impls (`Show for P::m`) and nested functions keep the
            // AST walk's verdict.
            (BodyKind::Method, Some(owner)) if !owner.contains(' ') && !owner.contains("::") => {
                if ok(*s) {
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

thread_local! {
    static EFFECTS_CAPTURE: std::cell::RefCell<Option<EffectsCapture>> =
        const { std::cell::RefCell::new(None) };
}

/// Effects recorded by [`start_effects_capture`] (`coil dissect --effects`).
#[derive(Debug, Default)]
pub struct EffectsCapture {
    /// `(body name, description)` per function and method, in compile order.
    pub fns: Vec<(String, String)>,
    /// Why auto-par left a loop or a function sequential.
    pub auto_par: Vec<String>,
    /// Auto-par was off for some module.
    pub auto_par_off: bool,
}

/// Start recording each module's effects as codegen compiles it.
pub fn start_effects_capture() {
    EFFECTS_CAPTURE.with(|c| *c.borrow_mut() = Some(EffectsCapture::default()));
}

/// Everything recorded since [`start_effects_capture`].
pub fn take_effects_capture() -> EffectsCapture {
    EFFECTS_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

pub(crate) fn effects_capture_active() -> bool {
    EFFECTS_CAPTURE.with(|c| c.borrow().is_some())
}

/// Record `fx`'s functions and the auto-par explanations (`None`: auto-par off).
pub(crate) fn capture(module: &HirModule, fx: &ModuleEffects<'_>, auto_par: Option<Vec<String>>) {
    let fns: Vec<(String, String)> = module
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.kind, BodyKind::Function | BodyKind::Method))
        .map(|(i, b)| (b.name.clone(), fx.describe(i)))
        .collect();
    EFFECTS_CAPTURE.with(|c| {
        if let Some(out) = c.borrow_mut().as_mut() {
            out.fns.extend(fns);
            match auto_par {
                Some(lines) => out.auto_par.extend(lines),
                None => out.auto_par_off = true,
            }
        }
    });
}

/// Effects of every function and method in one checked file (`""` module),
/// for editor hover: `(name, description)`.
pub fn describe_fns(checker: &Checker, ast: &parser::ast::Output<'_>) -> Vec<(String, String)> {
    let sidecar = checker.typed_sidecar();
    let module = super::build_module(checker, &sidecar, "", ast);
    let program = ProgramEffects::default();
    let fx = ModuleEffects::solve(&module, checker, "", &program);
    module
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.kind, BodyKind::Function | BodyKind::Method))
        .map(|(i, b)| (b.name.clone(), fx.describe(i)))
        .collect()
}

/// Broken effect declarations in one checked file (`""` module), for the
/// editor: codegen reports the same errors.
pub fn effect_declaration_errors(checker: &Checker, ast: &parser::ast::Output<'_>) -> Vec<reporting::Message> {
    if !declares_effects(ast) {
        return Vec::new();
    }
    let sidecar = checker.typed_sidecar();
    let module = super::build_module(checker, &sidecar, "", ast);
    let program = ProgramEffects::default();
    ModuleEffects::solve(&module, checker, "", &program).violation_messages()
}

/// Why auto-par leaves loops and functions of this module sequential when
/// purity is all that stops it: each counted loop or fork site that would
/// split if every function it calls were pure, with the impure callee and
/// the first reason it is impure ("`step` calls `write_all` (write)").
pub(crate) fn auto_par_explanations(
    ast: &parser::ast::Output<'_>,
    module: &HirModule,
    module_path: &str,
    fx: &ModuleEffects<'_>,
    pure: &HashSet<String>,
    cases: &crate::typechecking::bare_cases::BareCases,
) -> Vec<String> {
    use crate::typechecking::{analyze_loop_par_sites, analyze_par_fork_sites};
    let mut all = pure.clone();
    for body in &module.bodies {
        if matches!(body.kind, BodyKind::Function | BodyKind::Method)
            && let Some(local) = local_name(module_path, &body.name)
        {
            all.insert(local.to_string());
            all.insert(local.rsplit("::").next().unwrap_or(local).to_string());
        }
    }
    let mut out = Vec::new();
    let actual = analyze_loop_par_sites(ast, pure);
    let mut blocked: Vec<_> = analyze_loop_par_sites(ast, &all)
        .into_iter()
        .filter(|(span, _)| !actual.contains_key(span))
        .collect();
    blocked.sort_by_key(|(span, _)| *span);
    for ((start, end), site) in blocked {
        let owner = module
            .bodies
            .iter()
            .filter(|b| matches!(b.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test))
            .filter(|b| b.span.0 <= start && end <= b.span.1)
            .min_by_key(|b| b.span.1 - b.span.0);
        let Some(owner) = owner else { continue };
        let reasons = impure_callees(fx, owner, (start, end), pure);
        if reasons.is_empty() {
            continue;
        }
        out.push(format!(
            "loop over `{}` in `{}` not parallelized: {}",
            site.index,
            owner.name,
            reasons.join("; ")
        ));
    }
    let actual = analyze_par_fork_sites(ast, pure, cases);
    let mut blocked: Vec<String> = analyze_par_fork_sites(ast, &all, cases)
        .into_keys()
        .filter(|name| !actual.contains_key(name))
        .collect();
    blocked.sort();
    for name in blocked {
        let index = fx.cx.by_name.get(name.as_str()).copied().flatten();
        let Some(reason) = index.and_then(|i| fx.first_reason(i)) else { continue };
        out.push(format!("`{name}` not parallelized: `{name}` {reason}"));
    }
    out
}

/// "`f` calls `write_all` (write)" for each impure user function called in
/// `owner` inside `span`.
fn impure_callees(fx: &ModuleEffects<'_>, owner: &HirBody, span: super::Span, pure: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for e in &owner.exprs {
        let HirKind::Call { callee: Callee::Named { name, .. }, .. } = &e.kind else { continue };
        if e.span.0 < span.0 || span.1 < e.span.1 || pure.contains(name) || !seen.insert(name.clone()) {
            continue;
        }
        let Some(Some(i)) = fx.cx.by_name.get(name.as_str()) else { continue };
        if let Some(reason) = fx.first_reason(*i) {
            out.push(format!("`{name}` {reason}"));
        }
    }
    out
}

/// Some function in `ast` is `pure fn` or has a `uses {…}` clause.
pub fn declares_effects(ast: &parser::ast::Output<'_>) -> bool {
    use parser::ast::Expression as E;
    let any = |items: &[parser::ast::Output<'_>]| items.iter().any(declares_effects);
    match ast.1.as_ref() {
        E::Function { effects, .. } => effects.is_some(),
        E::Program(items) | E::Block(items) | E::Fragment(items) => any(items),
        E::Implementation { methods, .. } | E::TypeClassImpl { methods, .. } | E::TypeClass { methods, .. } => {
            any(methods)
        }
        E::Method(_, inner) | E::Module(_, inner) | E::Expr(inner) | E::Statement(inner) => declares_effects(inner),
        _ => false,
    }
}

/// The last segment of a path: `shapes::Shape` → `Shape`.
fn short_name(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
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

/// A module's solved summaries, with what it takes to explain them.
pub struct ModuleEffects<'a> {
    cx: Cx<'a>,
    pub summaries: Vec<Summary>,
}

impl<'a> ModuleEffects<'a> {
    pub fn solve(module: &'a HirModule, checker: &'a Checker, module_path: &'a str, program: &'a ProgramEffects) -> Self {
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
                return Self { cx, summaries: out };
            }
        }
    }

    /// Why each function and method mutates each parameter it mutates.
    pub fn mutations(&self) -> Mutations {
        let mut out = Mutations::new();
        for (i, body) in self.cx.module.bodies.iter().enumerate() {
            let m = self.summaries[i].mutates;
            if m == 0 || !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
                continue;
            }
            let reasons = (0..64u32)
                .filter(|j| m & (1 << j) != 0)
                .map(|j| (j, self.mutation_reason(i, j)))
                .collect();
            out.insert(body.name.clone(), reasons);
        }
        out
    }

    /// Why body `index` mutates its parameter `j`.
    pub fn mutation_reason(&self, index: usize, j: u32) -> String {
        self.cx.mutation_reason(index, j, &self.summaries)
    }

    /// E0107 for every call that passes a `readonly` value where the callee
    /// mutates it, and for every trait impl method that mutates a
    /// parameter its trait declares `readonly` (#1558).
    pub fn readonly_messages(&self) -> Vec<reporting::Message> {
        use reporting::{ErrorCode, Message};
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for i in 0..self.cx.module.bodies.len() {
            let w = self.cx.walk_mutations(i, &self.summaries);
            for hit in w.ro_hits.unwrap_or_default() {
                if !seen.insert((hit.span, hit.message.clone())) {
                    continue;
                }
                let mut msg = Message::error(ErrorCode::InvalidAssignment, hit.message, hit.span.0..hit.span.1);
                msg.with_help(hit.help);
                out.push(msg);
            }
        }
        for (i, body) in self.cx.module.bodies.iter().enumerate() {
            let local = local_name(self.cx.module_path, &body.name).unwrap_or(&body.name);
            let Some((path, m)) = local.rsplit_once("::") else { continue };
            let Some((tr, _)) = path.split_once(" for ") else { continue };
            let declared = self.cx.checker.trait_readonly_params(tr, m);
            let bad = self.summaries[i].mutates & declared;
            let Some(j) = (0..64u32).find(|j| bad & (1 << j) != 0) else { continue };
            let (_, param) = self.cx.callee_param(Some(i), None, j);
            let mut msg = Message::error(
                ErrorCode::InvalidAssignment,
                format!(
                    "`{local}` implements `{tr}::{m}`, which declares {param} `readonly`, but it mutates it: {}",
                    self.mutation_reason(i, j)
                ),
                body.span.0..body.span.1,
            );
            msg.with_help("an impl must keep to the trait's `readonly` parameters; drop the write, or the `readonly` from the trait".to_string());
            out.push(msg);
        }
        out
    }

    /// Every reason body `index` is not pure, in source order.
    pub fn explain(&self, index: usize) -> Vec<Cause> {
        self.cx.walk(index, &self.summaries, true).causes.unwrap_or_default()
    }

    /// Body `index`'s user-visible effects in one line: `pure`, or its
    /// `uses {…}` and the reasons for it (`uses {write}: calls `write_all`
    /// (write)`).
    pub fn describe(&self, index: usize) -> String {
        let s = self.summaries[index];
        if s.visible.is_pure() && s.latent == 0 {
            return "pure".into();
        }
        let body = &self.cx.module.bodies[index];
        let mut parts: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for cause in self.explain(index) {
            if !cause.visible.is_pure() && seen.insert(cause.what.clone()) {
                parts.push(format!("{} ({})", cause.what, uses_names(cause.visible).join(", ")));
            }
        }
        for (i, p) in body.params.iter().enumerate().take(64) {
            if s.latent & (1 << i) != 0 {
                parts.push(format!("calls parameter `{}`", body.local(*p).name));
            }
        }
        for (i, p) in body.params.iter().enumerate().take(64) {
            if s.mutates & (1 << i) != 0 {
                match body.local(*p).name.as_str() {
                    "self" => parts.push("mutates self".to_string()),
                    n => parts.push(format!("mutates `{n}`")),
                }
            }
        }
        let head = if s.visible.is_pure() {
            "pure apart from its parameters".to_string()
        } else {
            uses_clause(s.visible)
        };
        format!("{head}: {}", parts.join("; "))
    }

    /// Every function whose effects break what it declares (`pure fn`,
    /// `uses {…}`), and every trait impl method that breaks its trait
    /// method's declaration.
    pub fn violations(&self) -> Vec<Violation> {
        let module = self.cx.module;
        let mut out = Vec::new();
        for (i, body) in module.bodies.iter().enumerate() {
            if !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
                continue;
            }
            let local = local_name(self.cx.module_path, &body.name).unwrap_or(&body.name);
            // A class invariant is checked in many bodies; report it once.
            for v in self.contract_violations(i) {
                if !out.iter().any(|o: &Violation| o.span == v.span && o.message == v.message) {
                    out.push(v);
                }
            }
            let visible = self.summaries[i].visible;
            if let Some(d) = &body.declared
                && let Some(v) = self.violation(i, visible, d, || format!("`{local}` is declared `{}`", d.text), d.span)
            {
                out.push(v);
            }
            // `Trait for Owner::m` keeps to `Trait::m`'s declaration.
            let Some((path, m)) = local.rsplit_once("::") else { continue };
            let Some((tr, _)) = path.split_once(" for ") else { continue };
            let decl = module
                .trait_effects
                .iter()
                .chain(&self.cx.program.traits)
                .find(|t| short_name(&t.trait_name) == short_name(tr) && t.method == m)
                .and_then(|t| t.declared.as_ref());
            if let Some(d) = decl
                && let Some(v) =
                    self.violation(i, visible, d, || format!("`{local}` implements `{tr}::{m}`, declared `{}`", d.text), body.span)
            {
                out.push(v);
            }
        }
        out
    }

    /// A contract clause that does more than read memory: a
    /// check must not change what the program does.
    fn contract_violations(&self, index: usize) -> Vec<Violation> {
        let body = &self.cx.module.bodies[index];
        if body.contract_spans.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Violation> = Vec::new();
        for cause in self.explain(index) {
            if cause.visible.is_pure() {
                continue;
            }
            let Some(&clause) = body
                .contract_spans
                .iter()
                // By start: a node's span can run on over trailing trivia.
                .find(|(s, e)| *s <= cause.span.0 && cause.span.0 < *e)
            else {
                continue;
            };
            if out.iter().any(|v| v.span == clause) {
                continue;
            }
            out.push(Violation {
                span: clause,
                message: format!(
                    "a contract clause must not have effects, but it {} ({})",
                    cause.what,
                    uses_names(cause.visible).join(", ")
                ),
                help: "check with pure functions; contracts only read values".to_string(),
            });
        }
        out
    }

    /// [`Self::violations`] as errors.
    pub fn violation_messages(&self) -> Vec<reporting::Message> {
        use reporting::{ErrorCode, Message};
        self.violations()
            .into_iter()
            .map(|v| {
                let mut msg = Message::error(ErrorCode::EffectMismatch, v.message, v.span.0..v.span.1);
                msg.with_help(v.help);
                msg
            })
            .collect()
    }

    fn violation(
        &self,
        index: usize,
        visible: EffectFlags,
        declared: &super::DeclaredEffects,
        head: impl FnOnce() -> String,
        span: super::Span,
    ) -> Option<Violation> {
        let allowed = allowed(&declared.names);
        let missing = EffectFlags::from_bits(visible.bits() & !allowed.bits());
        if missing.is_pure() {
            return None;
        }
        let known = EffectFlags::from_bits(allowed.bits() | (visible.bits() & !EffectFlags::UNKNOWN));
        let help = if missing.bits() == EffectFlags::UNKNOWN {
            "a declaration needs every call resolved: call through a trait method that declares its effects".to_string()
        } else {
            format!("declare `{}`", uses_clause(known))
        };
        Some(Violation {
            span,
            message: format!(
                "{} but needs {}: {}",
                head(),
                uses_names(missing).join(", "),
                self.chain(index, missing)
            ),
            help,
        })
    }

    /// How body `index` comes to have the effects in `missing`, as a call
    /// chain: `load → parse_file → open needs read`.
    fn chain(&self, index: usize, missing: EffectFlags) -> String {
        let module = self.cx.module;
        let name = |i: usize| {
            let n = &module.bodies[i].name;
            local_name(self.cx.module_path, n).unwrap_or(n).to_string()
        };
        let mut names = vec![name(index)];
        let mut seen = HashSet::from([index]);
        let mut i = index;
        loop {
            let found = self
                .explain(i)
                .into_iter()
                .find(|c| c.visible.bits() & missing.bits() != 0);
            let Some(cause) = found else {
                return format!("{} needs {}", names.join(" → "), uses_names(missing).join(", "));
            };
            let need = uses_names(EffectFlags::from_bits(cause.visible.bits() & missing.bits())).join(", ");
            let Some(callee) = cause.callee else {
                return format!("{} needs {need}: it {}", names.join(" → "), cause.what);
            };
            names.push(callee.name);
            match callee.body {
                Some(j) if seen.insert(j) => i = j,
                _ => return format!("{} needs {need}", names.join(" → ")),
            }
        }
    }

    /// The first reason body `index` is not pure, as `calls `x` (write)`:
    /// in the `uses {…}` vocabulary, or the internal names for a reason no
    /// user sees (`may panic (unknown)`).
    pub fn first_reason(&self, index: usize) -> Option<String> {
        let s = self.summaries[index];
        if let Some(cause) = self.explain(index).into_iter().next() {
            let names = if cause.visible.is_pure() { effect_names(cause.flags) } else { uses_names(cause.visible) };
            return Some(format!("{} ({})", cause.what, names.join(", ")));
        }
        let body = &self.cx.module.bodies[index];
        let i = (0..64).find(|i| s.latent & (1 << i) != 0)?;
        Some(format!("calls parameter `{}`", body.local(body.params[i]).name))
    }
}

/// A broken effect declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The declaration, or the impl method that breaks its trait's.
    pub span: super::Span,
    pub message: String,
    pub help: String,
}

/// User-facing names of the bits in `flags`. `resize` is left out: it
/// always comes with another bit.
pub fn effect_names(flags: EffectFlags) -> Vec<&'static str> {
    const NAMES: &[(u16, &str)] = &[
        (EffectFlags::READ, "read"),
        (EffectFlags::WRITE, "write"),
        (EffectFlags::NET, "net"),
        (EffectFlags::ENV, "env"),
        (EffectFlags::EXEC, "exec"),
        (EffectFlags::SUSPEND, "suspend"),
        (EffectFlags::HEAP_MUT, "heap write"),
        (EffectFlags::HOST, "host state"),
        (EffectFlags::FFI, "ffi"),
        (EffectFlags::YIELD, "yield"),
        (EffectFlags::THREAD, "thread"),
        (EffectFlags::GC, "gc"),
        (EffectFlags::ATTACH_PARK, "attach"),
        (EffectFlags::UNKNOWN, "unknown"),
        (EffectFlags::ALLOC, "alloc"),
    ];
    NAMES.iter().filter(|(bit, _)| flags.contains(*bit)).map(|(_, n)| *n).collect()
}

/// Per-body facts that do not change while solving.
struct BodyFacts {
    /// Locals holding an object made in this body that never leaves it but
    /// by `return`: a write through one is invisible to callers.
    private: HashSet<LocalId>,
    /// `let f = <lambda or fn>` never reassigned: a call through `f` is a
    /// call of that function.
    fn_values: HashMap<LocalId, HirId>,
    /// Locals that may hold a value reached from parameters: bit `i` for
    /// parameter `i` ([`param_roots`]).
    derived: HashMap<LocalId, u64>,
    /// Private objects that may also be passed to calls ([`owned_locals`]):
    /// a call that mutates only these costs no `mutate`.
    owned: HashSet<LocalId>,
    /// Locals assigned after their binding: a parameter among them no
    /// longer holds what the caller passed.
    assigned: HashSet<LocalId>,
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
                            kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Class(_) | MakeKind::Record(_),
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
        let mut escaped = assigned.clone();
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
        let mut owned: HashSet<LocalId> = fresh.iter().copied().filter(|l| !escaped.contains(l)).collect();
        for (i, e) in body.exprs.iter().enumerate() {
            if let HirKind::Local(l) = e.kind
                && fresh.contains(&l)
                && !allowed.contains(&HirId(i as u32))
            {
                escaped.insert(l);
            }
        }
        fresh.retain(|l| !escaped.contains(l));
        owned_locals(body, &mut owned);
        let derived = derived_locals(body);
        Self {
            private: fresh,
            fn_values,
            derived,
            owned,
            assigned,
        }
    }
}

/// Whether a value of `id` can give a private object nothing a write
/// through that object could change but itself: a value of a type with
/// nothing mutable in it, a `readonly` one, or a new object or `Vec` made
/// of such values.
fn inert(body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    if !may_hold_mutable(e.ty.as_ref()) || matches!(e.ty, Some(Ty::Readonly(_))) {
        return true;
    }
    match &e.kind {
        HirKind::Named { value, .. } => inert(body, *value),
        HirKind::Make {
            kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Class(_) | MakeKind::Record(_),
            args,
        } => args.iter().all(|a| inert(body, *a)),
        HirKind::Call { callee: Callee::Named { name, .. }, args } if is_vec_ctor(name) => {
            args.iter().all(|a| inert(body, *a))
        }
        _ => false,
    }
}

/// Narrow `owned` (fresh, never reassigned, captured or deferred locals) to
/// the objects no write through them can reach anything else, and that
/// never leave the body (design §3.7, #1648). Each is made of
/// [`inert`] values, is given only inert values afterwards, and appears
/// only as:
/// - the base of `x.f` / `x[i]`, or the receiver of a `Vec` method;
/// - `return x` from a body that returns a `readonly` view;
/// - an argument of a call whose other arguments are inert or owned and
///   whose result holds nothing mutable or is `readonly`, so the callee
///   can neither hand back an alias nor store a shared value in it.
fn owned_locals(body: &HirBody, owned: &mut HashSet<LocalId>) {
    if owned.is_empty() {
        return;
    }
    // The local a place `x.f.g` / `x[i]` writes into.
    fn place_root(body: &HirBody, id: HirId) -> Option<LocalId> {
        match &body.expr(id).kind {
            HirKind::Local(l) => Some(*l),
            HirKind::Field { base, .. } | HirKind::Index { base, .. } => place_root(body, *base),
            _ => None,
        }
    }
    let readonly_ret = matches!(body.ret, Some(Ty::Readonly(_))) || !may_hold_mutable(body.ret.as_ref());
    let mut parent: HashMap<HirId, HirId> = HashMap::new();
    for i in 0..body.exprs.len() {
        let id = HirId(i as u32);
        for k in children(body, id) {
            parent.insert(k, id);
        }
    }
    for e in &body.exprs {
        match &e.kind {
            HirKind::Let { local, init: Some(init) } if owned.contains(local) && !inert(body, *init) => {
                owned.remove(local);
            }
            HirKind::Assign { place, value } | HirKind::Append { base: place, value } => {
                if let Some(l) = place_root(body, *place)
                    && !inert(body, *value)
                {
                    owned.remove(&l);
                }
            }
            _ => {}
        }
    }
    loop {
        let before = owned.len();
        for (i, e) in body.exprs.iter().enumerate() {
            let HirKind::Local(l) = e.kind else { continue };
            if !owned.contains(&l) {
                continue;
            }
            let id = HirId(i as u32);
            let ok = parent.get(&id).is_some_and(|&p| match &body.expr(p).kind {
                HirKind::Field { base, .. } | HirKind::Index { base, .. } | HirKind::Append { base, .. } => *base == id,
                HirKind::Return(Some(_)) => readonly_ret,
                HirKind::Call { callee: Callee::Method { name }, args }
                    if args.first().is_some_and(|&r| is_vec(body.expr(r).ty.as_ref()))
                        && (VEC_MUTATORS.contains(&name.as_str()) || matches!(name.as_str(), "len" | "capacity")) =>
                {
                    args[0] == id && args[1..].iter().all(|a| inert(body, *a))
                }
                HirKind::Call {
                    callee: Callee::Named { .. } | Callee::Method { .. },
                    args,
                } => {
                    let ty = body.expr(p).ty.as_ref();
                    (!may_hold_mutable(ty) || matches!(ty, Some(Ty::Readonly(_))))
                        && args.iter().all(|&a| {
                            inert(body, a) || matches!(body.expr(a).kind, HirKind::Local(k) if owned.contains(&k))
                        })
                }
                _ => false,
            });
            if !ok {
                owned.remove(&l);
            }
        }
        if owned.len() == before {
            return;
        }
    }
}

/// Whether a value of `ty` can reach something a write could change.
/// Scalars, strings and functions cannot.
fn may_hold_mutable(ty: Option<&Ty>) -> bool {
    match ty {
        Some(Ty::Readonly(inner)) => may_hold_mutable(Some(inner)),
        Some(Ty::Con(n)) => !matches!(
            n.as_str(),
            "int" | "float" | "bool" | "byte" | "string" | "unit" | "char" | "f32" | "f64"
        ),
        Some(Ty::Fun(..)) => false,
        _ => true,
    }
}

/// The parameters (bit `i` for parameter `i`) whose values `id` may
/// reach: a parameter, a field, element or pattern binding read from one,
/// or what a call returns when it was given one (a getter may return
/// `self.inner`).
fn param_roots(body: &HirBody, derived: &HashMap<LocalId, u64>, id: HirId) -> u64 {
    let e = body.expr(id);
    if !may_hold_mutable(e.ty.as_ref()) {
        return 0;
    }
    let roots = |k: HirId| param_roots(body, derived, k);
    match &e.kind {
        HirKind::Local(l) => local_roots(body, derived, *l),
        HirKind::Field { base, .. } | HirKind::Index { base, .. } => roots(*base),
        HirKind::Cast { value } | HirKind::Named { value, .. } | HirKind::Spread(value) => roots(*value),
        HirKind::Call { args, .. } | HirKind::Make { args, .. } | HirKind::Builtin { args, .. } => {
            args.iter().fold(0, |m, a| m | roots(*a))
        }
        HirKind::Block { tail: Some(t), .. } => roots(*t),
        HirKind::If { then, els, .. } => roots(*then) | els.map_or(0, roots),
        HirKind::Match { arms, .. } => arms.iter().fold(0, |m, a| m | roots(a.body)),
        _ => 0,
    }
}

/// [`param_roots`] of local `l`. A lambda's captures count as parameters
/// after its own: bit `params.len() + k` for capture `k`, which the body
/// making the lambda maps back to the captured local.
fn local_roots(body: &HirBody, derived: &HashMap<LocalId, u64>, l: LocalId) -> u64 {
    let capture = body.captures.iter().position(|(_, inner)| *inner == l).map(|k| body.params.len() + k);
    let own = body
        .params
        .iter()
        .position(|p| *p == l)
        .or(capture)
        .filter(|&i| i < 64)
        .map_or(0, |i| 1u64 << i);
    own | derived.get(&l).copied().unwrap_or(0)
}

fn pat_binds(pat: &HirPat, out: &mut Vec<LocalId>) {
    match pat {
        HirPat::Bind(l) => out.push(*l),
        HirPat::Tuple(items) => items.iter().for_each(|p| pat_binds(p, out)),
        HirPat::Record(fields) => fields.iter().for_each(|(_, p)| pat_binds(p, out)),
        HirPat::Variant { fields, .. } => match fields {
            HirPatFields::Unit => {}
            HirPatFields::Tuple(items) => items.iter().for_each(|p| pat_binds(p, out)),
            HirPatFields::Record(fields) => fields.iter().for_each(|(_, p)| pat_binds(p, out)),
        },
        HirPat::Wild | HirPat::Int(_) => {}
    }
}

/// [`BodyFacts::derived`]: what each local may reach of the parameters,
/// through `let`, assignment, pattern bindings and `for` variables.
fn derived_locals(body: &HirBody) -> HashMap<LocalId, u64> {
    let mut derived: HashMap<LocalId, u64> = HashMap::new();
    if body.params.is_empty() && body.captures.is_empty() {
        return derived;
    }
    loop {
        let mut changed = false;
        let mut add = |derived: &mut HashMap<LocalId, u64>, l: LocalId, bits: u64| {
            if bits == 0 {
                return;
            }
            let slot = derived.entry(l).or_insert(0);
            if *slot | bits != *slot {
                *slot |= bits;
                changed = true;
            }
        };
        for e in &body.exprs {
            let mut binds = Vec::new();
            let bits = match &e.kind {
                HirKind::Let { local, init: Some(init) } => {
                    binds.push(*local);
                    param_roots(body, &derived, *init)
                }
                HirKind::Assign { place, value } => match body.expr(*place).kind {
                    HirKind::Local(l) => {
                        binds.push(l);
                        param_roots(body, &derived, *value)
                    }
                    _ => 0,
                },
                HirKind::LetPat { pat, init } => {
                    pat_binds(pat, &mut binds);
                    param_roots(body, &derived, *init)
                }
                HirKind::ForIn { pat, iterable, .. } => {
                    pat_binds(pat, &mut binds);
                    param_roots(body, &derived, *iterable)
                }
                HirKind::Match { scrutinee, arms } => {
                    for arm in arms {
                        pat_binds(&arm.pat, &mut binds);
                    }
                    param_roots(body, &derived, *scrutinee)
                }
                _ => 0,
            };
            for l in binds {
                add(&mut derived, l, bits);
            }
        }
        if !changed {
            return derived;
        }
    }
}

/// The parameters a function value of type `ty` may mutate: every one
/// its type does not declare `readonly`. A type-variable parameter is left
/// out: the checker types the function passed for it with the argument's
/// own type, `readonly` included.
fn fn_type_mutates(ty: Option<&Ty>) -> u64 {
    let mut out = 0;
    let mut cur = ty;
    let mut i = 0;
    while let Some(Ty::Fun(p, r)) = cur {
        if i < 64 && !matches!(p.as_ref(), Ty::Readonly(_) | Ty::Var(_)) {
            out |= 1 << i;
        }
        i += 1;
        cur = Some(r.as_ref());
    }
    out
}

/// Whether passing a value of `ty` where a callee mutates it would write
/// through a `readonly`: a `readonly` value, or one in a tuple or an enum
/// payload (both immutable themselves).
fn readonly_at_top(checker: &Checker, ty: Option<&Ty>) -> bool {
    match ty {
        Some(Ty::Readonly(_)) => true,
        Some(Ty::Tuple(items)) => items.iter().any(|t| readonly_at_top(checker, Some(t))),
        Some(Ty::App(head, args)) => {
            matches!(head.as_ref(), Ty::Con(n) if common::is_poly_builtin_enum(n) || checker.enum_variants(n).is_some())
                && args.iter().any(|t| readonly_at_top(checker, Some(t)))
        }
        _ => false,
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
    /// Why the body has its effects, when [`explain`] asked.
    causes: Option<Vec<Cause>>,
    /// Why the body mutates its parameters, when asked.
    mut_causes: Option<Vec<MutCause>>,
    /// Calls that pass a `readonly` value where the callee mutates it,
    /// when asked.
    ro_hits: Option<Vec<RoHit>>,
    /// [`OWNED_BITS`] seen only from changing what parameters reach, and
    /// seen from anything else ([`Summary::param_only`]).
    through_params: u16,
    elsewhere: u16,
}

/// One place a body mutates what its parameters reach.
#[derive(Debug, Clone)]
struct MutCause {
    /// The parameters reached there.
    bits: u64,
    /// "writes field `x`", "calls `.push()` on a `Vec`".
    what: String,
    /// A callee in this module mutates it as its parameter `.1`.
    callee: Option<(usize, u32)>,
}

/// A value a call passes, the parameters it may fill, and its type.
struct Passed {
    /// One bit, or every parameter from its position on when a
    /// dynamic-length spread comes at or before it (#1681).
    params: u64,
    arg: HirId,
    ty: Option<Ty>,
    /// The local it reads, when the call passes one directly.
    name: Option<String>,
}

/// An argument after named and spread arguments are flattened: its value,
/// its type, the local it names, and whether it is a spread whose length
/// is not known statically.
struct Flat {
    arg: HirId,
    ty: Option<Ty>,
    name: Option<String>,
    dynamic: bool,
}

/// Every parameter from `j` on.
fn bits_from(j: usize) -> u64 {
    if j >= 64 { 0 } else { u64::MAX << j }
}

/// A call that passes a `readonly` value where the callee mutates it.
#[derive(Debug, Clone)]
struct RoHit {
    span: super::Span,
    message: String,
    help: String,
}

/// One reason a body is not pure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cause {
    pub span: super::Span,
    /// What the body does there: "calls `write_all`".
    pub what: String,
    pub flags: EffectFlags,
    /// The user-visible part of `flags` ([`Summary::visible`]).
    pub visible: EffectFlags,
    /// The function called there, for a call.
    pub callee: Option<Called>,
}

/// A called function: its name as shown, and its body when it is this
/// module's (so a call chain can go on through it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Called {
    pub name: String,
    pub body: Option<usize>,
}

impl Cx<'_> {
    fn body(&self, index: usize, summaries: &[Summary]) -> Summary {
        self.walk(index, summaries, false).out
    }

    fn walk<'s>(&self, index: usize, summaries: &'s [Summary], explain: bool) -> Walk<'s> {
        let body = &self.module.bodies[index];
        let mut w = Walk {
            index,
            summaries,
            out: Summary::default(),
            causes: explain.then(Vec::new),
            mut_causes: None,
            ro_hits: None,
            through_params: 0,
            elsewhere: 0,
        };
        if let Some(root) = body.root {
            self.expr(&mut w, root);
        }
        w.out.param_only = w.through_params & !w.elsewhere;
        w
    }

    /// Body `index` walked for its parameter mutations and its calls that
    /// mutate a `readonly` value, with the solved summaries.
    fn walk_mutations<'s>(&self, index: usize, summaries: &'s [Summary]) -> Walk<'s> {
        let body = &self.module.bodies[index];
        let mut w = Walk {
            index,
            summaries,
            out: Summary::default(),
            causes: None,
            mut_causes: Some(Vec::new()),
            ro_hits: Some(Vec::new()),
            through_params: 0,
            elsewhere: 0,
        };
        if let Some(root) = body.root {
            self.expr(&mut w, root);
        }
        w.out.param_only = w.through_params & !w.elsewhere;
        w
    }

    /// The parameters `id` may reach in the walked body.
    fn roots(&self, w: &Walk<'_>, id: HirId) -> u64 {
        param_roots(&self.module.bodies[w.index], &self.facts[w.index].derived, id)
    }

    /// The body mutates what `bits` of its parameters reach.
    fn mutate(&self, w: &mut Walk<'_>, bits: u64, callee: Option<(usize, u32)>, what: impl FnOnce() -> String) {
        if bits == 0 {
            return;
        }
        w.out.mutates |= bits;
        if let Some(causes) = w.mut_causes.as_mut() {
            causes.push(MutCause { bits, what: what(), callee });
        }
    }

    /// Fold callee `shown`'s parameter mutations into `w`: an argument it
    /// mutates is mutated here too, and one that is `readonly` is E0107.
    /// `key` names an earlier module's callee, for its reasons.
    #[allow(clippy::too_many_arguments)]
    fn call_mutates(
        &self,
        w: &mut Walk<'_>,
        at: HirId,
        shown: &str,
        mutates: u64,
        callee_body: Option<usize>,
        key: Option<&str>,
        args: &[HirId],
        trait_decl: bool,
    ) {
        if mutates == 0 {
            return;
        }
        let body = &self.module.bodies[w.index];
        let rest = self.rest_param(body, at, callee_body, key, args);
        let Some(passed) = self.passed_args(body, at, args, rest) else {
            let all = args.iter().fold(0, |m, a| m | self.roots(w, *a));
            self.mutate(w, all, None, || format!("passes it to `{shown}`"));
            return;
        };
        for Passed { params, arg, ty, name } in passed {
            let hit = params & mutates;
            if hit == 0 {
                continue;
            }
            let bits = self.roots(w, arg);
            for j in (0..64u32).filter(|j| hit & (1 << j) != 0) {
                self.mutate(w, bits, callee_body.map(|b| (b, j)), || {
                    if trait_decl && callee_body.is_none() && shown.starts_with('.') && j == 0 {
                        format!("calls `{shown}`, whose trait does not declare `self` `readonly`")
                    } else {
                        format!("passes it to `{shown}`")
                    }
                });
            }
            if w.ro_hits.is_none() || !readonly_at_top(self.checker, ty.as_ref()) {
                continue;
            }
            // One error for the argument, at the first parameter it mutates.
            let j = hit.trailing_zeros();
            let (what, mut param) = self.callee_param(callee_body, key, j);
            if trait_decl && j == 0 {
                param = "`self`".to_string();
            }
            let reason = match callee_body {
                Some(b) => Some(self.mutation_reason(b, j, w.summaries)),
                None => key.and_then(|k| self.program.mutation_reason(k, j)),
            };
            let because = reason.map_or_else(String::new, |r| format!(" ({r})"));
            // A method call's receiver is what it is called on; any other
            // argument is passed to it, by name when it is a local (#1681).
            let message = if shown.starts_with('.') && j == 0 {
                format!("Cannot call `{shown}` on a `readonly` value: it mutates {param}{because}")
            } else if let Some(n) = name {
                format!("Cannot pass the `readonly` `{n}` to `{shown}`: it mutates {param}{because}")
            } else {
                format!("Cannot pass a `readonly` value to `{shown}`: it mutates {param}{because}")
            };
            let help = if trait_decl {
                format!(
                    "a trait method may be called on a `readonly` value only when the trait declares that parameter `readonly` (`fn {what}(readonly A self)`)"
                )
            } else {
                "only a function or method that does not mutate it accepts a `readonly` value; mutate through a name that is not `readonly`".to_string()
            };
            let span = body.expr(at).span;
            if let Some(hits) = w.ro_hits.as_mut() {
                hits.push(RoHit { span, message, help });
            }
        }
    }

    /// Each value a call passes, with the parameter it fills: a named
    /// argument goes to the parameter the checker matched it to, a spread
    /// of a tuple or a fixed array passes each item in turn (#1646), and
    /// arguments packed into a rest parameter all fill it (#1681). A spread
    /// whose length is not known fills the rest parameter when the callee
    /// has one, and may fill any parameter from its position on otherwise.
    /// `None` when named arguments' parameters are not known.
    fn passed_args(&self, body: &HirBody, at: HirId, args: &[HirId], rest: Option<usize>) -> Option<Vec<Passed>> {
        let mut flat: Vec<Flat> = Vec::with_capacity(args.len());
        let mut named = false;
        let local = |id: HirId| match body.expr(id).kind {
            HirKind::Local(l) => Some(body.local(l).name.clone()),
            _ => None,
        };
        for &arg in args {
            match &body.expr(arg).kind {
                HirKind::Named { value, .. } => {
                    named = true;
                    flat.push(Flat { arg: *value, ty: body.expr(*value).ty.clone(), name: local(*value), dynamic: false });
                }
                HirKind::Spread(inner) => match self.spread_items(body, *inner) {
                    Some(items) => {
                        flat.extend(items.into_iter().map(|(arg, ty)| Flat { arg, ty, name: None, dynamic: false }))
                    }
                    None => flat.push(Flat {
                        arg: *inner,
                        ty: self.spread_element(body, *inner),
                        name: local(*inner).map(|n| format!("...{n}")),
                        dynamic: true,
                    }),
                },
                _ => flat.push(Flat { arg, ty: body.expr(arg).ty.clone(), name: local(arg), dynamic: false }),
            }
        }
        let passed = |params: u64, f: Flat| Passed { params, arg: f.arg, ty: f.ty, name: f.name };
        if !named {
            // Where a spread of unknown length starts that does not fill
            // the rest parameter: it and every later argument may fill any
            // parameter from there on.
            let mut unknown: Option<usize> = None;
            let mut out = Vec::with_capacity(flat.len());
            for (j, f) in flat.into_iter().enumerate() {
                let params = match (unknown, rest) {
                    (Some(k), _) => bits_from(k),
                    (None, Some(r)) if j >= r => bits_from(r) & !bits_from(r + 1),
                    (None, _) if f.dynamic => {
                        unknown = Some(j);
                        bits_from(j)
                    }
                    (None, _) => bits_from(j) & !bits_from(j + 1),
                };
                out.push(passed(params, f));
            }
            return Some(out);
        }
        let (start, end) = body.expr(at).span;
        let record = self.checker.named_arg_slots(start, end)?;
        // A method call's receiver comes before the matched arguments.
        let skip = flat.len().checked_sub(record.slots.len()).filter(|&k| k <= 1)?;
        Some(
            flat.into_iter()
                .enumerate()
                .map(|(i, f)| {
                    let j = if i < skip { i } else { skip + record.slots[i - skip] };
                    passed(bits_from(j) & !bits_from(j + 1), f)
                })
                .collect(),
        )
    }

    /// The parameter a call's trailing arguments are packed into, when the
    /// callee ends in a rest parameter (`T... xs`).
    fn rest_param(
        &self,
        body: &HirBody,
        at: HirId,
        callee_body: Option<usize>,
        key: Option<&str>,
        args: &[HirId],
    ) -> Option<usize> {
        let method = key.is_some_and(|k| k.starts_with('.'));
        let name = match callee_body {
            Some(b) => self.module.bodies[b].name.clone(),
            None if method => {
                let recv = body.expr(*args.first()?);
                let owner = self.checker.class_owner_at_span(recv.span).or_else(|| class_of(recv.ty.as_ref()).map(str::to_string))?;
                format!("{owner}::{}", key?.trim_start_matches('.').trim_end_matches("()"))
            }
            None => key?.to_string(),
        };
        let candidates = self.checker.overload_candidates(&name)?;
        let (fixed, is_rest) = match candidates {
            [c] => (c.fixed_arity, c.is_rest),
            _ => {
                let (start, end) = body.expr(at).span;
                let (fixed, is_rest, _) = self.checker.selected_overload_at(start, end)?;
                (fixed, is_rest)
            }
        };
        // A method's fixed parameters leave out its receiver.
        is_rest.then_some(fixed + usize::from(method))
    }

    /// What each item of `...inner` may be, for a spread whose length is
    /// not known: the element of a `Vec` or an array, `readonly` when the
    /// spread value is.
    fn spread_element(&self, body: &HirBody, inner: HirId) -> Option<Ty> {
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), body.expr(inner).ty.as_ref()?);
        let (bare, ro) = match &ty {
            Ty::Readonly(t) => (t.as_ref(), true),
            t => (t, false),
        };
        let element = match bare {
            Ty::App(head, items) if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE) => items.first()?,
            Ty::Array { element, .. } => element.as_ref(),
            _ => return Some(ty),
        };
        let element = element.clone();
        Some(if ro && may_hold_mutable(Some(&element)) { Ty::Readonly(Box::new(element)) } else { element })
    }

    /// The items `...inner` passes, each with its type: a literal's own
    /// items, or a tuple's or fixed array's elements (each read from
    /// `inner`, so it reaches what `inner` does).
    fn spread_items(&self, body: &HirBody, inner: HirId) -> Option<Vec<(HirId, Option<Ty>)>> {
        use crate::typechecking::ty::ArrayLength;
        let e = body.expr(inner);
        if let HirKind::Make { kind: MakeKind::Array | MakeKind::Tuple, args: items } = &e.kind {
            return Some(items.iter().map(|&it| (it, body.expr(it).ty.clone())).collect());
        }
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), e.ty.as_ref()?);
        let (ty, ro) = match ty {
            Ty::Readonly(t) => (*t, true),
            t => (t, false),
        };
        let wrap = |t: Ty| Some(if ro && may_hold_mutable(Some(&t)) { Ty::Readonly(Box::new(t)) } else { t });
        match ty {
            Ty::Tuple(elems) => Some(elems.into_iter().map(|t| (inner, wrap(t))).collect()),
            Ty::Array { element, length: ArrayLength::Static(n) } => {
                Some((0..n).map(|_| (inner, wrap((*element).clone()))).collect())
            }
            _ => None,
        }
    }

    /// How a diagnostic names parameter `j` of a callee: `self` or
    /// parameter `p`, and the callee's short name.
    fn callee_param(&self, callee_body: Option<usize>, key: Option<&str>, j: u32) -> (String, String) {
        let short = |n: &str| n.rsplit("::").next().unwrap_or(n).trim_end_matches("()").trim_start_matches('.').to_string();
        match callee_body {
            Some(b) => {
                let body = &self.module.bodies[b];
                let name = body.params.get(j as usize).map(|p| body.local(*p).name.clone());
                let param = match name.as_deref() {
                    Some("self") => "`self`".to_string(),
                    Some(n) => format!("parameter `{n}`"),
                    None => format!("argument {}", j + 1),
                };
                (short(&body.name), param)
            }
            None => {
                let param = if j == 0 && key.is_some_and(|k| k.starts_with('.')) {
                    "`self`".to_string()
                } else if j == 0 {
                    "its first argument".to_string()
                } else {
                    format!("argument {}", j + 1)
                };
                (key.map(short).unwrap_or_default(), param)
            }
        }
    }

    /// Why body `index` mutates its parameter `j`, following calls in this
    /// module: "writes field `x`", "calls `inner`, which writes field `x`".
    fn mutation_reason(&self, index: usize, j: u32, summaries: &[Summary]) -> String {
        let mut seen = HashSet::from([(index, j)]);
        let mut parts: Vec<String> = Vec::new();
        let (mut i, mut j) = (index, j);
        loop {
            let w = self.walk_mutations(i, summaries);
            let found = w.mut_causes.unwrap_or_default().into_iter().find(|c| c.bits & (1 << j) != 0);
            let Some(cause) = found else {
                parts.push("mutates it".to_string());
                break;
            };
            match cause.callee {
                Some((b, k)) if seen.insert((b, k)) => {
                    let callee = &self.module.bodies[b];
                    if callee.kind == BodyKind::Lambda {
                        parts.push("passes it to a lambda, which".to_string());
                    } else {
                        let name = local_name(self.module_path, &callee.name).unwrap_or(&callee.name);
                        parts.push(format!("calls `{name}`, which"));
                    }
                    (i, j) = (b, k);
                }
                _ => {
                    parts.push(cause.what);
                    break;
                }
            }
        }
        parts.join(" ")
    }

    /// Add `bits` for the node `at`; `what` says why, when explaining.
    fn note(&self, w: &mut Walk<'_>, at: HirId, bits: u16, what: impl FnOnce() -> String) {
        self.record(w, at, Summary::of(EffectFlags::from_bits(bits)), 0, || None, what);
    }

    /// As [`Self::note`], for a write of `values` into what the place
    /// `base` reaches. One through a parameter path ([`Self::param_path`])
    /// that stores only [`inert`] values is [`Summary::param_only`]: it
    /// cannot make an object the caller owns reach anything shared.
    fn note_write(
        &self,
        w: &mut Walk<'_>,
        at: HirId,
        base: HirId,
        values: &[HirId],
        bits: u16,
        what: impl FnOnce() -> String,
    ) {
        let body = &self.module.bodies[w.index];
        let through = if self.param_path(w, base) && values.iter().all(|v| inert(body, *v)) { bits } else { 0 };
        self.record(w, at, Summary::of(EffectFlags::from_bits(bits)), through, || None, what);
    }

    /// Whether `id` is a parameter the body never reassigns, or a field or
    /// element read from one.
    fn param_path(&self, w: &Walk<'_>, id: HirId) -> bool {
        let body = &self.module.bodies[w.index];
        match &body.expr(id).kind {
            HirKind::Local(l) => body.params.contains(l) && !self.facts[w.index].assigned.contains(l),
            HirKind::Field { base, .. } | HirKind::Index { base, .. } => self.param_path(w, *base),
            _ => false,
        }
    }

    fn is_owned(&self, w: &Walk<'_>, id: HirId) -> bool {
        let body = &self.module.bodies[w.index];
        matches!(body.expr(id).kind, HirKind::Local(l) if self.facts[w.index].owned.contains(&l))
    }

    /// As [`Self::note`], for an access to a mutable static.
    fn note_static(&self, w: &mut Walk<'_>, at: HirId, bits: u16, what: impl FnOnce() -> String) {
        let s = Summary {
            statics: true,
            ..Summary::of(EffectFlags::from_bits(bits))
        };
        self.record(w, at, s, || None, what);
    }

    /// As [`Self::note`], for effects no user sees (a panic, a `defer`).
    fn note_hidden(&self, w: &mut Walk<'_>, at: HirId, bits: u16, what: impl FnOnce() -> String) {
        let s = Summary {
            flags: EffectFlags::from_bits(bits),
            ..Summary::default()
        };
        self.record(w, at, s, 0, || None, what);
    }

    /// Add `s`'s effects (not its latent parameters) for the node `at`;
    /// those of `through` come only from changing what parameters reach.
    fn record(
        &self,
        w: &mut Walk<'_>,
        at: HirId,
        s: Summary,
        through: u16,
        callee: impl FnOnce() -> Option<Called>,
        what: impl FnOnce() -> String,
    ) {
        let owned_bits = s.flags.bits() & OWNED_BITS;
        w.through_params |= owned_bits & through;
        w.elsewhere |= owned_bits & !through;
        w.out.flags = w.out.flags.union(s.flags);
        w.out.visible = w.out.visible.union(s.visible);
        w.out.statics |= s.statics;
        if let Some(causes) = w.causes.as_mut()
            && !s.flags.is_pure()
        {
            let span = self.module.bodies[w.index].expr(at).span;
            causes.push(Cause {
                span,
                what: what(),
                flags: s.flags,
                visible: s.visible,
                callee: callee(),
            });
        }
    }

    fn expr(&self, w: &mut Walk<'_>, id: HirId) {
        let body = &self.module.bodies[w.index];
        let e = body.expr(id);
        match &e.kind {
            HirKind::Call { callee, args } => self.call(w, id, callee, args),
            HirKind::Assign { place, value } => match &body.expr(*place).kind {
                HirKind::Field { base, name } if self.roots(w, *base) != 0 => {
                    let bits = self.roots(w, *base);
                    self.mutate(w, bits, None, || format!("writes field `{name}`"));
                    if !self.is_private(w, *base) {
                        self.note_write(w, id, *base, &[*value], EffectFlags::HEAP_MUT, || {
                            format!("writes field `{name}` of a shared object")
                        })
                    }
                }
                HirKind::Index { base, .. } if self.roots(w, *base) != 0 => {
                    let bits = self.roots(w, *base);
                    self.mutate(w, bits, None, || "writes an element".to_string());
                    if !self.is_private(w, *base) {
                        self.note_write(w, id, *base, &[*value], EffectFlags::HEAP_MUT, || {
                            "writes an element of a shared array".into()
                        })
                    }
                }
                HirKind::Field { base, name } if !self.is_private(w, *base) => {
                    self.note(w, id, EffectFlags::HEAP_MUT, || format!("writes field `{name}` of a shared object"))
                }
                HirKind::Index { base, .. } if !self.is_private(w, *base) => {
                    self.note(w, id, EffectFlags::HEAP_MUT, || "writes an element of a shared array".into())
                }
                HirKind::Local(l) if body.local(*l).kind == LocalKind::Capture => {
                    let name = &body.local(*l).name;
                    self.note(w, id, EffectFlags::HEAP_MUT, || format!("writes captured `{name}`"))
                }
                HirKind::Global { name, .. } => {
                    self.note_static(w, id, EffectFlags::HEAP_MUT, || format!("writes static `{name}`"))
                }
                _ => {}
            },
            HirKind::Append { base, value } if !self.is_private(w, *base) => {
                let bits = self.roots(w, *base);
                self.mutate(w, bits, None, || "appends to an array".to_string());
                self.note_write(w, id, *base, &[*value], EffectFlags::HEAP_MUT | EffectFlags::RESIZE, || {
                    "appends to a shared array".into()
                })
            }
            // A lambda that mutates what it captured mutates the captured
            // local's values here, whenever it is called.
            HirKind::Lambda { body: inner } => {
                let lambda = &self.module.bodies[*inner];
                let mutates = w.summaries[*inner].mutates >> lambda.params.len().min(63);
                for (k, (outer, _)) in lambda.captures.iter().enumerate().take(64) {
                    if mutates & (1 << k) != 0 {
                        let bits = local_roots(body, &self.facts[w.index].derived, *outer);
                        let name = &body.local(*outer).name;
                        self.mutate(w, bits, Some((*inner, (lambda.params.len() + k) as u32)), || {
                            format!("captures `{name}` in a lambda that mutates it")
                        });
                    }
                }
            }
            HirKind::Yield { .. } => self.note(w, id, EffectFlags::YIELD | EffectFlags::RESIZE, || "yields".into()),
            HirKind::Resume { .. } => {
                self.note(w, id, EffectFlags::YIELD | EffectFlags::RESIZE, || "resumes a generator".into())
            }
            HirKind::ForIn { kind, .. } => {
                if !matches!(
                    kind,
                    Some(ForInKind::Array | ForInKind::Tuple { .. } | ForInKind::Range { .. })
                ) {
                    self.note(w, id, EffectFlags::UNKNOWN | EffectFlags::RESIZE, || {
                        "iterates a generator or a custom iterator".into()
                    });
                }
            }
            // As the AST walk: the deferred body is still walked.
            HirKind::Defer { .. } => self.note_hidden(w, id, EffectFlags::UNKNOWN, || "has a `defer`".into()),
            HirKind::Builtin { op, .. } => match op {
                // Not a user-visible effect: bounds checks panic almost
                // anywhere.
                Builtin::Panic => self.note_hidden(w, id, EffectFlags::UNKNOWN, || "may panic".into()),
                Builtin::Declare | Builtin::Invoke | Builtin::Dload => {
                    self.note(w, id, EffectFlags::FFI | EffectFlags::RESIZE, || "calls foreign code".into())
                }
                Builtin::TypeOf | Builtin::Done | Builtin::Default | Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout => {}
                Builtin::Print => self.note(w, id, EffectFlags::WRITE, || "prints".into()),
            },
            // Workers of a parallel region have no statics, and another
            // call may change one (coil-lang#793).
            HirKind::Global { name, .. } if self.checker.is_mutable_static(name) => {
                self.note_static(w, id, EffectFlags::HOST, || format!("reads static `{name}`"))
            }
            // A trait operator runs user code this pass does not resolve.
            HirKind::Bin { op: BinOp::Overloaded(op), lhs, rhs } => {
                self.note(w, id, EffectFlags::UNKNOWN, || format!("uses a trait operator `{op}`"));
                self.operator_mutates(w, id, op, *lhs, *rhs);
            }
            HirKind::Unsupported(what) => {
                self.note(w, id, UNKNOWN.bits(), || format!("uses {what}, which this analysis does not model"))
            }
            _ => {}
        }
        for k in children(body, id) {
            self.expr(w, k);
        }
    }

    fn is_private(&self, w: &Walk<'_>, base: HirId) -> bool {
        let body = &self.module.bodies[w.index];
        let facts = &self.facts[w.index];
        matches!(body.expr(base).kind, HirKind::Local(l) if facts.private.contains(&l) || facts.owned.contains(&l))
    }

    fn call(&self, w: &mut Walk<'_>, at: HirId, callee: &Callee, args: &[HirId]) {
        let body = &self.module.bodies[w.index];
        match callee {
            Callee::Named { name, def, .. } => {
                let mut s = self.named(w, name, *def);
                let callee = self.local_body(name, *def);
                if callee.is_none() {
                    s = open_mode_summary(body, name, args, s);
                }
                self.apply(w, at, name, s, callee, args);
                let trait_decl = callee.is_none() && self.known_named(w, name, *def).is_none() && self.is_trait_method(name);
                // `Trait::m(p)` on a concrete receiver runs its impl, whose
                // body says what it mutates, as `p.m()` (#1647).
                if trait_decl && let Some((mutates, impl_body)) = self.named_instance_mutates(w, name, args) {
                    self.call_mutates(w, at, name, mutates, impl_body, Some(name), args, false);
                    return;
                }
                self.call_mutates(w, at, name, s.mutates, callee, Some(name), args, trait_decl);
            }
            Callee::Method { name } => {
                let Some(&recv) = args.first() else { return };
                let recv_ty = body.expr(recv).ty.as_ref();
                // The structural length read has no effect.
                if name == "len" && args.len() == 1 && recv_ty.is_some_and(Checker::is_structural_len_ty_for_codegen) {
                    return;
                }
                if is_vec(recv_ty) {
                    if name == "capacity" {
                        return;
                    }
                    if VEC_MUTATORS.contains(&name.as_str()) {
                        let bits = self.roots(w, recv);
                        self.mutate(w, bits, None, || format!("calls `.{name}()` on a `Vec`"));
                        if !self.is_private(w, recv) {
                            self.note_write(w, at, recv, &args[1..], EffectFlags::HEAP_MUT | EffectFlags::RESIZE, || {
                                format!("calls `.{name}()` on a shared `Vec`")
                            });
                        }
                        return;
                    }
                }
                let shown = format!(".{name}()");
                match self.method(w, recv, name) {
                    Some((s, callee)) => {
                        self.apply(w, at, &shown, s, callee, args);
                        let trait_decl = callee.is_none() && self.method_is_trait_dispatch(w, recv, name);
                        self.call_mutates(w, at, &shown, s.mutates, callee, Some(&shown), args, trait_decl);
                    }
                    // As the AST walk: an unresolved method is unknown code,
                    // but `len` / `capacity` cannot resize.
                    None => {
                        let bits = if matches!(name.as_str(), "len" | "capacity") {
                            EffectFlags::UNKNOWN
                        } else {
                            EffectFlags::UNKNOWN | EffectFlags::RESIZE
                        };
                        self.note(w, at, bits, || format!("calls `{shown}`, which this analysis does not resolve"));
                        // A trait method whose trait leaves its effects
                        // undeclared still says which parameters it keeps
                        // `readonly`.
                        if let Some((mutates, callee)) = self.trait_call_mutates(w, recv, name) {
                            let trait_decl = callee.is_none();
                            self.call_mutates(w, at, &shown, mutates, callee, Some(&shown), args, trait_decl);
                        }
                    }
                }
            }
            Callee::Value(f) => {
                // A function value may mutate each argument its type does
                // not declare `readonly`.
                let by_type = fn_type_mutates(body.expr(*f).ty.as_ref());
                if let Some(i) = self.param_index(w.index, *f) {
                    w.out.latent |= 1 << i;
                    self.call_mutates(w, at, "a function value", by_type, None, None, args, false);
                    return;
                }
                match self.fn_value(w, *f) {
                    Some(s) => {
                        let callee = self.fn_value_body(w.index, *f);
                        self.apply(w, at, "a function value", s, callee, args);
                        self.call_mutates(w, at, "a function value", s.mutates, callee, None, args, false);
                    }
                    None => {
                        self.note(w, at, UNKNOWN.bits(), || "calls a function value it cannot resolve".into());
                        self.call_mutates(w, at, "a function value", by_type, None, None, args, false);
                    }
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

    /// The summary of calling `callee` (shown as `shown`) with `args`,
    /// folded into `w`: its own effects, plus for each parameter it calls,
    /// the effects of the function passed there.
    fn apply(&self, w: &mut Walk<'_>, at: HirId, shown: &str, callee: Summary, body: Option<usize>, args: &[HirId]) {
        let link = || {
            Some(Called {
                name: shown.to_string(),
                body,
            })
        };
        let body = &self.module.bodies[w.index];
        let positional = args
            .iter()
            .all(|a| !matches!(body.expr(*a).kind, HirKind::Named { .. } | HirKind::Spread(_)));
        let (callee_cost, through) = self.owned_discharge(w, callee, args, positional);
        self.record(w, at, callee_cost, through, link, || format!("calls `{shown}`"));
        if callee.latent == 0 {
            return;
        }
        for i in 0..64 {
            if callee.latent & (1 << i) == 0 {
                continue;
            }
            let arg = args.get(i).copied().filter(|_| positional);
            let Some(arg) = arg else {
                self.note(w, at, UNKNOWN.bits(), || format!("passes `{shown}` a function it cannot resolve"));
                continue;
            };
            if let Some(j) = self.param_index(w.index, arg) {
                w.out.latent |= 1 << j;
                continue;
            }
            match self.fn_value(w, arg) {
                // A function that calls its own function parameters gets
                // arguments this call site does not see.
                Some(s) if s.latent == 0 => {
                    let index = w.index;
                    let passed = || {
                        Some(Called {
                            name: self.fn_value_name(index, arg),
                            body: self.fn_value_body(index, arg),
                        })
                    };
                    self.record(w, arg, s, 0, passed, || format!("passes `{shown}` a function that is not pure"))
                }
                _ => self.note(w, arg, UNKNOWN.bits(), || format!("passes `{shown}` a function it cannot resolve")),
            }
        }
    }

    /// What a call of `callee` costs here, and which of that comes only
    /// from changing what this body's parameters reach (design §3.7,
    /// #1648). When the callee's `mutate` comes only from changing what
    /// its parameters reach, and each argument it mutates is an object
    /// this body owns, those writes are invisible to every caller and cost
    /// nothing. When some are this body's own parameter paths instead,
    /// the writes are this body's parameter writes.
    fn owned_discharge(&self, w: &Walk<'_>, callee: Summary, args: &[HirId], positional: bool) -> (Summary, u16) {
        let bits = callee.flags.bits() & OWNED_BITS & callee.param_only;
        if bits == 0 || callee.mutates == 0 || !positional {
            return (callee, 0);
        }
        let mut all_owned = true;
        for j in 0..64 {
            if callee.mutates & (1 << j) == 0 {
                continue;
            }
            match args.get(j) {
                Some(&a) if self.is_owned(w, a) => {}
                Some(&a) if self.param_path(w, a) => all_owned = false,
                _ => return (callee, 0),
            }
        }
        if !all_owned {
            return (callee, bits);
        }
        let keep = |f: EffectFlags| EffectFlags::from_bits(f.bits() & !bits);
        let cost = Summary {
            flags: keep(callee.flags),
            visible: keep(callee.visible),
            ..callee
        };
        (cost, 0)
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

    /// This module's body for the function value `id`, when it has one.
    fn fn_value_body(&self, index: usize, id: HirId) -> Option<usize> {
        let body = &self.module.bodies[index];
        match &body.expr(id).kind {
            HirKind::Lambda { body: inner } => Some(*inner),
            HirKind::Global { name, def } => self.local_body(name, *def),
            HirKind::Local(l) => self.fn_value_body(index, *self.facts[index].fn_values.get(l)?),
            _ => None,
        }
    }

    /// How a call chain shows the function value `id`.
    fn fn_value_name(&self, index: usize, id: HirId) -> String {
        let body = &self.module.bodies[index];
        match &body.expr(id).kind {
            HirKind::Global { name, .. } => name.clone(),
            HirKind::Local(l) => match self.facts[index].fn_values.get(l) {
                Some(init) if matches!(body.expr(*init).kind, HirKind::Global { .. }) => {
                    self.fn_value_name(index, *init)
                }
                _ => body.local(*l).name.clone(),
            },
            _ => "a lambda".into(),
        }
    }

    /// This module's body for the function `name`.
    fn local_body(&self, name: &str, def: Option<DefId>) -> Option<usize> {
        let def = def.or_else(|| {
            (self.checker.current_module_name() == self.module_path)
                .then(|| self.checker.def_id_of(name))
                .flatten()
        });
        if let Some(i) = def.and_then(|d| self.by_def.get(&d)) {
            return Some(*i);
        }
        self.by_name.get(name).copied().flatten()
    }

    /// What a call of trait method `method` costs, from the declarations
    /// of every trait with that method (of `trait_name` only, when given).
    /// `None` when there is no such trait or one leaves it undeclared.
    fn trait_method(&self, trait_name: Option<&str>, method: &str) -> Option<Summary> {
        let short = |n: &str| short_name(n).to_string();
        let full: Vec<String> = self
            .checker
            .generics()
            .typeclasses
            .iter()
            .filter(|(name, def)| {
                trait_name.is_none_or(|t| short(t) == short(name)) && def.methods.iter().any(|m| m.name == method)
            })
            .map(|(name, _)| name.clone())
            .collect();
        if full.is_empty() {
            return None;
        }
        // A call through the trait mutates every parameter the trait does
        // not declare `readonly` (#1558).
        let mutates = full.iter().fold(0u64, |m, t| m | self.trait_method_mutates(t, method));
        let traits: Vec<String> = full.iter().map(|n| short(n)).collect();
        let mut visible = EffectFlags::empty();
        for t in &traits {
            let decl = self
                .module
                .trait_effects
                .iter()
                .chain(&self.program.traits)
                .find(|d| short(&d.trait_name) == *t && d.method == method)?;
            visible = visible.union(allowed(&decl.declared.as_ref()?.names));
        }
        // Panics in the impls stay unknown to auto-par.
        Some(Summary {
            flags: visible.union(UNKNOWN),
            visible,
            latent: 0,
            mutates,
            // An impl may reach a static wherever its declaration allows
            // reads or writes.
            statics: visible.contains(EffectFlags::HOST) || visible.contains(EffectFlags::HEAP_MUT),
            param_only: 0,
        })
    }

    /// What a call of trait method `method` (of `trait_name` only, when
    /// given) mutates, whether or not the trait declares its effects.
    fn trait_mutates(&self, trait_name: Option<&str>, method: &str) -> Option<u64> {
        let short = |n: &str| short_name(n).to_string();
        let mut found = None;
        for (name, def) in self.checker.generics().typeclasses.iter() {
            if trait_name.is_none_or(|t| short(t) == short(name)) && def.methods.iter().any(|m| m.name == method) {
                found = Some(found.unwrap_or(0) | self.trait_method_mutates(name, method));
            }
        }
        found
    }

    /// What `recv.name()` mutates when it dispatches through a trait: the
    /// impl body's own summary on a concrete receiver whose impl was seen,
    /// else what the trait declares.
    fn trait_call_mutates(&self, w: &Walk<'_>, recv: HirId, name: &str) -> Option<(u64, Option<usize>)> {
        let e = self.module.bodies[w.index].expr(recv);
        if let Some(Ty::Existential { class }) = e.ty.as_ref().map(crate::typechecking::ty::strip_readonly) {
            return self.trait_mutates(Some(class), name).map(|m| (m, None));
        }
        let owner = self.checker.class_owner_at_span(e.span).or_else(|| class_of(e.ty.as_ref()).map(str::to_string));
        if let Some(owner) = owner.as_deref()
            && let Some(found) = self.instance_mutates(w, owner, name)
        {
            return Some(found);
        }
        self.trait_mutates(None, name).map(|m| (m, None))
    }

    /// The parameters of trait method `trait_name::method` that its trait
    /// does not declare `readonly`.
    fn trait_method_mutates(&self, trait_name: &str, method: &str) -> u64 {
        let arity = self.checker.typeclass_method_scheme(trait_name, method).map_or(64, |s| {
            let mut n = 0;
            let mut ty = &s.ty;
            while let Ty::Fun(_, r) = ty {
                n += 1;
                ty = r;
            }
            n
        });
        let all = if arity >= 64 { u64::MAX } else { (1u64 << arity) - 1 };
        all & !self.checker.trait_readonly_params(trait_name, method)
    }

    /// Whether `name` (`m`, `Trait::m`) names a trait method.
    fn is_trait_method(&self, name: &str) -> bool {
        let (owner, m) = name.rsplit_once("::").map_or((None, name), |(o, m)| (Some(o), m));
        self.checker.generics().typeclasses.iter().any(|(t, def)| {
            owner.is_none_or(|o| short_name(o) == short_name(t)) && def.methods.iter().any(|d| d.name == m)
        })
    }

    /// Whether `recv.name()` dispatches through a trait rather than to a
    /// body this analysis sees.
    fn method_is_trait_dispatch(&self, w: &Walk<'_>, recv: HirId, name: &str) -> bool {
        let e = self.module.bodies[w.index].expr(recv);
        if matches!(e.ty.as_ref().map(crate::typechecking::ty::strip_readonly), Some(Ty::Existential { .. })) {
            return true;
        }
        self.is_trait_method(name)
    }

    /// What trait method `name` (`m`, `Trait::m`) mutates when called on
    /// `args`, from the impl body of its first argument's class, when that
    /// is a class whose impl was seen.
    fn named_instance_mutates(&self, w: &Walk<'_>, name: &str, args: &[HirId]) -> Option<(u64, Option<usize>)> {
        let recv = *args.first()?;
        let body = &self.module.bodies[w.index];
        if matches!(body.expr(recv).kind, HirKind::Named { .. } | HirKind::Spread(_)) {
            return None;
        }
        let owner = class_of(body.expr(recv).ty.as_ref())?;
        match name.rsplit_once("::") {
            Some((trait_name, method)) => self
                .checker
                .generics()
                .typeclasses
                .keys()
                .filter(|t| short_name(t) == short_name(trait_name))
                .find_map(|t| self.instance_mutates_of(w, t, owner, method)),
            None => self.instance_mutates(w, owner, name),
        }
    }

    /// Fold what the trait operator `lhs op rhs` mutates into `w`, as the
    /// call `lhs.method(rhs)` of its instance would (#1645). Only an operand
    /// of a user class whose impl body was seen says: the built-in operator
    /// traits cannot declare their parameters `readonly`, so a bound
    /// operand's operator is not checked.
    fn operator_mutates(&self, w: &mut Walk<'_>, at: HirId, op: &str, lhs: HirId, rhs: HirId) {
        let (class, method) = match op {
            "==" => ("Eq", "eq"),
            "!=" => ("Eq", "ne"),
            "<" => ("Lt", "lt"),
            ">" => ("Gt", "gt"),
            "<=" => ("Le", "le"),
            ">=" => ("Ge", "ge"),
            _ => match crate::typechecking::generics::Generics::operator_trait(op) {
                Some(found) => found,
                None => return,
            },
        };
        let e = self.module.bodies[w.index].expr(lhs);
        let Some(owner) = class_of(e.ty.as_ref()) else { return };
        let Some((mutates, callee)) = self.instance_mutates_of(w, class, owner, method) else { return };
        let shown = format!("{class}::{method}");
        self.call_mutates(w, at, &shown, mutates, callee, Some(&shown), &[lhs, rhs], false);
    }

    /// What the impl of trait method `name` for class `owner` mutates, when
    /// its body was seen (this module's or an earlier one's).
    fn instance_mutates(&self, w: &Walk<'_>, owner: &str, name: &str) -> Option<(u64, Option<usize>)> {
        let traits: Vec<&String> = self
            .checker
            .generics()
            .typeclasses
            .iter()
            .filter(|(_, def)| def.methods.iter().any(|m| m.name == name))
            .map(|(t, _)| t)
            .collect();
        traits.into_iter().find_map(|t| self.instance_mutates_of(w, t, owner, name))
    }

    /// What the impl of `trait_name::name` for class `owner` mutates, when
    /// its body was seen.
    fn instance_mutates_of(&self, w: &Walk<'_>, trait_name: &str, owner: &str, name: &str) -> Option<(u64, Option<usize>)> {
        for tn in [trait_name, short_name(trait_name)] {
            let key = format!("{tn} for {owner}::{name}");
            if let Some(Some(i)) = self.by_name.get(key.as_str()) {
                return Some((w.summaries[*i].mutates, Some(*i)));
            }
            let qualified = format!("{}::{key}", self.module_path);
            for k in [key.as_str(), qualified.as_str()] {
                if let Some(s) = self.program.by_name.get(k) {
                    return Some((s.mutates, None));
                }
            }
        }
        None
    }

    fn named(&self, w: &Walk<'_>, name: &str, def: Option<DefId>) -> Summary {
        if let Some(s) = self.known_named(w, name, def) {
            return s;
        }
        let member = name.rsplit_once("::");
        let (owner, method) = (member.map(|(owner, _)| owner), member.map_or(name, |(_, m)| m));
        if let Some(s) = self.trait_method(owner, method) {
            return s;
        }
        let mut s = self.named_host(name);
        // An undeclared trait method still keeps its `readonly` parameters;
        // a host native that writes the heap may write any argument.
        s.mutates = match self.trait_mutates(owner, method) {
            Some(m) => m,
            None if s.flags.contains(EffectFlags::HEAP_MUT) => u64::MAX,
            None => 0,
        };
        s
    }

    /// [`Self::named`] for a name that is no user function or trait method.
    fn named_host(&self, name: &str) -> Summary {
        if is_vec_ctor(name) || name == "Vec::from" {
            return Summary::default();
        }
        if let Some((owner, member)) = name.rsplit_once("::")
            && self.checker.tag_for(owner, member).is_some()
        {
            return Summary::default();
        }
        // `use string::{to_bytes as tb}`: classify the builtin by its export
        // name, not the local alias (#1075).
        if self.checker.current_module_name() == self.module_path
            && let Some(export) = self.checker.scope_binding(name)
        {
            return Summary::of(classify_host_name(export.short_name()));
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

    /// The summary of `recv.name(..)` when the receiver is a user class or
    /// a trait existential, and the method's body when it is this module's.
    fn method(&self, w: &Walk<'_>, recv: HirId, name: &str) -> Option<(Summary, Option<usize>)> {
        let body = &self.module.bodies[w.index];
        let e = body.expr(recv);
        if let Some(Ty::Existential { class }) = e.ty.as_ref().map(crate::typechecking::ty::strip_readonly) {
            return self.trait_method(Some(class), name).map(|s| (s, None));
        }
        let owner = self
            .checker
            .class_owner_at_span(e.span)
            .or_else(|| class_of(e.ty.as_ref()).map(str::to_string));
        if let Some(owner) = owner.as_deref() {
            let key = format!("{owner}::{name}");
            if let Some(Some(i)) = self.by_name.get(key.as_str()) {
                return Some((w.summaries[*i], Some(*i)));
            }
            let qualified = format!("{}::{key}", self.module_path);
            for k in [key.as_str(), qualified.as_str()] {
                if let Some(&s) = self.program.by_name.get(k) {
                    return Some((s, None));
                }
                if let Some(fx) = self.checker.program_method_effects.get(k) {
                    return Some((Summary::of(*fx), None));
                }
            }
        }
        // Not an inherent method: a trait method on a bound or a concrete
        // receiver costs what the trait declares, as `name(recv)` does. On
        // a concrete receiver whose impl body is known, that body says what
        // it mutates.
        let mut s = self.trait_method(None, name)?;
        if let Some(owner) = owner.as_deref()
            && let Some((mutates, _)) = self.instance_mutates(w, owner, name)
        {
            s.mutates = mutates;
        }
        Some((s, None))
    }
}

#[cfg(test)]
#[path = "effects.tests.rs"]
mod tests;
