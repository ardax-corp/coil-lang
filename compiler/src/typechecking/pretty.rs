//! Pretty-printing for [`Ty`] and [`Scheme`] (diagnostics and tests).

use std::collections::HashMap;
use std::fmt;

use super::subst::{Subst, apply_ty_prune};
use super::ty::{ArrayLength, EnumVariantPayloadTy, Scheme, Ty, TyVarId, ftv_ty};

/// Format a type for user-facing diagnostics.
///
/// Applies the full substitution, then renames any remaining free
/// type variables to `a`, `b`, `c`, … so messages never show raw
/// counters like `` `t43` ``.
pub fn format_ty_for_diag(subst: &Subst, ty: &Ty) -> String {
    let pruned = apply_ty_prune(subst, ty);
    let mut rename = HashMap::new();
    let mut next = 0u32;
    format_ty_renamed(&pruned, &mut rename, &mut next)
}

/// Format a ground type as a fully-qualified name for `typeof`.
///
/// Returns `None` when the type still contains free variables, `never`,
/// or a forall binder — those are not resolvable to a stable string.
///
/// Nominal heads are qualified with their defining module when known and
/// non-empty (`prelude::Option<int>`, `math::Point`). Entry-file / empty
/// modules stay bare (`Point`).
pub fn format_ty_fqn(ty: &Ty, nominal_modules: &HashMap<String, String>) -> Option<String> {
    if !ftv_ty(ty).is_empty() {
        return None;
    }
    format_ty_fqn_inner(ty, nominal_modules)
}

fn qualify_nominal(name: &str, nominal_modules: &HashMap<String, String>) -> String {
    match nominal_modules.get(name) {
        Some(module) if !module.is_empty() => format!("{}::{}", module, name),
        _ => name.to_string(),
    }
}

fn format_ty_fqn_inner(ty: &Ty, nominal_modules: &HashMap<String, String>) -> Option<String> {
    match ty {
        Ty::Var(_) | Ty::Never | Ty::Forall { .. } => None,
        Ty::Con(name) => Some(qualify_nominal(name, nominal_modules)),
        Ty::Existential { class } => Some(class.clone()),
        Ty::Fun(a, b) => {
            let left = format_ty_fqn_inner(a, nominal_modules)?;
            let right = format_ty_fqn_inner(b, nominal_modules)?;
            if needs_paren(a) {
                Some(format!("({}) -> {}", left, right))
            } else {
                Some(format!("{} -> {}", left, right))
            }
        }
        Ty::App(c, args) => {
            if args.is_empty() {
                return format_ty_fqn_inner(c, nominal_modules);
            }
            let head = match c.as_ref() {
                Ty::Con(name) => qualify_nominal(name, nominal_modules),
                other => format_ty_fqn_inner(other, nominal_modules)?,
            };
            if let Ty::Con(name) = c.as_ref()
                && name == "coroutine"
                && args.len() == 2
            {
                let y = format_ty_fqn_inner(&args[0], nominal_modules)?;
                if matches!(&args[1], Ty::Con(n) if n == "unit") {
                    return Some(format!("coroutine<{}>", y));
                }
                let s = format_ty_fqn_inner(&args[1], nominal_modules)?;
                return Some(format!("coroutine<{}, {}>", y, s));
            }
            let inner = args
                .iter()
                .map(|t| format_ty_fqn_inner(t, nominal_modules))
                .collect::<Option<Vec<_>>>()?
                .join(", ");
            Some(format!("{}<{}>", head, inner))
        }
        Ty::List(inner) => Some(format!("[{}]", format_ty_fqn_inner(inner, nominal_modules)?)),
        Ty::Sum { name, variants } => {
            let head = qualify_nominal(name, nominal_modules);
            if name == common::BUILTIN_OPTION_ENUM {
                let inner = variants
                    .iter()
                    .find(|(n, _)| n == "Some")
                    .and_then(|(_, p)| match p {
                        EnumVariantPayloadTy::Tuple(tys) => tys.first(),
                        _ => None,
                    })?;
                return Some(format!(
                    "{}<{}>",
                    head,
                    format_ty_fqn_inner(inner, nominal_modules)?
                ));
            }
            if name == common::BUILTIN_RESULT_ENUM {
                let ok = variants
                    .iter()
                    .find(|(n, _)| n == "Ok")
                    .and_then(|(_, p)| match p {
                        EnumVariantPayloadTy::Tuple(tys) => tys.first(),
                        _ => None,
                    })?;
                let err = variants
                    .iter()
                    .find(|(n, _)| n == "Err")
                    .and_then(|(_, p)| match p {
                        EnumVariantPayloadTy::Tuple(tys) => tys.first(),
                        _ => None,
                    })?;
                return Some(format!(
                    "{}<{}, {}>",
                    head,
                    format_ty_fqn_inner(ok, nominal_modules)?,
                    format_ty_fqn_inner(err, nominal_modules)?
                ));
            }
            // User generic enums stored as Sum: recover args from the first
            // non-unit payload field types when they look like type params.
            Some(head)
        }
        Ty::Constructor { owner, .. } => format_ty_fqn_inner(owner, nominal_modules),
        Ty::Tuple(tys) => {
            let inner = tys
                .iter()
                .map(|t| format_ty_fqn_inner(t, nominal_modules))
                .collect::<Option<Vec<_>>>()?
                .join(", ");
            Some(format!("({})", inner))
        }
        Ty::Array { element, length } => {
            let elem = format_ty_fqn_inner(element, nominal_modules)?;
            match length {
                ArrayLength::Static(n) => Some(format!("[{}; {}]", elem, n)),
                ArrayLength::Dynamic => Some(format!("[{}]", elem)),
            }
        }
        Ty::Record { fields } => {
            let inner = fields
                .iter()
                .map(|(name, ty)| {
                    format_ty_fqn_inner(ty, nominal_modules)
                        .map(|t| format!("{}: {}", name, t))
                })
                .collect::<Option<Vec<_>>>()?
                .join(", ");
            Some(format!("{{ {} }}", inner))
        }
        Ty::Readonly(inner) => Some(format!(
            "readonly {}",
            format_ty_fqn_inner(inner, nominal_modules)?
        )),
    }
}

fn fresh_diag_name(next: &mut u32) -> String {
    // a..z, then a1, b1, …
    let n = *next;
    *next += 1;
    if n < 26 {
        ((b'a' + n as u8) as char).to_string()
    } else {
        let letter = (b'a' + (n % 26) as u8) as char;
        format!("{}{}", letter, n / 26)
    }
}

fn format_ty_renamed(ty: &Ty, rename: &mut HashMap<TyVarId, String>, next: &mut u32) -> String {
    match ty {
        Ty::Var(v) => rename
            .entry(*v)
            .or_insert_with(|| fresh_diag_name(next))
            .clone(),
        Ty::Con(name) => name.clone(),
        Ty::Fun(a, b) => {
            let left = format_ty_renamed(a, rename, next);
            let right = format_ty_renamed(b, rename, next);
            if needs_paren(a) {
                format!("({}) -> {}", left, right)
            } else {
                format!("{} -> {}", left, right)
            }
        }
        Ty::App(c, args) => {
            if args.is_empty() {
                format_ty_renamed(c, rename, next)
            } else if let Ty::Con(name) = c.as_ref() {
                if name == "coroutine" && args.len() == 2 {
                    let y = format_ty_renamed(&args[0], rename, next);
                    let s = format_ty_renamed(&args[1], rename, next);
                    if matches!(&args[1], Ty::Con(n) if n == "unit") {
                        return format!("coroutine<{}>", y);
                    }
                    return format!("coroutine<{}, {}>", y, s);
                }
                let inner = args
                    .iter()
                    .map(|t| format_ty_renamed(t, rename, next))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}<{}>", name, inner)
            } else {
                let head = format_ty_renamed(c, rename, next);
                let inner = args
                    .iter()
                    .map(|t| format_ty_renamed(t, rename, next))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{}<{}>", head, inner)
            }
        }
        Ty::List(inner) => format!("[{}]", format_ty_renamed(inner, rename, next)),
        Ty::Sum { name, variants } => {
            let mut out = format!("enum {} {{ ", name);
            for (i, (vname, payload)) in variants.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(vname);
                match payload {
                    EnumVariantPayloadTy::Unit => {}
                    EnumVariantPayloadTy::Tuple(tys) => {
                        out.push('(');
                        for (j, p) in tys.iter().enumerate() {
                            if j > 0 {
                                out.push_str(", ");
                            }
                            out.push_str(&format_ty_renamed(p, rename, next));
                        }
                        out.push(')');
                    }
                    EnumVariantPayloadTy::Record(fields) => {
                        out.push_str(" { ");
                        for (j, (fname, fty)) in fields.iter().enumerate() {
                            if j > 0 {
                                out.push_str(", ");
                            }
                            out.push_str(fname);
                            out.push_str(": ");
                            out.push_str(&format_ty_renamed(fty, rename, next));
                        }
                        out.push_str(" }");
                    }
                }
            }
            out.push_str(" }");
            out
        }
        Ty::Constructor { owner, tag, .. } => {
            format!("{}::v{}", format_ty_renamed(owner, rename, next), tag)
        }
        Ty::Tuple(tys) => {
            let inner = tys
                .iter()
                .map(|t| format_ty_renamed(t, rename, next))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({})", inner)
        }
        Ty::Array { element, length } => {
            let elem = format_ty_renamed(element, rename, next);
            match length {
                ArrayLength::Static(n) => format!("[{}; {}]", elem, n),
                ArrayLength::Dynamic => format!("[{}]", elem),
            }
        }
        Ty::Record { fields } => {
            let inner = fields
                .iter()
                .map(|(name, ty)| format!("{}: {}", name, format_ty_renamed(ty, rename, next)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {} }}", inner)
        }
        Ty::Readonly(inner) => format!("readonly {}", format_ty_renamed(inner, rename, next)),
        Ty::Existential { class } => class.clone(),
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => {
            for v in bounds {
                rename.entry(*v).or_insert_with(|| fresh_diag_name(next));
            }
            let vars = bounds
                .iter()
                .map(|v| {
                    let mut s = rename
                        .get(v)
                        .cloned()
                        .unwrap_or_else(|| fresh_diag_name(next));
                    let classes = constraints
                        .iter()
                        .filter(|c| c.is_unary_on(*v))
                        .map(|c| c.class.as_str())
                        .collect::<Vec<_>>();
                    if !classes.is_empty() {
                        s.push_str(": ");
                        s.push_str(&classes.join(" + "));
                    }
                    s
                })
                .collect::<Vec<_>>()
                .join(", ");
            let body_s = format_ty_renamed(body, rename, next);
            let multi: Vec<String> = constraints
                .iter()
                .filter(|c| {
                    c.args.len() != 1 || c.primary_var().is_none_or(|v| !bounds.contains(&v))
                })
                .map(|c| {
                    if c.args.is_empty() {
                        c.class.clone()
                    } else {
                        let args = c
                            .args
                            .iter()
                            .map(|t| format_ty_renamed(t, rename, next))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{}<{}>", c.class, args)
                    }
                })
                .collect();
            if multi.is_empty() {
                format!("forall {}. {}", vars, body_s)
            } else {
                format!("forall {}. {} where {}", vars, body_s, multi.join(", "))
            }
        }
        Ty::Never => "never".to_string(),
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Var(v) => write!(f, "t{}", v.raw()),
            Ty::Never => write!(f, "never"),
            Ty::Con(name) => write!(f, "{}", name),
            Ty::Fun(a, b) => {
                if needs_paren(a) {
                    write!(f, "({}) -> {}", a, b)
                } else {
                    write!(f, "{} -> {}", a, b)
                }
            }
            Ty::App(c, args) => {
                if args.is_empty() {
                    write!(f, "{}", c)
                } else if let Ty::Con(name) = c.as_ref() {
                    if name == "coroutine" && args.len() == 2 {
                        let y = &args[0];
                        let s = &args[1];
                        if matches!(s, Ty::Con(n) if n == "unit") {
                            return write!(f, "coroutine<{}>", y);
                        }
                        return write!(f, "coroutine<{}, {}>", y, s);
                    }
                    write!(
                        f,
                        "{}<{}>",
                        c,
                        args.iter()
                            .map(|t| t.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                } else {
                    let inner = args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, "{}<{}>", c, inner)
                }
            }
            Ty::List(inner) => write!(f, "[{}]", inner),
            Ty::Sum { name, variants } => {
                write!(f, "enum {} {{ ", name)?;
                for (i, (vname, payload)) in variants.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", vname)?;
                    match payload {
                        EnumVariantPayloadTy::Unit => {}
                        EnumVariantPayloadTy::Tuple(tys) => {
                            write!(f, "(")?;
                            for (j, p) in tys.iter().enumerate() {
                                if j > 0 {
                                    write!(f, ", ")?;
                                }
                                write!(f, "{}", p)?;
                            }
                            write!(f, ")")?;
                        }
                        EnumVariantPayloadTy::Record(fields) => {
                            write!(f, " {{ ")?;
                            for (j, (fname, fty)) in fields.iter().enumerate() {
                                if j > 0 {
                                    write!(f, ", ")?;
                                }
                                write!(f, "{}: {}", fname, fty)?;
                            }
                            write!(f, " }}")?;
                        }
                    }
                }
                write!(f, " }}")
            }
            Ty::Constructor { owner, tag, .. } => {
                write!(f, "{}::v{}", owner, tag)
            }
            Ty::Tuple(tys) => {
                write!(f, "(")?;
                for (i, t) in tys.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, ")")
            }
            Ty::Array { element, length } => match length {
                ArrayLength::Static(n) => write!(f, "[{}; {}]", element, n),
                ArrayLength::Dynamic => write!(f, "[{}]", element),
            },
            Ty::Record { fields } => {
                write!(f, "{{ ")?;
                for (i, (name, ty)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", name, ty)?;
                }
                write!(f, " }}")
            }
            Ty::Readonly(inner) => write!(f, "readonly {}", inner),
            Ty::Existential { class } => write!(f, "{}", class),
            Ty::Forall {
                bounds,
                constraints,
                body,
            } => {
                let vars = bounds
                    .iter()
                    .map(|v| {
                        let mut s = format!("t{}", v.raw());
                        // Unary binder-style constraints (`T: Num`) attach to the var.
                        let classes = constraints
                            .iter()
                            .filter(|c| c.is_unary_on(*v))
                            .map(|c| c.class.as_str())
                            .collect::<Vec<_>>();
                        if !classes.is_empty() {
                            s.push_str(": ");
                            s.push_str(&classes.join(" + "));
                        }
                        s
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                // Multi-arg / non-binder constraints render as a trailing where.
                let multi: Vec<String> = constraints
                    .iter()
                    .filter(|c| {
                        c.args.len() != 1 || c.primary_var().is_none_or(|v| !bounds.contains(&v))
                    })
                    .map(|c| c.to_string())
                    .collect();
                if multi.is_empty() {
                    write!(f, "forall {}. {}", vars, body)
                } else {
                    write!(f, "forall {}. {} where {}", vars, body, multi.join(", "))
                }
            }
        }
    }
}

fn needs_paren(t: &Ty) -> bool {
    matches!(t, Ty::Fun(_, _) | Ty::Forall { .. })
}

impl fmt::Display for Scheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.bounds.is_empty() {
            write!(f, "{}", self.ty)
        } else {
            let vars = self
                .bounds
                .iter()
                .map(|v| format!("t{}", v.raw()))
                .collect::<Vec<_>>()
                .join(" ");
            write!(f, "forall {}. {}", vars, self.ty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{TyVarId, float, int, list, option_ty, string};

    #[test]
    fn display_var() {
        assert_eq!(format!("{}", Ty::Var(TyVarId(0))), "t0");
        assert_eq!(format!("{}", Ty::Var(TyVarId(42))), "t42");
    }

    #[test]
    fn display_con() {
        assert_eq!(format!("{}", int()), "int");
        assert_eq!(format!("{}", float()), "float");
        assert_eq!(format!("{}", string()), "string");
        assert_eq!(format!("{}", Ty::Con("Foo".into())), "Foo");
    }

    #[test]
    fn display_fun() {
        let ty = Ty::Fun(Box::new(int()), Box::new(string()));
        assert_eq!(format!("{}", ty), "int -> string");
    }

    #[test]
    fn display_fun_with_nested_fun_adds_parens() {
        // (int -> string) -> bool
        let inner = Ty::Fun(Box::new(int()), Box::new(string()));
        let ty = Ty::Fun(Box::new(inner), Box::new(Ty::Con("bool".into())));
        assert_eq!(format!("{}", ty), "(int -> string) -> bool");
    }

    #[test]
    fn display_app_no_args() {
        let ty = Ty::App(Box::new(Ty::Con("Foo".into())), vec![]);
        assert_eq!(format!("{}", ty), "Foo");
    }

    #[test]
    fn display_app_with_args() {
        let ty = Ty::App(Box::new(Ty::Con("Foo".into())), vec![int(), string()]);
        assert_eq!(format!("{}", ty), "Foo<int, string>");
    }

    #[test]
    fn display_list() {
        assert_eq!(format!("{}", list(int())), "[int]");
    }

    #[test]
    fn display_scheme_mono() {
        let s = Scheme::mono(int());
        assert_eq!(format!("{}", s), "int");
    }

    #[test]
    fn display_scheme_poly() {
        let s = Scheme {
            bounds: vec![TyVarId(0), TyVarId(1)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(Ty::Var(TyVarId(0))), Box::new(Ty::Var(TyVarId(1)))),
        };
        assert_eq!(format!("{}", s), "forall t0 t1. t0 -> t1");
    }

    /// True if `s` contains a raw type-variable id like `t0` / `t43`
    /// (word-boundary `t` + digits). Must not use `contains('t')` —
    /// class names such as `Convert` contain the letter `t`.
    fn contains_raw_tvar_id(s: &str) -> bool {
        s.split(|c: char| !c.is_ascii_alphanumeric()).any(|tok| {
            tok.len() > 1
                && tok.starts_with('t')
                && tok.as_bytes()[1..].iter().all(u8::is_ascii_digit)
        })
    }

    #[test]
    fn format_ty_for_diag_renames_free_vars() {
        use crate::typechecking::subst::Subst;
        let ty = Ty::Fun(
            Box::new(Ty::Var(TyVarId(43))),
            Box::new(Ty::Var(TyVarId(99))),
        );
        let s = format_ty_for_diag(&Subst::empty(), &ty);
        assert_eq!(s, "a -> b");
        assert!(
            !contains_raw_tvar_id(&s),
            "diagnostics must not show raw tN ids: {s}"
        );
    }

    #[test]
    fn format_ty_for_diag_applies_subst_before_rename() {
        use crate::typechecking::subst::Subst;
        let mut subst = Subst::empty();
        subst.insert(TyVarId(43), int());
        let s = format_ty_for_diag(&subst, &Ty::Var(TyVarId(43)));
        assert_eq!(s, "int");
    }

    #[test]
    fn format_ty_for_diag_forall_renames_binders_and_unary_bounds() {
        use crate::typechecking::subst::Subst;
        use crate::typechecking::ty::Constraint;
        let ty = Ty::Forall {
            bounds: vec![TyVarId(7)],
            constraints: vec![Constraint::unary("Num", TyVarId(7))],
            body: Box::new(Ty::Fun(
                Box::new(Ty::Var(TyVarId(7))),
                Box::new(Ty::Var(TyVarId(7))),
            )),
        };
        let s = format_ty_for_diag(&Subst::empty(), &ty);
        assert_eq!(s, "forall a: Num. a -> a");
        assert!(
            !contains_raw_tvar_id(&s),
            "diagnostics must not show raw tN ids: {s}"
        );
    }

    #[test]
    fn format_ty_for_diag_forall_renames_multi_param_where() {
        use crate::typechecking::subst::Subst;
        use crate::typechecking::ty::Constraint;
        let ty = Ty::Forall {
            bounds: vec![TyVarId(3), TyVarId(9)],
            constraints: vec![Constraint {
                class: "Convert".into(),
                args: vec![Ty::Var(TyVarId(3)), Ty::Var(TyVarId(9))],
            }],
            body: Box::new(Ty::Fun(
                Box::new(Ty::Var(TyVarId(3))),
                Box::new(Ty::Var(TyVarId(9))),
            )),
        };
        let s = format_ty_for_diag(&Subst::empty(), &ty);
        assert_eq!(s, "forall a, b. a -> b where Convert<a, b>");
        assert!(
            !contains_raw_tvar_id(&s),
            "diagnostics must not show raw tN ids: {s}"
        );
    }


    #[test]
    fn display_sum_with_no_payloads() {
        let ty = Ty::Sum {
            name: "E".into(),
            variants: vec![
                ("A".into(), EnumVariantPayloadTy::Unit),
                ("B".into(), EnumVariantPayloadTy::Unit),
            ],
        };
        assert_eq!(format!("{}", ty), "enum E { A, B }");
    }

    #[test]
    fn display_sum_with_payloads() {
        let ty = Ty::Sum {
            name: "Option".into(),
            variants: vec![
                ("None".into(), EnumVariantPayloadTy::Unit),
                ("Some".into(), EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        };
        assert_eq!(format!("{}", ty), "enum Option { None, Some(int) }");
    }

    #[test]
    fn display_sum_with_record_payloads() {
        // enum E { Unit, Rec { x: int, y: string } }
        let ty = Ty::Sum {
            name: "E".into(),
            variants: vec![
                ("Unit".into(), EnumVariantPayloadTy::Unit),
                (
                    "Rec".into(),
                    EnumVariantPayloadTy::Record(vec![("x".into(), int()), ("y".into(), string())]),
                ),
            ],
        };
        assert_eq!(
            format!("{}", ty),
            "enum E { Unit, Rec { x: int, y: string } }"
        );
    }

    #[test]
    fn display_constructor() {
        let sum = Ty::Sum {
            name: "Option".into(),
            variants: vec![
                ("None".into(), EnumVariantPayloadTy::Unit),
                ("Some".into(), EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        };
        let ctor = Ty::Constructor {
            owner: Box::new(sum),
            tag: 1,
            arity: 1,
        };
        assert_eq!(format!("{}", ctor), "enum Option { None, Some(int) }::v1");
    }

    #[test]
    fn format_ty_fqn_primitives_and_option() {
        let modules = HashMap::from([(
            common::BUILTIN_OPTION_ENUM.to_string(),
            "prelude".to_string(),
        )]);
        assert_eq!(format_ty_fqn(&int(), &modules).as_deref(), Some("int"));
        assert_eq!(
            format_ty_fqn(&string(), &modules).as_deref(),
            Some("string")
        );
        let opt = Ty::App(
            Box::new(Ty::Con(common::BUILTIN_OPTION_ENUM.into())),
            vec![int()],
        );
        assert_eq!(
            format_ty_fqn(&opt, &modules).as_deref(),
            Some("prelude::Option<int>")
        );
        let sum = option_ty(int());
        assert_eq!(
            format_ty_fqn(&sum, &modules).as_deref(),
            Some("prelude::Option<int>")
        );
        assert!(format_ty_fqn(&Ty::Var(TyVarId(0)), &modules).is_none());
    }

    #[test]
    fn format_ty_fqn_aggregates_result_and_nominal_modules() {
        use crate::typechecking::ty::{array, array_fixed, record, result_ty, tuple};

        let modules = HashMap::from([
            (
                common::BUILTIN_RESULT_ENUM.to_string(),
                "prelude".to_string(),
            ),
            ("Point".to_string(), "math".to_string()),
            ("Local".to_string(), String::new()),
        ]);

        assert_eq!(
            format_ty_fqn(&tuple(vec![int(), string()]), &modules).as_deref(),
            Some("(int, string)")
        );
        assert_eq!(
            format_ty_fqn(&array(int()), &modules).as_deref(),
            Some("[int]")
        );
        assert_eq!(
            format_ty_fqn(&array_fixed(int(), 3), &modules).as_deref(),
            Some("[int; 3]")
        );
        assert_eq!(
            format_ty_fqn(
                &record(vec![("a".into(), int()), ("b".into(), string())]),
                &modules
            )
            .as_deref(),
            Some("{ a: int, b: string }")
        );
        assert_eq!(
            format_ty_fqn(&result_ty(int(), string()), &modules).as_deref(),
            Some("prelude::Result<int, string>")
        );
        assert_eq!(
            format_ty_fqn(&Ty::Con("Point".into()), &modules).as_deref(),
            Some("math::Point")
        );
        assert_eq!(
            format_ty_fqn(&Ty::Con("Local".into()), &modules).as_deref(),
            Some("Local"),
            "empty defining module stays bare"
        );
        assert!(format_ty_fqn(&Ty::Never, &modules).is_none());
        let fun = Ty::Fun(Box::new(int()), Box::new(string()));
        assert_eq!(
            format_ty_fqn(&fun, &modules).as_deref(),
            Some("int -> string")
        );
    }
}
