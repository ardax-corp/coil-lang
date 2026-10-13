//! HIR bodies the compiler makes itself, with no source of their own (the
//! auto-par fork-join workers). They lower like any other body (HIR
//! lowering, then direct MIR), instead of being written out as stack IL by
//! hand.
//!
//! Every node takes [`SPAN`], which no source node has, so no span-keyed
//! checker or sidecar fact is found for it; lowering clears the debug
//! locations it would derive from it.

use super::layout;
use super::{BinOp, Callee, HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, Lit, LocalId, LocalKind};
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

    pub fn block(&mut self, stmts: Vec<HirId>, tail: Option<HirId>, ty: Ty) -> HirId {
        self.push(HirKind::Block { stmts, tail }, ty)
    }

    pub fn if_(&mut self, cond: HirId, then: HirId, els: Option<HirId>, ty: Ty) -> HirId {
        self.push(HirKind::If { cond, then, els }, ty)
    }

    pub fn ret(&mut self, value: HirId) -> HirId {
        self.push(HirKind::Return(Some(value)), coil_ty::never())
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
