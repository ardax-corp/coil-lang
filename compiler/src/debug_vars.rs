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

use common::{Byte, DebugLoc};

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
            Ty::List(elem) => DebugTy::Array(Box::new(next(elem))),
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

    /// Def-site tag of component `i`: the name span, widened by `i` bytes at
    /// the end so each component's defining stores are told apart (the start
    /// byte, which decides the line, stays the name's).
    pub fn component_site(name_span: (u32, u32), i: usize) -> (u32, u32) {
        (name_span.0, name_span.1 + i as u32)
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

/// Compute [`DebugVar::ranges`] / [`DebugVar::comp_ranges`] for the
/// variables of one function body `[entry, end)` of the final bytecode.
///
/// Forward dataflow over the frame. Each slot may hold "component `c` of
/// variable `v`" (`c = 0` for a single-slot variable): after a store whose
/// debug location is one of `v`'s def sites (the tag survives when a pass
/// moves the store to another slot or into a dense register), or after a
/// copy (`DenseMove`, `LOAD s; STORE d`) from a slot that holds it. Any other
/// write clears the slot. A parameter holds its slot from entry. Joins keep
/// a claim only when every predecessor agrees. An opcode whose slot writes
/// are not modeled leaves the whole body unvalidated (fail closed to the
/// codegen slots, the old behavior).
pub fn location_lists(
    bytecode: &[Byte],
    constants: &[u64],
    debug_locs: &[DebugLoc],
    entry: usize,
    end: usize,
    vars: &mut [DebugVar],
) {
    // Only variables whose definitions are tagged (or parameters) can be
    // validated; others (match bindings, written through the cursor) keep
    // their codegen slot.
    let tracked: Vec<usize> = vars
        .iter()
        .enumerate()
        .filter(|(_, v)| v.is_param || !v.def_sites.is_empty())
        .map(|(i, _)| i)
        .collect();
    if tracked.is_empty() || end <= entry || end > bytecode.len() {
        return;
    }
    type Claim = (usize, u16);
    // Def site (file, start, end) → the variable component it defines.
    // Split layouts tag component `c` with `component_site(name, c)`.
    let mut site_var: HashMap<(u32, u32, u32), Claim> = HashMap::new();
    for &i in &tracked {
        let comps = vars[i].component_slots().len();
        for &(s, e) in &vars[i].def_sites {
            if comps == 0 {
                site_var.insert((vars[i].file, s, e), (i, 0));
            } else {
                for c in 0..comps {
                    let (cs, ce) = DebugVar::component_site((s, e), c);
                    site_var.insert((vars[i].file, cs, ce), (i, c as u16));
                }
            }
        }
    }
    let n = end - entry;
    let mut state_in: Vec<Option<HashMap<u32, Claim>>> = vec![None; n];
    let mut entry_state = HashMap::new();
    for &i in &tracked {
        if vars[i].is_param
            && let DebugVarLoc::Slot(s) = vars[i].loc
        {
            entry_state.insert(s, (i, 0));
        }
    }
    state_in[0] = Some(entry_state);
    let mut work = vec![0usize];
    while let Some(k) = work.pop() {
        let pc = entry + k;
        let Some(mut st) = state_in[k].clone() else {
            continue;
        };
        let Some(step) = step_of(bytecode, constants, pc, end) else {
            return;
        };
        // `LOAD s` right before `STORE d` copies `s`'s claim into `d`.
        let copied_from = match (*bytecode[pc].bytecode(), pc.checked_sub(1).map(|p| &bytecode[p])) {
            (common::Instruction::STORE | common::Instruction::StorePop, Some(prev))
                if pc > entry
                    && *prev.bytecode() == common::Instruction::LOAD
                    && prev.load_store_count() == 1 =>
            {
                Some(prev.load_store_slot_at(0))
            }
            _ => None,
        };
        for (word, written, copy_src) in &step.writes {
            let loc = debug_locs.get(*word).copied().unwrap_or(DebugLoc::unknown());
            let def = site_var.get(&(loc.file, loc.start_byte, loc.end_byte)).copied();
            let src = copy_src.or(copied_from.filter(|_| *word == pc));
            let copied = src.and_then(|s| st.get(&s).copied());
            for &slot in written {
                st.remove(&slot);
                if let Some(claim) = def {
                    // A new definition makes every older copy stale.
                    st.retain(|_, c| *c != claim);
                    st.insert(slot, claim);
                } else if let Some(claim) = copied {
                    st.insert(slot, claim);
                }
            }
        }
        for succ in step.succs {
            if !(entry..end).contains(&succ) {
                continue;
            }
            let j = succ - entry;
            let changed = match &mut state_in[j] {
                None => {
                    state_in[j] = Some(st.clone());
                    true
                }
                Some(prev) => {
                    let before = prev.len();
                    prev.retain(|slot, claim| st.get(slot) == Some(claim));
                    prev.len() != before
                }
            };
            if changed {
                work.push(j);
            }
        }
    }
    for &i in &tracked {
        let comps = vars[i].component_slots();
        let mut ranges: Vec<(u32, u32, u32)> = Vec::new();
        let mut comp_ranges: Vec<Vec<(u32, u32, u32)>> = vec![Vec::new(); comps.len()];
        let push = |list: &mut Vec<(u32, u32, u32)>, pc: u32, slot: u32| match list.last_mut() {
            Some(last) if last.1 == pc && last.2 == slot => last.1 = pc + 1,
            _ => list.push((pc, pc + 1, slot)),
        };
        for (k, st) in state_in.iter().enumerate() {
            let Some(st) = st else { continue };
            let pc = (entry + k) as u32;
            if let DebugVarLoc::Slot(home) = vars[i].loc {
                // Prefer the codegen slot when the value is in several.
                let slot = if st.get(&home) == Some(&(i, 0)) {
                    Some(home)
                } else {
                    st.iter().filter(|(_, c)| **c == (i, 0)).map(|(s, _)| *s).min()
                };
                if let Some(slot) = slot {
                    match ranges.last_mut() {
                        Some(last) if last.1 == pc && last.2 == slot => last.1 = pc + 1,
                        _ => ranges.push((pc, pc + 1, slot)),
                    }
                }
            } else {
                for (c, home) in comps.iter().enumerate() {
                    let claim = (i, c as u16);
                    let slot = if st.get(home) == Some(&claim) {
                        Some(*home)
                    } else {
                        st.iter().filter(|(_, v)| **v == claim).map(|(s, _)| *s).min()
                    };
                    if let Some(slot) = slot {
                        push(&mut comp_ranges[c], pc, slot);
                    }
                }
            }
        }
        vars[i].ranges = ranges;
        vars[i].comp_ranges = comp_ranges;
        vars[i].validated = true;
    }
}

/// Decoded effect of the instruction at `pc`.
struct Step {
    /// `(code word, slots written, copied-from slot)`: each word of a
    /// multi-word op has its own debug location.
    writes: Vec<(usize, Vec<u32>, Option<u32>)>,
    succs: Vec<usize>,
}

fn step_of(bytecode: &[Byte], constants: &[u64], pc: usize, end: usize) -> Option<Step> {
    use common::Instruction::*;
    let b = &bytecode[pc];
    let mut width = 1;
    let mut writes = vec![(pc, writes_of(b, constants)?, None)];
    if *b.bytecode() == DenseMove {
        let (_, s) = b.dense_move_parts();
        writes[0].2 = Some(s as u32);
    }
    let mut jump = crate::codegen::jump_target_of(b, constants);
    // Two-word dense packs: the tail word is a DenseBin (DenseBin2) or a
    // slot compare-and-jump.
    if matches!(*b.bytecode(), DenseBin2 | DenseBinJmpf | DenseIndexJmpf) {
        let tail = bytecode.get(pc + 1).filter(|_| pc + 2 <= end)?;
        width = 2;
        if *b.bytecode() == DenseBin2 {
            writes.push((pc + 1, vec![tail.dense_abc_parts().1 as u32], None));
        } else {
            jump = crate::codegen::jump_target_of(tail, constants);
        }
    }
    let falls = !matches!(
        *b.bytecode(),
        JMP | RETURN | ConstReturnImm | BinReturn | ReturnPair | MakeEnumReturn
            | MakeEnumReturnK | HALT | Panic | TailCall | LoadReturnSlot
    );
    let mut succs = Vec::new();
    if falls {
        succs.push(pc + width);
    }
    succs.extend(jump);
    Some(Step { writes, succs })
}

/// Frame slots `b` overwrites; `None` for opcodes this model does not know.
fn writes_of(b: &Byte, constants: &[u64]) -> Option<Vec<u32>> {
    use common::Instruction::*;
    let pool = |i: usize| constants.get(i).copied();
    Some(match *b.bytecode() {
        STORE | StorePop => (0..b.load_store_count()).map(|i| b.load_store_slot_at(i)).collect(),
        BinSlotImmStore => {
            let (_, _, pool_idx) = b.bin_slot_imm_store_parts();
            vec![(pool(pool_idx)? >> 32) as u32]
        }
        BinSlotSlotStore => vec![b.bin_slot_slot_store_parts().3 as u32],
        INC | DEC => vec![b.inc_dec_parts().0 as u32],
        UnpackAt => {
            let op = b.operand_u32();
            let (base, n) = (op & 0xFFFF, op >> 16);
            (base..base + n).collect()
        }
        DenseBin | DenseBin2 | DenseCmp | DenseIndex | DenseFieldLoad => {
            vec![b.dense_abc_parts().1 as u32]
        }
        DenseMake => vec![b.dense_abc_parts().1 as u32],
        DenseMakeK => vec![b.dense_make_k_parts(constants)?.1 as u32],
        DenseMakeObject => vec![common::dense::unpack_make_object(b.operand_u32()).0 as u32],
        DenseConst => vec![b.dense_const_parts().1 as u32],
        DenseMove | DenseArrayLen => vec![b.dense_move_parts().0 as u32],
        DenseUnary | DenseCast => vec![b.dense_unary_parts().1 as u32],
        // Ops without slot writes. Cursor pushes above the locals (LOAD,
        // CALL results, match payloads) are not variable homes.
        LOAD | CONST | STRING | DUPLICATE | POP | Seek | LoadReturnSlot | BinSlotImm
        | BinSlotSlot | BinSlotImmJmpf | BinSlotImmJmpt | BinSlotSlotJmpf | BinSlotSlotJmpt
        | DenseBinJmpf | DenseIndexJmpf | DenseStoreIndex | DenseFieldStore | DenseArrayPush
        | DensePush | NOOP | DATA | HALT | Panic | CodePtr | PRINT | ADD | SUB | MUL | DIV
        | MOD | LE | LEQ | GT | GEQ | EQ | NEQ | Pow | BITAND | BITOR | ADDF | SUBF | MULF
        | DIVF | MODF | LEF | LEQF | GTF | GEQF | PowF | SHL | SHR | XOR | AND | OR | NOT
        | LogNot | NEG | NEGF | CastIntToFloat | CastFloatToInt | CastIntToByte
        | CastByteToInt | CastIntToBool | CastBoolToInt | JMP | JMPF | JMPT | CmpJmpf
        | CmpJmpt | LogNotJmpf | LogNotJmpt | JumpIfMatch | Unpack | RETURN | ConstReturnImm
        | BinReturn | ReturnPair | MakeEnumReturn | MakeEnumReturnK | CALL | CallIndirect
        | HostInvoke | MakeArray | MakeArrayK | MakeTuple | MakeTupleK | MakeEnum | MakeEnumK
        | MakeDict | DictEntries | InitTyped | INIT | BoxValue | UnboxValue | FORMAT
        | STRINGIFY | Index | IndexUnchecked | StoreIndex | StoreIndexUnchecked | ArrayLen
        | ArrayPush | GetField | SetField | LoadField | ArrayPin | IndexPin | IndexPinUnchecked
        | StoreIndexPin | StoreIndexPinUnchecked | MakeFn | MakePolyFn | MakePolyFnCapture
        | MakeCoro | ResumeCoro | DoneCoro | LoadStatic | StoreStatic | TagEnumType
        | TagArrayKind | TailCall | YieldCoro | YieldFromCoro | VLoad | VStore | VBin | VMove
        | VFma => Vec::new(),
        VReduce => vec![b.dense_abc_parts().1 as u32],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Instruction;

    fn var(name: &str, home: u32, site: (u32, u32)) -> DebugVar {
        DebugVar {
            name: name.into(),
            file: 0,
            scope: (0, 1000),
            ty: DebugTy::Int,
            loc: DebugVarLoc::Slot(home),
            def_sites: vec![site],
            is_param: false,
            name_span: Some(site),
            ranges: Vec::new(),
            validated: false,
            comp_ranges: Vec::new(),
        }
    }

    fn at(site: (u32, u32)) -> DebugLoc {
        DebugLoc {
            file: 0,
            start_byte: site.0,
            end_byte: site.1,
        }
    }

    fn store(slot: u32) -> Byte {
        Byte::new(Instruction::STORE).with_load_store_slot(slot)
    }

    fn konst(n: u32) -> Byte {
        Byte::new(Instruction::CONST).with_operand_u32(n)
    }

    const X: (u32, u32) = (10, 11);
    const Y: (u32, u32) = (20, 21);

    #[test]
    fn tagged_store_defines_and_untagged_write_kills() {
        let code = vec![konst(1), store(0), konst(2), store(1), konst(3), store(1), Byte::new(Instruction::RETURN)];
        let mut locs = vec![DebugLoc::unknown(); code.len()];
        locs[1] = at(X);
        locs[3] = at(Y);
        let mut vars = vec![var("x", 0, X), var("y", 1, Y)];
        location_lists(&code, &[], &locs, 0, code.len(), &mut vars);
        assert!(vars.iter().all(|v| v.validated));
        assert_eq!(vars[0].slot_at(1), None, "not defined before its store runs");
        assert_eq!(vars[0].slot_at(2), Some(0));
        assert_eq!(vars[1].slot_at(4), Some(1));
        assert_eq!(vars[1].slot_at(6), None, "an untagged write replaced y");
    }

    #[test]
    fn value_is_followed_into_the_slot_a_pass_moved_it_to() {
        // Codegen put `x` in slot 0; an optimization now stores it in 5.
        let code = vec![konst(7), store(5), Byte::new(Instruction::RETURN)];
        let mut locs = vec![DebugLoc::unknown(); code.len()];
        locs[1] = at(X);
        let mut vars = vec![var("x", 0, X)];
        location_lists(&code, &[], &locs, 0, code.len(), &mut vars);
        assert_eq!(vars[0].slot_at(2), Some(5));
    }

    #[test]
    fn copies_follow_and_redefinition_makes_them_stale() {
        let code = vec![
            konst(1),
            store(0),
            Byte::new(Instruction::DenseMove).with_dense_move(3, 0),
            konst(2),
            store(4), // x redefined elsewhere: slot 0 and the copy in 3 are stale
            Byte::new(Instruction::RETURN),
        ];
        let mut locs = vec![DebugLoc::unknown(); code.len()];
        locs[1] = at(X);
        locs[4] = at(X);
        let mut vars = vec![var("x", 0, X)];
        location_lists(&code, &[], &locs, 0, code.len(), &mut vars);
        assert_eq!(vars[0].slot_at(3), Some(0), "home slot preferred over the copy");
        assert_eq!(vars[0].slot_at(5), Some(4));
    }

    #[test]
    fn join_keeps_only_agreeing_claims() {
        // 0: JMPF +3 ; then-branch defines x in 0 ; join at 4 without x on the
        // fall-through edge.
        let code = vec![
            Byte::new(Instruction::JMPF).with_operand_u32(3),
            konst(1),
            store(0),
            Byte::new(Instruction::RETURN),
        ];
        let mut locs = vec![DebugLoc::unknown(); code.len()];
        locs[2] = at(X);
        let mut vars = vec![var("x", 0, X)];
        location_lists(&code, &[], &locs, 0, code.len(), &mut vars);
        assert_eq!(vars[0].slot_at(3), None, "only one predecessor defines x");
    }

    #[test]
    fn slot_free_opcodes_keep_the_body_validated() {
        let code = vec![Byte::new(Instruction::ResumeCoro), Byte::new(Instruction::RETURN)];
        let locs = vec![DebugLoc::unknown(); code.len()];
        let mut vars = vec![var("x", 0, X)];
        location_lists(&code, &[], &locs, 0, code.len(), &mut vars);
        assert!(vars[0].validated);
        assert_eq!(vars[0].slot_at(1), None, "never defined");
    }
}
