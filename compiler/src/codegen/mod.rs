use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
};

use common::{
    Byte, DEBUG_FILE_UNKNOWN, DebugLoc, FnDebugSym, Instruction, Interner, Value, ValueTag,
    encode_tag_operand, likely, tag, unlikely,
};
use reporting::Label as DiagLabel;

use crate::block_builder::{BlockBuilder, JumpKind as BbJumpKind, Label as BbLabel};
use crate::const_fold::ConstValue;
use crate::il::{CodeBuf, EmitBuf, EntryKind, FuseHint, IlJumpKind, IlOp, Label as IlLabel};
use crate::monomorphize::{MonoKey, MonoPlan};
use crate::typechecking::{Checker, Ty};
use parser::{
    SimpleSpan,
    ast::{Expression, MatchArm, Output, Pattern, PatternPayload},
};
use reporting::{ErrorCode, Message};

/// Max native recursion depth for [`compiler::Compiler::do_compile`]. Chosen
/// well under what a debug-build stack of a few MiB can hold even with
/// `do_compile`'s current per-call frame size, see
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


/// Arms grouped by outer variant tag for dispatch and inner-pattern tests.
#[derive(Debug, Clone)]
struct TagGroup {
    tag: u32,
    /// Payload words the VM pushes when `tag` matches (IL tell / MIR edges).
    arity: u32,
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
/// Returns `None` for unknown shapes, callers must not invent `INT` and
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
                    for ch in chars.by_ref() {
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        hex.push(ch);
                    }
                    if closed
                        && let Ok(code) = u32::from_str_radix(&hex, 16)
                            && let Some(ch) = char::from_u32(code) {
                                out.push(ch);
                                continue;
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

pub(crate) fn primitive_cast_opcode(from: &str, to: &str) -> Option<Instruction> {
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

fn group_arms_by_outer_tag(arms: &[&MatchArm], checker: &Checker) -> Vec<TagGroup> {
    let mut groups: Vec<TagGroup> = Vec::new();
    let mut tag_to_idx: HashMap<u32, usize> = HashMap::new();
    for (i, arm) in arms.iter().enumerate() {
        let (tag, arity) = match &arm.pattern.1 {
            Pattern::Constructor {
                enum_name,
                variant_name,
                ..
            } => (
                checker.tag_for(enum_name, variant_name).unwrap_or(u32::MAX),
                checker.arity_for(enum_name, variant_name).unwrap_or(0) as u32,
            ),
            _ => (u32::MAX, 0),
        };
        if let Some(&idx) = tag_to_idx.get(&tag) {
            groups[idx].arm_indices.push(i);
        } else {
            tag_to_idx.insert(tag, groups.len());
            groups.push(TagGroup {
                tag,
                arity,
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

/// Collect `name → Ty` for every binding in a match pattern.
                        ///
/// Used so Access codegen (`p.y`) sees the *current arm's* binding type
/// rather than whatever last arm wrote into the flat
/// `codegen_var_types` side-table (same name reused across arms with
/// different payload types would otherwise emit the wrong `LoadField`).
                        ///
/// Open schema placeholders (`Ty::Var`, or `Ty::Con("T")` type-param
/// markers from poly enums like `Option` / `Result` / `Box<T>`) are
/// **not** inserted, they would shadow the instantiated binding type
/// that `infer_pattern` already wrote into `codegen_var_types`.
fn collect_pattern_binding_types(
    checker: &Checker,
    pattern: &Pattern<'_>,
    out: &mut HashMap<String, Ty>,
) {
    match pattern {
        Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
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
/// A declared payload type that mentions the enum's own type parameters
/// anywhere (`T`, and nested `Tree<T>` / `(T, int)`), so it does not describe
/// a particular instantiation: the binding's use sites keep the checker's type.
fn is_open_schema_ty(checker: &Checker, enum_name: &str, ty: &Ty) -> bool {
    let open = |t: &Ty| is_open_schema_ty(checker, enum_name, t);
    match ty {
        Ty::Var(_) => true,
        Ty::Con(name) => checker
            .generics()
            .generic_type_ctors
            .get(enum_name)
            .is_some_and(|params| params.iter().any(|p| p == name)),
        Ty::App(head, args) => open(head) || args.iter().any(open),
        Ty::Fun(a, b) => open(a) || open(b),
        Ty::Tuple(items) => items.iter().any(open),
        Ty::List(inner) | Ty::Readonly(inner) => open(inner),
        Ty::Array { element, .. } => open(element),
        Ty::Record { fields } => fields.iter().any(|(_, t)| open(t)),
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
        Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
        Pattern::Binding { name } => {
            if let Some(ty) = expected
                && !is_open_schema_ty(checker, enum_name, ty) {
                    out.insert(name.to_string(), ty.clone());
                }
        }
        Pattern::Constructor { .. } => {
            collect_pattern_binding_types(checker, pattern, out);
        }
    }
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
/// larger values must not reach here, partial-application arity is already
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
    /// First whole-object escape of a stack array in this frame → box slot (Q1).
    stack_array_box: HashMap<String, u32>,

    /// Frame-local / two-slot ObjEnum: name → (payload_slot, tag_slot, enum_name).
    unboxed_enum_locals: HashMap<String, (u32, u32, String)>,
    /// Named `new C` field-SROA: name → (base_slot, nfields, class_name).
    unboxed_class_locals: HashMap<String, (u32, usize, String)>,
    /// First identity use of an unboxed class → heap slot (Q2 box-once).
    unboxed_class_box: HashMap<String, u32>,

    prev: Option<Box<Self>>,
}


/// Speculative call-site emit: buffer prefix plus Q1/Q2 box-once caches.
struct EmitAttempt {
    bytecode: Option<CodeBuf>,
    stack_array_box: HashMap<String, u32>,
    unboxed_class_box: HashMap<String, u32>,
}

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

/// How one expression should represent the enum value it builds or loads,
/// when its consumer needs something other than the value's own layout.
///
/// Scoped to a single node: [`Compiler::do_compile`] hands the requested
/// context to the node it compiles (`repr_here`) and resets it for that
/// node's operands, so a call argument, payload, or operand never inherits
/// its parent's request. Only value-forwarding nodes pass it on: `Group` /
/// `Expr` / one-item `Fragment`, a block's tail, and a match's arm bodies
/// (not its scrutinee).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ReprCtx {
    /// Force ground pointer-niche `Option` expressions back to heap enums
    /// while using legacy pattern lowering or an unknown boundary.
    pub(super) force_heap_option: bool,
    /// Force a contextually typed `Option::None` / `Option::Some` onto the
    /// pointer-niche path when its constructor node has no standalone type.
    pub(super) force_niche_option: bool,
    /// Force heap-heap `Result` expressions back to `ObjEnum` for boxed match.
    pub(super) force_heap_result: bool,
    /// Force `Result::Ok` / `Result::Err` onto the pointer-niche path when
    /// the constructor node has no standalone type.
    pub(super) force_niche_result: bool,
    /// When > 0, frame-local ObjEnum construct/load emits `[payload, tag]`
    /// in locals / on the stack instead of `MakeEnum`, and a two-word
    /// `CALL` leaves its pair unboxed.
    pub(super) unbox_enum_context: u32,
}

impl ReprCtx {
    fn union(self, other: Self) -> Self {
        Self {
            force_heap_option: self.force_heap_option || other.force_heap_option,
            force_niche_option: self.force_niche_option || other.force_niche_option,
            force_heap_result: self.force_heap_result || other.force_heap_result,
            force_niche_result: self.force_niche_result || other.force_niche_result,
            unbox_enum_context: self.unbox_enum_context.max(other.unbox_enum_context),
        }
    }

    pub(super) fn unboxing(self) -> bool {
        self.unbox_enum_context > 0
    }
}

/// Target-side layouts at a trait-method call across a generic boundary;
/// see [`Compiler::trait_method_boundary_sig`].
pub(super) struct BoundarySig {
    /// Per parameter: the layout the target reads, when it can differ.
    pub(super) params: Vec<Option<crate::typechecking::value_layout::ValueLayout>>,
    /// The layout the target returns, when it can differ.
    pub(super) ret: Option<crate::typechecking::value_layout::ValueLayout>,
}

pub struct Compiler {
    namespace: String,
    /// Stack IL during emit; lowered `Vec<Byte>` after [`Self::finalize_bytecode`].
    bytecode: CodeBuf,

    aliases: HashMap<String, String>,
    functions: HashMap<String, usize>,
    /// Entry labels for names in [`Self::functions`] (step-3 binds).
    fn_entry_labels: HashMap<String, IlLabel>,
    /// `Show` thunks of the builtin error enums: reserved up front, emitted
    /// after the module only when a call used them (their variant names
    /// would otherwise land in every archive's string table).
    builtin_show_thunks: Vec<(String, &'static str, &'static [&'static str])>,
    builtin_show_used: HashSet<String>,
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
    /// Type checker. Run once per `compile` via
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

    /// Memoized two-word-return verdicts (enum name or boxed), keyed by the
    /// name codegen looks a function up by. The type env fills in as bodies
    /// are compiled, so an unmemoized query can answer differently for a
    /// body and for a later caller, see [`Compiler::two_word_return_kind`].
    pair_return_kinds: std::cell::RefCell<HashMap<String, Option<String>>>,

    /// Whole-program names used as function *values* (`Some` after the
    /// pipeline seeds every AST in the compile). `None` means "sidecar
    /// only" (single-module `compile` / tests). Fail closed: a name in
    /// this set never widens to a two-slot RETURN.
    fn_value_escaped_program: Option<HashSet<String>>,

    /// Counter for compiler-generated temporary slots.
    temp_counter: u32,

    /// Function-local cache of field-name string keys used ≥2 times
    /// (`STRING` materialized once at entry, then `LOAD`).
    field_key_slots: HashMap<String, u32>,

    /// Frame slots with a live `ArrayPin` (sidecar-proven helpers / for-in).
    pinned_array_slots: HashSet<u32>,

    /// Count of expression values currently live on the operand stack
    /// *above* interned locals (e.g. a `HostInvoke` native-id `CONST`
    /// pushed before argument codegen). `alloc_temp_slot` must allocate
    /// at or above `variables.len() + expr_depth` so `StorePop` does not
    /// clobber those live values (locals and the operand stack share
    /// memory).
    expr_depth: u32,

    /// First escapes (Q1/Q2 box-once) seen while compiling the current block
    /// statement. `Some` inside a statement; the block re-emits it with the
    /// boxes hoisted to its start so box slots never land on live operands.
    escape_hoist: Option<Vec<String>>,
    /// Nesting of [`Compiler::compile_block_stmt`] frames: a statement in a
    /// loop body or `if` arm is one deeper than the loop / `if` itself.
    stmt_depth: u32,
    /// Statement depth of the `let` that bound each local (block-scoped).
    /// A first escape deeper than its local's `let` must box at the `let`'s
    /// depth: boxing inside a loop body re-boxes stale slots every pass, and
    /// inside an `if` arm only one path boxes.
    escape_decl_depth: HashMap<String, u32>,

    /// Top-level functions whose frames can never hold a heap word
    /// ([`Compiler::fn_is_heap_free`]); finalize binds them to precise maps.
    precise_frame_fns: HashSet<String>,
    precise_frames: Vec<common::PreciseFrameMap>,

    /// Native call-stack depth of [`Compiler::do_compile`]'s recursion,
    /// guarded against a fixed limit, see the analogous `infer_depth` on
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

    /// Name of the function currently being codegen'd (for ctor/Instantiate routing).
    active_fn_name: Option<String>,

    /// Global static initializers (spliced at `program_start_offset`). Same sink as `ffi_init`.
    static_init: CodeBuf,

    /// `extern` dlopen/declare setup accumulated across modules, spliced into
    /// the prologue setup region at finalize (so imported-module `extern`
    /// still runs before `main`).
    ffi_init: CodeBuf,

    /// True while compiling an `impl` method. Function resets locals
    /// and reserves slot 0 for `self`.
    compiling_method: bool,
    /// True while lowering a monomorphized clone. Bound-method hints still
    /// point at `__dictN`; the clone has ground types and must CALL instead.
    compiling_mono_clone: bool,

    /// True while compiling a function whose return type is inferred
    /// as `Result<T, E>` via `raise` / `?` (wrap bare `return` in `Ok`).
    compiling_result_mode: bool,
    /// When result-mode Ok payload is itself `Result`, keep Ok-wrapping
    /// explicit `return Result::Ok(…)` (nested Result payload case).
    compiling_result_ok_is_result: bool,

    /// Enum representation requested for the expression being compiled;
    /// see [`ReprCtx`]. Raise it right before compiling that expression.
    repr: ReprCtx,
    /// The [`ReprCtx`] the node now inside [`Compiler::do_compile`] was
    /// entered with. Its operands see [`ReprCtx::default`] instead.
    repr_here: ReprCtx,
    /// Per enclosing `match`: the [`ReprCtx`] its arm bodies compile under.
    arm_repr: Vec<ReprCtx>,
    /// Spans of generic-call arguments whose parameter is not a bare type
    /// parameter: the shared body does not unbox them, so they are not boxed.
    generic_arg_no_box: HashSet<(usize, usize)>,
    /// Pending generic-boundary layout conversions for call arguments, keyed
    /// by argument span: `(from, to)` applied right after the argument is
    /// compiled (see [`Compiler::generic_enum_layout`]).
    boundary_arg_convs: HashMap<
        (usize, usize),
        (
            crate::typechecking::value_layout::ValueLayout,
            crate::typechecking::value_layout::ValueLayout,
        ),
    >,

    /// Kind when the function whose body is being compiled uses the
    /// two-slot `CALL`/`RETURN` ABI (`[payload, tag]` or product `[a, b]`).
    /// `None` while boxed / niche / unbounded `T`. See
    /// [`Compiler::two_word_return_kind`].
    compiling_two_word_enum: Option<String>,
    /// Shared two-slot `?` miss label + reconstructed fail tag, bound after
    /// the function body so invert/convoy cannot swallow later call args.
    compiling_try_fail: Option<(BbLabel, i32)>,

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
    /// Coverage compiles: functions whose body lies in a source file this
    /// accepts are tree-shake roots, so never-called code is still emitted
    /// (and reported uncovered) instead of dropped.
    keep_fns_in: Option<crate::KeepFnFilter>,
    /// Function name → index into `source_file_list` of the file it was
    /// compiled from (tracked only while `keep_fns_in` is set).
    fn_source_files: HashMap<String, u32>,

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
    /// Entry name of each mono clone: calls bind through its entry label
    /// (a raw offset goes stale when code moves before it).
    mono_names: HashMap<MonoKey, String>,
    /// Temporary variable-type overrides while emitting a specialized clone.
    mono_codegen_var_types: Vec<HashMap<String, Ty>>,
    /// Type parameter name → concrete type of the mono clone being
    /// compiled (`T::from_val(v)` selects `T`'s instance directly).
    mono_type_param_tys: Vec<HashMap<String, Ty>>,
    /// Set while compiling a bounded generic instance's method: the index of
    /// the first context dictionary in `__dict0` and how many there are.
    /// The method prologue unpacks them into `__dict1..` (#551).
    pending_instance_ctx: Option<(usize, usize)>,
    /// Checker type variable → concrete type, per mono clone being compiled
    /// (the source function's type parameters), for open dictionary goals.
    mono_var_tys: Vec<HashMap<crate::typechecking::ty::TyVarId, Ty>>,

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
    /// `dissect --il-post` snapshot from the last capturing finalize.
    #[cfg(any(test, feature = "dissect"))]
    post_il_snapshot: Option<crate::dissect::IlSnapshot>,
    /// Debug variables per function (see [`crate::debug_vars`]).
    fn_debug_vars: HashMap<String, Vec<crate::debug_vars::DebugVar>>,
    /// Source end offset of each enclosing scope (block / function body).
    debug_scope_ends: Vec<u32>,
    /// Source start of the statement being compiled (a `let`'s scope start).
    debug_stmt_start: u32,
    /// `(address, len)` of the current module's source text: AST names are
    /// slices of it, so a name's byte offset is pointer arithmetic.
    source_base: (usize, usize),
    /// A copy of that text: HIR names are owned, so their spans are found
    /// in it.
    source_text: String,

    /// When true, [`Expression::Match`] binds `end` as a plain label instead of
    /// a value-join (`JoinLabel`). Set while compiling a match whose value is
    /// consumed immediately by `StorePop` / `StoreStatic` (e.g. `let x = match …`).
    suppress_match_fusion_barrier: bool,

    /// Set by a statement `match` (`ExprStatement(Match)`) for the next
    /// [`Expression::Match`] compiled; taken at its entry so the scrutinee
    /// and nested matches never see it.
    statement_match_pending: bool,
    /// Per match being compiled (innermost last): whether each arm discards
    /// its own value. Arm bodies of a statement match need not push a value
    /// (`B => {}`), so a single POP after the match would pop a local.
    arm_discard: Vec<bool>,

    /// User `fn` names that sit on a call-graph cycle (self or mutual).
    recursive_fns: HashSet<String>,
    /// Self-recursive pure function names eligible for auto fork-join.
    recursive_pure: HashSet<String>,
    /// Side-effect-free user `fn` names (loop bounds / COI-99).
    pure_fns: HashSet<String>,
    /// Detected independent-parallel-arm fork sites for pure fns.
    par_shapes: HashMap<String, crate::typechecking::ParForkSite>,
    /// Functions that emit one parameterized `__coil_par_*` fork worker.
    par_workers: HashSet<String>,
    /// Counted loops whose iterations are independent arms, by loop span.
    loop_par_sites: crate::typechecking::LoopParSites,
    /// Chunk workers emitted so far, for `__coil_par_loop_*` naming.
    loop_par_helpers: usize,
    /// When false, skip auto fork-join even if `COIL_AUTO_PAR` is on.
    auto_par: bool,

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
    /// S2b interpreter maps (empty when no alloc body lifted).
    stack_maps: Vec<common::FrameStackMap>,
    /// Drafts before PC bind (tests / diagnostics).
    stack_map_drafts: Vec<crate::mir::DraftFrameMap>,
    deopt_map_drafts: Vec<crate::mir::DraftDeoptMap>,

    /// Lower function bodies from HIR instead of the AST walk (the default;
    /// `COIL_HIR=0` keeps the AST walk).
    hir_lowering: bool,
    /// HIR of the module being compiled, when [`Self::hir_lowering`] is on.
    hir_module: Option<crate::hir::HirModule>,
    /// Function body index in [`Self::hir_module`] by declaration span.
    hir_fns: HashMap<(usize, usize), usize>,
    /// Typed inlining on HIR bodies (`COIL_HIR_INLINE=1`).
    hir_inline: bool,
    /// Function and method body index in [`Self::hir_module`] by name
    /// (`None` when two bodies share it).
    hir_fn_names: HashMap<String, Option<usize>>,
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
            builtin_show_thunks: Vec::new(),
            builtin_show_used: HashSet::new(),
            fn_arities: HashMap::with_capacity(32),
            module_items: std::collections::HashMap::default(),
            native: HashMap::default(),
            extern_runtime_libs: HashMap::with_capacity(4),
            extern_runtime_functions: HashMap::with_capacity(16),
            extern_runtime_libs_loaded: HashSet::new(),
            messages: Vec::default(),
            context: Context::default(),
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
            pinned_array_slots: HashSet::new(),
            expr_depth: 0,
            escape_hoist: None,
            stmt_depth: 0,
            escape_decl_depth: HashMap::new(),
            precise_frame_fns: HashSet::new(),
            precise_frames: Vec::new(),
            codegen_depth: 0,
            loop_stack: Vec::new(),
            loop_bbs: Vec::new(),
            fn_defers: Vec::new(),
            active_fn_name: None,
            compiling_method: false,
            compiling_mono_clone: false,
            compiling_result_mode: false,
            compiling_result_ok_is_result: false,
            repr: ReprCtx::default(),
            repr_here: ReprCtx::default(),
            arm_repr: Vec::new(),
            generic_arg_no_box: HashSet::new(),
            boundary_arg_convs: HashMap::new(),
            compiling_two_word_enum: None,
            compiling_try_fail: None,
            pair_return_kinds: std::cell::RefCell::new(HashMap::new()),
            fn_value_escaped_program: None,
            test_cases: Vec::new(),
            user_main_defined: false,
            include_tests: false,
            keep_fns_in: None,
            fn_source_files: HashMap::new(),
            polyfn_vars: HashSet::new(),
            polyfn_sources: HashMap::new(),
            mono_plan: MonoPlan::default(),
            mono_offsets: HashMap::new(),
            mono_names: HashMap::new(),
            mono_codegen_var_types: Vec::new(),
            mono_type_param_tys: Vec::new(),
            pending_instance_ctx: None,
            mono_var_tys: Vec::new(),
            static_init: CodeBuf::new(),
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
            #[cfg(any(test, feature = "dissect"))]
            post_il_snapshot: None,
            fn_debug_vars: HashMap::new(),
            debug_scope_ends: Vec::new(),
            debug_stmt_start: 0,
            source_base: (0, 0),
            source_text: String::new(),
            suppress_match_fusion_barrier: false,
            statement_match_pending: false,
            arm_discard: Vec::new(),
            match_tail_call: false,
            recursive_fns: HashSet::new(),
            recursive_pure: HashSet::new(),
            pure_fns: HashSet::new(),
            par_shapes: HashMap::new(),
            par_workers: HashSet::new(),
            loop_par_sites: crate::typechecking::LoopParSites::new(),
            loop_par_helpers: 0,
            auto_par: true,
            operand_stack_slots: crate::typechecking::DEFAULT_OPERAND_STACK_SLOTS,
            opt_options: crate::il::opt::OptimizeOptions::default(),
            inline_cost: inline_cost::InlineCostOptions::default(),
            retain_cursor_il: false,
            cursor_il: None,
            stack_maps: Vec::new(),
            stack_map_drafts: Vec::new(),
            deopt_map_drafts: Vec::new(),
            hir_lowering: crate::hir::lowering_from_env(),
            hir_module: None,
            hir_fns: HashMap::new(),
            hir_inline: crate::hir::inline::inline_from_env(),
            hir_fn_names: HashMap::new(),
        }
    }
}

impl Context {
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
            stack_array_box: self.stack_array_box.clone(),
            unboxed_enum_locals: self.unboxed_enum_locals.clone(),
            unboxed_class_locals: self.unboxed_class_locals.clone(),
            unboxed_class_box: self.unboxed_class_box.clone(),
            prev: Some(Box::new(self.to_owned())),
        }
    }
}

impl Context {
    pub fn get_prev(&self) -> &Option<Box<Self>> {
        &self.prev
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
    !matches!(
        std::env::var("COIL_AUTO_PAR"),
        Ok(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
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
mod emit_loop;
mod inline_cost;
mod precise_frames;
pub(crate) use precise_frames::jump_target as jump_target_of;
