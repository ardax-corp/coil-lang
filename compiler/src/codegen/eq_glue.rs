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
//!
//! A shared generic body has no ground type to key glue by: `==` on an
//! aggregate of its type parameters (`Vec<T>`, `(T, int)`, `Option<T>`)
//! calls an open glue function instead (#1642), one per such type up to
//! renaming its variables, which takes each variable's `Eq` dictionary
//! after the two values and compares a bare variable's part through it.

use super::*;
use crate::hir::{BinOp, Callee, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, LocalKind, UnOp};
use crate::hir::synth::Synth;
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
/// the VM's structural `EQ` compares the two values (#1694).
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

/// The type variables of `ty`, in order of first appearance.
fn vars_in_order(ty: &Ty, out: &mut Vec<TyVarId>) {
    match ty {
        Ty::Var(v) => {
            if !out.contains(v) {
                out.push(*v);
            }
        }
        Ty::App(head, items) => {
            vars_in_order(head, out);
            items.iter().for_each(|t| vars_in_order(t, out));
        }
        Ty::Fun(a, r) => {
            vars_in_order(a, out);
            vars_in_order(r, out);
        }
        Ty::Tuple(items) => items.iter().for_each(|t| vars_in_order(t, out)),
        Ty::Record { fields } => fields.iter().for_each(|(_, t)| vars_in_order(t, out)),
        Ty::Array { element, .. } | Ty::Readonly(element) => vars_in_order(element, out),
        _ => {}
    }
}

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
        if !crate::layout::ty_is_closed(ty) || visiting.contains(ty) {
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

impl Compiler {
    /// The `Eq` dictionary of type variable `v` here: its frame slot and
    /// the `eq` method's slot in it. An open glue body's own, else one of
    /// this shared body's bounds (`T: Eq`, or a subclass's as `T: Ord`).
    fn eq_var_dict(&self, v: TyVarId) -> Option<(u32, u32)> {
        if let Some(&(n, method)) = self.synth_eq_dicts.get(&v) {
            return Some((self.lookup_slot(&format!("__dict{n}"))?, method));
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
        for name in [self.current_function_qualified.as_deref(), self.current_function_table_key.as_deref()].into_iter().flatten() {
            let Some(scheme) = self.checker.env().lookup(name) else {
                continue;
            };
            for (i, c) in scheme.constraints.iter().enumerate() {
                let on_var = matches!(c.args.as_slice(), [a] if crate::typechecking::subst::apply_ty_prune(subst, a) == var);
                if on_var
                    && let Some(method) = eq_slot(&c.class)
                    && let Some(slot) = self.lookup_slot(&format!("__dict{i}"))
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
        let mut needed = false;
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
            Some(&(n, 0)) => self.lookup_slot(&format!("__dict{n}")),
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
        vars_in_order(&key, &mut order);
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
            // (`Vec<Box<T>>` at `T: Lt`, #1694); else the VM's `EQ` compares
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
            return (method == 0).then(|| self.lookup_slot(&format!("__dict{n}"))).flatten().map(VarEqDict::Frame);
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
        for name in [self.current_function_qualified.as_deref(), self.current_function_table_key.as_deref()].into_iter().flatten() {
            let Some(scheme) = self.checker.env().lookup(name) else {
                continue;
            };
            for (i, c) in scheme.constraints.iter().enumerate() {
                let on_var = matches!(c.args.as_slice(), [a] if crate::typechecking::subst::apply_ty_prune(subst, a) == var);
                let Some(class) = generics.typeclass(&c.class).filter(|_| on_var) else {
                    continue;
                };
                let flat = class.flattened_methods(generics);
                let at = |method: &str| flat.iter().position(|(owner, m)| *owner == "Eq" && m.name == method).map(|i| i as u32);
                if let (Some(eq), Some(ne)) = (at("eq"), at("ne"))
                    && let Some(slot) = self.lookup_slot(&format!("__dict{i}"))
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
            VarEqDict::Project { slot, eq, ne } => {
                out.extend([DictStep::Load(slot), DictStep::Field(eq), DictStep::Load(slot), DictStep::Field(ne), DictStep::Tuple(2)]);
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

    /// Emit [`EQ_ANY`]'s `fqn`: the VM's structural `EQ` of the two words
    /// (`ne` its negation), each unboxed first as a shared body holds a
    /// type variable's value; the trailing dictionary is not read.
    pub(super) fn emit_eq_any(&mut self, fqn: &str, ne: bool) {
        let int = coil_ty::int();
        let boolean = coil_ty::boolean();
        let native = common::VALUES_EQ_SHAPED_NATIVE;
        let params = [(int.clone(), Some(common::UNBOX_ANY_TAG)), (int.clone(), Some(common::UNBOX_ANY_TAG))];
        let had = self.hir_synth_callees.insert(native.to_string(), SynthCallee { host: true, ..SynthCallee::default() });
        self.emit_builtin_entry(fqn, &params, boolean.clone(), None, |b, a| {
            // No shape: `values_eq`.
            let none = b.int(0);
            let eq = b.call(native, vec![a[0], a[1], none], int.clone());
            let zero = b.int(0);
            b.bin(if ne { BinOp::Eq } else { BinOp::Ne }, eq, zero, boolean.clone())
        });
        match had {
            Some(callee) => self.hir_synth_callees.insert(native.to_string(), callee),
            None => self.hir_synth_callees.remove(native),
        };
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
        self.emit_synth_fn(fqn, &hir, HashMap::new(), false);
        self.synth_plain_dicts = None;
        self.synth_eq_dicts.clear();
    }
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
