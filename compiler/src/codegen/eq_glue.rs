//! `==` / `!=` on an aggregate whose elements have a user `Eq` (#1629),
//! or a `float` leaf (#1253).
//!
//! structural equality compares a class instance by identity: it cannot
//! call an `Eq` instance. So an array, `Vec`, tuple, record, `Option`,
//! `Result` or enum payload (of an enum with no `Eq` of its own) that
//! holds, at any depth, a class or enum with an `Eq` instance compares
//! through a compiler-made glue function instead: one per such type,
//! comparing part by part with `==` at the part's type. That `==`
//! dispatches as any other: a part with an `Eq` instance calls it, a part
//! that needs glue calls its glue, and anything else compares as the
//! runtime does (a class without `Eq` by identity). An array, `Vec`, tuple
//! or record with a `float` leaf compares through glue too, so the leaf
//! compares by IEEE 754 as scalar `float` `==` does, in plain code and with
//! no runtime call.
//!
//! A glue function is reserved the first time a call of it is planned (in
//! a body, a monomorphic clone, a structural `Eq` thunk or another glue)
//! and emitted with the structural thunks.
//!
//! A shared generic body has no ground type to key glue by: `==` on an
//! aggregate of its type parameters (`Vec<T>`, `(T, int)`, `Option<T>`)
//! calls an open glue function instead (#1642), one per such type up to
//! renaming its variables, which takes each variable's `Eq` dictionary
//! after the two values and compares a bare variable's part through it.

use super::*;
use crate::hir::{BinOp, Builtin, Callee, HirId, HirKind, HirPat, IndexKind, Lit, LocalId, LocalKind, UnOp};
use crate::hir::synth::{Product, ProductCtor, Synth};
use crate::typechecking::generics::Generics;
use crate::typechecking::ty::{self as coil_ty, TyVarId, strip_readonly};

/// The pseudo-class glue functions are named under (`EqGlue__Vec<Q>__eq`).
const EQ_GLUE: &str = "EqGlue";

/// The pseudo-class open glue functions are named under
/// (`EqDictGlue__Vec<$0>__eq`, a variable as `$i`, or `$i.s` when its
/// dictionary holds `eq` at slot `s`, a subclass bound's).
const EQ_DICT_GLUE: &str = "EqDictGlue";

/// The pseudo-class of the `Eq` dictionary a shared body passes for a
/// type variable none of whose bounds has `Eq` (`T: Lt`): `eq` / `ne` as
/// structural equality compares the two values (#1694).
const EQ_ANY: &str = "EqAny";

/// An open glue call: the glue's key type and name, each variable of the
/// key with its `eq` slot, in the glue's dictionary order, and each one's
/// dictionary at the call.
#[derive(Clone, Debug)]
pub(super) struct DictGlue {
    pub key: Ty,
    pub fqn: String,
    pub vars: Vec<(TyVarId, u32)>,
    pub dicts: Vec<VarEqDict>,
}

impl DictGlue {
    /// The frame dictionaries the call reads.
    pub fn slots(&self) -> impl Iterator<Item = u32> + '_ {
        self.dicts.iter().filter_map(|d| match *d {
            VarEqDict::Frame(slot) | VarEqDict::Project { slot, .. } => Some(slot),
            VarEqDict::Any => None,
        })
    }
}

/// Where a shared body takes a type variable's dictionary for an `Eq`
/// goal from (#1694).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum VarEqDict {
    /// A frame dictionary, passed as it is.
    Frame(u32),
    /// A subclass bound's (`T: Keyed` with `trait Keyed<T: Eq>`): a new
    /// `Eq` dictionary of the `eq` and `ne` in frame dictionary `slot`.
    Project { slot: u32, eq: u32, ne: u32 },
    /// No bound has `Eq`: [`EQ_ANY`]'s.
    Any,
}


/// How a glue function takes its type apart.
#[derive(Clone, Debug)]
pub(super) enum GlueShape {
    /// An array or `Vec` of this element.
    Seq(Ty),
    /// A tuple or a record.
    Product(Product),
    /// An enum's variants, each with its payload's types in declaration
    /// order.
    Enum(Vec<Product>),
}

impl GlueShape {
    fn parts(&self) -> Vec<&Ty> {
        match self {
            Self::Seq(element) => vec![element],
            Self::Product(product) => product.parts.iter().collect(),
            Self::Enum(variants) => variants.iter().flat_map(|v| &v.parts).collect(),
        }
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
            Ty::Tuple(items) if !items.is_empty() => Some(GlueShape::Product(Product { ctor: ProductCtor::Tuple, parts: items.iter().map(key).collect() })),
            Ty::Record { fields } if !fields.is_empty() => Some(GlueShape::Product(Product {
                ctor: ProductCtor::Record(fields.iter().map(|(n, _)| n.clone()).collect()),
                parts: fields.iter().map(|(_, t)| key(t)).collect(),
            })),
            Ty::Con(_) | Ty::App(..) if !self.eq_user_instance(ty) => {
                let (name, args) = match ty {
                    Ty::Con(name) => (name.clone(), Vec::new()),
                    Ty::App(head, args) => match head.as_ref() {
                        Ty::Con(name) => (name.clone(), args.clone()),
                        _ => return None,
                    },
                    _ => return None,
                };
                let variant = |vname: &str, tag: Option<u32>, parts: Vec<Ty>| Product {
                    ctor: ProductCtor::Variant { enum_name: name.clone(), name: vname.to_string(), tag },
                    parts,
                };
                let builtin = |vname: &str, parts: Vec<Ty>| variant(vname, self.checker.tag_for(&name, vname), parts);
                if name == common::BUILTIN_OPTION_ENUM {
                    let [some] = args.as_slice() else { return None };
                    return Some(GlueShape::Enum(vec![builtin("Some", vec![key(some)]), builtin("None", Vec::new())]));
                }
                if name == common::BUILTIN_RESULT_ENUM {
                    let [ok, err] = args.as_slice() else { return None };
                    return Some(GlueShape::Enum(vec![builtin("Ok", vec![key(ok)]), builtin("Err", vec![key(err)])]));
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
                    .map(|(vname, tag, fields)| variant(vname, Some(*tag), fields.iter().map(|t| key(&crate::layout::bind_params(t, &params, &args))).collect()))
                    .collect();
                Some(GlueShape::Enum(variants))
            }
            _ => None,
        }
    }

    /// Whether `ty` (a key) compares through glue: it is taken apart and
    /// some part, at any depth, has a user `Eq` instance, or it is an
    /// array, `Vec`, tuple or record with a `float` leaf ([`float_leaf`]).
    fn eq_glue_needed(&self, ty: &Ty, visiting: &mut Vec<Ty>) -> bool {
        self.eq_glue_needed_in(ty, visiting, false)
    }

    /// [`Self::eq_glue_needed`]; `closed`: `ty` is a part of a closed
    /// tuple, array, `Vec` or record, so closed too. Only a named type can
    /// lead back to itself, so only those are kept in `visiting`: asking a
    /// structural part again whether it is closed and whether it is being
    /// visited compared whole types at every level, `O(d^3)` for a value
    /// `d` levels deep (#2657).
    fn eq_glue_needed_in(&self, ty: &Ty, visiting: &mut Vec<Ty>, closed: bool) -> bool {
        let structural = matches!(ty, Ty::Tuple(_) | Ty::Array { .. } | Ty::Record { .. }) || coil_ty::vec_element_ty(ty).is_some();
        if !(closed || crate::layout::ty_is_closed(ty)) || !structural && visiting.contains(ty) {
            return false;
        }
        let Some(shape) = self.eq_glue_shape(ty) else {
            return false;
        };
        // A two-word tuple of immediates (`(int, float)`) needs none: its
        // `==` compares the two words, a `float` one as a `float`, with no
        // box made (`from_hir::pair_eq`).
        if float_leaf(&shape) && !matches!(crate::layout::two_word_kind(&self.checker, ty), Some(crate::layout::PairKind::Product)) {
            return true;
        }
        if !structural {
            visiting.push(ty.clone());
        }
        let needed = shape.parts().into_iter().any(|p| self.eq_user_instance(p) || self.eq_glue_needed_in(p, visiting, structural));
        if !structural {
            visiting.pop();
        }
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
        let params = [(ty.clone(), false), (ty.clone(), false)];
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

impl Compiler {
    /// The `Eq` dictionary of type variable `v` here: its frame slot and
    /// the `eq` method's slot in it. An open glue body's own, else one of
    /// this shared body's bounds (`T: Eq`, or a subclass's as `T: Ord`).
    fn eq_var_dict(&self, v: TyVarId) -> Option<(u32, u32)> {
        if let Some(&(n, method)) = self.synth_eq_dicts.get(&v) {
            return Some((self.frame_dict(n)?, method));
        }
        if self.compiling_mono_clone {
            return None;
        }
        let generics = self.checker.generics();
        let eq_slot = |class: &str| {
            let flat = generics.typeclass(class)?.flattened_methods(generics);
            flat.iter().position(|(owner, m)| *owner == "Eq" && m.name == "eq").map(|i| i as u32)
        };
        let var = Ty::Var(v);
        if let Some(OpenDictGoal::Slot(slot)) = self.resolve_open_dict_goal("Eq", std::slice::from_ref(&var)) {
            return Some((slot, eq_slot("Eq")?));
        }
        let subst = self.checker.subst();
        for name in &self.dict_owner_names() {
            let Some(dicts) = self.body_dicts_of(name) else {
                continue;
            };
            for (i, c) in dicts.iter().enumerate() {
                let on_var = matches!(c.args.as_slice(), [a] if crate::typechecking::subst::apply_ty_prune(subst, a) == var);
                if on_var
                    && let Some(method) = eq_slot(&c.class)
                    && let Some(slot) = self.frame_dict(i)
                {
                    return Some((slot, method));
                }
            }
        }
        None
    }

    /// Whether an open part `ty` with a user `Eq` instance (`Box<T>`
    /// deriving it) compares in open glue: its instance's context is only
    /// `Eq`, which the glue builds from its variables' dictionaries
    /// (#1661).
    fn eq_open_instance_ok(&self, ty: &Ty) -> bool {
        let generics = self.checker.generics();
        generics
            .find_generic_instance("Eq", std::slice::from_ref(ty))
            .is_some_and(|inst| inst.context.iter().all(|c| c.class == "Eq"))
    }

    /// Whether open `ty` (a key) compares through open glue: it is taken
    /// apart and some part, at any depth, is a type variable, a type with
    /// a user `Eq` instance or a type needing glue. `None` when a part is
    /// an open type with a user `Eq` instance whose context the glue
    /// cannot build ([`Self::eq_open_instance_ok`]).
    /// With `vars` false a bare type variable part does not count: whether
    /// the key needs glue for its other parts.
    fn eq_dict_glue_needed(&self, ty: &Ty, vars: bool, visiting: &mut Vec<Ty>) -> Option<bool> {
        if visiting.contains(ty) {
            return Some(false);
        }
        let Some(shape) = self.eq_glue_shape(ty) else {
            return Some(false);
        };
        visiting.push(ty.clone());
        let mut needed = float_leaf(&shape);
        for part in shape.parts() {
            needed |= match part {
                Ty::Var(_) => vars,
                p if crate::layout::ty_is_closed(p) => self.eq_user_instance(p) || self.eq_glue_needed(p, &mut Vec::new()),
                p if self.eq_user_instance(p) && self.eq_open_instance_ok(p) => true,
                p if self.eq_user_instance(p) => {
                    visiting.pop();
                    return None;
                }
                p => match self.eq_dict_glue_needed(p, vars, visiting) {
                    Some(n) => n,
                    None => {
                        visiting.pop();
                        return None;
                    }
                },
            };
        }
        visiting.pop();
        Some(needed)
    }

    /// Whether some part of open key `ty`, at any depth, is an open type
    /// with a user `Eq` instance.
    fn eq_open_instance_part(&self, ty: &Ty, visiting: &mut Vec<Ty>) -> bool {
        if visiting.contains(ty) {
            return false;
        }
        let Some(shape) = self.eq_glue_shape(ty) else {
            return false;
        };
        visiting.push(ty.clone());
        let found = shape.parts().into_iter().any(|p| {
            !matches!(p, Ty::Var(_))
                && !crate::layout::ty_is_closed(p)
                && (self.eq_user_instance(p) || self.eq_open_instance_part(p, visiting))
        });
        visiting.pop();
        found
    }

    /// `Eq<v>` in an open glue body: `v`'s own dictionary, when it is an
    /// `Eq` one (`eq` at slot 0), for an open part's instance context
    /// (#1661).
    pub(super) fn eq_glue_body_var_dict(&self, v: TyVarId) -> Option<u32> {
        match self.synth_eq_dicts.get(&v) {
            Some(&(n, 0)) => self.frame_dict(n),
            _ => None,
        }
    }

    /// The open glue `==` (`ne`: `!=`) of open `ty` calls here, when `ty`
    /// needs one and each of its variables has an `Eq` dictionary.
    pub(super) fn eq_dict_glue_target(&self, ty: &Ty, ne: bool) -> Option<DictGlue> {
        let key = self.eq_glue_key(ty);
        if crate::layout::ty_is_closed(&key) || matches!(key, Ty::Var(_)) {
            return None;
        }
        let mut order = Vec::new();
        key.collect_vars(&mut order);
        if order.len() != coil_ty::ftv_ty(&key).len() {
            return None;
        }
        let frame = order.iter().map(|&v| self.eq_var_dict(v)).collect::<Option<Vec<_>>>();
        let (vars, dicts) = match frame {
            // An open part's instance takes its variables' dictionaries as
            // `Eq` ones: a subclass bound's holding `eq` elsewhere is
            // projected to one, below (#1661).
            Some(dicts) if !(dicts.iter().any(|&(_, method)| method != 0) && self.eq_open_instance_part(&key, &mut Vec::new())) => {
                if self.eq_dict_glue_needed(&key, true, &mut Vec::new()) != Some(true) {
                    return None;
                }
                let vars = order.into_iter().zip(dicts.iter().map(|&(_, m)| m)).collect();
                (vars, dicts.into_iter().map(|(slot, _)| VarEqDict::Frame(slot)).collect())
            }
            // Each variable's dictionary as an `Eq` one, [`EQ_ANY`]'s for
            // one with no `Eq` bound, when some other part needs the glue
            // (`Vec<Box<T>>` at `T: Lt`, #1694); else structural equality compares
            // the value, as it compares the variables' parts.
            _ => {
                if self.eq_dict_glue_needed(&key, false, &mut Vec::new()) != Some(true) {
                    return None;
                }
                let dicts = order.iter().map(|&v| self.var_eq_dict(v)).collect::<Option<Vec<_>>>()?;
                (order.into_iter().map(|v| (v, 0)).collect::<Vec<_>>(), dicts)
            }
        };
        let names: HashMap<TyVarId, Ty> = vars
            .iter()
            .enumerate()
            .map(|(i, &(v, method))| (v, Ty::Con(if method == 0 { format!("${i}") } else { format!("${i}.{method}") })))
            .collect();
        let canon = Self::apply_ty_var_map(&key, &names);
        let fqn = Generics::builtin_instance_fqn(EQ_DICT_GLUE, &canon.to_string(), if ne { "ne" } else { "eq" });
        Some(DictGlue { vars, dicts, key, fqn })
    }

    /// `v`'s dictionary for an `Eq` goal here ([`VarEqDict`]): a frame
    /// one of `Eq`, one projected from a bound whose class has `Eq` as a
    /// superclass, or, in a shared body with no such bound, [`EQ_ANY`]'s.
    pub(super) fn var_eq_dict(&self, v: TyVarId) -> Option<VarEqDict> {
        if let Some(&(n, method)) = self.synth_eq_dicts.get(&v) {
            // An open glue body's own; a subclass's holds no `ne` it knows.
            return (method == 0).then(|| self.frame_dict(n)).flatten().map(VarEqDict::Frame);
        }
        if self.compiling_mono_clone {
            return None;
        }
        let var = Ty::Var(v);
        if let Some(OpenDictGoal::Slot(slot)) = self.resolve_open_dict_goal("Eq", std::slice::from_ref(&var)) {
            return Some(VarEqDict::Frame(slot));
        }
        let generics = self.checker.generics();
        let subst = self.checker.subst();
        for name in &self.dict_owner_names() {
            let Some(dicts) = self.body_dicts_of(name) else {
                continue;
            };
            for (i, c) in dicts.iter().enumerate() {
                let on_var = matches!(c.args.as_slice(), [a] if crate::typechecking::subst::apply_ty_prune(subst, a) == var);
                let Some(class) = generics.typeclass(&c.class).filter(|_| on_var) else {
                    continue;
                };
                let flat = class.flattened_methods(generics);
                let at = |method: &str| flat.iter().position(|(owner, m)| *owner == "Eq" && m.name == method).map(|i| i as u32);
                if let (Some(eq), Some(ne)) = (at("eq"), at("ne"))
                    && let Some(slot) = self.frame_dict(i)
                {
                    return Some(VarEqDict::Project { slot, eq, ne });
                }
            }
        }
        Some(VarEqDict::Any)
    }

    /// Plan dictionary `dict` (an `Eq` one) into `out`.
    pub(super) fn plan_var_eq_dict(&mut self, out: &mut Vec<DictStep>, dict: VarEqDict) -> bool {
        match dict {
            VarEqDict::Frame(slot) => out.push(DictStep::Load(slot)),
            // `(eq, ne, D)`: `D`'s `eq` and `ne` take `D` itself (each may
            // be a forwarder into `D`, or read a context that follows
            // `D`'s methods), so the two slots here forward to `D`, the
            // third element (#2476).
            VarEqDict::Project { slot, eq, ne } => {
                let start = out.len();
                for (method, at) in [("eq", eq), ("ne", ne)] {
                    let Some(code) = self.dict_forwarder("Eq", method, 2, Some(at)) else {
                        out.truncate(start);
                        return false;
                    };
                    out.push(code);
                }
                out.extend([DictStep::Load(slot), DictStep::Tuple(3)]);
            }
            VarEqDict::Any => {
                let start = out.len();
                for method in ["eq", "ne"] {
                    let fqn = Generics::builtin_instance_fqn(EQ_ANY, "T", method);
                    if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
                        self.reserve_function_entry(fqn.clone());
                        self.structural_thunks.push((fqn.clone(), format!("any_{method}"), coil_ty::int()));
                    }
                    let Some(code) = self.dict_code_step(&fqn) else {
                        out.truncate(start);
                        return false;
                    };
                    out.push(code);
                }
                out.push(DictStep::Tuple(2));
            }
        }
        true
    }

    /// Emit [`EQ_ANY`]'s `fqn`: structural equality of the two words
    /// (`ne` its negation), each in its own representation as a shared
    /// body holds a type variable's value; the trailing dictionary is not
    /// read. The words are retyped to `string`, whose `==` is the runtime's
    /// structural equality of two words (an immediate equal by its bits).
    pub(super) fn emit_eq_any(&mut self, fqn: &str, ne: bool) {
        let (int, string, boolean) = (coil_ty::int(), coil_ty::string(), coil_ty::boolean());
        let params = [(int.clone(), true), (int, true)];
        self.emit_builtin_entry(fqn, &params, boolean.clone(), false, |b, a| {
            let x = b.builtin(Builtin::Retype, vec![a[0]], string.clone());
            let y = b.builtin(Builtin::Retype, vec![a[1]], string);
            b.bin(if ne { BinOp::Ne } else { BinOp::Eq }, x, y, boolean)
        });
    }

    /// Reserve the open glue functions of `glue`'s key, as
    /// [`Self::reserve_eq_glue`] does.
    pub(super) fn reserve_eq_dict_glue(&mut self, glue: &DictGlue) {
        for method in ["eq", "ne"] {
            let fqn = match glue.fqn.rsplit_once("__") {
                Some((base, _)) => format!("{base}__{method}"),
                None => continue,
            };
            if self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn) {
                continue;
            }
            self.reserve_function_entry(fqn.clone());
            self.eq_dict_glue.insert(fqn.clone(), glue.vars.clone());
            self.structural_thunks.push((fqn, format!("dglue_{method}"), glue.key.clone()));
        }
    }

    /// `==` at type variable `v` in an open glue body: its dictionary's
    /// `eq`.
    pub(super) fn eq_dict_glue_bound(&self, ty: &Ty) -> Option<(u32, u32)> {
        match crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty) {
            Ty::Var(v) if self.synth_eq_dicts.contains_key(&v) => self.eq_var_dict(v),
            _ => None,
        }
    }

    /// Emit the open glue `fqn` of `ty`: [`Self::emit_eq_glue`]'s body, its
    /// variables' `Eq` dictionaries `__dict0..` after the two values.
    pub(super) fn emit_eq_dict_glue(&mut self, fqn: &str, ty: &Ty, ne: bool) {
        use crate::hir::synth::Synth;
        let (Some(vars), Some(shape)) = (self.eq_dict_glue.get(fqn).cloned(), self.eq_glue_shape(ty)) else {
            return;
        };
        let boolean = coil_ty::boolean();
        let mut b = Synth::new(&self.checker, fqn, boolean.clone());
        let a = b.local("arg0", ty.clone(), LocalKind::Param);
        let c = b.local("arg1", ty.clone(), LocalKind::Param);
        let value = if ne {
            let (av, cv) = (b.read(a), b.read(c));
            let eq = b.bin(BinOp::Overloaded("=="), av, cv, boolean.clone());
            b.push(HirKind::Un { op: UnOp::Not, operand: eq }, boolean)
        } else {
            glue_body(&mut b, a, c, &shape)
        };
        let ret = b.ret(value);
        let root = b.block(vec![ret], None, coil_ty::never());
        let hir = Self::synth_generic(b.finish(root));
        self.synth_plain_dicts = Some(vars.len());
        self.synth_eq_dicts = vars.iter().enumerate().map(|(i, &(v, method))| (v, (i, method))).collect();
        self.emit_synth_fn(fqn, &hir, HashMap::new());
        self.synth_plain_dicts = None;
        self.synth_eq_dicts.clear();
    }
}

/// Whether an array, `Vec`, tuple or record shape has a `float` leaf
/// through arrays, `Vec`s, tuples and records: its glue compares that
/// leaf with `float` `==`, by IEEE 754 (`NaN != NaN`, `0.0 == -0.0`),
/// where structural equality compares the bits (#1253). An enum payload's
/// `float` is not one: a two-word `Option<float>` already compares its
/// payload word as a `float`.
fn float_leaf(shape: &GlueShape) -> bool {
    fn ty_leaf(ty: &Ty) -> bool {
        let ty = strip_readonly(ty);
        if crate::hir::classify::primitive(ty) == Some(coil_ty::FLOAT) {
            return true;
        }
        if let Some(element) = coil_ty::vec_element_ty(ty) {
            return ty_leaf(element);
        }
        match ty {
            Ty::Array { element, .. } => ty_leaf(element),
            Ty::Tuple(items) => items.iter().any(ty_leaf),
            Ty::Record { fields } => fields.iter().any(|(_, t)| ty_leaf(t)),
            _ => false,
        }
    }
    !matches!(shape, GlueShape::Enum(_)) && shape.parts().into_iter().any(ty_leaf)
}

/// `x == y` at `ty`; `()` parts are always equal.
fn part_eq(b: &mut Synth, x: HirId, y: HirId, ty: &Ty) -> HirId {
    if crate::layout::is_unit(ty) {
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

/// One part of an opened product: a local its pattern bound, or a
/// record's field, read from the record each time.
enum Part {
    Local(LocalId),
    Field(LocalId, String, Ty),
}

/// The parts of tuple or record `v`, with the `let` that binds a tuple's
/// to `{side}{i}`.
fn open(b: &mut Synth, product: &Product, v: LocalId, side: &str) -> (Option<HirId>, Vec<Part>) {
    if let ProductCtor::Record(names) = &product.ctor {
        return (None, names.iter().zip(&product.parts).map(|(n, t)| Part::Field(v, n.clone(), t.clone())).collect());
    }
    let init = b.read(v);
    let (bound, locals) = product.bind(b, init, side);
    (Some(bound), locals.into_iter().map(Part::Local).collect())
}

/// `x0 == y0 && ..` over the parts of two values of one product.
fn parts_eq(b: &mut Synth, xs: Vec<Part>, ys: Vec<Part>, tys: &[Ty]) -> HirId {
    let read = |b: &mut Synth, part: Part| match part {
        Part::Local(l) => b.read(l),
        Part::Field(v, name, ty) => {
            let base = b.read(v);
            b.push(HirKind::Field { base, name }, ty)
        }
    };
    let parts = xs
        .into_iter()
        .zip(ys)
        .zip(tys)
        .map(|((x, y), t)| {
            let (xv, yv) = (read(b, x), read(b, y));
            part_eq(b, xv, yv, t)
        })
        .collect();
    all(b, parts)
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
        // `let (x..) = a; let (y..) = c; x.. == y..`; a record's fields
        // are read in place, each as it is compared.
        GlueShape::Product(product) => {
            let (la, xs) = open(b, product, a, "x");
            let (lc, ys) = open(b, product, c, "y");
            let eq = parts_eq(b, xs, ys, &product.parts);
            match (la, lc) {
                (Some(la), Some(lc)) => b.block(vec![la, lc], Some(eq), boolean),
                _ => eq,
            }
        }
        // `match a { V(x..) => match c { V(y..) => x.. == y.., _ => false }, .. }`
        GlueShape::Enum(variants) => {
            let mut arms = Vec::with_capacity(variants.len());
            for v in variants {
                let (pa, xs) = v.pat(b, "x");
                let (pc, ys) = v.pat(b, "y");
                let eq = parts_eq(b, xs.into_iter().map(Part::Local).collect(), ys.into_iter().map(Part::Local).collect(), &v.parts);
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
