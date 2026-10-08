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
    /// What a user sees and declares (`uses {…}`): `flags` without panics,
    /// `defer`, GC, generator yields and resizes.
    pub visible: EffectFlags,
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
        // Host tables pair `unknown` with host state to be safe; unknown
        // already says it all.
        let hidden = if flags.contains(EffectFlags::UNKNOWN) { HIDDEN | EffectFlags::HOST } else { HIDDEN };
        Self {
            flags,
            visible: EffectFlags::from_bits(flags.bits() & !hidden),
            latent: 0,
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

/// Summaries of every body compiled so far, by full body name
/// (`module::f`, `module::Owner::m`) and by [`DefId`] for functions.
#[derive(Debug, Default)]
pub struct ProgramEffects {
    fns: HashMap<DefId, Summary>,
    by_name: HashMap<String, Summary>,
    /// Trait methods of every module so far, with their declarations.
    traits: Vec<super::TraitEffects>,
}

impl ProgramEffects {
    /// Keep `module`'s summaries for the modules compiled after it.
    /// Some module so far declares effects on a trait method.
    pub fn has_trait_declarations(&self) -> bool {
        self.traits.iter().any(|t| t.declared.is_some())
    }

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
    let actual = analyze_par_fork_sites(ast, pure);
    let mut blocked: Vec<String> = analyze_par_fork_sites(ast, &all)
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

    /// The first reason body `index` is not pure, as `calls `x` (write)`.
    pub fn first_reason(&self, index: usize) -> Option<String> {
        let s = self.summaries[index];
        if let Some(cause) = self.explain(index).into_iter().next() {
            return Some(format!("{} ({})", cause.what, effect_names(cause.flags).join(", ")));
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
    /// Why the body has its effects, when [`explain`] asked.
    causes: Option<Vec<Cause>>,
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
        };
        if let Some(root) = body.root {
            self.expr(&mut w, root);
        }
        w
    }

    /// Add `bits` for the node `at`; `what` says why, when explaining.
    fn note(&self, w: &mut Walk<'_>, at: HirId, bits: u16, what: impl FnOnce() -> String) {
        self.record(w, at, Summary::of(EffectFlags::from_bits(bits)), || None, what);
    }

    /// As [`Self::note`], for effects no user sees (a panic, a `defer`).
    fn note_hidden(&self, w: &mut Walk<'_>, at: HirId, bits: u16, what: impl FnOnce() -> String) {
        let s = Summary {
            flags: EffectFlags::from_bits(bits),
            ..Summary::default()
        };
        self.record(w, at, s, || None, what);
    }

    /// Add `s`'s effects (not its latent parameters) for the node `at`.
    fn record(
        &self,
        w: &mut Walk<'_>,
        at: HirId,
        s: Summary,
        callee: impl FnOnce() -> Option<Called>,
        what: impl FnOnce() -> String,
    ) {
        w.out.flags = w.out.flags.union(s.flags);
        w.out.visible = w.out.visible.union(s.visible);
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
            HirKind::Assign { place, .. } => match &body.expr(*place).kind {
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
                    self.note(w, id, EffectFlags::HEAP_MUT, || format!("writes static `{name}`"))
                }
                _ => {}
            },
            HirKind::Append { base, .. } if !self.is_private(w, *base) => {
                self.note(w, id, EffectFlags::HEAP_MUT | EffectFlags::RESIZE, || "appends to a shared array".into())
            }
            HirKind::Yield { .. } => self.note(w, id, EffectFlags::YIELD | EffectFlags::RESIZE, || "yields".into()),
            HirKind::Resume { .. } => {
                self.note(w, id, EffectFlags::YIELD | EffectFlags::RESIZE, || "resumes a generator".into())
            }
            HirKind::ForIn { kind, .. } => {
                if !matches!(
                    kind,
                    Some(ForInKind::Array | ForInKind::Tuple { .. } | ForInKind::Dict | ForInKind::Range { .. })
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
                Builtin::TypeOf | Builtin::Done | Builtin::Readonly | Builtin::Default => {}
            },
            // Workers of a parallel region have no statics, and another
            // call may change one (coil-lang#793).
            HirKind::Global { name, .. } if self.checker.is_mutable_static(name) => {
                self.note(w, id, EffectFlags::HOST, || format!("reads static `{name}`"))
            }
            // A trait operator runs user code this pass does not resolve.
            HirKind::Bin { op: BinOp::Overloaded(op), .. } => {
                self.note(w, id, EffectFlags::UNKNOWN, || format!("uses a trait operator `{op}`"))
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
        matches!(body.expr(base).kind, HirKind::Local(l) if self.facts[w.index].private.contains(&l))
    }

    fn call(&self, w: &mut Walk<'_>, at: HirId, callee: &Callee, args: &[HirId]) {
        let body = &self.module.bodies[w.index];
        match callee {
            // The host table keeps `len` unknown for any receiver; a
            // length read has no effect.
            Callee::Named { name, .. } if name == "len" && args.len() == 1 => {}
            // A failed `assert` panics, which is no user-visible effect.
            Callee::Named { name, def, .. } if name == "assert" && self.local_body(name, *def).is_none() => {
                self.note_hidden(w, at, EffectFlags::UNKNOWN | EffectFlags::HOST, || "may panic".into())
            }
            Callee::Named { name, def, .. } => {
                let s = self.named(w, name, *def);
                let callee = self.local_body(name, *def);
                self.apply(w, at, name, s, callee, args);
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
                            self.note(w, at, EffectFlags::HEAP_MUT | EffectFlags::RESIZE, || {
                                format!("calls `.{name}()` on a shared `Vec`")
                            });
                        }
                        return;
                    }
                }
                let shown = format!(".{name}()");
                match self.method(w, recv, name) {
                    Some((s, callee)) => self.apply(w, at, &shown, s, callee, args),
                    // As the AST walk: an unresolved method is unknown code,
                    // but `len` / `capacity` cannot resize.
                    None => {
                        let bits = if matches!(name.as_str(), "len" | "capacity") {
                            EffectFlags::UNKNOWN
                        } else {
                            EffectFlags::UNKNOWN | EffectFlags::RESIZE
                        };
                        self.note(w, at, bits, || format!("calls `{shown}`, which this analysis does not resolve"));
                    }
                }
            }
            Callee::Value(f) => {
                if let Some(i) = self.param_index(w.index, *f) {
                    w.out.latent |= 1 << i;
                    return;
                }
                match self.fn_value(w, *f) {
                    Some(s) => {
                        let callee = self.fn_value_body(w.index, *f);
                        self.apply(w, at, "a function value", s, callee, args)
                    }
                    None => self.note(w, at, UNKNOWN.bits(), || "calls a function value it cannot resolve".into()),
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
        self.record(w, at, callee, link, || format!("calls `{shown}`"));
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
                    self.record(w, arg, s, passed, || format!("passes `{shown}` a function that is not pure"))
                }
                _ => self.note(w, arg, UNKNOWN.bits(), || format!("passes `{shown}` a function it cannot resolve")),
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
        let traits: Vec<String> = self
            .checker
            .generics()
            .typeclasses
            .iter()
            .filter(|(name, def)| {
                trait_name.is_none_or(|t| short(t) == short(name)) && def.methods.iter().any(|m| m.name == method)
            })
            .map(|(name, _)| short(name))
            .collect();
        if traits.is_empty() {
            return None;
        }
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
        })
    }

    fn named(&self, w: &Walk<'_>, name: &str, def: Option<DefId>) -> Summary {
        if let Some(s) = self.known_named(w, name, def) {
            return s;
        }
        let member = name.rsplit_once("::");
        if let Some(s) = self.trait_method(member.map(|(owner, _)| owner), member.map_or(name, |(_, m)| m)) {
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

    /// The summary of `recv.name(..)` when the receiver is a user class or
    /// a trait existential, and the method's body when it is this module's.
    fn method(&self, w: &Walk<'_>, recv: HirId, name: &str) -> Option<(Summary, Option<usize>)> {
        let body = &self.module.bodies[w.index];
        let e = body.expr(recv);
        if let Some(Ty::Existential { class }) = e.ty.as_ref() {
            return self.trait_method(Some(class), name).map(|s| (s, None));
        }
        let owner = self
            .checker
            .class_owner_at_span(e.span)
            .or_else(|| class_of(e.ty.as_ref()).map(str::to_string))?;
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
        None
    }
}

#[cfg(test)]
#[path = "effects.tests.rs"]
mod tests;
