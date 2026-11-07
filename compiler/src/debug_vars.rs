//! Debug variables: where a named local lives, over which code, as what type.
//!
//! Codegen records one [`DebugVar`] per binding (a `let`, a parameter, a
//! match binding) with its **source** scope (declaration to the end of the
//! enclosing block) and a layout-aware [`DebugVarLoc`]: one slot, class
//! fields split into slots (Q2), an array kept in slots (Q1), or a two-slot
//! enum. Stores that define the variable carry a debug location equal to the
//! variable's name token (its *def site*); optimizer passes keep op
//! locations when they rewrite or move an op, so after the whole pipeline
//! [`location_lists`] can follow the variable into whatever slot or dense
//! register its defining stores now write, and report it unavailable where
//! a pass removed them. The debugger shows a value only where the list says
//! it is live, and `<optimized out>` elsewhere.

use std::collections::HashMap;


use crate::typechecking::ty::Ty;

/// How to render a frame word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugTy {
    Int,
    Float,
    Bool,
    Byte,
    Str,
    Unit,
    /// Heap class instance.
    Class(String),
    /// Heap array / `Vec` of the element type.
    Array(Box<DebugTy>),
    Tuple(Vec<DebugTy>),
    /// User or builtin enum (`Option`, `Result`, …) by name.
    Enum(String, Vec<DebugTy>),
    /// Anything else: shown as its type name and raw word.
    Other(String),
}

impl DebugTy {
    /// Describe `ty` (already substituted) for display.
    pub fn from_ty(ty: &Ty) -> Self {
        Self::from_ty_depth(ty, 0)
    }

    fn from_ty_depth(ty: &Ty, depth: u32) -> Self {
        if depth > 4 {
            return DebugTy::Other("…".into());
        }
        let next = |t: &Ty| Self::from_ty_depth(t, depth + 1);
        match ty {
            Ty::Con(name) => match name.as_str() {
                "int" => DebugTy::Int,
                "float" => DebugTy::Float,
                "bool" => DebugTy::Bool,
                "byte" => DebugTy::Byte,
                "string" => DebugTy::Str,
                "unit" | "()" => DebugTy::Unit,
                other if other.chars().next().is_some_and(char::is_uppercase) => {
                    DebugTy::Class(other.to_string())
                }
                other => DebugTy::Other(other.to_string()),
            },
            Ty::Readonly(inner) => next(inner),
            Ty::Array { element, .. } => DebugTy::Array(Box::new(next(element))),
            Ty::Tuple(items) => DebugTy::Tuple(items.iter().map(next).collect()),
            Ty::App(con, args) => match con.as_ref() {
                Ty::Con(name) if name == "Vec" && args.len() == 1 => {
                    DebugTy::Array(Box::new(next(&args[0])))
                }
                Ty::Con(name) => DebugTy::Enum(name.clone(), args.iter().map(next).collect()),
                other => DebugTy::Other(format!("{other:?}")),
            },
            Ty::Sum { name, .. } => DebugTy::Enum(name.clone(), Vec::new()),
            Ty::Constructor { owner, .. } => next(owner),
            other => DebugTy::Other(crate::format_ty_for_diag(&Default::default(), other)),
        }
    }

    /// Type name as the user writes it.
    pub fn name(&self) -> String {
        match self {
            DebugTy::Int => "int".into(),
            DebugTy::Float => "float".into(),
            DebugTy::Bool => "bool".into(),
            DebugTy::Byte => "byte".into(),
            DebugTy::Str => "string".into(),
            DebugTy::Unit => "unit".into(),
            DebugTy::Class(n) | DebugTy::Other(n) => n.clone(),
            DebugTy::Array(e) => format!("Vec<{}>", e.name()),
            DebugTy::Tuple(items) => format!(
                "({})",
                items.iter().map(DebugTy::name).collect::<Vec<_>>().join(", ")
            ),
            DebugTy::Enum(n, args) if args.is_empty() => n.clone(),
            DebugTy::Enum(n, args) => format!(
                "{n}<{}>",
                args.iter().map(DebugTy::name).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

/// Where a variable's value lives (frame slots are relative to the frame).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugVarLoc {
    Slot(u32),
    /// Q2 class scalar replacement: each field in its own slot.
    Fields { class: String, fields: Vec<(String, u32, DebugTy)> },
    /// Q1 fixed array kept in consecutive slots.
    Elems { slots: Vec<u32>, elem: DebugTy },
    /// Frame-local / two-slot enum: payload word and tag word.
    Pair { enum_name: String, payload: u32, tag: u32, payload_ty: DebugTy },
}

impl DebugVarLoc {
    /// Apply a slot renaming (dense register assignment).
    pub fn remap(&mut self, map: &HashMap<u32, u32>) {
        let r = |s: &mut u32| {
            if let Some(n) = map.get(s) {
                *s = *n;
            }
        };
        match self {
            DebugVarLoc::Slot(s) => r(s),
            DebugVarLoc::Fields { fields, .. } => fields.iter_mut().for_each(|(_, s, _)| r(s)),
            DebugVarLoc::Elems { slots, .. } => slots.iter_mut().for_each(r),
            DebugVarLoc::Pair { payload, tag, .. } => {
                r(payload);
                r(tag);
            }
        }
    }
}

/// One named local of a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugVar {
    pub name: String,
    /// Source file index (as in `ProgramDebug::source_files`).
    pub file: u32,
    /// Source bytes where the name is visible: its declaration statement to
    /// the end of the enclosing block (a parameter: the whole function).
    pub scope: (u32, u32),
    pub ty: DebugTy,
    pub loc: DebugVarLoc,
    /// Name-token spans of the stores that define it (`let x`, `x = …`).
    pub def_sites: Vec<(u32, u32)>,
    /// A parameter: its slot holds the value from function entry.
    pub is_param: bool,
    /// Byte span of the binding's name token (its def-site tag), when the
    /// name is a slice of the source.
    pub name_span: Option<(u32, u32)>,
    /// For a [`DebugVarLoc::Slot`] variable after [`location_lists`]:
    /// `(start_pc, end_pc, slot)` where the value is known to be live.
    /// Empty and `validated` means "never available" (optimized out).
    pub ranges: Vec<(u32, u32, u32)>,
    /// `ranges` was computed (the body was analyzable).
    pub validated: bool,
    /// Split layouts after [`location_lists`]: per component (field /
    /// element / payload-then-tag), `(start_pc, end_pc, slot)` where it is
    /// known to be live.
    pub comp_ranges: Vec<Vec<(u32, u32, u32)>>,
}

impl DebugVar {
    /// Visible at a stop whose statement starts at `offset` in `file`.
    pub fn in_scope(&self, file: u32, offset: u32) -> bool {
        self.file == file && self.scope.0 <= offset && offset < self.scope.1
    }

    /// Slot holding component `i` of a split layout at `pc` (`None`:
    /// optimized out). Unvalidated bodies use the codegen home slot.
    pub fn component_slot(&self, i: usize, pc: u32) -> Option<u32> {
        if !self.validated {
            return self.component_slots().get(i).copied();
        }
        self.comp_ranges
            .get(i)?
            .iter()
            .find(|(s, e, _)| *s <= pc && pc < *e)
            .map(|(_, _, slot)| *slot)
    }

    /// Home slots of the components of a split layout (none for `Slot`).
    pub fn component_slots(&self) -> Vec<u32> {
        match &self.loc {
            DebugVarLoc::Slot(_) => Vec::new(),
            DebugVarLoc::Fields { fields, .. } => fields.iter().map(|f| f.1).collect(),
            DebugVarLoc::Elems { slots, .. } => slots.clone(),
            DebugVarLoc::Pair { payload, tag, .. } => vec![*payload, *tag],
        }
    }

    /// Slot holding the value at `pc`: `None` when optimized out there.
    /// Unvalidated bodies fall back to the codegen slot.
    pub fn slot_at(&self, pc: u32) -> Option<u32> {
        let DebugVarLoc::Slot(slot) = self.loc else {
            return None;
        };
        if !self.validated {
            return Some(slot);
        }
        self.ranges
            .iter()
            .find(|(start, end, _)| *start <= pc && pc < *end)
            .map(|(_, _, s)| *s)
    }
}

/// Class field tables for rendering heap instances: name → fields in slot order.
pub type DebugClassTable = HashMap<String, Vec<(String, DebugTy)>>;
/// Enum tables: name → variant names in tag order.
pub type DebugEnumTable = HashMap<String, Vec<String>>;
