//! HIR bodies the compiler makes itself, with no source of their own: the
//! auto-par fork-join workers, the test-runner `main` and the builtin
//! methods' bodies (`Range::to_vec`, …). They lower like
//! any other body (HIR lowering, then direct MIR), instead of being written
//! out as stack IL by hand.
//!
//! Every node takes [`SPAN`], which no source node has, so no span-keyed
//! checker or sidecar fact is found for it; lowering clears the debug
//! locations it would derive from it.

use crate::layout;
use super::{BinOp, Builtin, Callee, HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, Lit, LocalId, LocalKind};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::{self as coil_ty, Ty};

/// The span of every synthesized node.
pub const SPAN: super::Span = (0, 0);

/// Builds one synthesized body, stamping each node's layout from its type.
pub struct Synth<'a> {
    checker: &'a Checker,
    pub body: HirBody,
}

impl<'a> Synth<'a> {
    pub fn new(checker: &'a Checker, name: &str, ret: Ty) -> Self {
        let ret_layout = layout::of_resolved(checker, &ret);
        Self {
            checker,
            body: HirBody {
                name: name.to_string(),
                kind: super::BodyKind::Function,
                span: SPAN,
                params: Vec::new(),
                ret: Some(ret),
                ret_layout,
                result_mode: false,
                is_coro: false,
                is_generic: false,
                pinned_param: false,
                captures: Vec::new(),
                declared: None,
                stated_escape: None,
                contract_spans: Vec::new(),
                locals: Vec::new(),
                exprs: Vec::new(),
                root: None,
                aliased_fn_values: Vec::new(),
            },
        }
    }

    pub fn local(&mut self, name: &str, ty: Ty, kind: LocalKind) -> LocalId {
        let id = LocalId(self.body.locals.len() as u32);
        self.body.locals.push(HirLocal {
            name: name.to_string(),
            ty: Some(ty),
            kind,
            captured: false,
        });
        if kind == LocalKind::Param {
            self.body.params.push(id);
        }
        id
    }

    pub fn push(&mut self, kind: HirKind, ty: Ty) -> HirId {
        let id = HirId(self.body.exprs.len() as u32);
        self.body.exprs.push(HirExpr {
            kind,
            layout: layout::of_resolved(self.checker, &ty),
            ty: Some(ty),
            span: SPAN,
            node: None,
            flags: HirFlags::default(),
        });
        id
    }

    pub fn int(&mut self, n: i64) -> HirId {
        self.push(HirKind::Lit(Lit::Int(n)), coil_ty::int())
    }

    pub fn str(&mut self, s: &str) -> HirId {
        self.push(HirKind::Lit(Lit::Str(s.to_string())), coil_ty::string())
    }

    pub fn unit(&mut self) -> HirId {
        self.push(HirKind::Lit(Lit::Unit), coil_ty::unit())
    }

    pub fn read(&mut self, local: LocalId) -> HirId {
        let ty = self.body.local(local).ty.clone().unwrap_or_else(coil_ty::int);
        self.push(HirKind::Local(local), ty)
    }

    pub fn bin(&mut self, op: BinOp, lhs: HirId, rhs: HirId, ty: Ty) -> HirId {
        self.push(HirKind::Bin { op, lhs, rhs }, ty)
    }

    pub fn call(&mut self, name: &str, args: Vec<HirId>, ty: Ty) -> HirId {
        let callee = Callee::Named {
            name: name.to_string(),
            def: None,
            overload: None,
        };
        self.push(HirKind::Call { callee, args }, ty)
    }

    pub fn let_(&mut self, local: LocalId, init: HirId) -> HirId {
        self.push(HirKind::Let { local, init: Some(init) }, coil_ty::unit())
    }

    pub fn assign(&mut self, local: LocalId, value: HirId) -> HirId {
        let place = self.read(local);
        let ty = self.body.local(local).ty.clone().unwrap_or_else(coil_ty::int);
        self.push(HirKind::Assign { place, value }, ty)
    }

    pub fn block(&mut self, stmts: Vec<HirId>, tail: Option<HirId>, ty: Ty) -> HirId {
        self.push(HirKind::Block { stmts, tail }, ty)
    }

    pub fn if_(&mut self, cond: HirId, then: HirId, els: Option<HirId>, ty: Ty) -> HirId {
        self.push(HirKind::If { cond, then, els }, ty)
    }

    pub fn ret(&mut self, value: HirId) -> HirId {
        self.push(HirKind::Return(Some(value)), coil_ty::never())
    }

    pub fn builtin(&mut self, op: Builtin, args: Vec<HirId>, ty: Ty) -> HirId {
        self.push(HirKind::Builtin { op, args }, ty)
    }

    pub fn match_(&mut self, scrutinee: HirId, arms: Vec<(HirPat, HirId)>, ty: Ty) -> HirId {
        let arms = arms.into_iter().map(|(pat, body)| HirArm { pat, body }).collect();
        self.push(HirKind::Match { scrutinee, arms }, ty)
    }

    /// `Result::Ok(p)` / `Result::Err(p)` as a pattern.
    pub fn result_pat(&self, ok: bool, payload: HirPat) -> HirPat {
        let variant = if ok { "Ok" } else { "Err" };
        HirPat::Variant {
            enum_name: "Result".to_string(),
            variant: variant.to_string(),
            tag: self.checker.tag_for("Result", variant),
            fields: super::HirPatFields::Tuple(vec![payload]),
        }
    }

    pub fn finish(mut self, root: HirId) -> HirBody {
        self.body.root = Some(root);
        self.body
    }
}

/// `Range::to_vec` / `RangeInclusive::to_vec` (`float`: the
/// `__float_to_vec` sibling a float range calls): every value `for x in
/// self` visits, pushed onto a new `Vec`. An `int` body serves `byte`
/// ranges too, as the same words.
pub fn range_to_vec(checker: &Checker, name: &str, inclusive: bool, float: bool) -> HirBody {
    use super::MakeKind;
    let elem = if float { coil_ty::float() } else { coil_ty::int() };
    let range = if inclusive { coil_ty::range_inclusive_ty(elem.clone()) } else { coil_ty::range_ty(elem.clone()) };
    let vec = coil_ty::vec_app_ty(elem.clone());
    let mut b = Synth::new(checker, name, vec.clone());
    let this = b.local("self", range, LocalKind::Param);
    let out = b.local("out", vec.clone(), LocalKind::Let);
    let x = b.local("x", elem, LocalKind::Pattern);
    let empty = b.push(HirKind::Make { kind: MakeKind::Array, args: Vec::new() }, vec.clone());
    let init = b.let_(out, empty);
    let iterable = b.read(this);
    let (o, v) = (b.read(out), b.read(x));
    let callee = Callee::Method { name: "push".to_string() };
    let push = b.push(HirKind::Call { callee, args: vec![o, v] }, coil_ty::unit());
    let body = b.block(vec![push], None, coil_ty::unit());
    let kind = Some(crate::typechecking::infer::ForInKind::Range { inclusive, float });
    let each = b.push(HirKind::ForIn { pat: HirPat::Bind(x), iterable, body, kind }, coil_ty::unit());
    let o = b.read(out);
    let ret = b.ret(o);
    let root = b.block(vec![init, each, ret], None, coil_ty::never());
    b.finish(root)
}

/// `Show::show` of a builtin error enum (`IoError`, …): the variant's name,
/// or the enum's name for a word that is none of its variants. The argument
/// comes as a dictionary passes it, so it is unboxed first.
pub fn builtin_enum_show(checker: &Checker, name: &str, enum_name: &str, variants: &[&str]) -> HirBody {
    use super::HirPatFields;
    let ty = Ty::Con(enum_name.to_string());
    let mut b = Synth::new(checker, name, coil_ty::string());
    let this = b.local("self", ty.clone(), LocalKind::Param);
    let word = b.read(this);
    let tag = common::ValueTag::Instance as u32;
    let scrutinee = b.builtin(Builtin::Unbox { tag }, vec![word], ty);
    let mut arms = Vec::with_capacity(variants.len() + 1);
    for (i, variant) in variants.iter().enumerate() {
        // A one-field variant (`TtyError::Io(e)`) shows as `Io(<e>)`.
        let payload = checker.payload_tys_for(enum_name, variant);
        let (fields, shown) = match payload.as_slice() {
            [(_, ty)] => {
                let e = b.local("payload", ty.clone(), LocalKind::Pattern);
                let fmt = b.str(&format!("{variant}(%v)"));
                let v = b.read(e);
                let shown = b.call("format", vec![fmt, v], coil_ty::string());
                (HirPatFields::Tuple(vec![HirPat::Bind(e)]), shown)
            }
            _ => (HirPatFields::Unit, b.str(variant)),
        };
        let pat = HirPat::Variant {
            enum_name: enum_name.to_string(),
            variant: (*variant).to_string(),
            tag: Some(i as u32),
            fields,
        };
        arms.push((pat, shown));
    }
    arms.push((HirPat::Wild, b.str(enum_name)));
    let shown = b.match_(scrutinee, arms, coil_ty::string());
    let ret = b.ret(shown);
    let root = b.block(vec![ret], None, coil_ty::never());
    b.finish(root)
}

/// A builtin instance method's body (`Add__int__add`, `Hash__string__hash`,
/// …) over `params`: each parameter of its type, read unboxed with its tag
/// when it has one (a dictionary passes it boxed), then `op` on them, then
/// returned, boxed with `box_tag` when given. A boxed result is read as an
/// `int` word. A trailing dictionary argument is never read.
pub fn builtin_entry(
    checker: &Checker,
    name: &str,
    params: &[(Ty, Option<u32>)],
    ret: Ty,
    box_tag: Option<u32>,
    op: impl FnOnce(&mut Synth, Vec<HirId>) -> HirId,
) -> HirBody {
    let ret = if box_tag.is_some() { coil_ty::int() } else { ret };
    let mut b = Synth::new(checker, name, ret);
    let locals: Vec<_> = params
        .iter()
        .enumerate()
        .map(|(i, (ty, _))| b.local(&format!("arg{i}"), ty.clone(), LocalKind::Param))
        .collect();
    let args = locals
        .into_iter()
        .zip(params)
        .map(|(local, (ty, tag))| {
            let word = b.read(local);
            match tag {
                Some(tag) => b.builtin(Builtin::Unbox { tag: *tag }, vec![word], ty.clone()),
                None => word,
            }
        })
        .collect();
    let mut value = op(&mut b, args);
    if let Some(tag) = box_tag {
        value = b.builtin(Builtin::Box { tag }, vec![value], coil_ty::int());
    }
    let ret = b.ret(value);
    let root = b.block(vec![ret], None, coil_ty::never());
    b.finish(root)
}

/// `Show::show` of a fixed array `array` of `element`s (#1479):
/// `[a, b, c]`, each element as `format("%v", x)` shows it. The caller maps
/// `format` to the builtin.
pub fn array_show(b: &mut Synth, array: HirId, element: Ty) -> HirId {
    let string = coil_ty::string();
    let out = b.local("out", string.clone(), LocalKind::Let);
    let sep = b.local("sep", string.clone(), LocalKind::Let);
    let x = b.local("x", element, LocalKind::Pattern);
    let open = b.str("[");
    let init_out = b.let_(out, open);
    let empty = b.str("");
    let init_sep = b.let_(sep, empty);
    let (o, s) = (b.read(out), b.read(sep));
    let joined = b.bin(BinOp::StrConcat, o, s, string.clone());
    let fmt = b.str("%v");
    let v = b.read(x);
    let item = b.call("format", vec![fmt, v], string.clone());
    let joined = b.bin(BinOp::StrConcat, joined, item, string.clone());
    let append = b.assign(out, joined);
    let comma = b.str(", ");
    let next_sep = b.assign(sep, comma);
    let body = b.block(vec![append, next_sep], None, coil_ty::unit());
    let kind = Some(crate::typechecking::infer::ForInKind::Array);
    let each = b.push(HirKind::ForIn { pat: HirPat::Bind(x), iterable: array, body, kind }, coil_ty::unit());
    let o = b.read(out);
    let close = b.str("]");
    let shown = b.bin(BinOp::StrConcat, o, close, string.clone());
    b.block(vec![init_out, init_sep, each], Some(shown), string)
}

/// `Hash::hash` of a tuple (`items`, one per item) or array (`items` its
/// element) `value` (#1479): `h = ((h & M) * 31) ^ hash(x)` over the items
/// from `0`, as a derived `Hash` combines fields. `targets` names each
/// item's `hash` entry (`None`: a `()` item, hashed as `0`).
pub fn structural_hash(b: &mut Synth, value: HirId, items: &[Ty], targets: &[Option<String>]) -> HirId {
    let int = coil_ty::int();
    let acc = b.local("h", int.clone(), LocalKind::Let);
    let zero = b.int(0);
    let init = b.let_(acc, zero);
    let step = |b: &mut Synth, item: Option<HirId>, target: &Option<String>| {
        let h = match (item, target) {
            (Some(item), Some(target)) => b.call(target, vec![item], int.clone()),
            _ => b.int(0),
        };
        let cur = b.read(acc);
        let mask = b.int(288230376151711743);
        let low = b.bin(BinOp::BitAnd, cur, mask, int.clone());
        let k = b.int(31);
        let spread = b.bin(BinOp::IntMul, low, k, int.clone());
        let mixed = b.bin(BinOp::BitXor, spread, h, int.clone());
        b.assign(acc, mixed)
    };
    let mut stmts = vec![init];
    match b.body.exprs[value.0 as usize].ty.clone() {
        Some(Ty::Array { element, .. }) => {
            let x = b.local("x", (*element).clone(), LocalKind::Pattern);
            let v = b.read(x);
            let body = step(b, Some(v), &targets[0]);
            let body = b.block(vec![body], None, coil_ty::unit());
            let kind = Some(crate::typechecking::infer::ForInKind::Array);
            stmts.push(b.push(HirKind::ForIn { pat: HirPat::Bind(x), iterable: value, body, kind }, coil_ty::unit()));
        }
        _ => {
            let mut pats = Vec::with_capacity(items.len());
            let mut locals = Vec::with_capacity(items.len());
            for (i, item) in items.iter().enumerate() {
                if targets[i].is_none() {
                    pats.push(HirPat::Wild);
                    locals.push(None);
                } else {
                    let l = b.local(&format!("x{i}"), item.clone(), LocalKind::Pattern);
                    pats.push(HirPat::Bind(l));
                    locals.push(Some(l));
                }
            }
            stmts.push(b.push(HirKind::LetPat { pat: HirPat::Tuple(pats), init: value }, coil_ty::unit()));
            for (local, target) in locals.into_iter().zip(targets) {
                let v = local.map(|l| b.read(l));
                let s = step(b, v, target);
                stmts.push(s);
            }
        }
    }
    let out = b.read(acc);
    b.block(stmts, Some(out), int)
}

/// `Lt::lt` (`method`, or `le` / `gt` / `ge`) of two tuples `a` and `b`
/// of `items` (#1721): lexicographic, the first item that differs decides.
/// Item `i` before the last decides when it orders strictly
/// (`x_i < y_i`), and passes on only when neither orders before the other
/// (`x_i <= y_i && y_i <= x_i`, so a `NaN` item decides `false`); the last
/// takes the operator itself. Each item compares as the operator does at
/// its type (a primitive, a string's bytes, its own instance).
pub fn tuple_order(b: &mut Synth, a: HirId, c: HirId, items: &[Ty], method: &str) -> HirId {
    let boolean = coil_ty::boolean();
    let (strict, op): (&'static str, &'static str) = match method {
        "lt" => ("<", "<"),
        "le" => ("<", "<="),
        "gt" => (">", ">"),
        _ => (">", ">="),
    };
    let mut stmts = Vec::with_capacity(2);
    let mut sides = [Vec::with_capacity(items.len()), Vec::with_capacity(items.len())];
    for (side, (value, prefix)) in [(a, "x"), (c, "y")].into_iter().enumerate() {
        let mut pats = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let l = b.local(&format!("{prefix}{i}"), item.clone(), LocalKind::Pattern);
            pats.push(HirPat::Bind(l));
            sides[side].push(l);
        }
        stmts.push(b.push(HirKind::LetPat { pat: HirPat::Tuple(pats), init: value }, coil_ty::unit()));
    }
    let cmp = |b: &mut Synth, sym: &'static str, i: usize, swap: bool| {
        let (l, r) = if swap { (sides[1][i], sides[0][i]) } else { (sides[0][i], sides[1][i]) };
        let (l, r) = (b.read(l), b.read(r));
        let op = super::build::resolve_bin(sym, Some(&items[i]), Some(&items[i]));
        b.bin(op, l, r, boolean.clone())
    };
    let last = items.len() - 1;
    let mut acc = cmp(b, op, last, false);
    for i in (0..last).rev() {
        let before = cmp(b, strict, i, false);
        let le = cmp(b, "<=", i, false);
        let ge = cmp(b, "<=", i, true);
        let same = b.push(HirKind::Logic { and: true, lhs: le, rhs: ge }, boolean.clone());
        let rest = b.push(HirKind::Logic { and: true, lhs: same, rhs: acc }, boolean.clone());
        acc = b.push(HirKind::Logic { and: false, lhs: before, rhs: rest }, boolean.clone());
    }
    b.block(stmts, Some(acc), boolean)
}
