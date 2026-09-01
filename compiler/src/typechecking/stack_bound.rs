//! Static recursion-depth / operand-stack bound analysis.
//!
//! Proves call-frame depth when a recursive function has a decreasing int/byte
//! **measure** parameter (possibly among multiple args), a recognizable base
//! case, and **known** entry measure values (literals, intra-proc const bindings,
//! or shallow interprocedural wrappers). When unprovable, `#[max_depth(N)]` is
//! required.

use std::collections::{BTreeSet, HashMap, HashSet};

use parser::ast::{AttrArgs, AttrLit, Attribute, Expression, Output};
use reporting::{ErrorCode, Message};

use crate::const_fold::{ConstValue, eval_expr};

use super::purity::analyze_recursive_fns;

/// Default / minimum operand-stack capacity for programs without deep recursion.
pub const DEFAULT_OPERAND_STACK_SLOTS: u32 = 256;

/// Hard ceiling matching [`machine::MAX_OPERAND_STACK_SLOTS`].
pub const MAX_OPERAND_STACK_SLOTS: u32 = 1_048_576;

/// Conservative per-frame slot estimate when IL footprints are not yet known.
const DEFAULT_FRAME_SLOTS: u32 = 16;

/// Compute operand-stack slots from a max live-frame count.
pub fn operand_slots_for_frames(max_frames: u32) -> u32 {
    let need = max_frames
        .saturating_mul(DEFAULT_FRAME_SLOTS)
        .saturating_add(DEFAULT_FRAME_SLOTS);
    need.max(DEFAULT_OPERAND_STACK_SLOTS)
        .min(MAX_OPERAND_STACK_SLOTS)
}

/// Proven or attributed max live frames for one recursive function.
///
/// Recorded on [`StackBoundReport`]; frame sizing uses `operand_slots_needed` only.
#[derive(Debug, Clone)]
#[allow(dead_code)] // per-fn rows are test/diagnostic; not wired into frames
pub struct FnStackBound {
    pub fn_name: String,
    /// Maximum simultaneous frames of this function (and its SCC peers).
    pub max_frames: u32,
    /// How the bound was obtained.
    pub source: BoundSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundSource {
    /// Decreasing measure + known entry args.
    Proven,
    /// User `#[max_depth(N)]`.
    Attribute,
    /// All recursive calls are tail calls (`return f(...)`).
    TailOnly,
}

/// Result of whole-program recursion bound checking.
#[derive(Debug, Default)]
pub struct StackBoundReport {
    pub messages: Vec<Message>,
    pub bounds: Vec<FnStackBound>,
    /// Conservative operand-stack slots required (`max_frames * frame_slots` + slack).
    pub operand_slots_needed: u32,
}

/// How the measure parameter decreases on each self-call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MeasureStep {
    /// `param - k` with fixed positive `k`.
    Subtract(i64),
    /// `param / 2` or `param >> 1`.
    Half,
}

/// Unified decreasing-measure shape for stack-bound proofs.
#[derive(Debug, Clone)]
struct RecMeasureShape {
    #[allow(dead_code)]
    measure_param: String,
    measure_index: usize,
    base_bound: Option<i64>,
    /// Minimum positive decrease across all self-calls.
    step: MeasureStep,
    #[allow(dead_code)]
    self_calls: usize,
}

/// Analyze recursion depth bounds; emit errors when `#[max_depth]` is required.
pub fn analyze_stack_bounds(ast: &Output<'_>) -> StackBoundReport {
    let mut report = StackBoundReport {
        operand_slots_needed: DEFAULT_OPERAND_STACK_SLOTS,
        ..StackBoundReport::default()
    };

    let recursive = analyze_recursive_fns(ast);
    if recursive.is_empty() {
        return report;
    }

    let mut measure_shapes: HashMap<String, RecMeasureShape> = HashMap::new();
    let mut tail_only: HashSet<String> = HashSet::new();
    let mut fn_meta: HashMap<String, FnMeta<'_>> = HashMap::new();
    collect_fn_meta(ast, &recursive, &mut fn_meta, &mut measure_shapes, &mut tail_only);

    // Wrapper / entry const params (interprocedural), then entry sites.
    let wrapper_consts = propagate_const_args(ast, &recursive);
    let mut const_args: HashMap<String, BTreeSet<i64>> = HashMap::new();
    let mut dynamic_calls: HashSet<String> = HashSet::new();
    collect_rec_entry_sites(
        ast,
        &recursive,
        &measure_shapes,
        &wrapper_consts,
        &mut const_args,
        &mut dynamic_calls,
    );

    let mut max_frames_any: u32 = 1;

    for name in &recursive {
        let span = fn_meta
            .get(name)
            .map(|m| m.span.clone())
            .unwrap_or(0..0);
        let attr_depth = fn_meta.get(name).and_then(|m| m.max_depth);
        let attr_err = fn_meta.get(name).and_then(|m| m.attr_error.clone());
        if let Some(msg) = attr_err {
            report.messages.push(msg);
            continue;
        }

        let is_self = fn_meta.get(name).map(|m| m.self_recursive).unwrap_or(false);
        if !is_self {
            match attr_depth {
                Some(d) => {
                    max_frames_any = max_frames_any.max(d);
                    report.bounds.push(FnStackBound {
                        fn_name: name.clone(),
                        max_frames: d,
                        source: BoundSource::Attribute,
                    });
                }
                None => report.messages.push(Message::error(
                    ErrorCode::UnboundedRecursion,
                    format!(
                        "recursive function `{name}` participates in mutual recursion; \
                         add `#[max_depth(N)]` with a safe upper bound on call-frame depth"
                    ),
                    span.clone(),
                )),
            }
            continue;
        }

        if tail_only.contains(name) {
            let frames = attr_depth.unwrap_or(1);
            max_frames_any = max_frames_any.max(frames);
            report.bounds.push(FnStackBound {
                fn_name: name.clone(),
                max_frames: frames,
                source: if attr_depth.is_some() {
                    BoundSource::Attribute
                } else {
                    BoundSource::TailOnly
                },
            });
            continue;
        }

        let has_dynamic = dynamic_calls.contains(name);
        let consts = const_args.get(name);
        let has_const_entry = consts.is_some_and(|s| !s.is_empty());
        if !has_dynamic && !has_const_entry {
            continue;
        }

        let proven = if let Some(shape) = measure_shapes.get(name) {
            prove_measure_depth(shape, consts, has_dynamic)
        } else {
            DepthProof::Unprovable
        };

        match (proven, attr_depth) {
            (DepthProof::Frames(n), _) => {
                let frames = attr_depth.map(|a| a.max(n)).unwrap_or(n);
                max_frames_any = max_frames_any.max(frames);
                report.bounds.push(FnStackBound {
                    fn_name: name.clone(),
                    max_frames: frames,
                    source: if attr_depth.is_some_and(|a| a >= n) {
                        BoundSource::Attribute
                    } else {
                        BoundSource::Proven
                    },
                });
            }
            (DepthProof::Unprovable, Some(d)) => {
                max_frames_any = max_frames_any.max(d);
                report.bounds.push(FnStackBound {
                    fn_name: name.clone(),
                    max_frames: d,
                    source: BoundSource::Attribute,
                });
            }
            (DepthProof::Unprovable, None) => {
                let reason = if has_dynamic {
                    "is called with a non-constant measure argument"
                } else if measure_shapes
                    .get(name)
                    .is_some_and(|s| s.base_bound.is_none())
                {
                    "needs a recognizable base case (`if n <= K` / `n < K` / `n == K`)"
                } else if measure_shapes.contains_key(name) {
                    "has no constant entry call site to bound its measure"
                } else {
                    "has no analyzable decreasing measure / base-case shape"
                };
                report.messages.push(Message::error(
                    ErrorCode::UnboundedRecursion,
                    format!(
                        "recursive function `{name}` {reason}; \
                         add `#[max_depth(N)]` with a safe upper bound on call-frame depth"
                    ),
                    span,
                ));
            }
        }
    }

    report.operand_slots_needed = operand_slots_for_frames(max_frames_any);
    if max_frames_any
        .saturating_mul(DEFAULT_FRAME_SLOTS)
        .saturating_add(DEFAULT_FRAME_SLOTS)
        > MAX_OPERAND_STACK_SLOTS
    {
        report.messages.push(Message::error(
            ErrorCode::StackDepthExceeded,
            format!(
                "estimated operand stack need exceeds the VM limit of {MAX_OPERAND_STACK_SLOTS} slots"
            ),
            0..0,
        ));
    }

    report
}

#[derive(Debug, Clone, Copy)]
enum DepthProof {
    Frames(u32),
    Unprovable,
}

struct FnMeta<'a> {
    span: std::ops::Range<usize>,
    max_depth: Option<u32>,
    attr_error: Option<Message>,
    self_recursive: bool,
    #[allow(dead_code)]
    attrs: &'a [Attribute<'a>],
}

fn prove_measure_depth(
    shape: &RecMeasureShape,
    consts: Option<&BTreeSet<i64>>,
    has_dynamic: bool,
) -> DepthProof {
    if has_dynamic {
        return DepthProof::Unprovable;
    }
    let Some(set) = consts else {
        return DepthProof::Unprovable;
    };
    if set.is_empty() {
        return DepthProof::Unprovable;
    }
    let Some(base) = shape.base_bound else {
        return DepthProof::Unprovable;
    };
    let mut max_d = 1u32;
    for &n in set {
        max_d = max_d.max(measure_depth(n, base, shape.step));
    }
    DepthProof::Frames(max_d)
}

/// Worst-case frames along a chain that decreases `n` until `n <= base`.
fn measure_depth(n: i64, base: i64, step: MeasureStep) -> u32 {
    if n <= base {
        return 1;
    }
    match step {
        MeasureStep::Subtract(k) => {
            let k = k.max(1) as u64;
            let delta = (n - base) as u64;
            (delta.div_ceil(k) as u32).saturating_add(1)
        }
        MeasureStep::Half => {
            let mut depth = 1u32;
            let mut cur = n;
            while cur > base {
                cur = cur / 2;
                depth += 1;
                if depth > 10_000 {
                    break;
                }
            }
            depth
        }
    }
}

fn parse_max_depth_attr(
    attrs: &[Attribute<'_>],
    span: std::ops::Range<usize>,
) -> (Option<u32>, Option<Message>) {
    let Some(attr) = attrs.iter().find(|a| a.name == "max_depth") else {
        return (None, None);
    };
    let n = match &attr.args {
        AttrArgs::Positional(lits) if lits.len() == 1 => match &lits[0] {
            AttrLit::Int(v) if *v > 0 && *v <= u32::MAX as i64 => Some(*v as u32),
            AttrLit::Int(_) => None,
            _ => None,
        },
        AttrArgs::KeyValues(kvs) => {
            let mut found = None;
            for (k, v) in kvs {
                if *k == "n" || *k == "depth" {
                    if let AttrLit::Int(i) = v
                        && *i > 0
                        && *i <= u32::MAX as i64
                    {
                        found = Some(*i as u32);
                    }
                }
            }
            found
        }
        _ => None,
    };
    match n {
        Some(v) => (Some(v), None),
        None => (
            None,
            Some(Message::error(
                ErrorCode::GenericTypeError,
                "`#[max_depth(N)]` requires a positive integer depth \
                 (e.g. `#[max_depth(64)]`)"
                    .to_string(),
                span,
            )),
        ),
    }
}

fn collect_fn_meta<'a>(
    ast: &'a Output<'a>,
    recursive: &HashSet<String>,
    out: &mut HashMap<String, FnMeta<'a>>,
    shapes: &mut HashMap<String, RecMeasureShape>,
    tail_only: &mut HashSet<String>,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fn_meta(item, recursive, out, shapes, tail_only);
            }
        }
        Expression::Module(_, body) => collect_fn_meta(body, recursive, out, shapes, tail_only),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_fn_meta(inner, recursive, out, shapes, tail_only),
        Expression::Function {
            attrs,
            name,
            args,
            body: Some(body),
            ..
        } if recursive.contains(*name) => {
            let span = ast.0.into_range();
            let (max_depth, attr_error) = parse_max_depth_attr(attrs, span.clone());
            let self_recursive = body_calls_self(body, name);
            out.insert(
                (*name).to_string(),
                FnMeta {
                    span,
                    max_depth,
                    attr_error,
                    self_recursive,
                    attrs,
                },
            );
            if let Some(shape) = detect_measure_shape(name, args, body) {
                shapes.insert((*name).to_string(), shape);
            }
            if is_tail_only_recursive(body, name) {
                tail_only.insert((*name).to_string());
            }
            collect_fn_meta(body, recursive, out, shapes, tail_only);
        }
        Expression::Function {
            body: Some(body), ..
        } => collect_fn_meta(body, recursive, out, shapes, tail_only),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fn_meta(m, recursive, out, shapes, tail_only);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_fn_meta(inner, recursive, out, shapes, tail_only);
        }
        _ => {}
    }
}

fn body_calls_self(body: &Output<'_>, name: &str) -> bool {
    let mut found = false;
    walk_all_calls(body, &mut |callee, _| {
        if callee == name {
            found = true;
        }
    });
    found
}

/// Pick the first int/byte param that has a base case and decreases on every self-call.
fn detect_measure_shape(
    name: &str,
    args: &Output<'_>,
    body: &Output<'_>,
) -> Option<RecMeasureShape> {
    let params = int_like_params(args);
    if params.is_empty() {
        return None;
    }
    let self_calls = collect_self_calls(body, name);
    if self_calls.is_empty() {
        return None;
    }

    for (measure_index, measure_param) in params.iter().enumerate() {
        let base_bound = find_base_bound(body, measure_param);
        let mut min_step: Option<MeasureStep> = None;
        let mut ok = true;
        for call_args in &self_calls {
            let Some(arg) = call_args.get(measure_index) else {
                ok = false;
                break;
            };
            let Some(step) = match_param_decrease(peel(arg), measure_param) else {
                ok = false;
                break;
            };
            min_step = Some(match min_step {
                Some(MeasureStep::Subtract(m)) => match step {
                    MeasureStep::Subtract(s) => MeasureStep::Subtract(m.min(s)),
                    MeasureStep::Half => MeasureStep::Half,
                },
                Some(MeasureStep::Half) => MeasureStep::Half,
                None => step,
            });
        }
        if !ok {
            continue;
        }
        let Some(step) = min_step else {
            continue;
        };
        if matches!(step, MeasureStep::Subtract(k) if k <= 0) {
            continue;
        }
        // Accept shape even without base_bound so diagnostics can ask for a base case.
        return Some(RecMeasureShape {
            measure_param: measure_param.clone(),
            measure_index,
            base_bound,
            step,
            self_calls: self_calls.len(),
        });
    }
    None
}

fn int_like_params(args: &Output<'_>) -> Vec<String> {
    let items = match args.1.as_ref() {
        Expression::Fragment(items) | Expression::Block(items) => items.as_slice(),
        _ => return Vec::new(),
    };
    let mut out = Vec::new();
    for item in items {
        let arg = peel(item);
        let Expression::Argument { name, ty, .. } = arg.1.as_ref() else {
            continue;
        };
        let Some(ty) = ty else {
            continue;
        };
        let ty_name = match peel(ty).1.as_ref() {
            Expression::Type(t) | Expression::Identifier(t) => *t,
            _ => continue,
        };
        if matches!(ty_name, "int" | "byte") {
            out.push((*name).to_string());
        }
    }
    out
}

/// All self-call argument lists in `body` (order of discovery).
fn collect_self_calls<'a>(body: &'a Output<'a>, fn_name: &str) -> Vec<&'a [Output<'a>]> {
    let mut out = Vec::new();
    walk_self_calls(body, fn_name, &mut out);
    out
}

fn walk_self_calls<'a>(
    ast: &'a Output<'a>,
    fn_name: &str,
    out: &mut Vec<&'a [Output<'a>]>,
) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                walk_self_calls(item, fn_name, out);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_self_calls(c, fn_name, out);
            }
            walk_self_calls(body, fn_name, out);
        }
        Expression::Call { name, args } => {
            let callee = match peel(name).1.as_ref() {
                Expression::Identifier(n) => Some(*n),
                _ => None,
            };
            if let Some(args) = args {
                for a in args {
                    walk_self_calls(a, fn_name, out);
                }
                if callee == Some(fn_name) {
                    out.push(args.as_slice());
                }
            }
            walk_self_calls(name, fn_name, out);
        }
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Cast(inner, _)
        | Expression::Try(inner)
        | Expression::Readonly(inner)
        | Expression::Panic(inner)
        | Expression::Raise(inner)
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::TypeOf(inner) => walk_self_calls(inner, fn_name, out),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
        | Expression::Shl(a, b)
        | Expression::Shr(a, b)
        | Expression::Xor(a, b)
        | Expression::And(a, b)
        | Expression::BitAnd(a, b)
        | Expression::Or(a, b)
        | Expression::BitOr(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b) => {
            walk_self_calls(a, fn_name, out);
            walk_self_calls(b, fn_name, out);
        }
        Expression::Adjust { target, .. } => walk_self_calls(target, fn_name, out),
        Expression::Match { scrutinee, arms } => {
            walk_self_calls(scrutinee, fn_name, out);
            for arm in arms {
                walk_self_calls(&arm.body, fn_name, out);
            }
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                walk_self_calls(id, fn_name, out);
            }
            walk_self_calls(iterable, fn_name, out);
            walk_self_calls(body, fn_name, out);
        }
        Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init)) => {
            walk_self_calls(init, fn_name, out);
        }
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::Defer { body, .. }
        | Expression::TestCase { body, .. } => walk_self_calls(body, fn_name, out),
        Expression::NamedArg(_, v) => walk_self_calls(v, fn_name, out),
        _ => {}
    }
}

fn find_base_bound(body: &Output<'_>, param: &str) -> Option<i64> {
    let mut base = None;
    walk_base_bound(body, param, &mut base);
    base
}

fn walk_base_bound(ast: &Output<'_>, param: &str, base: &mut Option<i64>) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::If(items) => {
            for item in items {
                walk_base_bound(item, param, base);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond
                && let Some(b) = match_base_bound(c, param)
            {
                *base = Some(b);
            }
            walk_base_bound(body, param, base);
        }
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner) => walk_base_bound(inner, param, base),
        _ => {}
    }
}

fn is_tail_only_recursive(body: &Output<'_>, name: &str) -> bool {
    let mut self_calls = 0;
    let mut non_tail = false;
    walk_tail_rec(body, name, true, &mut self_calls, &mut non_tail);
    self_calls > 0 && !non_tail
}

fn walk_tail_rec(
    ast: &Output<'_>,
    name: &str,
    tail_ctx: bool,
    self_calls: &mut i32,
    non_tail: &mut bool,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            let n = items.len();
            for (i, item) in items.iter().enumerate() {
                walk_tail_rec(item, name, tail_ctx && i + 1 == n, self_calls, non_tail);
            }
        }
        Expression::If(branches) => {
            for b in branches {
                walk_tail_rec(b, name, tail_ctx, self_calls, non_tail);
            }
        }
        Expression::Branch(_, body) => walk_tail_rec(body, name, tail_ctx, self_calls, non_tail),
        Expression::Return(inner) | Expression::ImplicitReturn(inner) => {
            walk_tail_rec(inner, name, true, self_calls, non_tail);
        }
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => walk_tail_rec(inner, name, tail_ctx, self_calls, non_tail),
        Expression::Call {
            name: callee,
            args,
        } => {
            let is_self =
                matches!(peel(callee).1.as_ref(), Expression::Identifier(n) if *n == name);
            if is_self {
                *self_calls += 1;
                if !tail_ctx {
                    *non_tail = true;
                }
            }
            if let Some(args) = args {
                for a in args {
                    walk_tail_rec(a, name, false, self_calls, non_tail);
                }
            }
        }
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
        | Expression::BitAnd(a, b)
        | Expression::BitOr(a, b)
        | Expression::Xor(a, b) => {
            walk_tail_rec(a, name, false, self_calls, non_tail);
            walk_tail_rec(b, name, false, self_calls, non_tail);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Entry sites + const environments
// ---------------------------------------------------------------------------

/// Param-slot constants known for each function (from all agreeing call sites).
type FnConstParams = HashMap<String, Vec<Option<i64>>>;

/// Shallow interprocedural: if every call site into `g` agrees on constant
/// values for some param slots, record those for use inside `g`.
fn propagate_const_args(ast: &Output<'_>, recursive: &HashSet<String>) -> FnConstParams {
    let mut fn_params: HashMap<String, usize> = HashMap::new();
    collect_fn_arities(ast, &mut fn_params);

    // Gather raw call-site arg vectors (evaluated with empty/local env only first).
    let mut site_args: HashMap<String, Vec<Vec<Option<i64>>>> = HashMap::new();
    gather_call_site_args(ast, None, &HashMap::new(), &mut site_args);

    // Fixed-point: refine wrapper consts, then re-gather with those envs.
    let mut wrapper: FnConstParams = HashMap::new();
    for _ in 0..8 {
        let mut next: FnConstParams = HashMap::new();
        for (name, sites) in &site_args {
            if recursive.contains(name) {
                continue; // only wrappers
            }
            let arity = fn_params.get(name).copied().unwrap_or(0);
            if arity == 0 || sites.is_empty() {
                continue;
            }
            let mut slots = vec![None; arity];
            for i in 0..arity {
                let mut agreed: Option<Option<i64>> = None;
                for site in sites {
                    let v = site.get(i).copied().flatten();
                    match agreed {
                        None => agreed = Some(v),
                        Some(prev) if prev == v => {}
                        Some(_) => {
                            agreed = Some(None);
                            break;
                        }
                    }
                }
                if let Some(Some(c)) = agreed {
                    slots[i] = Some(c);
                }
            }
            if slots.iter().any(|s| s.is_some()) {
                next.insert(name.clone(), slots);
            }
        }
        if next == wrapper {
            break;
        }
        wrapper = next;
        site_args.clear();
        gather_call_site_args(ast, None, &wrapper, &mut site_args);
    }
    wrapper
}

fn collect_fn_arities(ast: &Output<'_>, out: &mut HashMap<String, usize>) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fn_arities(item, out);
            }
        }
        Expression::Module(_, body) => collect_fn_arities(body, out),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            out.insert((*name).to_string(), count_params(args));
            collect_fn_arities(body, out);
        }
        Expression::TestCase { body, .. } => collect_fn_arities(body, out),
        _ => {}
    }
}

fn count_params(args: &Output<'_>) -> usize {
    let items = match args.1.as_ref() {
        Expression::Fragment(items) | Expression::Block(items) => items.as_slice(),
        _ => return 0,
    };
    items
        .iter()
        .filter(|i| matches!(peel(i).1.as_ref(), Expression::Argument { .. }))
        .count()
}

fn gather_call_site_args(
    ast: &Output<'_>,
    inside: Option<&str>,
    wrapper_consts: &FnConstParams,
    out: &mut HashMap<String, Vec<Vec<Option<i64>>>>,
) {
    let mut env = HashMap::new();
    if let Some(name) = inside
        && let Some(slots) = wrapper_consts.get(name)
    {
        // Bind formal names from wrapper const params when available.
        // Formal names are recovered only inside Function — here we just
        // keep slot values for eval via a synthetic env filled by callers.
        let _ = slots;
    }
    walk_gather_calls(ast, inside, wrapper_consts, &mut env, out);
}

fn walk_gather_calls(
    ast: &Output<'_>,
    inside: Option<&str>,
    wrapper_consts: &FnConstParams,
    env: &mut HashMap<String, ConstValue>,
    out: &mut HashMap<String, Vec<Vec<Option<i64>>>>,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::If(items) => {
            for item in items {
                walk_gather_calls(item, inside, wrapper_consts, env, out);
            }
        }
        Expression::Fragment(items) => {
            if let Some((name, init)) = let_const_binding(items) {
                walk_gather_calls(init, inside, wrapper_consts, env, out);
                match eval_expr(init, env) {
                    Some(v) => {
                        env.insert(name.to_string(), v);
                    }
                    None => {
                        env.remove(name);
                    }
                }
            } else {
                for item in items {
                    walk_gather_calls(item, inside, wrapper_consts, env, out);
                }
            }
        }
        Expression::Module(_, body)
        | Expression::Statement(body)
        | Expression::Expr(body)
        | Expression::ExprStatement(body)
        | Expression::Group(body)
        | Expression::Return(body)
        | Expression::ImplicitReturn(body)
        | Expression::Try(body) => {
            walk_gather_calls(body, inside, wrapper_consts, env, out);
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_gather_calls(c, inside, wrapper_consts, env, out);
            }
            walk_gather_calls(body, inside, wrapper_consts, env, out);
        }
        Expression::Assignment(lhs, rhs) => {
            walk_gather_calls(rhs, inside, wrapper_consts, env, out);
            kill_binding(lhs, env);
        }
        Expression::CompoundAssign(lhs, _, rhs) => {
            walk_gather_calls(lhs, inside, wrapper_consts, env, out);
            walk_gather_calls(rhs, inside, wrapper_consts, env, out);
            kill_binding(lhs, env);
        }
        Expression::Adjust { target, .. } => {
            walk_gather_calls(target, inside, wrapper_consts, env, out);
            kill_binding(target, env);
        }
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b) => {
            walk_gather_calls(a, inside, wrapper_consts, env, out);
            walk_gather_calls(b, inside, wrapper_consts, env, out);
        }
        Expression::Call { name, args } => {
            walk_gather_calls(name, inside, wrapper_consts, env, out);
            let callee = match peel(name).1.as_ref() {
                Expression::Identifier(n) => Some(*n),
                _ => None,
            };
            let mut slot_vals = Vec::new();
            if let Some(args) = args {
                for a in args {
                    walk_gather_calls(a, inside, wrapper_consts, env, out);
                    slot_vals.push(eval_expr(a, env).and_then(|v| match v {
                        ConstValue::Int(i) => Some(i),
                        _ => None,
                    }));
                }
            }
            if let Some(cname) = callee {
                out.entry(cname.to_string()).or_default().push(slot_vals);
            }
        }
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            let mut inner = HashMap::new();
            // Seed formals from wrapper const params.
            if let Some(slots) = wrapper_consts.get(*name) {
                seed_formals(args, slots, &mut inner);
            }
            walk_gather_calls(body, Some(*name), wrapper_consts, &mut inner, out);
        }
        Expression::TestCase { body, .. } => {
            let mut inner = HashMap::new();
            walk_gather_calls(body, None, wrapper_consts, &mut inner, out);
        }
        Expression::Lambda { body, .. } | Expression::Defer { body, .. } => {
            walk_gather_calls(body, inside, wrapper_consts, env, out);
        }
        Expression::Match { scrutinee, arms } => {
            walk_gather_calls(scrutinee, inside, wrapper_consts, env, out);
            for arm in arms {
                walk_gather_calls(&arm.body, inside, wrapper_consts, env, out);
            }
        }
        _ => {}
    }
}

fn seed_formals(
    args: &Output<'_>,
    slots: &[Option<i64>],
    env: &mut HashMap<String, ConstValue>,
) {
    let items = match args.1.as_ref() {
        Expression::Fragment(items) | Expression::Block(items) => items.as_slice(),
        _ => return,
    };
    let mut i = 0;
    for item in items {
        let arg = peel(item);
        let Expression::Argument { name, .. } = arg.1.as_ref() else {
            continue;
        };
        if let Some(Some(v)) = slots.get(i) {
            env.insert((*name).to_string(), ConstValue::Int(*v));
        }
        i += 1;
    }
}

fn collect_rec_entry_sites(
    ast: &Output<'_>,
    recursive: &HashSet<String>,
    shapes: &HashMap<String, RecMeasureShape>,
    wrapper_consts: &FnConstParams,
    consts: &mut HashMap<String, BTreeSet<i64>>,
    dynamic: &mut HashSet<String>,
) {
    let mut env = HashMap::new();
    walk_entry_sites(
        ast,
        None,
        recursive,
        shapes,
        wrapper_consts,
        &mut env,
        consts,
        dynamic,
    );
}

fn walk_entry_sites(
    ast: &Output<'_>,
    inside: Option<&str>,
    recursive: &HashSet<String>,
    shapes: &HashMap<String, RecMeasureShape>,
    wrapper_consts: &FnConstParams,
    env: &mut HashMap<String, ConstValue>,
    consts: &mut HashMap<String, BTreeSet<i64>>,
    dynamic: &mut HashSet<String>,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::If(items) => {
            for item in items {
                walk_entry_sites(
                    item,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
            }
        }
        Expression::Fragment(items) => {
            if let Some((name, init)) = let_const_binding(items) {
                walk_entry_sites(
                    init,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
                match eval_expr(init, env) {
                    Some(v) => {
                        env.insert(name.to_string(), v);
                    }
                    None => {
                        env.remove(name);
                    }
                }
            } else {
                for item in items {
                    walk_entry_sites(
                        item,
                        inside,
                        recursive,
                        shapes,
                        wrapper_consts,
                        env,
                        consts,
                        dynamic,
                    );
                }
            }
        }
        Expression::Module(_, body)
        | Expression::Statement(body)
        | Expression::Expr(body)
        | Expression::ExprStatement(body)
        | Expression::Group(body)
        | Expression::Return(body)
        | Expression::ImplicitReturn(body)
        | Expression::Negate(body)
        | Expression::Not(body)
        | Expression::LogicalNot(body)
        | Expression::Positive(body)
        | Expression::Cast(body, _)
        | Expression::Try(body)
        | Expression::Readonly(body)
        | Expression::Panic(body)
        | Expression::Raise(body)
        | Expression::Yield(body)
        | Expression::YieldFrom(body)
        | Expression::TypeOf(body) => {
            walk_entry_sites(
                body,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
        | Expression::Shl(a, b)
        | Expression::Shr(a, b)
        | Expression::Xor(a, b)
        | Expression::And(a, b)
        | Expression::BitAnd(a, b)
        | Expression::Or(a, b)
        | Expression::BitOr(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b) => {
            walk_entry_sites(
                a,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            walk_entry_sites(
                b,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        Expression::Assignment(lhs, rhs) => {
            walk_entry_sites(
                rhs,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            kill_binding(lhs, env);
        }
        Expression::CompoundAssign(lhs, _, rhs) => {
            walk_entry_sites(
                lhs,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            walk_entry_sites(
                rhs,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            kill_binding(lhs, env);
        }
        Expression::Adjust { target, .. } => {
            walk_entry_sites(
                target,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            kill_binding(target, env);
        }
        Expression::Call { name, args } => {
            walk_entry_sites(
                name,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            let callee = match peel(name).1.as_ref() {
                Expression::Identifier(n) => Some(*n),
                _ => None,
            };
            if let Some(args) = args {
                for a in args {
                    walk_entry_sites(
                        a,
                        inside,
                        recursive,
                        shapes,
                        wrapper_consts,
                        env,
                        consts,
                        dynamic,
                    );
                }
            }
            let Some(cname) = callee else {
                return;
            };
            if !recursive.contains(cname) {
                return;
            }
            // Self-calls inside the recursive body are the measure, not entries.
            if inside == Some(cname) {
                return;
            }
            let measure_idx = shapes.get(cname).map(|s| s.measure_index).unwrap_or(0);
            match args {
                Some(args) if measure_idx < args.len() => {
                    match eval_expr(&args[measure_idx], env) {
                        Some(ConstValue::Int(n)) => {
                            consts.entry(cname.to_string()).or_default().insert(n);
                        }
                        _ => {
                            dynamic.insert(cname.to_string());
                        }
                    }
                }
                _ => {
                    dynamic.insert(cname.to_string());
                }
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_entry_sites(
                    c,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
            }
            walk_entry_sites(
                body,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        Expression::Match { scrutinee, arms } => {
            walk_entry_sites(
                scrutinee,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            for arm in arms {
                walk_entry_sites(
                    &arm.body,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
            }
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                walk_entry_sites(
                    id,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
            }
            walk_entry_sites(
                iterable,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
            walk_entry_sites(
                body,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            let mut inner = HashMap::new();
            if let Some(slots) = wrapper_consts.get(*name) {
                seed_formals(args, slots, &mut inner);
            }
            walk_entry_sites(
                body,
                Some(*name),
                recursive,
                shapes,
                wrapper_consts,
                &mut inner,
                consts,
                dynamic,
            );
        }
        Expression::Lambda { body, .. } | Expression::Defer { body, .. } => {
            walk_entry_sites(
                body,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        Expression::TestCase { body, .. } => {
            let mut inner = HashMap::new();
            walk_entry_sites(
                body,
                None,
                recursive,
                shapes,
                wrapper_consts,
                &mut inner,
                consts,
                dynamic,
            );
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                walk_entry_sites(
                    m,
                    inside,
                    recursive,
                    shapes,
                    wrapper_consts,
                    env,
                    consts,
                    dynamic,
                );
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) | Expression::NamedArg(_, inner) => {
            walk_entry_sites(
                inner,
                inside,
                recursive,
                shapes,
                wrapper_consts,
                env,
                consts,
                dynamic,
            );
        }
        _ => {}
    }
}

fn walk_all_calls(ast: &Output<'_>, f: &mut dyn FnMut(&str, Option<i64>)) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::If(items) => {
            for item in items {
                walk_all_calls(item, f);
            }
        }
        Expression::Call { name, args } => {
            let callee = match peel(name).1.as_ref() {
                Expression::Identifier(n) => Some(*n),
                _ => None,
            };
            if let Some(args) = args {
                for a in args {
                    walk_all_calls(a, f);
                }
            }
            if let Some(c) = callee {
                f(c, None);
            }
        }
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::TestCase { body, .. }
        | Expression::Lambda { body, .. }
        | Expression::Defer { body, .. }
        | Expression::Return(body)
        | Expression::ImplicitReturn(body)
        | Expression::Statement(body)
        | Expression::Expr(body)
        | Expression::Group(body) => walk_all_calls(body, f),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b) => {
            walk_all_calls(a, f);
            walk_all_calls(b, f);
        }
        Expression::Adjust { target, .. } => walk_all_calls(target, f),
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_all_calls(c, f);
            }
            walk_all_calls(body, f);
        }
        _ => {}
    }
}

fn match_base_bound(cond: &Output<'_>, param: &str) -> Option<i64> {
    let cond = peel(cond);
    match cond.1.as_ref() {
        Expression::Leq(lhs, rhs) | Expression::Le(lhs, rhs) => {
            let lhs = peel(lhs);
            let rhs = peel(rhs);
            let is_le = matches!(cond.1.as_ref(), Expression::Leq(_, _));
            match (lhs.1.as_ref(), rhs.1.as_ref()) {
                (Expression::Identifier(p), Expression::Integer(k)) if *p == param => {
                    Some(if is_le { *k } else { *k - 1 })
                }
                _ => None,
            }
        }
        Expression::Eq(lhs, rhs) => {
            let lhs = peel(lhs);
            let rhs = peel(rhs);
            match (lhs.1.as_ref(), rhs.1.as_ref()) {
                (Expression::Identifier(p), Expression::Integer(k)) if *p == param => Some(*k),
                (Expression::Integer(k), Expression::Identifier(p)) if *p == param => Some(*k),
                _ => None,
            }
        }
        _ => None,
    }
}

fn match_param_decrease(expr: &Output<'_>, param: &str) -> Option<MeasureStep> {
    if let Some(k) = match_param_minus_const(expr, param) {
        return Some(MeasureStep::Subtract(k));
    }
    if match_param_half(expr, param) {
        return Some(MeasureStep::Half);
    }
    None
}

fn match_param_half(expr: &Output<'_>, param: &str) -> bool {
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Div(lhs, rhs) => {
            let lhs = peel(lhs);
            let rhs = peel(rhs);
            matches!(lhs.1.as_ref(), Expression::Identifier(p) if *p == param)
                && matches!(rhs.1.as_ref(), Expression::Integer(2))
        }
        Expression::Shr(lhs, rhs) => {
            let lhs = peel(lhs);
            let rhs = peel(rhs);
            matches!(lhs.1.as_ref(), Expression::Identifier(p) if *p == param)
                && matches!(rhs.1.as_ref(), Expression::Integer(1))
        }
        _ => false,
    }
}

fn match_param_minus_const(expr: &Output<'_>, param: &str) -> Option<i64> {
    let expr = peel(expr);
    match expr.1.as_ref() {
        Expression::Sub(lhs, rhs) => {
            let lhs = peel(lhs);
            let rhs = peel(rhs);
            match (lhs.1.as_ref(), rhs.1.as_ref()) {
                (Expression::Identifier(p), Expression::Integer(k)) if *p == param && *k > 0 => {
                    Some(*k)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Drop a const-env binding when its name is assigned / adjusted.
fn kill_binding(target: &Output<'_>, env: &mut HashMap<String, ConstValue>) {
    if let Expression::Identifier(n) = peel(target).1.as_ref() {
        env.remove(*n);
    }
}

/// `let x = e` / `const x = e` parse as `Fragment([Variable|Constant, e])`.
/// The second field of Variable/Constant is a type annotation, not the initializer.
fn let_const_binding<'a>(items: &'a [Output<'a>]) -> Option<(&'a str, &'a Output<'a>)> {
    if items.len() != 2 {
        return None;
    }
    match peel(&items[0]).1.as_ref() {
        Expression::Variable(n, _) => Some((*n, &items[1])),
        Expression::Constant(name_e, _) => match peel(name_e).1.as_ref() {
            Expression::Identifier(n) => Some((*n, &items[1])),
            _ => None,
        },
        _ => None,
    }
}

fn peel<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
    match expr.1.as_ref() {
        Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Statement(inner)
        | Expression::ExprStatement(inner) => peel(inner),
        Expression::Fragment(items) if items.len() == 1 => peel(&items[0]),
        _ => expr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn parse(src: &str) -> Output<'static> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse")
    }

    #[test]
    fn fib_const_entry_is_proven_without_attr() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(10);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report.messages.is_empty(),
            "unexpected errors: {:?}",
            report.messages
        );
        let b = report
            .bounds
            .iter()
            .find(|b| b.fn_name == "fib")
            .expect("fib bound");
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 9);
    }

    #[test]
    fn fib_bench_32_proven() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(32);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.max_frames, 31);
    }

    #[test]
    fn traced_local_const_is_proven() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 10;
    let x = fib(k);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 9);
    }

    #[test]
    fn dynamic_rec_requires_max_depth() {
        let ast = parse(
            r#"
fn noise() -> int { return 10; }
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(noise());
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("max_depth")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn max_depth_attr_satisfies_dynamic() {
        let ast = parse(
            r#"
#[max_depth(64)]
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn noise() -> int { return 10; }
fn main() {
    let x = fib(noise());
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.max_frames, 64);
        assert_eq!(b.source, BoundSource::Attribute);
    }

    #[test]
    fn wrapper_const_propagates_to_fib() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn helper(int n) -> int {
    return fib(n);
}
fn main() {
    let x = helper(32);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 31);
    }

    #[test]
    fn wrapper_traced_local_propagates_to_fib() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn helper(int n) -> int {
    return fib(n);
}
fn main() {
    let n = 30;
    let x = helper(n);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 29);
    }

    #[test]
    fn const_binding_entry_is_proven() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    const N = 10;
    let x = fib(N);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 9);
    }

    #[test]
    fn assignment_kills_traced_const() {
        let ast = parse(
            r#"
fn noise() -> int { return 10; }
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 10;
    k = noise();
    let x = fib(k);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("max_depth")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn compound_assign_kills_traced_const() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 10;
    k += 1;
    let x = fib(k);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("max_depth")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn inc_dec_kills_traced_const() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 10;
    k++;
    let x = fib(k);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("max_depth")),
            "{:?}",
            report.messages
        );

        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let k = 10;
    --k;
    let x = fib(k);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("max_depth")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn call_inside_compound_assign_is_entry() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let acc = 0;
    acc += fib(8);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 7);
    }

    #[test]
    fn multi_arg_measure_is_proven() {
        let ast = parse(
            r#"
fn go(int n, string s) -> int {
    if n <= 0 { return 0; }
    return go(n - 1, s) + 1;
}
fn main() {
    let x = go(10, "hi");
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "go").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        // (10 - 0) / 1 + 1 = 11
        assert_eq!(b.max_frames, 11);
    }

    #[test]
    fn multi_call_body_with_div_mod_is_proven() {
        let ast = parse(
            r#"
fn f(int n) -> int {
    if n <= 1 { return 1; }
    let a = f(n - 1);
    let b = f(n - 2);
    let c = f(n - 3);
    return (a + b) / (c % 2 + 1);
}
fn main() {
    let x = f(8);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "f").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        // min_step=1, base=1 → (8-1)/1+1 = 8
        assert_eq!(b.max_frames, 8);
    }

    #[test]
    fn tail_rec_needs_no_attr() {
        let ast = parse(
            r#"
fn countdown(int n) -> int {
    if n <= 0 { return 0; }
    return countdown(n - 1);
}
fn main() {
    let x = countdown(100);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report
            .bounds
            .iter()
            .find(|b| b.fn_name == "countdown")
            .unwrap();
        assert_eq!(b.source, BoundSource::TailOnly);
        assert_eq!(b.max_frames, 1);
    }

    #[test]
    fn fib_bench_sizes_operand_stack_above_default() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(32);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(report.operand_slots_needed, 512);
        assert!(report.operand_slots_needed > DEFAULT_OPERAND_STACK_SLOTS);
    }

    #[test]
    fn measure_depth_helpers() {
        use MeasureStep::{Half, Subtract};
        assert_eq!(measure_depth(2, 2, Subtract(1)), 1);
        assert_eq!(measure_depth(10, 2, Subtract(1)), 9);
        assert_eq!(measure_depth(32, 2, Subtract(1)), 31);
        assert_eq!(measure_depth(16, 1, Half), 5); // 16→8→4→2→1
        assert_eq!(measure_depth(1, 1, Half), 1);
    }

    #[test]
    fn half_measure_recursion_is_proven() {
        let ast = parse(
            r#"
fn dig(int n) -> int {
    if n <= 1 { return 1; }
    return 1 + dig(n / 2);
}
fn main() {
    let x = dig(16);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report
            .bounds
            .iter()
            .find(|b| b.fn_name == "dig")
            .expect("dig bound");
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 5);
    }

    #[test]
    fn shr_one_measure_recursion_is_proven() {
        let ast = parse(
            r#"
fn dig(int n) -> int {
    if n <= 1 { return 0; }
    return 1 + dig(n >> 1);
}
fn main() {
    let x = dig(8);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "dig").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 4); // 8→4→2→1
    }

    #[test]
    fn mutual_recursion_requires_max_depth() {
        let ast = parse(
            r#"
fn ping(int n) -> int {
    if n <= 0 { return 0; }
    return pong(n - 1);
}
fn pong(int n) -> int {
    if n <= 0 { return 1; }
    return ping(n - 1);
}
fn main() {
    let x = ping(3);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report.messages.iter().any(|m| {
                m.code() == Some(ErrorCode::UnboundedRecursion) && m.message().contains("mutual")
            }),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn mutual_recursion_with_max_depth_ok() {
        let ast = parse(
            r#"
#[max_depth(8)]
fn ping(int n) -> int {
    if n <= 0 { return 0; }
    return pong(n - 1);
}
#[max_depth(8)]
fn pong(int n) -> int {
    if n <= 0 { return 1; }
    return ping(n - 1);
}
fn main() {
    let x = ping(3);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert!(report.bounds.iter().any(|b| {
            b.fn_name == "ping" && b.source == BoundSource::Attribute && b.max_frames == 8
        }));
    }

    #[test]
    fn unary_fact_const_entry_is_proven() {
        let ast = parse(
            r#"
fn fact(int n) -> int {
    if n <= 1 { return 1; }
    return n * fact(n - 1);
}
fn main() {
    let x = fact(5);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fact").unwrap();
        assert_eq!(b.source, BoundSource::Proven);
        assert_eq!(b.max_frames, 5);
    }

    #[test]
    fn base_case_lt_and_eq_shapes_are_proven() {
        let lt = parse(
            r#"
fn f(int n) -> int {
    if n < 3 { return 1; }
    return f(n - 1) + f(n - 2);
}
fn main() { let x = f(5); return; }
"#,
        );
        let lt_report = analyze_stack_bounds(&lt);
        assert!(lt_report.messages.is_empty(), "{:?}", lt_report.messages);
        assert_eq!(
            lt_report
                .bounds
                .iter()
                .find(|b| b.fn_name == "f")
                .unwrap()
                .max_frames,
            4
        );

        let eq = parse(
            r#"
fn g(int n) -> int {
    if n == 0 { return 1; }
    return g(n - 1) + 1;
}
fn main() { let x = g(4); return; }
"#,
        );
        let eq_report = analyze_stack_bounds(&eq);
        assert!(eq_report.messages.is_empty(), "{:?}", eq_report.messages);
        assert_eq!(
            eq_report
                .bounds
                .iter()
                .find(|b| b.fn_name == "g")
                .unwrap()
                .max_frames,
            5
        );
    }

    #[test]
    fn missing_base_case_requires_max_depth() {
        let ast = parse(
            r#"
fn f(int n) -> int {
    return 1 + f(n - 1);
}
fn main() {
    let x = f(3);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report.messages.iter().any(|m| {
                m.code() == Some(ErrorCode::UnboundedRecursion)
                    && m.message().contains("base case")
            }),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn unrecognized_shape_requires_max_depth() {
        let ast = parse(
            r#"
fn boom(int n) -> int {
    return boom(n + 1) + 1;
}
fn main() {
    let x = boom(1);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report.messages.iter().any(|m| {
                m.code() == Some(ErrorCode::UnboundedRecursion)
                    && m.message().contains("analyzable decreasing measure")
            }),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn invalid_max_depth_attr_rejected() {
        let ast = parse(
            r#"
#[max_depth(0)]
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn noise() -> int { return 10; }
fn main() {
    let x = fib(noise());
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.message().contains("positive integer")),
            "{:?}",
            report.messages
        );
    }

    #[test]
    fn absurd_max_depth_emits_stack_depth_exceeded() {
        let ast = parse(
            r#"
#[max_depth(65536)]
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn noise() -> int { return 10; }
fn main() {
    let x = fib(noise());
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(
            report
                .messages
                .iter()
                .any(|m| m.code() == Some(ErrorCode::StackDepthExceeded)),
            "{:?}",
            report.messages
        );
        assert_eq!(report.operand_slots_needed, MAX_OPERAND_STACK_SLOTS);
    }

    #[test]
    fn attr_larger_than_proven_wins_source() {
        let ast = parse(
            r#"
#[max_depth(20)]
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(10);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.max_frames, 20);
        assert_eq!(b.source, BoundSource::Attribute);
    }

    #[test]
    fn attr_smaller_than_proven_still_uses_proven_frames() {
        let ast = parse(
            r#"
#[max_depth(5)]
fn fib(int n) -> int {
    if n <= 2 { return 1; }
    return fib(n - 1) + fib(n - 2);
}
fn main() {
    let x = fib(10);
    return;
}
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let b = report.bounds.iter().find(|b| b.fn_name == "fib").unwrap();
        assert_eq!(b.max_frames, 9);
        assert_eq!(b.source, BoundSource::Proven);
    }

    #[test]
    fn operand_slots_for_frames_clamps_and_floors() {
        assert_eq!(operand_slots_for_frames(1), DEFAULT_OPERAND_STACK_SLOTS);
        assert_eq!(operand_slots_for_frames(9), DEFAULT_OPERAND_STACK_SLOTS);
        assert_eq!(operand_slots_for_frames(31), 512);
        assert_eq!(
            operand_slots_for_frames(u32::MAX),
            MAX_OPERAND_STACK_SLOTS
        );
    }

    #[test]
    fn non_recursive_program_keeps_default_slots() {
        let ast = parse(
            r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { let x = add(1, 2); return; }
"#,
        );
        let report = analyze_stack_bounds(&ast);
        assert!(report.messages.is_empty());
        assert!(report.bounds.is_empty());
        assert_eq!(report.operand_slots_needed, DEFAULT_OPERAND_STACK_SLOTS);
    }
}
