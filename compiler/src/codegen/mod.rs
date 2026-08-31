use std::{
    borrow::Borrow,
    collections::{BTreeSet, HashMap, HashSet},
};

use common::{
    Byte, DEBUG_FILE_UNKNOWN, DebugLoc, FnDebugSym, Instruction, Interner, Value, ValueTag,
    encode_tag_operand, likely, tag, unlikely,
};
use reporting::Label as DiagLabel;

use crate::block_builder::{BlockBuilder, JumpKind as BbJumpKind, Label as BbLabel};
use crate::const_fold::ConstValue;
use crate::il::{CodeBuf, EmitBuf, EntryKind, FuseHint, IlJumpKind, IlOp, Label as IlLabel};
use crate::monomorphize::{MonoKey, MonoPlan, parse_mono_ty_name};
use crate::typechecking::{Checker, Ty};
use parser::{
    SimpleSpan,
    ast::{Expression, MatchArm, Output, Pattern, PatternPayload},
};
use reporting::{ErrorCode, Message};

/// Max native recursion depth for [`compiler::Compiler::do_compile`]. Chosen
/// well under what a debug-build stack of a few MiB can hold even with
/// `do_compile`'s current per-call frame size — see
/// docs/internals/limitations.md.
const CODEGEN_RECURSION_LIMIT: u32 = 2000;

/// Private unwind payload for `do_compile`'s recursion-limit panic. Caught in
/// [`Compiler::compile_module`]; never lets user input abort the process the
/// way a genuine native stack overflow does.
struct CodegenRecursionLimitExceeded;

macro_rules! unary {
    ($result: expr, $self: expr, $rhs: expr, $instruction: expr) => {
        $result.append(&mut $self.do_compile($rhs));

        $result.push($instruction);
    };
}
macro_rules! binary {
    ($result: expr, $self: expr, $lhs: expr, $rhs: expr, $instruction: expr) => {
        let _ = $self.compile_binary_operands(&mut $result, $lhs, $rhs);
        $result.push($instruction);
    };
}

// --- Match helpers ---

/// Arms grouped by outer variant tag for dispatch and inner-pattern tests.
#[derive(Debug, Clone)]
struct TagGroup {
    tag: u32,
    arm_indices: Vec<usize>,
    is_single_arm_group: bool,
}

/// Map FFI type expressions to runtime `(tag, aux)` for declare/invoke codegen.
fn ffi_type_tag_from_output(checker: &Checker, expr: &Output) -> Option<(u32, u32)> {
    checker.ffi_type_tag_from_output(expr)
}

/// Fallback FFI tag from a call-site expression when the typechecker did not
/// record tags (recovery / missing side-table entry).
///
/// Returns `None` for unknown shapes — callers must not invent `INT` and
/// silently mis-promote; prefer skipping the variadic tag tuple or emitting
/// a diagnostic instead.
fn ffi_tag_for_expr_fallback(expr: &Output) -> Option<(u32, u32)> {
    use common::tag;
    match expr.1.as_ref() {
        Expression::Float(_) => Some((tag::FLOAT, 0)),
        Expression::String(_) => Some((tag::STRING, 0)),
        Expression::Bool(_) => Some((tag::BOOL, 0)),
        Expression::Integer(_) => Some((tag::INT, 0)),
        Expression::Expr(inner) | Expression::Group(inner) | Expression::Statement(inner) => {
            ffi_tag_for_expr_fallback(inner)
        }
        _ => None,
    }
}

/// Decode escape sequences in a coil string literal (`\n`, `\x41`, `\u{1F}`, …).
pub fn unescape_coil_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => out.push('\0'),
            Some('e') => out.push('\x1b'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let hi = chars.next();
                let lo = chars.next();
                if let (Some(h), Some(l)) = (hi, lo) {
                    let hex = format!("{h}{l}");
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        out.push(v as char);
                        continue;
                    }
                }
                out.push('\\');
                out.push('x');
                if let Some(h) = hi {
                    out.push(h);
                }
                if let Some(l) = lo {
                    out.push(l);
                }
            }
            Some('u') => {
                if chars.next() == Some('{') {
                    let mut hex = String::new();
                    let mut closed = false;
                    while let Some(ch) = chars.next() {
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        hex.push(ch);
                    }
                    if closed {
                        if let Ok(code) = u32::from_str_radix(&hex, 16) {
                            if let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                continue;
                            }
                        }
                    }
                }
                out.push('\\');
                out.push('u');
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Decode a coil string literal to its UTF-8 bytes (after escapes).
pub fn string_literal_as_bytes(raw: &str) -> Vec<u8> {
    unescape_coil_string(raw).into_bytes()
}

/// If `raw` (coil string-literal contents) unescapes to exactly one UTF-8 byte,
/// return that byte. Used for static `string` → `byte` literal coercion.
pub fn string_literal_as_single_byte(raw: &str) -> Result<u8, StringLiteralByteError> {
    match string_literal_as_bytes(raw).as_slice() {
        [b] => Ok(*b),
        [] => Err(StringLiteralByteError::Empty),
        _ => Err(StringLiteralByteError::NotSingleByte),
    }
}

/// Why a string literal cannot coerce to `byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringLiteralByteError {
    Empty,
    NotSingleByte,
}

fn primitive_name_from_type_ann(ty: &Output) -> Option<&'static str> {
    match ty.1.as_ref() {
        Expression::Type(name) => primitive_type_name(&Ty::Con((*name).into())),
        _ => None,
    }
}

fn primitive_type_name(ty: &Ty) -> Option<&'static str> {
    use crate::typechecking::ty::{BOOL, BYTE, FLOAT, INT};
    match ty {
        Ty::Con(name) => match name.as_str() {
            INT => Some("int"),
            FLOAT => Some("float"),
            BYTE => Some("byte"),
            BOOL => Some("bool"),
            _ => None,
        },
        _ => None,
    }
}

fn primitive_cast_opcode(from: &str, to: &str) -> Option<Instruction> {
    match (from, to) {
        ("int", "float") => Some(Instruction::CastIntToFloat),
        ("float", "int") => Some(Instruction::CastFloatToInt),
        ("int", "byte") => Some(Instruction::CastIntToByte),
        ("byte", "int") => Some(Instruction::CastByteToInt),
        ("int", "bool") => Some(Instruction::CastIntToBool),
        ("bool", "int") => Some(Instruction::CastBoolToInt),
        (a, b) if a == b => None,
        _ => None,
    }
}

fn into_primitive_fqn(from: &str, to: &str) -> String {
    format!("Into__{}__to_{}__into", from, to)
}

fn emit_ffi_type_const(bytecode: &mut impl EmitBuf, tag: u32, aux: u32) {
    bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(encode_tag_operand(tag, aux)));
}

/// Resolve variadic FFI arg tags from the typechecker side-table, falling back
/// to literal shapes only. Unknown expressions yield no tags (and a diagnostic)
/// rather than silently promoting as `INT`.
fn resolve_variadic_ffi_tags(
    checker: &Checker,
    span: (usize, usize),
    args: &[&Output<'_>],
    messages: &mut Vec<Message>,
) -> Option<Vec<(u32, u32)>> {
    if let Some(tags) = checker.variadic_arg_tags_at(span) {
        return Some(tags.to_vec());
    }
    let mut tags = Vec::with_capacity(args.len());
    for arg in args {
        match ffi_tag_for_expr_fallback(arg) {
            Some(t) => tags.push(t),
            None => {
                let range = arg.0.start..arg.0.end;
                let mut m = Message::error(
                    ErrorCode::GenericTypeError,
                    "cannot determine FFI type tag for variadic argument".into(),
                    range.clone(),
                );
                m.push(DiagLabel::new(
                    "variadic FFI arg tags missing from typechecker; \
                     use a literal or ensure the call is typechecked"
                        .to_string(),
                    range,
                ));
                messages.push(m);
                return None;
            }
        }
    }
    Some(tags)
}

fn is_instance_method_fqn(checker: &Checker, name: &str) -> bool {
    checker.generics().instances.iter().any(|instance| {
        instance
            .method_fqns
            .values()
            .any(|method_fqn| method_fqn == name)
    })
}

fn group_arms_by_outer_tag(arms: &[MatchArm], checker: &Checker) -> Vec<TagGroup> {
    let mut groups: Vec<TagGroup> = Vec::new();
    let mut tag_to_idx: HashMap<u32, usize> = HashMap::new();
    for (i, arm) in arms.iter().enumerate() {
        let tag = match &arm.pattern.1 {
            Pattern::Constructor {
                enum_name,
                variant_name,
                ..
            } => checker.tag_for(enum_name, variant_name).unwrap_or(u32::MAX),
            _ => u32::MAX,
        };
        if let Some(&idx) = tag_to_idx.get(&tag) {
            groups[idx].arm_indices.push(i);
        } else {
            tag_to_idx.insert(tag, groups.len());
            groups.push(TagGroup {
                tag,
                arm_indices: vec![i],
                is_single_arm_group: false,
            });
        }
    }
    for g in &mut groups {
        g.is_single_arm_group = g.arm_indices.len() == 1;
    }
    groups
}

/// True when an arm needs inner-pattern runtime tests (nested bindings/constructors).
#[allow(dead_code)]
fn arm_has_runtime_test(arm: &MatchArm) -> bool {
    /// Recursive helper: does the inner payload of this arm's
    /// outer Constructor pattern carry a `Binding` or further
    /// nested `Constructor` (i.e., a value to extract)?
    fn inner_carries_value(pattern: &Pattern) -> bool {
        match pattern {
            Pattern::Wildcard | Pattern::Binding { .. } => false,
            Pattern::Constructor { payload, .. } => match payload {
                PatternPayload::Unit => false,
                PatternPayload::Tuple(parts) => parts
                    .iter()
                    .any(|p| matches!(p.1, Pattern::Binding { .. } | Pattern::Constructor { .. })),
                PatternPayload::Record(fields) => fields.iter().any(|f| {
                    matches!(
                        f.pattern.1,
                        Pattern::Binding { .. } | Pattern::Constructor { .. }
                    )
                }),
            },
        }
    }
    if let Pattern::Constructor { payload, .. } = &arm.pattern.1 {
        match payload {
            PatternPayload::Unit => false,
            PatternPayload::Tuple(parts) => parts.iter().any(|p| inner_carries_value(&p.1)),
            PatternPayload::Record(fields) => {
                fields.iter().any(|f| inner_carries_value(&f.pattern.1))
            }
        }
    } else {
        false
    }
}

/// Emit inner-pattern tests after outer tag dispatch (multi-arm groups).
#[allow(dead_code, unused_variables)]
fn emit_inner_test<'compiler>(
    arm_idx: usize,
    checker: &Checker,
    enum_name: &str,
    variant_name: &str,
    payload: &PatternPayload<'compiler>,
    match_bindings_per_arm: &mut HashMap<usize, HashMap<String, u32>>,
    bytecode: &mut CodeBuf,
    bb: &mut BlockBuilder,
    pass_label: Option<crate::block_builder::Label>,
    fail_label: crate::block_builder::Label,
    payload_base: u32,
) {
    use parser::ast::PatternPayload;
    match payload {
        PatternPayload::Unit => {
            // Unit inner (e.g. `Option::None`): always matches.
            // JMP to the arm body when we have a pass_label —
            // required when a later tag group follows so we
            // don't fall through into that group's body.
            if let Some(label) = pass_label {
                bb.emit_jump_to(label, BbJumpKind::Unconditional, bytecode.il_mut());
            }
        }
        PatternPayload::Tuple(parts) => {
            // POP/STORE for wildcards/bindings; JUMP_IF_MATCH for nested constructors.
            let mut any_nested_ctor = false;
            for sub in parts {
                match &sub.1 {
                    Pattern::Wildcard => {
                        bytecode.push_pop();
                    }
                    Pattern::Binding { name } => {
                        let slot = next_available_slot(match_bindings_per_arm, payload_base);
                        match_bindings_per_arm
                            .entry(arm_idx)
                            .or_default()
                            .insert(name.to_string(), slot);
                        // Value already lives in `slot` via UNPACK / JUMP_IF_MATCH.
                    }
                    Pattern::Constructor {
                        enum_name: sub_enum,
                        variant_name: sub_variant,
                        payload: sub_payload,
                        ..
                    } => {
                        // Nested constructor: JUMP_IF_MATCH on inner tag, or recurse for records.
                        any_nested_ctor = true;
                        if matches!(sub_payload, PatternPayload::Record(_)) {
                            // Nested record — recurse. The recursion
                            // walks the inner record's declared fields
                            // in decl_order and emits per-field
                            // tests (POP / STORE / JUMP_IF_MATCH on
                            // further-nested tags).
                            emit_inner_test(
                                arm_idx,
                                checker,
                                sub_enum,
                                sub_variant,
                                sub_payload,
                                match_bindings_per_arm,
                                bytecode,
                                bb,
                                pass_label,
                                fail_label,
                                payload_base,
                            );
                        } else if let Some(label) = pass_label {
                            if let Some(inner_tag) = checker.tag_for(sub_enum, sub_variant) {
                                bb.emit_jump_to(
                                    label,
                                    BbJumpKind::JumpIfMatch {
                                        tag: inner_tag,
                                        arity: 0,
                                    },
                                    bytecode.il_mut(),
                                );
                            } else {
                                bytecode.push_pop();
                            }
                        } else {
                            // Last arm in the group — emit POP to
                            // consume the inner value. The arm
                            // body is reached by fall-through.
                            bytecode.push_pop();
                        }
                    }
                }
            }
            // Trailing JMP to pass_label when inner tests are all wildcards/bindings.
            if !any_nested_ctor && let Some(label) = pass_label {
                bb.emit_jump_to(label, BbJumpKind::Unconditional, bytecode.il_mut());
            }
        }
        PatternPayload::Record(fields) => {
            // Walk record fields in declaration order (matches UNPACK slot layout).
            let decl_order = checker.payload_tys_for(enum_name, variant_name);
            let pattern_site: std::collections::HashMap<&str, &Pattern<'compiler>> =
                fields.iter().map(|pf| (pf.name, &pf.pattern.1)).collect();
            let mut any_nested_ctor = false;
            for (decl_name, _) in decl_order.iter() {
                let sub_pat = match pattern_site.get(decl_name.as_str()) {
                    Some(p) => *p,
                    None => {
                        // Field omitted from the pattern — emit
                        // POP to discard the value (the test
                        // chain always consumes every slot, so
                        // this is unconditional).
                        bytecode.push_pop();
                        continue;
                    }
                };
                match sub_pat {
                    Pattern::Wildcard => {
                        bytecode.push_pop();
                    }
                    Pattern::Binding { name } => {
                        let slot = next_available_slot(match_bindings_per_arm, payload_base);
                        match_bindings_per_arm
                            .entry(arm_idx)
                            .or_default()
                            .insert(name.to_string(), slot);
                        // Value already lives in `slot` via UNPACK / JUMP_IF_MATCH.
                    }
                    Pattern::Constructor {
                        enum_name: sub_enum,
                        variant_name: sub_variant,
                        payload: sub_payload,
                        ..
                    } => {
                        // Nested Constructor sub-pattern on a
                        // record field. If the nested
                        // Constructor's payload is itself a
                        // Record, recurse to dispatch on the
                        // inner record's nested tags. Otherwise
                        // (Unit / Tuple), emit JUMP_IF_MATCH on
                        // the inner tag as before.
                        any_nested_ctor = true;
                        if matches!(sub_payload, PatternPayload::Record(_)) {
                            emit_inner_test(
                                arm_idx,
                                checker,
                                sub_enum,
                                sub_variant,
                                sub_payload,
                                match_bindings_per_arm,
                                bytecode,
                                bb,
                                pass_label,
                                fail_label,
                                payload_base,
                            );
                        } else if let Some(label) = pass_label {
                            if let Some(inner_tag) = checker.tag_for(sub_enum, sub_variant) {
                                bb.emit_jump_to(
                                    label,
                                    BbJumpKind::JumpIfMatch {
                                        tag: inner_tag,
                                        arity: 0,
                                    },
                                    bytecode.il_mut(),
                                );
                            } else {
                                bytecode.push_pop();
                            }
                        } else {
                            // Last arm in the group — emit POP
                            // to consume the inner value. The
                            // arm body is reached by
                            // fall-through.
                            bytecode.push_pop();
                        }
                    }
                }
            }
            if !any_nested_ctor && let Some(label) = pass_label {
                bb.emit_jump_to(label, BbJumpKind::Unconditional, bytecode.il_mut());
            }
        }
    }
}

/// Collect `name → Ty` for every binding in a match pattern.
///
/// Used so Access codegen (`p.y`) sees the *current arm's* binding type
/// rather than whatever last arm wrote into the flat
/// `codegen_var_types` side-table (same name reused across arms with
/// different payload types would otherwise emit the wrong `LoadField`).
///
/// Open schema placeholders (`Ty::Var`, or `Ty::Con("T")` type-param
/// markers from poly enums like `Option` / `Result` / `Box<T>`) are
/// **not** inserted — they would shadow the instantiated binding type
/// that `infer_pattern` already wrote into `codegen_var_types`.
fn collect_pattern_binding_types(
    checker: &Checker,
    pattern: &Pattern<'_>,
    out: &mut HashMap<String, Ty>,
) {
    match pattern {
        Pattern::Wildcard => {}
        Pattern::Binding { .. } => {
            // Bare `name =>` needs the scrutinee type from the side-table;
            // caller may fill that in. Constructor/record payloads below
            // carry declared field types.
        }
        Pattern::Constructor {
            enum_name,
            variant_name,
            payload,
        } => {
            let decl = checker.payload_tys_for(enum_name, variant_name);
            match payload {
                PatternPayload::Unit => {}
                PatternPayload::Tuple(parts) => {
                    for (i, part) in parts.iter().enumerate() {
                        let expected = decl.get(i).map(|(_, ty)| ty);
                        collect_pattern_binding_types_with_expected(
                            checker, enum_name, &part.1, expected, out,
                        );
                    }
                }
                PatternPayload::Record(fields) => {
                    let by_name: HashMap<&str, &Ty> =
                        decl.iter().map(|(n, ty)| (n.as_str(), ty)).collect();
                    for pf in fields {
                        let expected = by_name.get(pf.name).copied();
                        collect_pattern_binding_types_with_expected(
                            checker,
                            enum_name,
                            &pf.pattern.1,
                            expected,
                            out,
                        );
                    }
                }
            }
        }
    }
}

/// True when `ty` is a poly-enum schema placeholder for `enum_name`
/// (type-param `Con("T")` / `Con("E")` / …) or an open `Ty::Var`.
fn is_open_schema_ty(checker: &Checker, enum_name: &str, ty: &Ty) -> bool {
    match ty {
        Ty::Var(_) => true,
        Ty::Con(name) => checker
            .generics()
            .generic_type_ctors
            .get(enum_name)
            .is_some_and(|params| params.iter().any(|p| p == name)),
        _ => false,
    }
}

fn collect_pattern_binding_types_with_expected(
    checker: &Checker,
    enum_name: &str,
    pattern: &Pattern<'_>,
    expected: Option<&Ty>,
    out: &mut HashMap<String, Ty>,
) {
    match pattern {
        Pattern::Wildcard => {}
        Pattern::Binding { name } => {
            if let Some(ty) = expected {
                if !is_open_schema_ty(checker, enum_name, ty) {
                    out.insert(name.to_string(), ty.clone());
                }
            }
        }
        Pattern::Constructor { .. } => {
            collect_pattern_binding_types(checker, pattern, out);
        }
    }
}

/// Next free binding slot for match payloads.
///
/// `base` is the first payload slot (`context.variables.len()` at match
/// entry). Slot 0 is reserved for the first function argument; trailing
/// dictionary locals occupy 1..base-1 when `dict_arity > 0`.
#[allow(dead_code)]
fn next_available_slot(match_bindings: &HashMap<usize, HashMap<String, u32>>, base: u32) -> u32 {
    let mut max_slot = base.saturating_sub(1);
    for arm_bindings in match_bindings.values() {
        for &slot in arm_bindings.values() {
            if slot > max_slot {
                max_slot = slot;
            }
        }
    }
    max_slot + 1
}

/// Bytecode table key for an overload: `name#2.0` or `name#rest1.0`.
///
/// `id` distinguishes same-arity typed overloads (`sum#1.0` vs `sum#1.1`).
fn overload_fn_key(name: &str, fixed_arity: usize, is_rest: bool, id: u32) -> String {
    if is_rest {
        format!("{name}#rest{fixed_arity}.{id}")
    } else {
        format!("{name}#{fixed_arity}.{id}")
    }
}

/// Strip `#N.id` / `#restN.id` (or legacy `#N`) suffix from an overload table key.
fn strip_overload_key(name: &str) -> &str {
    match name.rfind('#') {
        Some(i) => &name[..i],
        None => name,
    }
}

/// `MakeFn` operand: `[7:0]=n_cap [15:8]=n_filled [23:16]=arity [24]=is_rest`.
///
/// `n_cap` and `n_filled` are packed into 8-bit fields (max 255). Callers with
/// larger values must not reach here — partial-application arity is already
/// capped at 32 for `filled_mask`.
fn make_fn_operand(n_cap: u32, n_filled: u32, arity: u32, is_rest: bool) -> u32 {
    debug_assert!(
        n_cap <= 0xFF && n_filled <= 0xFF,
        "MakeFn n_cap/n_filled overflow 8-bit fields: n_cap={n_cap} n_filled={n_filled}"
    );
    (n_cap & 0xFF) | ((n_filled & 0xFF) << 8) | (arity << 16) | if is_rest { 1 << 24 } else { 0 }
}

/// Fixed-arity / rest flag from a function's `Argument` fragment.
fn fn_arity_from_args(args: &Output<'_>) -> (usize, bool) {
    match args.1.as_ref() {
        Expression::Fragment(children) => {
            let has_rest = children.last().is_some_and(|c| {
                matches!(c.1.as_ref(), Expression::Argument { is_rest: true, .. })
            });
            let n = children
                .iter()
                .filter(|c| matches!(c.1.as_ref(), Expression::Argument { .. }))
                .count();
            if has_rest {
                (n.saturating_sub(1), true)
            } else {
                (n, false)
            }
        }
        _ => (0, false),
    }
}

#[derive(Default, Clone)]
struct Context {
    current: Option<String>,
    variables: Interner<String>,
    symbols: Interner<String>,
    assignments: HashMap<String, bool>,
    constants: HashMap<usize, bool>,
    classes: HashMap<String, Vec<(String, usize)>>,
    impementations: HashMap<String, String>,
    methods: HashMap<String, HashMap<String, String>>,

    /// Nested match arm bindings (payload slots). Inner maps merge over outer
    /// ones so nested `match` can still load the enclosing arm's names.
    match_bindings: Option<HashMap<String, u32>>,

    /// Block-local binding overlays. When `Some`, shadowed names allocate a
    /// fresh slot instead of reusing the outer Interner id (so exiting the
    /// block leaves the outer binding's stack value intact).
    block_bindings: Option<HashMap<String, u32>>,

    /// Fixed `[T; N]` locals laid out as `N` consecutive frame slots:
    /// name → (base_slot, N). Escaping uses → `MakeArray`.
    stack_array_locals: HashMap<String, (u32, usize)>,

    prev: Option<Box<Self>>,
}

// --- Compiler ---

/// Length of the CALL + JMP + HALT prologue every [`Compiler`] starts with.
/// Multi-file linking treats `bytecode.len() <= PROLOGUE_BYTECODE_LEN` as a
/// fresh compile (safe to clear the shared constant pool).
pub const PROLOGUE_BYTECODE_LEN: usize = 3;

/// Matched base-case opening for caller-side predicate peel (2B).
struct PredicatePeel {
    cond: Vec<IlOp>,
    then_value: IlOp,
    /// One past the highest callee slot referenced by cond/then.
    arity_hint: usize,
}

/// A peeled guard op rewritten against the caller's argument expressions.
enum PeelRematOp {
    /// Re-materialize argument `idx`.
    Arg(usize),
    /// Argument `idx`, then `imm`, then the binary op (unfused `BinSlotImm`).
    ArgImm {
        op: Instruction,
        idx: usize,
        imm: i32,
    },
    /// Arguments `a` then `b`, then the binary op (unfused `BinSlotSlot`).
    ArgArg { op: Instruction, a: usize, b: usize },
    /// Argument-independent op, copied as the callee emitted it.
    Copy(IlOp),
}

/// A callee guard ready to emit at a call site without spilling arguments.
struct PeelRematPlan {
    cond: Vec<PeelRematOp>,
    then_value: PeelRematOp,
    /// Argument indices the guard reads (must be re-materializable).
    guard_args: Vec<usize>,
}
pub struct Compiler {
    namespace: String,
    /// Stack IL during emit; lowered `Vec<Byte>` after [`Self::finalize_bytecode`].
    bytecode: CodeBuf,

    aliases: HashMap<String, String>,
    functions: HashMap<String, usize>,
    /// Entry labels for names in [`Self::functions`] (step-3 binds).
    fn_entry_labels: HashMap<String, IlLabel>,
    /// Fixed arity + rest flag per function table key. Survives multi-file
    /// `check_program` clears of `Checker::fn_param_names`, so `MakeFn` for
    /// imported names (e.g. `spawn(run_jobs, …)` after `use pool::worker::run_jobs`)
    /// still packs the real arity.
    fn_arities: HashMap<String, (u32, bool)>,
    /// Top-level items per namespace (legacy; disk `::*` no longer expands).
    module_items: std::collections::HashMap<String, Vec<String>>,
    native: HashMap<String, usize>,
    /// Let-slot holding each extern library handle (now a static slot index).
    extern_runtime_libs: HashMap<String, u32>,
    /// Source name → (lib_static_slot, fn_id_static_slot) for runtime FFI calls.
    extern_runtime_functions: HashMap<String, (u32, u32)>,
    /// Records which FFI library short names have already
    /// been loaded in the current compilation unit. Cleared
    /// each `compile`.
    extern_runtime_libs_loaded: std::collections::HashSet<String>,
    // --
    messages: Vec<Message>,
    context: Context,
    // --
    /// Hindley–Milner checker. Run once per `compile` via
    /// `Checker::check_program`; its cache is consulted by `do_compile`
    /// to pick `ADD` vs `ADDF`, `==` vs `==` (floats), etc.
    checker: crate::typechecking::Checker,
    /// NodeId/DefId facts from the last `check_program` (B2).
    typed_sidecar: crate::typechecking::TypedSidecar,
    /// Index into [`crate::typechecking::Checker::ids`] used by
    /// `do_compile` to recover the `NodeId` of the node it's currently
    /// emitting. Reset at the start of each `compile`.
    emit_idx: usize,
    /// Offset where user code starts (after prologue). Extern blocks precede main.
    program_start_offset: u32,
    /// Entry for prologue `JMP` when static initializers are spliced (unchanged
    /// by the post-splice `program_start_offset` bump).
    setup_entry_offset: u32,
    /// Wide immediates referenced from compact 8-byte `Byte`
    /// operands (floats, `JumpIfMatch` targets, etc.).
    constants: Vec<u64>,
    /// Program string literals referenced by `Instruction::STRING`.
    strings: Vec<String>,
    string_indices: HashMap<String, u32>,

    /// Qualified names of `async fn` declarations (emit `MakeCoro` at call sites).
    coroutine_fns: std::collections::HashSet<String>,

    /// Memoized pair-ABI verdicts, keyed by the name codegen looks a function up
    /// by. The type env fills in as bodies are compiled, so an unmemoized query
    /// can answer differently for a body and for a later caller — see
    /// [`Compiler::pair_return_kind`].
    pair_return_kinds: std::cell::RefCell<HashMap<String, Option<bool>>>,

    /// Counter for compiler-generated temporary slots.
    temp_counter: u32,

    /// Function-local cache of field-name string keys used ≥2 times
    /// (`STRING` materialized once at entry, then `LOAD`).
    field_key_slots: HashMap<String, u32>,

    /// Count of expression values currently live on the operand stack
    /// *above* interned locals (e.g. a `HostInvoke` native-id `CONST`
    /// pushed before argument codegen). `alloc_temp_slot` must allocate
    /// at or above `variables.len() + expr_depth` so `StorePop` does not
    /// clobber those live values (locals and the operand stack share
    /// memory).
    expr_depth: u32,

    /// Native call-stack depth of [`Compiler::do_compile`]'s recursion,
    /// guarded against a fixed limit — see the analogous `infer_depth` on
    /// the typechecker's `Checker`.
    codegen_depth: u32,

    /// Active loop labels: `(continue_target, break_target)`.
    loop_stack: Vec<(BbLabel, BbLabel)>,

    /// Active loop patchers. Break/continue emit through the innermost builder.
    loop_bbs: Vec<BlockBuilder>,

    /// Registered `defer` thunks in the function currently being compiled
    /// (declaration order). Run LIFO on return / fall-through via
    /// `emit_run_defers`. Kept on `Compiler` (not `Context`) so nested
    /// block frames do not drop registered defers.
    ///
    /// Each thunk stores an IL label bound at its body entry and the `use (…)`
    /// capture names. At run time those captures are LOADed from the enclosing
    /// frame and passed as CALL arguments so the thunk's fresh frame sees them
    /// at slots 0..N-1 (same layout as lambda capture slots).
    fn_defers: Vec<(BbLabel, Vec<String>)>,

    /// Class name → decorated constructor function name (from attr expansion).
    decorated_class_ctors: HashMap<String, String>,

    /// Name of the function currently being codegen'd (for ctor/Instantiate routing).
    active_fn_name: Option<String>,

    /// Bytecode for global static initializers (spliced at `program_start_offset`).
    static_init_bytecode: Vec<Byte>,

    /// `extern` dlopen/declare setup accumulated across modules, spliced into
    /// the prologue setup region at finalize (so imported-module `extern`
    /// still runs before `main`).
    ffi_init: CodeBuf,

    /// True while compiling an `impl` method — Function resets locals
    /// and reserves slot 0 for `self`.
    compiling_method: bool,

    /// True while compiling a function whose return type is inferred
    /// as `Result<T, E>` via `raise` / `?` (wrap bare `return` in `Ok`).
    compiling_result_mode: bool,
    /// When result-mode Ok payload is itself `Result`, keep Ok-wrapping
    /// explicit `return Result::Ok(…)` (nested Result payload case).
    compiling_result_ok_is_result: bool,

    /// Force ground pointer-niche `Option` expressions back to heap enums
    /// while using legacy pattern lowering or an unknown boundary.
    force_heap_option: bool,
    /// Force a contextually typed `Option::None` / `Option::Some` onto the
    /// pointer-niche path when its constructor node has no standalone type.
    force_niche_option: bool,

    /// Emit and consume the two-slot `[payload, tag]` ABI for a unary
    /// `Option`/`Result` return while compiling a statically known function.
    compiling_pair_mode: bool,
    /// Whether [`Self::compiling_pair_mode`]'s return is an `Option` (tag 0 is
    /// then payload-less `None`). Travels in the `ReturnPair` operand so a host
    /// entry can re-box the pair.
    compiling_pair_is_option: bool,
    /// The current expression is allowed to remain in the pair ABI instead of
    /// being materialized back into a heap enum.
    pair_value_context: bool,

    /// Harness metadata: `(description, bytecode offset)` for each
    /// top-level `test("…") { … }` case, in source order.
    test_cases: Vec<(String, u32)>,

    /// True when a user-written `fn main` was emitted this compile.
    user_main_defined: bool,

    /// When true, [`Expression::Match`] arm bodies may emit tail calls.
    match_tail_call: bool,

    /// When false (default), harness `test("…")` blocks and `#[test]` functions
    /// are stripped before typecheck/codegen. Set true for `coil test`
    /// or `compile --include-tests`.
    include_tests: bool,

    /// Local variable names that hold an `ObjPolyFn` heap pointer
    /// (i.e. `let f = some_generic_fn;`). When these are invoked via
    /// `Expression::Call`, the codegen emits `CallIndirect` instead
    /// of a direct `CALL` opcode.
    polyfn_vars: HashSet<String>,
    /// Local PolyFn variable → source generic function name.
    polyfn_sources: HashMap<String, String>,

    /// Monomorphization plan for this compile unit plus emitted clone offsets.
    mono_plan: MonoPlan,
    mono_offsets: HashMap<MonoKey, usize>,
    /// Temporary variable-type overrides while emitting a specialized clone.
    mono_codegen_var_types: Vec<HashMap<String, Ty>>,

    /// Project-relative path of the module currently being codegen'd.
    current_source_file: Option<std::path::PathBuf>,
    /// Stable `DebugLoc::file` indices (path string → id).
    source_file_indices: std::collections::BTreeMap<String, u32>,
    /// `source_files` order for the archive (index → path).
    source_file_list: Vec<String>,
    /// One [`DebugLoc`] per bytecode slot (grows with [`Self::bytecode`]).
    debug_locs: Vec<DebugLoc>,

    /// Compile-time scalar values for `const` bindings (frame stack).
    const_env_stack: Vec<HashMap<String, ConstValue>>,
    /// Folded scalar initializers for module `static const` / `static` slots.
    static_const_values: HashMap<String, ConstValue>,

    /// Qualified name of the function currently being codegen'd (tail-call eligibility).
    current_function_qualified: Option<String>,
    /// `functions` map key for the active function (overload-aware).
    current_function_table_key: Option<String>,
    /// Peel/unroll spans for the module currently being compiled.
    fn_bytecode_spans: HashMap<String, (usize, usize)>,
    /// Callee spans kept across files for tiny-inline (COI-125).
    fn_inline_spans: HashMap<String, (usize, usize)>,
    /// Module namespace that defined each [`Self::fn_inline_spans`] key.
    fn_defining_module: HashMap<String, String>,
    /// Debug: FQN → user-facing local/param name → frame slot (last write wins).
    fn_debug_locals: HashMap<String, HashMap<String, u32>>,

    /// When true, [`Expression::Match`] binds `end` as a plain label instead of
    /// a value-join (`JoinLabel`). Set while compiling a match whose value is
    /// consumed immediately by `StorePop` / `StoreStatic` (e.g. `let x = match …`).
    suppress_match_fusion_barrier: bool,

    /// Self-recursive pure function names eligible for auto fork-join.
    recursive_pure: HashSet<String>,
    /// Side-effect-free user `fn` names (loop bounds / COI-99).
    pure_fns: HashSet<String>,
    /// Detected independent-parallel-arm fork sites for pure fns.
    par_shapes: HashMap<String, crate::typechecking::ParForkSite>,
    /// Concrete arg vectors requiring `__coil_par_*` specializations.
    par_spec_args: HashMap<String, BTreeSet<Vec<i64>>>,
    /// Counted loops whose iterations are independent arms, by loop span.
    loop_par_sites: crate::typechecking::LoopParSites,
    /// Chunk workers emitted so far, for `__coil_par_loop_*` naming.
    loop_par_helpers: usize,

    /// Operand-stack capacity for the VM (from recursion-depth analysis).
    operand_stack_slots: u32,

    /// IL optimization preset (COI-127).
    opt_options: crate::il::opt::OptimizeOptions,

    /// Cost budgets for tiny-inline (COI-124).
    pub inline_cost: inline_cost::InlineCostOptions,

    /// When true, [`Self::finalize_bytecode`] keeps post-opt pre-fuse IL.
    retain_cursor_il: bool,
    /// Snapshot filled by finalize when [`Self::retain_cursor_il`] is set.
    cursor_il: Option<crate::il::tell::CursorIlSnap>,
}

impl Default for Compiler {
    fn default() -> Self {
        let mut bytecode = CodeBuf::new();
        bytecode.push(Byte::new(Instruction::CALL));
        bytecode.push_prologue_jmp();
        bytecode.push(Byte::new(Instruction::HALT));
        debug_assert_eq!(bytecode.len(), PROLOGUE_BYTECODE_LEN);
        let program_start_offset = bytecode.len() as u32;
        let debug_locs = vec![DebugLoc::unknown(); bytecode.len()];

        Self {
            namespace: String::default(),
            bytecode,
            debug_locs,
            aliases: HashMap::default(),
            functions: HashMap::with_capacity(32),
            fn_entry_labels: HashMap::with_capacity(32),
            fn_arities: HashMap::with_capacity(32),
            module_items: std::collections::HashMap::default(),
            native: HashMap::default(),
            extern_runtime_libs: HashMap::with_capacity(4),
            extern_runtime_functions: HashMap::with_capacity(16),
            extern_runtime_libs_loaded: HashSet::new(),
            // ---
            messages: Vec::default(),
            context: Context::default(),
            // ---
            checker: crate::typechecking::Checker::new(),
            typed_sidecar: crate::typechecking::TypedSidecar::default(),
            emit_idx: 0,
            program_start_offset,
            setup_entry_offset: program_start_offset,
            constants: Vec::default(),
            strings: Vec::default(),
            string_indices: HashMap::default(),
            coroutine_fns: std::collections::HashSet::new(),
            temp_counter: 0,
            field_key_slots: HashMap::new(),
            expr_depth: 0,
            codegen_depth: 0,
            loop_stack: Vec::new(),
            loop_bbs: Vec::new(),
            fn_defers: Vec::new(),
            decorated_class_ctors: HashMap::new(),
            active_fn_name: None,
            compiling_method: false,
            compiling_result_mode: false,
            compiling_result_ok_is_result: false,
            force_heap_option: false,
            force_niche_option: false,
            compiling_pair_mode: false,
            compiling_pair_is_option: false,
            pair_return_kinds: std::cell::RefCell::new(HashMap::new()),
            pair_value_context: false,
            test_cases: Vec::new(),
            user_main_defined: false,
            include_tests: false,
            polyfn_vars: HashSet::new(),
            polyfn_sources: HashMap::new(),
            mono_plan: MonoPlan::default(),
            mono_offsets: HashMap::new(),
            mono_codegen_var_types: Vec::new(),
            static_init_bytecode: Vec::new(),
            ffi_init: CodeBuf::new(),
            current_source_file: None,
            source_file_indices: std::collections::BTreeMap::new(),
            source_file_list: Vec::new(),
            const_env_stack: Vec::new(),
            static_const_values: HashMap::new(),
            current_function_qualified: None,
            current_function_table_key: None,
            fn_bytecode_spans: HashMap::new(),
            fn_inline_spans: HashMap::new(),
            fn_defining_module: HashMap::new(),
            fn_debug_locals: HashMap::new(),
            suppress_match_fusion_barrier: false,
            match_tail_call: false,
            recursive_pure: HashSet::new(),
            pure_fns: HashSet::new(),
            par_shapes: HashMap::new(),
            par_spec_args: HashMap::new(),
            loop_par_sites: crate::typechecking::LoopParSites::new(),
            loop_par_helpers: 0,
            operand_stack_slots: crate::typechecking::DEFAULT_OPERAND_STACK_SLOTS,
            opt_options: crate::il::opt::OptimizeOptions::default(),
            inline_cost: inline_cost::InlineCostOptions::default(),
            retain_cursor_il: false,
            cursor_il: None,
        }
    }
}

impl<'ctx> Context {
    fn child(&self) -> Self {
        Self {
            current: self.current.clone(),
            impementations: self.impementations.clone(),
            methods: self.methods.clone(),
            constants: self.constants.clone(),
            assignments: self.assignments.clone(),
            variables: self.variables.clone(),
            symbols: self.symbols.clone(),
            classes: self.classes.clone(),
            match_bindings: self.match_bindings.clone(),
            // Fresh overlay so inner `let` / destructure can shadow outer names.
            block_bindings: Some(HashMap::new()),
            stack_array_locals: self.stack_array_locals.clone(),
            prev: Some(Box::new(self.to_owned())),
        }
    }
}

impl<'ctx> Context {
    pub fn get_prev(&self) -> &Option<Box<Self>> {
        &self.prev
    }
}

fn emit_pattern_binding<'compiler>(
    checker: &Checker,
    match_bindings: &mut HashMap<String, u32>,
    next_slot: &mut u32,
    pattern: &Pattern<'compiler>,
    parent_decl_order: &[(String, Ty)],
    bytecode: &mut CodeBuf,
    consume_values: bool,
    is_outer: bool,
) {
    use parser::ast::PatternPayload;
    match pattern {
        Pattern::Wildcard => {
            if consume_values {
                bytecode.push_pop();
            }
        }
        Pattern::Binding { name } => {
            let slot = *next_slot;
            // Always record the binding — the body still
            // needs to be able to look up the slot via
            // `Identifier` / `Assignment`, even if we don't
            // emit the redundant STORE (the test chain
            // already pushed the value at this slot via
            // JUMP_IF_MATCH).
            match_bindings.insert(name.to_string(), slot);
            // Value already lives in `slot` via JUMP_IF_MATCH / UNPACK.
            let _ = consume_values;
            *next_slot += 1;
        }
        Pattern::Constructor { payload, .. } => match payload {
            PatternPayload::Unit => {
                // A unit-variant nested pattern (e.g. `Option::None`)
                // is invalid — unit variants have no payload. But
                // the typechecker would have rejected this. Emit a
                // defensive POP only if the caller expects a value
                // to consume on the stack.
                //
                // The OUTER-level Unit case is handled by the
                // caller (the forward pass emits POP / STORE 1 /
                // nothing depending on whether the arm is the
                // last, non-last, or a wildcard/binding
                // catch-all). The recursion's Unit case (when
                // is_outer = false) emits POP only when the
                // caller expects a value on the stack
                // (`consume_values = true`).
                if consume_values && !is_outer {
                    bytecode.push_pop();
                }
            }
            PatternPayload::Tuple(parts) => {
                // The OUTER-level Tuple case: the forward pass
                // already emitted UNPACK for the last arm (or
                // JUMP_IF_MATCH for non-last arms). Suppress
                // UNPACK emission at the OUTER level.
                //
                // The recursion's Tuple case (when is_outer =
                // false): we have a nested constructor on the
                // stack (pushed by the outer JUMP_IF_MATCH or
                // UNPACK above), and we need to UNPACK it to
                // get its payload values at the right slot
                // positions before binding its sub-patterns.
                if consume_values && !is_outer {
                    bytecode
                        .push(Byte::new(Instruction::Unpack).with_operand_u32(parts.len() as u32));
                }
                // Recurse for sub-patterns with the same
                // `consume_values` flag. The inner values were
                // pushed either by the (emitted) UNPACK above,
                // or by the outer JUMP_IF_MATCH in the test
                // chain case (when consume_values was false).
                // When `consume_values = false`, the test
                // chain has already emitted the
                // POP / JUMP_IF_MATCH for the inner values, so
                // we suppress the redundant bytecode in the
                // recursion too.
                //
                // The recursion is ALWAYS at `is_outer = false`
                // (the OUTER level is reached exactly once per
                // arm body — by the caller).
                //
                // The sub-pattern's `parent_decl_order` is
                // empty unless the sub-pattern is itself a
                // record constructor — then it's the
                // sub-pattern's declared field order. Tuple
                // sub-patterns don't use `parent_decl_order`
                // (they walk in source order).
                for sub in parts {
                    let sub_decl_order: Vec<(String, Ty)> = if let Pattern::Constructor {
                        enum_name: sub_enum,
                        variant_name: sub_variant,
                        payload: PatternPayload::Record(_),
                        ..
                    } = &sub.1
                    {
                        checker.payload_tys_for(sub_enum, sub_variant)
                    } else {
                        Vec::new()
                    };
                    emit_pattern_binding(
                        checker,
                        match_bindings,
                        next_slot,
                        &sub.1,
                        &sub_decl_order,
                        bytecode,
                        consume_values,
                        false, // is_outer = false (recursion)
                    );
                }
            }
            PatternPayload::Record(fields) => {
                // Declaration-order walk. Nested records unpack into a
                // scratch region past this record's field slots so
                // multi-field inners cannot clobber sibling outers.
                // `record_base` is the first payload slot for this record
                // (normally 1; higher when trailing dict locals precede
                // the match).
                let record_base = *next_slot;
                let n_fields = parent_decl_order.len() as u32;
                let pattern_site: std::collections::HashMap<&str, &Pattern<'compiler>> =
                    fields.iter().map(|pf| (pf.name, &pf.pattern.1)).collect();
                for (i, (decl_name, _)) in parent_decl_order.iter().enumerate() {
                    let field_slot = record_base + i as u32;
                    if let Some(sub_pat) = pattern_site.get(decl_name.as_str()) {
                        // Nested record + consume: copy the enum into a
                        // scratch base (≥ end of this record's fields /
                        // prior scratch), UnpackAt there, then bind from
                        // scratch. Plain siblings after a nested unpack
                        // relocate field_slot → next_slot before binding.
                        //
                        // UnpackAt operands: [31:16]=arity, [15:0]=slot.
                        if consume_values
                            && let Pattern::Constructor {
                                enum_name: sub_enum,
                                variant_name: sub_variant,
                                payload: PatternPayload::Record(_),
                            } = sub_pat
                        {
                            let inner_arity =
                                checker.payload_tys_for(sub_enum, sub_variant).len() as u16;
                            let scratch_base = (*next_slot).max(record_base + n_fields);
                            if scratch_base != field_slot {
                                bytecode.push_load(field_slot);
                                bytecode.push_store_pop(scratch_base);
                            }
                            bytecode.push(
                                Byte::new(Instruction::UnpackAt)
                                    .with_operands_u16([inner_arity, scratch_base as u16]),
                            );
                            *next_slot = scratch_base;
                        } else if consume_values && field_slot != *next_slot {
                            bytecode.push_load(field_slot);
                            bytecode.push_store_pop(*next_slot);
                        }
                        let sub_decl_order: Vec<(String, Ty)> = if let Pattern::Constructor {
                            enum_name: sub_enum,
                            variant_name: sub_variant,
                            payload: PatternPayload::Record(_),
                            ..
                        } = sub_pat
                        {
                            checker.payload_tys_for(sub_enum, sub_variant)
                        } else {
                            Vec::new()
                        };
                        emit_pattern_binding(
                            checker,
                            match_bindings,
                            next_slot,
                            sub_pat,
                            &sub_decl_order,
                            bytecode,
                            consume_values,
                            false, // is_outer = false (recursion)
                        );
                    } else if consume_values {
                        // Field omitted from the pattern.
                        // Emit POP to keep the stack
                        // consistent with the declaration-
                        // order walk. The typechecker
                        // already reported the error (if
                        // any). At the OUTER level, the
                        // forward pass handled missing
                        // fields via UNPACK with the right
                        // arity (the field's slot is just
                        // left dangling — that's by
                        // design — UNPACK still pushes N
                        // values; we just don't bind any of
                        // them). At recursion levels, the
                        // previous UnpackAt exposed N
                        // values; we POP them so the slot
                        // cursor advances correctly for
                        // subsequent fields.
                        bytecode.push_pop();
                    }
                    // else: `consume_values = false` —
                    // the test chain already consumed the
                    // value. Skip silently.
                }
            }
        },
    }
}

fn unwrap_expr_output<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
    match expr.1.as_ref() {
        Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Statement(inner)
        | Expression::ExprStatement(inner) => unwrap_expr_output(inner),
        // Parenthesized conditions often parse as a one-element Fragment.
        Expression::Fragment(items) if items.len() == 1 => unwrap_expr_output(&items[0]),
        _ => expr,
    }
}

/// `COIL_AUTO_PAR=0` disables automatic fork-join of pure recursive binops.
fn auto_par_enabled() -> bool {
    match std::env::var("COIL_AUTO_PAR") {
        Ok(v) if matches!(v.as_str(), "0" | "false" | "off" | "no") => false,
        _ => true,
    }
}

fn unwrapped_identifier<'a>(expr: &'a Output<'a>) -> Option<&'a str> {
    match unwrap_expr_output(expr).1.as_ref() {
        Expression::Identifier(name) => Some(name),
        _ => None,
    }
}

/// Extract enum name from `Ty::Con` / `Ty::Sum` / nested `Ty::Constructor`.
fn extract_enum_name(ty: &crate::typechecking::ty::Ty) -> Option<String> {
    use crate::typechecking::ty::Ty;
    match ty {
        Ty::Con(name) => Some(name.clone()),
        Ty::Sum { name, .. } => Some(name.clone()),
        Ty::Constructor { owner, .. } => extract_enum_name(owner),
        _ => None,
    }
}

mod compiler;
mod inline_cost;
