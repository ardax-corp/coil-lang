//! HIR bodies the compiler makes itself, with no source of their own: the
//! auto-par fork-join workers, the test-runner `main` and the builtin
//! methods' bodies (`Range::to_vec`, …). They lower like
//! any other body (HIR lowering, then direct MIR), instead of being written
//! out by hand.
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
                hints: Default::default(),
                contract_spans: Vec::new(),
                locals: Vec::new(),
                exprs: Vec::new(),
                root: None,
                aliased_fn_values: Vec::new(),
                fn_key_sites: Vec::new(),
                stray_muts: Vec::new(),
                inline_scopes: Vec::new(),
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
            borrow: false,
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
            scope: 0,
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

/// A product type taken apart: a tuple, an anonymous record or one enum
/// variant. At run time each is one slot store; here they differ only in
/// the pattern that opens them.
#[derive(Clone, Debug)]
pub struct Product {
    pub ctor: ProductCtor,
    /// Each part's type, in pattern order (a record variant's too: a
    /// pattern binds it positionally).
    pub parts: Vec<Ty>,
}

#[derive(Clone, Debug)]
pub enum ProductCtor {
    Tuple,
    /// A record, by its field names.
    Record(Vec<String>),
    Variant { enum_name: String, name: String, tag: Option<u32> },
}

impl Product {
    pub fn tuple(items: &[Ty]) -> Self {
        Self { ctor: ProductCtor::Tuple, parts: items.to_vec() }
    }

    /// A record's product, its fields in name order (the order a ground
    /// record's and an open one's structural thunks take them in).
    pub fn record(fields: &[(String, Ty)]) -> Self {
        let mut fields = fields.to_vec();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        let (names, parts) = fields.into_iter().unzip();
        Self { ctor: ProductCtor::Record(names), parts }
    }

    /// A tuple's or record's product.
    pub fn of(ty: &Ty) -> Option<Self> {
        match ty {
            Ty::Tuple(items) => Some(Self::tuple(items)),
            Ty::Record { fields } => Some(Self::record(fields)),
            _ => None,
        }
    }

    /// This product's pattern over one pattern per part.
    pub fn pat_of(&self, parts: Vec<HirPat>) -> HirPat {
        match &self.ctor {
            ProductCtor::Tuple => HirPat::Tuple(parts),
            ProductCtor::Record(names) => HirPat::Record(names.iter().cloned().zip(parts).collect()),
            ProductCtor::Variant { enum_name, name, tag } => HirPat::Variant {
                enum_name: enum_name.clone(),
                variant: name.clone(),
                tag: *tag,
                fields: if parts.is_empty() { super::HirPatFields::Unit } else { super::HirPatFields::Tuple(parts) },
            },
        }
    }

    /// The pattern that binds each part to a new local `{side}{i}`, and
    /// those locals.
    pub fn pat(&self, b: &mut Synth, side: &str) -> (HirPat, Vec<LocalId>) {
        let locals: Vec<_> = self.parts.iter().enumerate().map(|(i, t)| b.local(&format!("{side}{i}"), t.clone(), LocalKind::Pattern)).collect();
        (self.pat_of(locals.iter().map(|&l| HirPat::Bind(l)).collect()), locals)
    }

    /// `let <pat> = init` of [`Self::pat`], for a tuple or record.
    pub fn bind(&self, b: &mut Synth, init: HirId, side: &str) -> (HirId, Vec<LocalId>) {
        let (pat, locals) = self.pat(b, side);
        (b.push(HirKind::LetPat { pat, init }, coil_ty::unit()), locals)
    }
}

/// Whether `a ⊕ p` leaves the `int` range, as a test that does not trap
/// (`None`: `op` cannot overflow). A fork-join asks before it folds two
/// results, and runs the sequential code instead of trapping itself.
pub fn int_overflows(b: &mut Synth, op: BinOp, a: LocalId, p: LocalId) -> Option<HirId> {
    let boolean = coil_ty::boolean;
    let cmp = |b: &mut Synth, op: BinOp, l: LocalId, r: HirId| {
        let l = b.read(l);
        b.bin(op, l, r, boolean())
    };
    let positive = |b: &mut Synth, l: LocalId| {
        let zero = b.int(0);
        cmp(b, BinOp::Gt, l, zero)
    };
    // `limit ⊘ x` for a literal `limit`.
    let room = |b: &mut Synth, op: BinOp, limit: i64, x: LocalId| {
        let (limit, x) = (b.int(limit), b.read(x));
        b.bin(op, limit, x, coil_ty::int())
    };
    match op {
        // `a + p` leaves the range past `MAX - p` or under `MIN - p`.
        BinOp::IntAdd => {
            let up = positive(b, p);
            let most = room(b, BinOp::IntSub, i64::MAX, p);
            let over = cmp(b, BinOp::Gt, a, most);
            let least = room(b, BinOp::IntSub, i64::MIN, p);
            let under = cmp(b, BinOp::Lt, a, least);
            Some(b.if_(up, over, Some(under), boolean()))
        }
        // `a - p` leaves it under `MIN + p` or past `MAX + p`.
        BinOp::IntSub => {
            let up = positive(b, p);
            let least = room(b, BinOp::IntAdd, i64::MIN, p);
            let under = cmp(b, BinOp::Lt, a, least);
            let most = room(b, BinOp::IntAdd, i64::MAX, p);
            let over = cmp(b, BinOp::Gt, a, most);
            Some(b.if_(up, under, Some(over), boolean()))
        }
        // `a * p` by the signs: each quotient is of a literal by a
        // non-zero operand whose sign keeps it in range.
        BinOp::IntMul => {
            let a_up = positive(b, a);
            let on_up = {
                let p_up = positive(b, p);
                let most = room(b, BinOp::IntDiv, i64::MAX, p);
                let over = cmp(b, BinOp::Gt, a, most);
                let least = room(b, BinOp::IntDiv, i64::MIN, a);
                let under = cmp(b, BinOp::Lt, p, least);
                b.if_(p_up, over, Some(under), boolean())
            };
            let on_down = {
                let p_up = positive(b, p);
                let least = room(b, BinOp::IntDiv, i64::MIN, p);
                let under = cmp(b, BinOp::Lt, a, least);
                let zero = b.int(0);
                let p_down = cmp(b, BinOp::Lt, p, zero);
                let most = room(b, BinOp::IntDiv, i64::MAX, p);
                let over = cmp(b, BinOp::Lt, a, most);
                let over = b.push(HirKind::Logic { and: true, lhs: p_down, rhs: over }, boolean());
                b.if_(p_up, under, Some(over), boolean())
            };
            Some(b.if_(a_up, on_up, Some(on_down), boolean()))
        }
        _ => None,
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
    let scrutinee = b.builtin(Builtin::Retype, vec![word], ty);
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
/// …) over `params`: each parameter of its type, retyped from the word a
/// dictionary passes where marked, then `op` on them, then returned,
/// retyped to an `int` word when `word_ret`. A trailing dictionary
/// argument is never read.
pub fn builtin_entry(
    checker: &Checker,
    name: &str,
    params: &[(Ty, bool)],
    ret: Ty,
    word_ret: bool,
    op: impl FnOnce(&mut Synth, Vec<HirId>) -> HirId,
) -> HirBody {
    let ret = if word_ret { coil_ty::int() } else { ret };
    let mut b = Synth::new(checker, name, ret);
    let locals: Vec<_> = params
        .iter()
        .enumerate()
        .map(|(i, (ty, _))| b.local(&format!("arg{i}"), ty.clone(), LocalKind::Param))
        .collect();
    let args = locals
        .into_iter()
        .zip(params)
        .map(|(local, &(ref ty, retype))| {
            let word = b.read(local);
            if retype { b.builtin(Builtin::Retype, vec![word], ty.clone()) } else { word }
        })
        .collect();
    let mut value = op(&mut b, args);
    if word_ret {
        value = b.builtin(Builtin::Retype, vec![value], coil_ty::int());
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

/// The callee of item `i`'s comparison `sym` in [`tuple_order`].
pub type OrderTarget = dyn Fn(&str, usize) -> String;

/// `Hash::hash` of `value` (#1479, #2380), a tuple or record (`product`,
/// a record's fields in name order) or an array (`None`; `targets` holds
/// its element's): `h = ((h & M) * 31) ^ hash(x)` over the items from `0`,
/// as a derived `Hash` combines fields. `targets` names each item's `hash`
/// entry (`None`: a `()` item, hashed as `0`); with `dict`, each is
/// called with that local after the item (an open instance's, #2554).
///
/// `relayout[i]`, where given, is the type in whose layout item `i`'s
/// `hash` takes it (a shared entry's open `Option<T>`, #2590).
pub fn structural_hash(b: &mut Synth, value: HirId, product: Option<&Product>, targets: &[Option<String>], relayout: &[Option<Ty>], dict: Option<LocalId>) -> HirId {
    let int = coil_ty::int();
    let acc = b.local("h", int.clone(), LocalKind::Let);
    let zero = b.int(0);
    let init = b.let_(acc, zero);
    let step = |b: &mut Synth, item: Option<HirId>, i: usize| {
        let h = match (item, &targets[i]) {
            (Some(mut item), Some(target)) => {
                if let Some(Some(ty)) = relayout.get(i) {
                    item = b.builtin(crate::hir::Builtin::Relayout, vec![item], ty.clone());
                }
                let mut args = vec![item];
                args.extend(dict.map(|d| b.read(d)));
                b.call(target, args, int.clone())
            }
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
    match (product, b.body.exprs[value.0 as usize].ty.clone()) {
        (None, Some(Ty::Array { element, .. })) => {
            let x = b.local("x", (*element).clone(), LocalKind::Pattern);
            let v = b.read(x);
            let body = step(b, Some(v), 0);
            let body = b.block(vec![body], None, coil_ty::unit());
            let kind = Some(crate::typechecking::infer::ForInKind::Array);
            stmts.push(b.push(HirKind::ForIn { pat: HirPat::Bind(x), iterable: value, body, kind }, coil_ty::unit()));
        }
        (None, _) => unreachable!("structural hash of a tuple, record or array"),
        (Some(product), _) => {
            let items = &product.parts;
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
            stmts.push(b.push(HirKind::LetPat { pat: product.pat_of(pats), init: value }, coil_ty::unit()));
            for (i, local) in locals.into_iter().enumerate() {
                let v = local.map(|l| b.read(l));
                let s = step(b, v, i);
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
///
/// With `bound`, item `i`'s `sym` is a call of `bound.1(sym, i)` with the
/// two items and local `bound.0` (an open instance's dictionary, #2554).
pub fn tuple_order(b: &mut Synth, a: HirId, c: HirId, items: &[Ty], method: &str, bound: Option<(LocalId, &OrderTarget)>) -> HirId {
    let boolean = coil_ty::boolean();
    let (strict, op): (&'static str, &'static str) = match method {
        "lt" => ("<", "<"),
        "le" => ("<", "<="),
        "gt" => (">", ">"),
        _ => (">", ">="),
    };
    let product = Product::tuple(items);
    let (la, xs) = product.bind(b, a, "x");
    let (lc, ys) = product.bind(b, c, "y");
    let sides = [xs, ys];
    let cmp = |b: &mut Synth, sym: &'static str, i: usize, swap: bool| {
        let (l, r) = if swap { (sides[1][i], sides[0][i]) } else { (sides[0][i], sides[1][i]) };
        let (l, r) = (b.read(l), b.read(r));
        if let Some((dict, target)) = bound {
            let d = b.read(dict);
            return b.call(&target(sym, i), vec![l, r, d], boolean.clone());
        }
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
    b.block(vec![la, lc], Some(acc), boolean)
}
