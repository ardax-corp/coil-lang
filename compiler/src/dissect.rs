//! In-memory program dissection for DX (`coil dissect`).
//!
//! Captures final fused bytecode plus optional pre-opt stack IL and formats
//! filtered views by function FQN. Does not write archives.

use std::collections::HashMap;
use std::fmt::Write as _;

use common::{Byte, Instruction, ProgramDebug};

use crate::il::{EntryKind, IlFunc, IlJumpKind, IlOp};

/// Post-lower function symbol (name → entry PC + debug locals).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSym {
    pub name: String,
    pub entry_pc: u32,
    /// User-facing locals/params `(name, slot)`, sorted by slot.
    pub locals: Vec<(String, u32)>,
}

/// Pre-opt IL snapshot taken after finalize splices, before `lower_in_place`.
#[derive(Clone)]
pub struct IlSnapshot {
    ops: Vec<IlOp>,
    funcs: Vec<IlFunc>,
}

impl IlSnapshot {
    pub(crate) fn new(ops: Vec<IlOp>, funcs: Vec<IlFunc>) -> Self {
        Self { ops, funcs }
    }
}

/// Artifacts from an in-memory dissect compile.
#[derive(Clone)]
pub struct DissectArtifacts {
    pub bytecode: Vec<Byte>,
    pub constants: Vec<u64>,
    pub strings: Vec<String>,
    pub functions: Vec<FnSym>,
    pub il: Option<IlSnapshot>,
    pub debug: ProgramDebug,
}

impl DissectArtifacts {
    /// Sorted `[start, end)` PC range for each function (by entry PC).
    pub fn function_ranges(&self) -> Vec<(FnSym, usize, usize)> {
        let mut syms = self.functions.clone();
        syms.sort_by_key(|s| s.entry_pc);
        let len = self.bytecode.len();
        let mut out = Vec::with_capacity(syms.len());
        for (i, sym) in syms.iter().enumerate() {
            let start = sym.entry_pc as usize;
            let end = syms
                .get(i + 1)
                .map(|n| n.entry_pc as usize)
                .unwrap_or(len)
                .min(len);
            out.push((sym.clone(), start, end.max(start)));
        }
        out
    }

    /// Map entry PC → FQN for call-target annotations.
    pub fn pc_to_name(&self) -> HashMap<usize, &str> {
        self.functions
            .iter()
            .map(|s| (s.entry_pc as usize, s.name.as_str()))
            .collect()
    }
}

/// Case-insensitive FQN match: exact, substring, trailing segment, or `name#N` overload.
pub fn matches_fn_pat(name: &str, pat: &str) -> bool {
    let name_l = name.to_ascii_lowercase();
    let pat_l = pat.to_ascii_lowercase();
    if pat_l.is_empty() {
        return true;
    }
    if name_l == pat_l || name_l.contains(&pat_l) {
        return true;
    }
    let seg = name_l.rsplit("::").next().unwrap_or(&name_l);
    if seg == pat_l {
        return true;
    }
    if let Some(rest) = seg.strip_prefix(&pat_l) {
        return rest.starts_with('#');
    }
    false
}

/// Filter symbols whose names match `pat` (or all when `pat` is `None`).
pub fn filter_symbols<'a>(syms: &'a [FnSym], pat: Option<&str>) -> Vec<&'a FnSym> {
    match pat {
        None => syms.iter().collect(),
        Some(p) => syms.iter().filter(|s| matches_fn_pat(&s.name, p)).collect(),
    }
}

/// Format the symbol index (name → entry PC).
pub fn format_symbol_index(functions: &[FnSym]) -> String {
    let mut syms = functions.to_vec();
    syms.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::from(";; symbols\n");
    for s in &syms {
        let _ = writeln!(out, "{:>6}  {}", s.entry_pc, s.name);
    }
    out
}

/// Format one bytecode instruction line.
pub fn format_byte_line(
    pc: usize,
    byte: &Byte,
    constants: &[u64],
    pc_names: &HashMap<usize, &str>,
) -> String {
    let op = *byte.bytecode();
    let mn = op.mnemonic();
    let operands = format_operands(op, byte, constants, pc_names);
    if operands.is_empty() {
        format!("{pc:05}  {mn}")
    } else {
        format!("{pc:05}  {mn:<16} {operands}")
    }
}

fn annotate_pc(pc: usize, pc_names: &HashMap<usize, &str>) -> String {
    match pc_names.get(&pc) {
        Some(name) => format!("{pc} ({name})"),
        None => pc.to_string(),
    }
}

fn format_load_store_slots(byte: &Byte) -> String {
    let n = byte.load_store_count();
    if n == 1 {
        if let Some(s) = byte.load_store_single_slot() {
            return format!("slot={s}");
        }
    }
    let mut parts = Vec::with_capacity(n);
    for i in 0..n {
        parts.push(format!("s{i}={}", byte.load_store_slot_at(i)));
    }
    parts.join(",")
}

fn bin_op_name(op: u8) -> String {
    Instruction::from(op).mnemonic().to_string()
}

fn format_operands(
    op: Instruction,
    byte: &Byte,
    constants: &[u64],
    pc_names: &HashMap<usize, &str>,
) -> String {
    match op {
        Instruction::LOAD | Instruction::STORE | Instruction::StorePop => {
            format_load_store_slots(byte)
        }
        Instruction::CALL | Instruction::TailCall | Instruction::MakeCoro => {
            let (arity, target) = byte.call_parts();
            format!("arity={arity} target={}", annotate_pc(target, pc_names))
        }
        Instruction::JMP | Instruction::JMPT | Instruction::JMPF => {
            let t = byte.operand_u32() as usize;
            format!("target={}", annotate_pc(t, pc_names))
        }
        Instruction::CONST => {
            let raw = byte.operand_u32();
            if raw & Byte::POOL_FLAG != 0 {
                let idx = raw & !Byte::POOL_FLAG;
                let bits = constants.get(idx as usize).copied().unwrap_or(0);
                format!("pool[{idx}]={bits:#x}")
            } else {
                format!("imm={}", raw as i32)
            }
        }
        Instruction::ConstReturnImm => format!("imm={}", byte.operand_u32() as i32),
        Instruction::LoadReturnSlot => format!("slot={}", byte.operand_u32()),
        Instruction::BinSlotImm => {
            let (bop, slot, imm) = byte.bin_slot_imm_parts();
            format!("op={} slot={slot} imm={imm}", bin_op_name(bop))
        }
        Instruction::BinSlotSlot => {
            let (bop, a, b) = byte.bin_slot_slot_parts();
            format!("op={} a={a} b={b}", bin_op_name(bop))
        }
        Instruction::BinReturn => format!("op={}", bin_op_name(byte.bin_return_op())),
        Instruction::CmpJmpf | Instruction::CmpJmpt => {
            let (bop, t_or_idx) = byte.cmp_jmpf_parts();
            if byte.cmp_jmpf_is_pool() {
                format!("op={} pool_idx={t_or_idx}", bin_op_name(bop))
            } else {
                format!(
                    "op={} target={}",
                    bin_op_name(bop),
                    annotate_pc(t_or_idx, pc_names)
                )
            }
        }
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            let (bop, slot, pool_idx) = byte.bin_slot_imm_jmpf_parts();
            format!("op={} slot={slot} pool_idx={pool_idx}", bin_op_name(bop))
        }
        Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            let (bop, a, pool_idx) = byte.bin_slot_slot_jmpf_parts();
            format!("op={} a={a} pool_idx={pool_idx}", bin_op_name(bop))
        }
        Instruction::BinSlotSlotConstJmpf | Instruction::BinSlotSlotConstJmpt => {
            let (bop, a, pool_idx) = byte.bin_slot_slot_const_jmpf_parts();
            format!("bin={} a={a} pool_idx={pool_idx}", bin_op_name(bop))
        }
        Instruction::BinSlotImmStore => {
            let (bop, src, pool_idx) = byte.bin_slot_imm_store_parts();
            format!("op={} src={src} pool_idx={pool_idx}", bin_op_name(bop))
        }
        Instruction::BinSlotSlotStore => {
            let (bop, a, b, dest) = byte.bin_slot_slot_store_parts();
            format!("op={} a={a} b={b} dest={dest}", bin_op_name(bop))
        }
        Instruction::Seek => format!("slot={}", byte.operand_u32()),
        Instruction::LogNotJmpf | Instruction::LogNotJmpt => {
            let t = byte.operand_u32() as usize;
            if (byte.operand_u32() & (1 << 16)) != 0 {
                format!("pool_idx={}", t & 0xFFFF)
            } else {
                format!("target={}", annotate_pc(t & 0xFFFF, pc_names))
            }
        }
        Instruction::MakeEnum => {
            format!("tag={} arity={}", byte.operand_u16(0), byte.operand_u16(1))
        }
        Instruction::JumpIfMatch => {
            let tag = byte.operand_u16(0);
            let pool = byte.operand_u16(1);
            format!("tag={tag} pool_idx={pool}")
        }
        Instruction::Unpack => format!("arity={}", byte.operand_u32()),
        Instruction::UnpackAt => {
            format!("arity={} slot={}", byte.operand_u16(0), byte.operand_u16(1))
        }
        Instruction::LoadField => format!("index={}", byte.operand_u32()),
        Instruction::InitTyped => {
            let (tid, n) = common::unpack_init_typed(byte.operand_u32());
            format!("type_id={tid} fields={n}")
        }
        Instruction::SetField => match common::set_field_slot_index(byte.operand_u32()) {
            Some(i) => format!("slot={i}"),
            None => String::new(),
        },
        Instruction::MakeTuple | Instruction::MakeArray | Instruction::MakeDict => {
            format!("arity={}", byte.operand_u32())
        }
        Instruction::HostInvoke | Instruction::FfiInvoke => {
            format!("arity={}", byte.operand_u32() & 0xFFFF)
        }
        Instruction::DeclareFFI => format!("arity={}", byte.operand_u32() & 0xFFFF),
        Instruction::CallIndirect => {
            let o = byte.operand_u32();
            format!(
                "value_arity={} dict_arity={}",
                o & 0xFFFF,
                (o >> 16) & 0xFFFF
            )
        }
        Instruction::BoxValue | Instruction::UnboxValue => {
            format!("tag={}", byte.operand_u32())
        }
        Instruction::CodePtr | Instruction::MakePolyFn => {
            let t = byte.operand_u32() as usize;
            format!("entry={}", annotate_pc(t, pc_names))
        }
        Instruction::LoadStatic | Instruction::StoreStatic => {
            format!("slot={}", byte.operand_u32())
        }
        Instruction::INC | Instruction::DEC => {
            let o = byte.operand_u32();
            let slot = o >> 3;
            let prefix = (o & 0b100) != 0;
            let is_float = (o & 0b010) != 0;
            format!("slot={slot} prefix={prefix} float={is_float}")
        }
        Instruction::STRING | Instruction::DATA | Instruction::NATIVE | Instruction::FORMAT => {
            format!("op={:#x}", byte.operand_u32())
        }
        Instruction::MakeFn => {
            let o = byte.operand_u32();
            format!(
                "captures={} filled={} arity={} rest={}",
                o & 0xFF,
                (o >> 8) & 0xFF,
                (o >> 16) & 0xFF,
                (o >> 24) & 1
            )
        }
        Instruction::MakePolyFnCapture => format!("dicts={}", byte.operand_u32() & 0xFF),
        Instruction::ResumeCoro => {
            format!("has_send={}", (byte.operand_u32() & 1) != 0)
        }
        Instruction::DynCmp => format!("kind={}", byte.operand_u32() & 0xFF),
        _ => {
            let o = byte.operand_u32();
            if o == 0 {
                String::new()
            } else {
                format!("op={o:#x}")
            }
        }
    }
}

/// Format a bytecode slice with an optional function header.
pub fn format_bytecode_section(
    name: &str,
    start: usize,
    end: usize,
    bytecode: &[Byte],
    constants: &[u64],
    pc_names: &HashMap<usize, &str>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, ";; fn {name}  bc[{start}..{end})");
    for pc in start..end.min(bytecode.len()) {
        let _ = writeln!(
            out,
            "{}",
            format_byte_line(pc, &bytecode[pc], constants, pc_names)
        );
    }
    out
}

/// Format prologue / unlabeled prefix before the first function entry.
pub fn format_prologue(
    bytecode: &[Byte],
    constants: &[u64],
    functions: &[FnSym],
    pc_names: &HashMap<usize, &str>,
) -> String {
    let first = functions
        .iter()
        .map(|s| s.entry_pc as usize)
        .min()
        .unwrap_or(0);
    if first == 0 {
        return String::new();
    }
    format_bytecode_section("<prologue>", 0, first, bytecode, constants, pc_names)
}

/// Format final bytecode for matching functions (or all when `pat` is `None`).
pub fn format_bytecode(artifacts: &DissectArtifacts, pat: Option<&str>) -> Result<String, String> {
    let matched = filter_symbols(&artifacts.functions, pat);
    if let Some(p) = pat {
        if matched.is_empty() {
            return Err(format!("no functions matching `--fn {p}`"));
        }
    }
    let pc_names = artifacts.pc_to_name();
    let ranges = artifacts.function_ranges();
    let mut out = String::new();
    if pat.is_none() {
        out.push_str(&format_prologue(
            &artifacts.bytecode,
            &artifacts.constants,
            &artifacts.functions,
            &pc_names,
        ));
    }
    let match_set: std::collections::HashSet<&str> =
        matched.iter().map(|s| s.name.as_str()).collect();
    for (sym, start, end) in &ranges {
        if pat.is_some() && !match_set.contains(sym.name.as_str()) {
            continue;
        }
        out.push_str(&format_bytecode_section(
            &sym.name,
            *start,
            *end,
            &artifacts.bytecode,
            &artifacts.constants,
            &pc_names,
        ));
        out.push('\n');
    }
    Ok(out)
}

fn format_il_op(op: &IlOp) -> String {
    match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => format!("Label L{}", l.0),
        IlOp::Jump { kind, target, .. } => {
            let k = match kind {
                IlJumpKind::Unconditional => "JMP",
                IlJumpKind::JumpIfFalse => "JMPF",
                IlJumpKind::JumpIfTrue => "JMPT",
                IlJumpKind::JumpIfMatch { tag, arity } => {
                    return format!("JumpIfMatch tag={tag} arity={arity} -> L{}", target.0);
                }
            };
            format!("{k} -> L{}", target.0)
        }
        IlOp::Entry {
            kind,
            arity,
            target,
            .. } => {
            let k = match kind {
                EntryKind::Call => "CALL",
                EntryKind::TailCall => "TailCall",
                EntryKind::MakeCoro => "MakeCoro",
                EntryKind::CodePtr => "CodePtr",
                EntryKind::MakePolyFn => "MakePolyFn",
            };
            format!("{k} arity={arity} -> L{}", target.0)
        }
        IlOp::PrologueJmp { .. } => "PrologueJmp".to_string(),
        IlOp::Load { slot, .. } => format!("LOAD slot={slot}"),
        IlOp::StorePop { slot, .. } => format!("STORE slot={slot}"),
        IlOp::Const { imm, .. } => format!("CONST imm={imm}"),
        IlOp::ConstPool { idx, .. } => format!("CONST pool[{idx}]"),
        IlOp::String { idx, .. } => format!("STRING table[{idx}]"),
        IlOp::Dup { .. } => "DUPLICATE".to_string(),
        IlOp::Pop { .. } => "POP".to_string(),
        IlOp::LogNot { .. } => "LogNot".to_string(),
        IlOp::Index { .. } => "Index".to_string(),
        IlOp::IndexUnchecked { .. } => "IndexUnchecked".to_string(),
        IlOp::ArrayPin { slot, .. } => format!("ArrayPin slot={slot}"),
        IlOp::IndexPin { slot, .. } => format!("IndexPin slot={slot}"),
        IlOp::IndexPinUnchecked { slot, .. } => format!("IndexPinUnchecked slot={slot}"),
        IlOp::StoreIndexPin { slot, .. } => format!("StoreIndexPin slot={slot}"),
        IlOp::StoreIndexPinUnchecked { slot, .. } => {
            format!("StoreIndexPinUnchecked slot={slot}")
        }
        IlOp::MakeTuple { arity, .. } => format!("MakeTuple arity={arity}"),
        IlOp::MakeArray { arity, .. } => format!("MakeArray arity={arity}"),
        IlOp::MakeEnum { tag, arity, .. } => format!("MakeEnum tag={tag} arity={arity}"),
        IlOp::BoxValue { tag, .. } => format!("BoxValue tag={tag}"),
        IlOp::UnboxValue { tag, .. } => format!("UnboxValue tag={tag}"),
        IlOp::LoadField { index, .. } => format!("LoadField index={index}"),
        IlOp::GetField { .. } => "GetField".to_string(),
        IlOp::SetField { index, .. } => match index {
            Some(i) => format!("SetField slot={i}"),
            None => "SetField".to_string(),
        },
        IlOp::HostInvoke { arity, layout, .. } if *layout != 0 => {
            format!("HostInvoke arity={arity} layout={layout}")
        }
        IlOp::HostInvoke { arity, .. } => format!("HostInvoke arity={arity}"),
        IlOp::Print { .. } => "PRINT".to_string(),
        IlOp::Return { .. } => "RETURN".to_string(),
        IlOp::Halt { .. } => "HALT".to_string(),
        IlOp::Bin { op, .. } => op.mnemonic().to_string(),
        IlOp::BinSlotImm { op, slot, imm, .. } => {
            format!("BinSlotImm op={} slot={slot} imm={imm}", bin_op_name(*op))
        }
        IlOp::BinSlotSlot { op, a, b, .. } => {
            format!("BinSlotSlot op={} a={a} b={b}", bin_op_name(*op))
        }
        IlOp::LoadReturnSlot { slot, .. } => format!("LoadReturnSlot slot={slot}"),
        IlOp::ConstReturnImm { imm, .. } => format!("ConstReturnImm imm={imm}"),
        IlOp::BinReturn { op, .. } => format!("BinReturn op={}", op.mnemonic()),
        IlOp::Byte { byte, .. } => {
            let empty = HashMap::new();
            format_byte_line(0, byte, &[], &empty)
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ')
                .to_string()
        }
    }
}

/// Format pre-opt IL for matching functions (or all when `pat` is `None`).
pub fn format_il(snapshot: &IlSnapshot, pat: Option<&str>) -> Result<String, String> {
    let mut funcs: Vec<&IlFunc> = snapshot.funcs.iter().collect();
    if let Some(p) = pat {
        funcs.retain(|f| matches_fn_pat(&f.name, p));
        if funcs.is_empty() {
            return Err(format!("no IL functions matching `--fn {p}`"));
        }
    }
    funcs.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for f in funcs {
        let _ = writeln!(
            out,
            ";; fn {}  il[{}..{})",
            f.name, f.code_start, f.code_end
        );
        let mut emitting = 0usize;
        for op in &snapshot.ops {
            if let IlOp::Label(_) = op {
                if emitting >= f.code_start && emitting <= f.code_end {
                    let _ = writeln!(out, "  {emitting:5}  {}", format_il_op(op));
                }
                continue;
            }
            if op.emits_code() {
                if emitting >= f.code_start && emitting < f.code_end {
                    let _ = writeln!(out, "  {emitting:5}  {}", format_il_op(op));
                }
                emitting += 1;
            }
        }
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, ProgramDebug};

    #[test]
    fn mnemonic_load_and_call_line() {
        let load = Byte::new(Instruction::LOAD).with_load_store_slot(3);
        let empty = HashMap::new();
        let line = format_byte_line(42, &load, &[], &empty);
        assert_eq!(line, "00042  LOAD             slot=3");

        let call = Byte::new(Instruction::CALL).with_call_packed(1, 10);
        let mut names = HashMap::new();
        names.insert(10usize, "fib");
        let line = format_byte_line(7, &call, &[], &names);
        assert!(line.contains("CALL"));
        assert!(line.contains("arity=1"));
        assert!(line.contains("10 (fib)"));
    }

    #[test]
    fn fn_pat_matches_trailing_and_overload() {
        assert!(matches_fn_pat("fib", "fib"));
        assert!(matches_fn_pat("mod::fib", "fib"));
        assert!(matches_fn_pat("Foo::fib", "fib"));
        assert!(matches_fn_pat("fib#2", "fib"));
        assert!(matches_fn_pat("Show__int__show", "show"));
        assert!(!matches_fn_pat("main", "fib"));
        assert!(matches_fn_pat("anything", ""));
        assert!(matches_fn_pat("FIB", "fib"));
    }

    #[test]
    fn format_bytecode_miss_and_function_ranges() {
        let arts = DissectArtifacts {
            bytecode: vec![
                Byte::new(Instruction::LOAD).with_load_store_slot(0),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::CONST).with_const_inline(1),
                Byte::new(Instruction::RETURN),
            ],
            constants: vec![],
            strings: vec![],
            functions: vec![
                FnSym {
                    name: "a".into(),
                    entry_pc: 0,
                    locals: vec![("x".into(), 0)],
                },
                FnSym {
                    name: "b".into(),
                    entry_pc: 2,
                    locals: vec![],
                },
            ],
            il: None,
            debug: ProgramDebug::default(),
        };
        let ranges = arts.function_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].1, ranges[0].2), (0, 2));
        assert_eq!((ranges[1].1, ranges[1].2), (2, 4));

        let miss = format_bytecode(&arts, Some("nope"));
        assert!(miss.unwrap_err().contains("no functions matching"));

        let out = format_bytecode(&arts, Some("a")).unwrap();
        assert!(out.contains(";; fn a"));
        assert!(out.contains("LOAD"));
        assert!(!out.contains(";; fn b"));
    }

    #[test]
    fn format_packed_load_and_bin_slot_imm() {
        let packed = Byte::new(Instruction::LOAD).with_load_store_packed(3, 1, 2, 3);
        let empty = HashMap::new();
        let line = format_byte_line(0, &packed, &[], &empty);
        assert!(line.contains("s0=1"));
        assert!(line.contains("s1=2"));
        assert!(line.contains("s2=3"));

        let bin =
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::SUB as u8, 4, -1);
        let line = format_byte_line(9, &bin, &[], &empty);
        assert!(line.contains("BinSlotImm"));
        assert!(line.contains("slot=4"));
        assert!(line.contains("imm=-1"));
    }
}
