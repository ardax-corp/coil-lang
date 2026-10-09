//! Scalar replacement of local enums.
//!
//! A local enum whose every value is a variant built in place (`let s =
//! E::A(x)`, `s = E::B(y, z)`) and whose only reads are `match s` with
//! variant patterns becomes a tag local plus one local per variant field:
//! a build sets the fields then the tag, and the match switches on the tag
//! with each arm binding its fields from those locals. Nothing is
//! allocated. Fields must be scalars (`int` / `float` / `bool` / `byte`),
//! and every pattern a variant with plain bindings or `_`, or a bare `_`.

use std::collections::HashMap;

use super::{HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, HirPatFields, Lit, LocalId, LocalKind, MakeKind};
use crate::typechecking::ty::{self, Ty};

/// One scalarized local: its tag local and each variant's field locals.
struct Split {
    tag: LocalId,
    fields: HashMap<u32, Vec<LocalId>>,
    /// The tag of the only build, when the local is never reassigned.
    fixed: Option<u32>,
    /// A record variant's field names, in its builds' argument order.
    names: HashMap<u32, Vec<String>>,
}

/// A variant pattern's field patterns by argument position: a record
/// pattern's by `names`, the fields it leaves out as `_`.
fn positional(fields: &HirPatFields, names: Option<&Vec<String>>) -> Option<Vec<HirPat>> {
    match fields {
        HirPatFields::Unit => Some(Vec::new()),
        HirPatFields::Tuple(ps) => Some(ps.clone()),
        HirPatFields::Record(fs) => {
            let names = names?;
            let mut out = vec![HirPat::Wild; names.len()];
            for (n, p) in fs {
                out[names.iter().position(|x| x == n)?] = p.clone();
            }
            Some(out)
        }
    }
}

/// `body` with every scalarizable local enum split, or `None` when there
/// is none. `ok(enum)` admits an enum by name (one without a finalizer).
pub fn scalarize(body: &HirBody, ok: impl Fn(&str) -> bool) -> Option<HirBody> {
    let parent = parents(body);
    let mut candidates: Vec<LocalId> = Vec::new();
    'local: for (k, l) in body.locals.iter().enumerate() {
        let local = LocalId(k as u32);
        if l.captured || !matches!(l.kind, LocalKind::Let | LocalKind::Temp) {
            continue;
        }
        let mut name: Option<String> = None;
        let mut agree = |n: &str| match &name {
            Some(seen) => seen == n,
            None => {
                name = Some(n.to_string());
                true
            }
        };
        let mut built = false;
        let mut matched = false;
        let mut tags: Vec<u32> = Vec::new();
        // Tags whose arms bind their fields other than plainly.
        let mut nested: Vec<u32> = Vec::new();
        for (i, e) in body.exprs.iter().enumerate() {
            let id = HirId(i as u32);
            let use_ok = match &e.kind {
                HirKind::Let { local: l, init } if *l == local => {
                    built = true;
                    init.and_then(|v| variant_of(body, v)).is_some_and(|(n, t)| {
                        tags.push(t);
                        agree(n)
                    })
                }
                HirKind::Local(l) if *l == local => match parent[i].map(|p| (p, &body.expr(p).kind)) {
                    Some((_, HirKind::Assign { place, value })) if *place == id => {
                        !body.expr(parent[i].unwrap()).flags.contains(HirFlags::COMPOUND)
                            && variant_of(body, *value).is_some_and(|(n, t)| {
                                tags.push(t);
                                agree(n)
                            })
                    }
                    Some((_, HirKind::Match { scrutinee, arms })) if *scrutinee == id => {
                        matched = true;
                        arms.iter().all(|a| match &a.pat {
                            HirPat::Wild => true,
                            HirPat::Variant { enum_name, tag: Some(tag), fields, .. } => {
                                let plain = match fields {
                                    HirPatFields::Unit => true,
                                    HirPatFields::Tuple(ps) => ps.iter().all(|p| matches!(p, HirPat::Wild | HirPat::Bind(_))),
                                    HirPatFields::Record(fs) => fs.iter().all(|(_, p)| matches!(p, HirPat::Wild | HirPat::Bind(_))),
                                };
                                if !plain {
                                    nested.push(*tag);
                                }
                                agree(enum_name)
                            }
                            _ => false,
                        })
                    }
                    _ => false,
                },
                HirKind::Defer { captures, .. } => !captures.contains(&Some(local)),
                HirKind::LetPat { pat, .. } => !binds(pat, local),
                HirKind::Match { arms, .. } => !arms.iter().any(|a| binds(&a.pat, local)),
                HirKind::ForIn { pat, .. } => !binds(pat, local),
                _ => true,
            };
            if !use_ok {
                continue 'local;
            }
        }
        // An arm of a tag no build makes never runs, whatever it binds.
        if let Some(n) = name
            && built
            && matched
            && !nested.iter().any(|t| tags.contains(t))
            && ok(&n)
        {
            candidates.push(local);
        }
    }
    if candidates.is_empty() {
        return None;
    }
    let mut out = body.clone();
    let mut done = false;
    for local in candidates {
        let Some(split) = plan(&mut out, local) else { continue };
        rewrite(&mut out, local, &split);
        done = true;
    }
    done.then_some(out)
}

/// The enum and tag a variant build `v` makes, when every argument fits
/// a field local.
fn variant_of(body: &HirBody, v: HirId) -> Option<(&str, u32)> {
    match &body.expr(v).kind {
        HirKind::Make {
            kind: MakeKind::Variant { enum_name, tag: Some(tag), .. },
            args,
        } if args.iter().all(|&a| body.expr(a).ty.as_ref().is_some_and(field)) => Some((enum_name, *tag)),
        _ => None,
    }
}

/// A type a field local holds: a scalar, or a `string`.
fn field(t: &Ty) -> bool {
    super::lower::primitive(t).is_some() || matches!(ty::strip_readonly(t), Ty::Con(n) if n == "string")
}

fn binds(pat: &HirPat, local: LocalId) -> bool {
    match pat {
        HirPat::Bind(l) => *l == local,
        HirPat::Variant { fields: HirPatFields::Tuple(ps), .. } | HirPat::Tuple(ps) => ps.iter().any(|p| binds(p, local)),
        HirPat::Variant { fields: HirPatFields::Record(fs), .. } | HirPat::Record(fs) => fs.iter().any(|(_, p)| binds(p, local)),
        _ => false,
    }
}

fn parents(body: &HirBody) -> Vec<Option<HirId>> {
    let mut parent = vec![None; body.exprs.len()];
    for i in 0..body.exprs.len() {
        for c in super::lower::children(body, HirId(i as u32)) {
            parent[c.0 as usize] = Some(HirId(i as u32));
        }
    }
    parent
}

/// The tag and field locals for `local`: a field's type from a build's
/// argument, else from a pattern's binding. `None` when a field's type
/// is not known or two builds disagree.
fn plan(body: &mut HirBody, local: LocalId) -> Option<Split> {
    let mut tys: HashMap<u32, Vec<Option<Ty>>> = HashMap::new();
    let mut note = |tag: u32, i: usize, n: usize, t: Option<Ty>| -> bool {
        let row = tys.entry(tag).or_insert_with(|| vec![None; n]);
        if row.len() != n {
            return false;
        }
        if n == 0 {
            return true;
        }
        match (&row[i], t) {
            (_, None) => true,
            (None, t) => {
                row[i] = t;
                true
            }
            (Some(a), Some(b)) => *a == b,
        }
    };
    // This local's builds: `let` inits and assigned values.
    let builds: Vec<HirId> = body
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Let { local: l, init } if *l == local => *init,
            HirKind::Assign { place, value } if body.expr(*place).kind == HirKind::Local(local) => Some(*value),
            _ => None,
        })
        .collect();
    let made: Vec<u32> = builds
        .iter()
        .filter_map(|&v| match &body.expr(v).kind {
            HirKind::Make {
                kind: MakeKind::Variant { tag: Some(t), .. },
                ..
            } => Some(*t),
            _ => None,
        })
        .collect();
    let mut names: HashMap<u32, Vec<String>> = HashMap::new();
    for &v in &builds {
        if let HirKind::Make {
            kind: MakeKind::Variant { tag: Some(tag), fields: Some(fs), .. },
            ..
        } = &body.expr(v).kind
            && names.insert(*tag, fs.clone()).is_some_and(|seen| seen != *fs)
        {
            return None;
        }
    }
    for (i, e) in body.exprs.iter().enumerate() {
        match &e.kind {
            HirKind::Make {
                kind: MakeKind::Variant { tag: Some(tag), .. },
                args,
            } if builds.contains(&HirId(i as u32)) => {
                for (i, &a) in args.iter().enumerate() {
                    let t = body.expr(a).ty.as_ref().filter(|t| field(t)).map(|t| ty::strip_readonly(t).clone());
                    if !note(*tag, i, args.len(), t) {
                        return None;
                    }
                }
                if args.is_empty() {
                    note(*tag, 0, 0, None);
                }
            }
            HirKind::Match { scrutinee, arms } if body.expr(*scrutinee).kind == HirKind::Local(local) => {
                for a in arms {
                    let HirPat::Variant { tag: Some(tag), fields, .. } = &a.pat else { continue };
                    if !made.contains(tag) {
                        continue;
                    }
                    let ps = positional(fields, names.get(tag))?;
                    for (i, p) in ps.iter().enumerate() {
                        let t = match p {
                            HirPat::Bind(b) => body.local(*b).ty.clone(),
                            _ => None,
                        };
                        if !note(*tag, i, ps.len(), t) {
                            return None;
                        }
                    }
                    if ps.is_empty() {
                        note(*tag, 0, 0, None);
                    }
                }
            }
            _ => {}
        }
    }
    let mut tags: Vec<_> = tys.into_iter().collect();
    tags.sort_by_key(|(t, _)| *t);
    if tags.iter().flat_map(|(_, row)| row).any(|ty| !ty.as_ref().is_some_and(field)) {
        return None;
    }
    let name = body.local(local).name.clone();
    let fresh = |body: &mut HirBody, suffix: String, ty: Ty| {
        let id = LocalId(body.locals.len() as u32);
        body.locals.push(HirLocal {
            name: format!("__{name}_{suffix}"),
            ty: Some(ty),
            kind: LocalKind::Temp,
            captured: false,
        });
        id
    };
    let tag = fresh(body, "tag".into(), ty::int());
    // Variants share a field local by position and type.
    let mut shared: Vec<((usize, Ty), LocalId)> = Vec::new();
    let mut fields = HashMap::new();
    for (t, row) in tags {
        let mut ids = Vec::with_capacity(row.len());
        for (i, ty) in row.into_iter().enumerate() {
            let ty = ty?;
            let id = match shared.iter().find(|(k, _)| k.0 == i && k.1 == ty) {
                Some(&(_, id)) => id,
                None => {
                    let id = fresh(body, format!("{i}"), ty.clone());
                    shared.push(((i, ty), id));
                    id
                }
            };
            ids.push(id);
        }
        fields.insert(t, ids);
    }
    // One build and no reassignment: every match knows its arm.
    let fixed = match builds.as_slice() {
        [v] if body.exprs.iter().any(|e| matches!(e.kind, HirKind::Let { local: l, init: Some(i) } if l == local && i == *v)) => {
            match &body.expr(*v).kind {
                HirKind::Make {
                    kind: MakeKind::Variant { tag: Some(t), .. },
                    ..
                } => Some(*t),
                _ => None,
            }
        }
        _ => None,
    };
    Some(Split { tag, fields, fixed, names })
}

/// Replace every build and match of `local` with its split locals.
fn rewrite(body: &mut HirBody, local: LocalId, split: &Split) {
    let parent = parents(body);
    for (i, &up) in parent.iter().enumerate() {
        let id = HirId(i as u32);
        match body.exprs[i].kind.clone() {
            HirKind::Let { local: l, init: Some(v) } if l == local => {
                let stmts = build(body, v, split, true);
                set_block(body, id, stmts);
            }
            HirKind::Local(l) if l == local => {
                let Some(p) = up else { continue };
                match body.expr(p).kind.clone() {
                    HirKind::Assign { place, value } if place == id => {
                        let stmts = build(body, value, split, false);
                        set_block(body, p, stmts);
                    }
                    HirKind::Match { scrutinee, arms } if scrutinee == id => {
                        body.exprs[i].kind = HirKind::Local(split.tag);
                        body.exprs[i].ty = Some(ty::int());
                        // Arms of tags no build makes never run.
                        let mut arms: Vec<HirArm> = arms
                            .into_iter()
                            .filter(|a| !matches!(&a.pat, HirPat::Variant { tag: Some(t), .. } if !split.fields.contains_key(t)))
                            .map(|a| arm(body, a, split))
                            .collect();
                        // An exhaustive variant match: its last arm closes
                        // the tag match.
                        if !arms.iter().any(|a| a.pat == HirPat::Wild)
                            && let Some(last) = arms.last_mut()
                        {
                            last.pat = HirPat::Wild;
                        }
                        let taken = split.fixed.and_then(|t| {
                            arms.iter().find(|a| matches!(a.pat, HirPat::Wild) || a.pat == HirPat::Int(i64::from(t)))
                        });
                        body.exprs[p.0 as usize].kind = match taken {
                            Some(a) => HirKind::Block { stmts: Vec::new(), tail: Some(a.body) },
                            None => HirKind::Match { scrutinee, arms },
                        };
                    }
                    _ => {}
                }
            }
            HirKind::Clear(xs) if xs.contains(&local) => {
                body.exprs[i].kind = HirKind::Clear(xs.into_iter().filter(|&x| x != local).collect());
            }
            _ => {}
        }
    }
}

/// The statements of a variant build `v`: each argument into its field,
/// then the tag. A first definition (`define`) also zeroes every other
/// field, so each split local is set before any read.
fn build(body: &mut HirBody, v: HirId, split: &Split, define: bool) -> Vec<HirId> {
    let span = body.expr(v).span;
    let HirKind::Make {
        kind: MakeKind::Variant { tag: Some(tag), .. },
        args,
    } = body.expr(v).kind.clone()
    else {
        unreachable!("checked variant build")
    };
    let mut stmts = Vec::new();
    let set = |body: &mut HirBody, local: LocalId, value: HirId| {
        let kind = if define {
            HirKind::Let { local, init: Some(value) }
        } else {
            let ty = body.local(local).ty.clone();
            let place = push(body, HirKind::Local(local), ty, span);
            HirKind::Assign { place, value }
        };
        push(body, kind, Some(ty::unit()), span)
    };
    let own = split.fields.get(&tag).cloned().unwrap_or_default();
    for (&f, &a) in own.iter().zip(&args) {
        stmts.push(set(body, f, a));
    }
    if define {
        let mut others: Vec<LocalId> = split.fields.values().flatten().copied().filter(|f| !own.contains(f)).collect();
        others.sort_by_key(|f| f.0);
        others.dedup();
        {
            for f in others {
                let ty = body.local(f).ty.clone();
                let zero = match ty.as_ref().and_then(super::lower::primitive) {
                    Some("float") => Lit::Float(0.0),
                    Some("bool") => Lit::Bool(false),
                    Some(_) => Lit::Int(0),
                    None => Lit::Str(String::new()),
                };
                let z = push(body, HirKind::Lit(zero), ty, span);
                stmts.push(set(body, f, z));
            }
        }
    }
    let t = push(body, HirKind::Lit(Lit::Int(i64::from(tag))), Some(ty::int()), span);
    stmts.push(set(body, split.tag, t));
    // The build itself is gone.
    body.exprs[v.0 as usize].kind = HirKind::Lit(Lit::Unit);
    body.exprs[v.0 as usize].ty = Some(ty::unit());
    stmts
}

/// `a` as an arm on the tag: its variant becomes the tag's value and its
/// bindings read the variant's fields first.
fn arm(body: &mut HirBody, a: HirArm, split: &Split) -> HirArm {
    let HirPat::Variant { tag: Some(tag), fields, .. } = &a.pat else {
        return a;
    };
    let span = body.expr(a.body).span;
    let ps = positional(fields, split.names.get(tag)).unwrap_or_default();
    let own = split.fields.get(tag).cloned().unwrap_or_default();
    let mut stmts = Vec::new();
    for (p, &f) in ps.iter().zip(&own) {
        if let HirPat::Bind(b) = p {
            let ty = body.local(f).ty.clone();
            let read = push(body, HirKind::Local(f), ty, span);
            stmts.push(push(body, HirKind::Let { local: *b, init: Some(read) }, Some(ty::unit()), span));
        }
    }
    let arm_body = if stmts.is_empty() {
        a.body
    } else {
        let ty = body.expr(a.body).ty.clone();
        push(body, HirKind::Block { stmts, tail: Some(a.body) }, ty, span)
    };
    HirArm {
        pat: HirPat::Int(i64::from(*tag)),
        body: arm_body,
    }
}

fn set_block(body: &mut HirBody, at: HirId, stmts: Vec<HirId>) {
    let e = &mut body.exprs[at.0 as usize];
    e.kind = HirKind::Block { stmts, tail: None };
    e.ty = Some(ty::unit());
    e.layout = super::layout::Layout::Word;
    e.node = None;
}

fn push(body: &mut HirBody, kind: HirKind, ty: Option<Ty>, span: super::Span) -> HirId {
    let id = HirId(body.exprs.len() as u32);
    body.exprs.push(HirExpr {
        kind,
        ty,
        layout: super::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}
