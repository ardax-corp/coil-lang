//! `==` / `!=` on an aggregate whose elements have a user `Eq` (#1629).
//!
//! The VM's structural `EQ` (and `values_eq_shaped`) compares a class
//! instance by identity: it cannot call an `Eq` instance. So an array,
//! `Vec`, tuple, record, `Option`, `Result` or enum payload (of an enum with
//! no `Eq` of its own) that holds, at any depth, a class or enum with an
//! `Eq` instance compares through a compiler-made glue function instead:
//! one per such type, comparing part by part with `==` at the part's type.
//! That `==` dispatches as any other: a part with an `Eq` instance calls
//! it, a part that needs glue calls its glue, and anything else compares
//! as the VM does (a class without `Eq` by identity).
//!
//! A glue function is reserved the first time a call of it is planned (in
//! a body, a monomorphic clone, a structural `Eq` thunk or another glue)
//! and emitted with the structural thunks.

use super::*;
use crate::hir::{BinOp, Callee, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, LocalKind, UnOp};
use crate::hir::synth::Synth;
use crate::typechecking::generics::Generics;
use crate::typechecking::ty::{self as coil_ty, strip_readonly};

/// The pseudo-class glue functions are named under (`EqGlue__Vec<Q>__eq`).
const EQ_GLUE: &str = "EqGlue";

/// How a glue function takes its type apart.
#[derive(Clone, Debug)]
pub(super) enum GlueShape {
    /// An array or `Vec` of this element.
    Seq(Ty),
    Tuple(Vec<Ty>),
    /// A record's fields.
    Record(Vec<(String, Ty)>),
    /// An enum's variants, each with its payload's types in declaration
    /// order (a record variant's too: a pattern binds it positionally).
    Enum { name: String, variants: Vec<GlueVariant> },
}

#[derive(Clone, Debug)]
pub(super) struct GlueVariant {
    name: String,
    tag: Option<u32>,
    fields: Vec<Ty>,
}

impl GlueShape {
    fn parts(&self) -> Vec<&Ty> {
        match self {
            Self::Seq(element) => vec![element],
            Self::Tuple(items) => items.iter().collect(),
            Self::Record(fields) => fields.iter().map(|(_, t)| t).collect(),
            Self::Enum { variants, .. } => variants.iter().flat_map(|v| &v.fields).collect(),
        }
    }
}

/// `ty` with the type parameters `params` replaced by `args`.
fn subst_params(ty: &Ty, params: &[String], args: &[Ty]) -> Ty {
    let go = |t: &Ty| subst_params(t, params, args);
    match ty {
        Ty::Con(name) => params.iter().position(|p| p == name).and_then(|i| args.get(i).cloned()).unwrap_or_else(|| ty.clone()),
        Ty::App(head, items) => Ty::App(Box::new(go(head)), items.iter().map(go).collect()),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(go).collect()),
        Ty::Record { fields } => Ty::Record {
            fields: fields.iter().map(|(n, t)| (n.clone(), go(t))).collect(),
        },
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(go(element)),
            length: *length,
        },
        Ty::Readonly(inner) => Ty::Readonly(Box::new(go(inner))),
        other => other.clone(),
    }
}

impl Compiler {
    /// `ty` as glue keys it: resolved, without `readonly`, an enum value's
    /// refinement as its enum.
    pub(super) fn eq_glue_key(&self, ty: &Ty) -> Ty {
        let ty = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        Self::show_lookup_ty_for_instance(strip_readonly(&ty))
    }

    /// The name of `ty`'s glue for `method` (`eq` / `ne`).
    fn eq_glue_fqn(ty: &Ty, method: &str) -> String {
        Generics::builtin_instance_fqn(EQ_GLUE, &ty.to_string(), method)
    }

    /// Whether `fqn` names a glue function.
    pub(super) fn is_eq_glue(fqn: &str) -> bool {
        fqn.strip_prefix(EQ_GLUE).is_some_and(|rest| rest.starts_with("__"))
    }

    /// Whether `ty` (a key) is a class or user enum with an `Eq` instance.
    fn eq_user_instance(&self, ty: &Ty) -> bool {
        let name = match ty {
            Ty::Con(name) => name,
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(name) => name,
                _ => return false,
            },
            _ => return false,
        };
        if name == common::BUILTIN_OPTION_ENUM || name == common::BUILTIN_RESULT_ENUM || name == common::BUILTIN_VEC_TYPE {
            return false;
        }
        (self.checker.is_class(name) || self.checker.enum_variants(name).is_some())
            && self.checker.generics().find_instance_relaxed("Eq", std::slice::from_ref(ty)).is_some()
    }

    /// How `ty` (a key) is compared part by part, when it is an aggregate
    /// or an enum with no `Eq` of its own.
    fn eq_glue_shape(&self, ty: &Ty) -> Option<GlueShape> {
        let key = |t: &Ty| self.eq_glue_key(t);
        if let Some(element) = coil_ty::vec_element_ty(ty) {
            return Some(GlueShape::Seq(key(element)));
        }
        match ty {
            Ty::Array { element, .. } => Some(GlueShape::Seq(key(element))),
            Ty::Tuple(items) if !items.is_empty() => Some(GlueShape::Tuple(items.iter().map(key).collect())),
            Ty::Record { fields } if !fields.is_empty() => Some(GlueShape::Record(fields.iter().map(|(n, t)| (n.clone(), key(t))).collect())),
            Ty::Con(_) | Ty::App(..) if !self.eq_user_instance(ty) => {
                let (name, args) = match ty {
                    Ty::Con(name) => (name.clone(), Vec::new()),
                    Ty::App(head, args) => match head.as_ref() {
                        Ty::Con(name) => (name.clone(), args.clone()),
                        _ => return None,
                    },
                    _ => return None,
                };
                let variant = |vname: &str, fields: Vec<Ty>| GlueVariant {
                    name: vname.to_string(),
                    tag: self.checker.tag_for(&name, vname),
                    fields,
                };
                if name == common::BUILTIN_OPTION_ENUM {
                    let [some] = args.as_slice() else { return None };
                    let variants = vec![variant("Some", vec![key(some)]), variant("None", Vec::new())];
                    return Some(GlueShape::Enum { name, variants });
                }
                if name == common::BUILTIN_RESULT_ENUM {
                    let [ok, err] = args.as_slice() else { return None };
                    let variants = vec![variant("Ok", vec![key(ok)]), variant("Err", vec![key(err)])];
                    return Some(GlueShape::Enum { name, variants });
                }
                if self.checker.is_class(&name) {
                    return None;
                }
                let declared = self.checker.enum_variants(&name)?;
                let params = self.checker.type_ctor_params(&name).cloned().unwrap_or_default();
                if params.len() != args.len() {
                    return None;
                }
                let variants = declared
                    .iter()
                    .map(|(vname, tag, fields)| GlueVariant {
                        name: vname.clone(),
                        tag: Some(*tag),
                        fields: fields.iter().map(|t| key(&subst_params(t, &params, &args))).collect(),
                    })
                    .collect();
                Some(GlueShape::Enum { name, variants })
            }
            _ => None,
        }
    }

    /// Whether `ty` (a key) compares through glue: it is taken apart and
    /// some part, at any depth, has a user `Eq` instance.
    fn eq_glue_needed(&self, ty: &Ty, visiting: &mut Vec<Ty>) -> bool {
        if !crate::hir::layout::ty_is_closed(ty) || visiting.contains(ty) {
            return false;
        }
        let Some(shape) = self.eq_glue_shape(ty) else {
            return false;
        };
        visiting.push(ty.clone());
        let needed = shape.parts().into_iter().any(|p| self.eq_user_instance(p) || self.eq_glue_needed(p, visiting));
        visiting.pop();
        needed
    }

    /// The glue `==` (`ne`: `!=`) of `ty` calls, when `ty` needs one.
    pub(super) fn eq_glue_target(&self, ty: &Ty, ne: bool) -> Option<(Ty, String)> {
        let key = self.eq_glue_key(ty);
        if !self.eq_glue_needed(&key, &mut Vec::new()) {
            return None;
        }
        let fqn = Self::eq_glue_fqn(&key, if ne { "ne" } else { "eq" });
        Some((key, fqn))
    }

    /// Reserve the glue functions of `ty` (a key) when a call first needs
    /// them, emitted with the structural thunks. A glue body's own `==`
    /// reserves its parts' as it is planned.
    pub(super) fn reserve_eq_glue(&mut self, ty: &Ty) {
        for method in ["eq", "ne"] {
            let fqn = Self::eq_glue_fqn(ty, method);
            if self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn) {
                continue;
            }
            self.note_raw_builtin_thunk(&fqn);
            self.reserve_function_entry(fqn.clone());
            self.structural_thunk_params.insert(fqn.clone(), vec![ty.clone(), ty.clone()]);
            self.structural_thunks.push((fqn, format!("glue_{method}"), ty.clone()));
        }
    }

    /// Emit the glue `fqn` of `ty`: `true` when every part is `==`; `ne`
    /// the negation of `ty`'s `==`, which calls the `eq` glue.
    pub(super) fn emit_eq_glue(&mut self, fqn: &str, ty: &Ty, ne: bool) {
        let Some(shape) = self.eq_glue_shape(ty) else {
            return;
        };
        let boolean = coil_ty::boolean();
        let params = [(ty.clone(), None), (ty.clone(), None)];
        self.emit_builtin_method(fqn, &params, boolean.clone(), |b, _| {
            // Each use reads the parameter afresh: a node has one parent.
            let (a, c) = (b.body.params[0], b.body.params[1]);
            if ne {
                let (av, cv) = (b.read(a), b.read(c));
                let eq = b.bin(BinOp::Overloaded("=="), av, cv, boolean.clone());
                return b.push(HirKind::Un { op: UnOp::Not, operand: eq }, boolean);
            }
            glue_body(b, a, c, &shape)
        });
    }
}

/// `x == y` at `ty`; `()` parts are always equal.
fn part_eq(b: &mut Synth, x: HirId, y: HirId, ty: &Ty) -> HirId {
    if crate::hir::layout::is_unit(ty) {
        return b.push(HirKind::Lit(Lit::Bool(true)), coil_ty::boolean());
    }
    let op = crate::hir::build::resolve_bin("==", Some(ty), Some(ty));
    b.bin(op, x, y, coil_ty::boolean())
}

/// `a && b && ..`, `true` for none.
fn all(b: &mut Synth, parts: Vec<HirId>) -> HirId {
    let mut parts = parts.into_iter();
    let Some(first) = parts.next() else {
        return b.push(HirKind::Lit(Lit::Bool(true)), coil_ty::boolean());
    };
    parts.fold(first, |acc, p| b.push(HirKind::Logic { and: true, lhs: acc, rhs: p }, coil_ty::boolean()))
}

fn bool_lit(b: &mut Synth, v: bool) -> HirId {
    b.push(HirKind::Lit(Lit::Bool(v)), coil_ty::boolean())
}

/// Whether `a` and `c` (of the shape's type) are equal, part by part.
fn glue_body(b: &mut Synth, a: LocalId, c: LocalId, shape: &GlueShape) -> HirId {
    let boolean = coil_ty::boolean();
    let unit = coil_ty::unit();
    let int = coil_ty::int();
    match shape {
        // `if a.len() != c.len() { return false }`, then each element
        // against `c`'s at its index.
        GlueShape::Seq(element) => {
            let len = |b: &mut Synth, v: HirId| {
                b.push(HirKind::Call { callee: Callee::Method { name: "len".to_string() }, args: vec![v] }, coil_ty::int())
            };
            let (av, cv) = (b.read(a), b.read(c));
            let la = len(b, av);
            let lc = len(b, cv);
            let differ = b.bin(BinOp::Ne, la, lc, boolean.clone());
            let no = bool_lit(b, false);
            let early = b.ret(no);
            let then = b.block(vec![early], None, unit.clone());
            let check_len = b.if_(differ, then, None, unit.clone());
            let i = b.local("i", int.clone(), LocalKind::Let);
            let zero = b.int(0);
            let init = b.let_(i, zero);
            let x = b.local("x", element.clone(), LocalKind::Pattern);
            let xv = b.read(x);
            let iv = b.read(i);
            let cv = b.read(c);
            let y = b.push(HirKind::Index { base: cv, index: iv, kind: IndexKind::Array }, element.clone());
            let same = part_eq(b, xv, y, element);
            let differ = b.push(HirKind::Un { op: UnOp::Not, operand: same }, boolean.clone());
            let no = bool_lit(b, false);
            let early = b.ret(no);
            let then = b.block(vec![early], None, unit.clone());
            let check = b.if_(differ, then, None, unit.clone());
            let iv = b.read(i);
            let one = b.int(1);
            let next = b.bin(BinOp::IntAdd, iv, one, int.clone());
            let step = b.assign(i, next);
            let body = b.block(vec![check, step], None, unit.clone());
            let kind = Some(crate::typechecking::infer::ForInKind::Array);
            let av = b.read(a);
            let each = b.push(HirKind::ForIn { pat: HirPat::Bind(x), iterable: av, body, kind }, unit);
            let yes = bool_lit(b, true);
            b.block(vec![check_len, init, each], Some(yes), boolean)
        }
        GlueShape::Tuple(items) => {
            let bind = |b: &mut Synth, v: LocalId, side: &str| {
                let v = b.read(v);
                let locals: Vec<_> = items.iter().enumerate().map(|(i, t)| b.local(&format!("{side}{i}"), t.clone(), LocalKind::Pattern)).collect();
                let pat = HirPat::Tuple(locals.iter().map(|&l| HirPat::Bind(l)).collect());
                (b.push(HirKind::LetPat { pat, init: v }, coil_ty::unit()), locals)
            };
            let (la, xs) = bind(b, a, "x");
            let (lc, ys) = bind(b, c, "y");
            let parts = xs
                .into_iter()
                .zip(ys)
                .zip(items)
                .map(|((x, y), t)| {
                    let (xv, yv) = (b.read(x), b.read(y));
                    part_eq(b, xv, yv, t)
                })
                .collect();
            let eq = all(b, parts);
            b.block(vec![la, lc], Some(eq), boolean)
        }
        GlueShape::Record(fields) => {
            let parts = fields
                .iter()
                .map(|(name, t)| {
                    let (av, cv) = (b.read(a), b.read(c));
                    let x = b.push(HirKind::Field { base: av, name: name.clone() }, t.clone());
                    let y = b.push(HirKind::Field { base: cv, name: name.clone() }, t.clone());
                    part_eq(b, x, y, t)
                })
                .collect();
            all(b, parts)
        }
        // `match a { V(x..) => match c { V(y..) => x.. == y.., _ => false }, .. }`
        GlueShape::Enum { name, variants } => {
            let pat = |b: &mut Synth, v: &GlueVariant, side: &str| {
                let locals: Vec<_> = v.fields.iter().enumerate().map(|(i, t)| b.local(&format!("{side}{i}"), t.clone(), LocalKind::Pattern)).collect();
                let fields = if v.fields.is_empty() {
                    HirPatFields::Unit
                } else {
                    HirPatFields::Tuple(locals.iter().map(|&l| HirPat::Bind(l)).collect())
                };
                let pat = HirPat::Variant {
                    enum_name: name.clone(),
                    variant: v.name.clone(),
                    tag: v.tag,
                    fields,
                };
                (pat, locals)
            };
            let mut arms = Vec::with_capacity(variants.len());
            for v in variants {
                let (pa, xs) = pat(b, v, "x");
                let (pc, ys) = pat(b, v, "y");
                let parts = xs
                    .into_iter()
                    .zip(ys)
                    .zip(&v.fields)
                    .map(|((x, y), t)| {
                        let (xv, yv) = (b.read(x), b.read(y));
                        part_eq(b, xv, yv, t)
                    })
                    .collect();
                let eq = all(b, parts);
                let no = bool_lit(b, false);
                let cv = b.read(c);
                let inner = b.match_(cv, vec![(pc, eq), (HirPat::Wild, no)], boolean.clone());
                arms.push((pa, inner));
            }
            let av = b.read(a);
            b.match_(av, arms, boolean)
        }
    }
}
