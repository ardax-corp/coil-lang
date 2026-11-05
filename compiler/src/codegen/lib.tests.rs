
use super::*;
use parser::Pratt;

/// A standard host native's real id, as `Pipeline` registers it (direct
/// MIR types its host edges by id).
fn host_id(name: &str) -> usize {
    common::host_native_ids().find(|&(n, _)| n == name).map(|(_, id)| id).unwrap_or_else(|| panic!("no host native `{name}`"))
}

/// A conditional branch, plain or fused.
fn is_cond_branch(b: &Byte) -> bool {
    use common::Instruction as I;
    matches!(
        b.bytecode(),
        I::JMPT
            | I::JMPF
            | I::JumpIfMatch
            | I::CmpJmpf
            | I::CmpJmpt
            | I::BinSlotImmJmpf
            | I::BinSlotImmJmpt
            | I::LogNotJmpf
            | I::LogNotJmpt
            | I::BinSlotSlotJmpf
            | I::BinSlotSlotJmpt
            | I::BinSlotSlotConstJmpf
            | I::BinSlotSlotConstJmpt
            | I::DenseBinJmpf
            | I::DenseIndexJmpf
    )
}

/// A heap make of `kind` (`dense::MAKE_ARRAY` / `MAKE_TUPLE` /
/// `MAKE_ENUM`, any tag), on the stack or the dense tier.
fn is_make(b: &Byte, kind: u8) -> bool {
    use common::{Instruction as I, dense};
    match b.bytecode() {
        I::MakeArray | I::MakeArrayK => kind == dense::MAKE_ARRAY,
        I::MakeTuple | I::MakeTupleK => kind == dense::MAKE_TUPLE,
        I::MakeEnum | I::MakeEnumK => kind == dense::MAKE_ENUM,
        I::DenseMake | I::DenseMakeK => {
            let k = (b.operand_u32() >> 24) as u8;
            if kind == dense::MAKE_ENUM { k >= kind } else { k == kind }
        }
        _ => false,
    }
}

/// The `CodePtr` feeding the make at `pc`: right before it on the stack
/// tier, or stored to its slot just before it on the dense tier.
fn code_ptr_before(bc: &[Byte], pc: usize) -> Option<&Byte> {
    use common::Instruction as I;
    bc[..pc].iter().rev().take(2).find(|b| matches!(b.bytecode(), I::CodePtr))
}

#[test]
fn codegen_depth_guard_panics_with_expected_diagnostic_past_limit() {
    // See the analogous typechecker test (infer_depth_guard_...) for why
    // this seeds the counter directly rather than compiling a literally
    // deep AST: dropping a deep Box<Expression> chain overflows the
    // stack on its own, independent of do_compile's frame size.
    let ast = Pratt::default()
        .parse("1;")
        .expect("trivial literal parses");
    let mut compiler = Compiler {
        codegen_depth: super::CODEGEN_RECURSION_LIMIT,
        ..Default::default()
    };
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compiler.do_compile(&ast)));
    assert!(result.is_err(), "expected the recursion-limit panic");
    assert!(
        compiler
            .messages
            .iter()
            .any(|m| m.code() == Some(ErrorCode::ExpressionNestingTooDeep)),
        "expected an ExpressionNestingTooDeep diagnostic to be recorded before panicking"
    );
}

/// True when bytecode contains a strength-reduced `x << shift`
/// (`LOAD; CONST; SHL` or fused `BinSlotImm(SHL, shift)`).
fn bytecode_has_shl_by(bc: &[Byte], shift: i64) -> bool {
    use common::Instruction;
    let has_load_const_shl = bc.windows(3).any(|w| {
        matches!(w[0].bytecode(), Instruction::LOAD)
            && matches!(w[1].bytecode(), Instruction::CONST)
            && matches!(w[2].bytecode(), Instruction::SHL)
            && (w[1].operand_u32() & Byte::POOL_FLAG) == 0
            && w[1].operand_u32() as i32 == shift as i32
    });
    let has_fused_shl = bc.iter().any(|b| {
        *b.bytecode() == Instruction::BinSlotImm
            && b.bin_slot_imm_parts().0 == Instruction::SHL as u8
            && b.bin_slot_imm_parts().2 == shift
    });
    has_load_const_shl || has_fused_shl
}

/// A two-word `CALL` whose tag a jump tests: next to it on emit_hir's stack
/// IL, or a store / load later (stack IL from MIR keeps `[payload, tag]` in
/// slots and fuses the test).
fn two_slot_call_followed_by_tag_jump(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.iter().enumerate().any(|(i, b)| {
        matches!(b.bytecode(), Instruction::CALL)
            && b.call_ret_words() == 2
            && bc[i + 1..].iter().take(4).any(|j| {
                matches!(
                    j.bytecode(),
                    Instruction::JMPF | Instruction::JMPT | Instruction::BinSlotImmJmpt | Instruction::BinSlotImmJmpf
                )
            })
    })
}

fn bytecode_has_any_shl(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.iter().any(|b| {
        matches!(b.bytecode(), Instruction::SHL)
            || (*b.bytecode() == Instruction::BinSlotImm
                && b.bin_slot_imm_parts().0 == Instruction::SHL as u8)
    })
}

fn bytecode_has_shr_by(bc: &[Byte], shift: i64) -> bool {
    use common::Instruction;
    let has_load_const_shr = bc.windows(3).any(|w| {
        matches!(w[0].bytecode(), Instruction::LOAD)
            && matches!(w[1].bytecode(), Instruction::CONST)
            && matches!(w[2].bytecode(), Instruction::SHR)
            && (w[1].operand_u32() & Byte::POOL_FLAG) == 0
            && w[1].operand_u32() as i32 == shift as i32
    });
    let has_fused_shr = bc.iter().any(|b| {
        *b.bytecode() == Instruction::BinSlotImm
            && b.bin_slot_imm_parts().0 == Instruction::SHR as u8
            && b.bin_slot_imm_parts().2 == shift
    });
    has_load_const_shr || has_fused_shr
}

fn bytecode_has_any_shr(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.iter().any(|b| {
        matches!(b.bytecode(), Instruction::SHR)
            || (*b.bytecode() == Instruction::BinSlotImm
                && b.bin_slot_imm_parts().0 == Instruction::SHR as u8)
    })
}

fn bytecode_has_dup_pop_barrier(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.windows(2).any(|w| {
        matches!(w[0].bytecode(), Instruction::DUPLICATE)
            && matches!(w[1].bytecode(), Instruction::POP)
    })
}

/// `x ** 0` folds to const 1.

fn bytecode_has_bitand(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.iter().any(|b| {
        matches!(b.bytecode(), Instruction::BITAND)
            || (*b.bytecode() == Instruction::BinSlotImm
                && b.bin_slot_imm_parts().0 == Instruction::BITAND as u8)
            || (*b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::BITAND as u8)
    })
}

fn bytecode_has_bitor(bc: &[Byte]) -> bool {
    use common::Instruction;
    bc.iter().any(|b| {
        matches!(b.bytecode(), Instruction::BITOR)
            || (*b.bytecode() == Instruction::BinSlotImm
                && b.bin_slot_imm_parts().0 == Instruction::BITOR as u8)
            || (*b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::BITOR as u8)
    })
}

/// The HM checker should record diagnostic messages on type errors;
/// `compile` should drain them into the compiler's message list.
#[test]
fn type_errors_appear_in_messages() {
    let mut ast = Pratt::default().parse("x;").expect("parse failed");
    let mut c = Compiler::default();
    c.compile("test", &mut ast);
    assert!(
        !c.messages.is_empty(),
        "expected at least one error message for unknown identifier"
    );
}

/// `register_native` adds the native to the HM checker. A subsequent
/// call to the native type-checks cleanly.
#[test]
fn register_native_visible_to_emitter() {
    use crate::typechecking::ty::{string, unit};
    let mut c = Compiler::default();
    c.register("native_print", &[string()], &unit());
    // Native calls registered with the checker should compile without errors.
    let mut ast = Pratt::default()
        .parse("fn main() { native_print(\"hi\"); }")
        .expect("parse failed");
    let _bc = c.compile("test", &mut ast);
    let msgs = std::mem::take(&mut c.messages);
    assert!(msgs.is_empty(), "expected no messages, got: {:?}", msgs);
}

#[test]
fn check_program_impl_calls_later_helper_via_compiler_checker() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { panic "bump"; }
}
"#;
    let ast = Pratt::default().parse(src).expect("parse");
    let mut compiler = Compiler::default();
    let _ = compiler.checker.check_program(&ast);
    assert!(
        compiler.checker.messages().is_empty(),
        "unexpected: {:?}",
        compiler
            .checker
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn compiler_compile_impl_calls_later_helper() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { panic "bump"; }
}
"#;
    let mut ast = Pratt::default().parse(src).expect("parse");
    let mut compiler = Compiler::default();
    let _ = compiler.compile("", &mut ast);
    assert!(
        compiler.get_messages().is_empty(),
        "unexpected: {:?}",
        compiler
            .get_messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

// sum types and pattern matching codegen

// VM perf: peephole superinstruction fusion

/// Count loop exit branches (unfused `JMPF` or fused `CmpJmpf` / `BinSlotImmJmpf`).
fn loop_exit_branch_count(bc: &[common::Byte]) -> usize {
    use common::Instruction;
    bc.iter()
        .filter(|b| {
            matches!(
                b.bytecode(),
                Instruction::JMPF
                    | Instruction::CmpJmpf
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::LogNotJmpf
            )
        })
        .count()
}

/// A loop's back edge: `JMP`, or the `*Jmpt` of an inverted latch (#1867).
fn is_back_edge_op(op: common::Instruction) -> bool {
    use common::Instruction;
    matches!(
        op,
        Instruction::JMP
            | Instruction::JMPT
            | Instruction::CmpJmpt
            | Instruction::BinSlotImmJmpt
            | Instruction::BinSlotSlotJmpt
            | Instruction::BinSlotSlotConstJmpt
            | Instruction::LogNotJmpt
    )
}

fn loop_exit_target(bc: &[common::Byte], pool: &[u64]) -> Option<usize> {
    use common::Instruction;
    for b in bc {
        match b.bytecode() {
            Instruction::JMPF => return Some(b.operand_u32() as usize),
            Instruction::CmpJmpf => {
                let (_, t) = b.cmp_jmpf_parts();
                return if b.cmp_jmpf_is_pool() {
                    pool.get(t).map(|p| *p as usize)
                } else {
                    Some(t)
                };
            }
            Instruction::BinSlotImmJmpf => {
                let pool_idx = b.bin_slot_imm_jmpf_parts().2;
                return pool.get(pool_idx).map(|p| (*p >> 32) as usize);
            }
            Instruction::BinSlotSlotJmpf => {
                let pool_idx = b.bin_slot_slot_jmpf_parts().2;
                return pool.get(pool_idx).map(|p| (*p >> 32) as usize);
            }
            Instruction::LogNotJmpf => {
                let t = b.log_not_jmpf_target();
                return if b.log_not_jmpf_is_pool() {
                    pool.get(t).map(|p| *p as usize)
                } else {
                    Some(t)
                };
            }
            _ => {}
        }
    }
    None
}

#[test]
fn break_and_continue_outside_loop_emit_diagnostics() {
    let mut ast = Pratt::default()
        .parse("fn main() { break; continue; }")
        .expect("parse failed");
    let mut compiler = Compiler::default();
    compiler.compile("", &mut ast);
    let rendered = compiler
        .get_messages()
        .iter()
        .map(|m| format!("{:?}", m))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compiler
            .get_messages()
            .iter()
            .any(|m| m.code() == Some(ErrorCode::CodegenError)
                && m.message().contains("break outside of loop")),
        "expected CodegenError for break, got {rendered}"
    );
    assert!(
        compiler
            .get_messages()
            .iter()
            .any(|m| m.code() == Some(ErrorCode::CodegenError)
                && m.message().contains("continue outside of loop")),
        "expected CodegenError for continue, got {rendered}"
    );
}

// field-access codegen tests
//
// The spec locked in 2 codegen tests for the new
// `Expression::Access` arm. Both verify the bytecode SHAPE
// (MakeEnum → LoadField) and the operand (field_index) so the
// runtime extraction reads the right slot.

fn count_calls(bc: &[Byte]) -> usize {
    use common::Instruction;
    bc.iter()
        .filter(|b| matches!(b.bytecode(), Instruction::CALL))
        .count()
}

#[test]
fn cross_module_does_not_inline_private_method() {
    let util = "class Box { pub n: int, }\n\
             impl Box {\n\
               fn secret() -> int { return 2; }\n\
               pub fn shown() -> int { return 1; }\n\
             }";
    let mut ast = Pratt::default().parse(util).expect("parse util");
    let mut compiler = Compiler::default();
    let _ = compiler.compile_module("util", &mut ast);
    let secret = compiler
        .checker()
        .inherent_method_visibility("util::Box::secret")
        .or_else(|| compiler.checker().inherent_method_visibility("Box::secret"));
    let shown = compiler
        .checker()
        .inherent_method_visibility("util::Box::shown")
        .or_else(|| compiler.checker().inherent_method_visibility("Box::shown"));
    assert_eq!(secret, Some(parser::ast::Visibility::Private));
    assert_eq!(shown, Some(parser::ast::Visibility::Public));
}

// growing array builtin codegen tests

// Dims over the `u8` packed ceiling fall back to scalar unroll.

fn packed_host_meta(bc: &[common::Byte]) -> u32 {
    use common::Instruction;
    // Layout: CONST packed id, the operands, CONST meta, HostInvoke.
    let ids = u32::from(common::PACKED_DOT_ID)..=u32::from(common::PACKED_VEC_ARITH_ID);
    let packed_id = |b: &common::Byte| matches!(b.bytecode(), Instruction::CONST) && ids.contains(&b.operand_u32());
    let id_at = bc.iter().position(packed_id).expect("expected a packed native id");
    let hi = id_at
        + bc[id_at..]
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::HostInvoke))
            .expect("expected HostInvoke");
    assert!(
        matches!(bc[hi - 1].bytecode(), Instruction::CONST),
        "HostInvoke must follow meta CONST: {:?}",
        bc.iter().map(|b| format!("{:?} {:#x}", b.bytecode(), b.operand_u32())).collect::<Vec<_>>()
    );
    bc[hi - 1].operand_u32()
}

#[test]
fn unescape_coil_string_supports_hex_and_unicode() {
    assert_eq!(unescape_coil_string(r"\x41"), "A");
    assert_eq!(unescape_coil_string(r"\u{42}"), "B");
    assert_eq!(unescape_coil_string("\\\""), "\"");
    assert_eq!(unescape_coil_string(r"\e"), "\x1b");
}

/// A stack (`HostInvoke`) or register (`DenseHostCall`) host call.
fn is_host_call(b: &Byte) -> bool {
    matches!(b.bytecode(), Instruction::HostInvoke | Instruction::DenseHostCall)
}

/// The enum layout a host call asks the native for (`HOST_ENUM_LAYOUT_*`).
fn host_call_layout(b: &Byte) -> u32 {
    if matches!(b.bytecode(), Instruction::DenseHostCall) {
        (b.operand_u32() >> 16) & 0x3
    } else {
        common::host_invoke_enum_layout(b.operand_u32())
    }
}

fn host_niche_immediately_unwraps(bc: &[Byte]) -> bool {
    bc.windows(2).any(|w| {
        matches!(w[0].bytecode(), Instruction::HostInvoke)
            && common::host_invoke_enum_layout(w[0].operand_u32())
                == common::HOST_ENUM_LAYOUT_OPTION_NICHE
            && matches!(w[1].bytecode(), Instruction::JumpIfMatch)
    })
}

/// True for a plain `BITAND`, or a `CmpJmpf`/`CmpJmpt` fused superinstruction
/// whose comparison op is `BITAND` ,  `branch_opt`/fuse-select may fold the
/// niche-Result Err-bit test (`DUP; CONST 1; BITAND; JMPF`) into one op
/// once a `DUPLICATE` keeps the pointer alive under the jump.
fn tests_err_bit(b: &Byte) -> bool {
    let bitand = Instruction::BITAND as u8;
    match b.bytecode() {
        Instruction::BITAND => true,
        Instruction::CmpJmpf | Instruction::CmpJmpt => b.cmp_jmpf_parts().0 == bitand,
        Instruction::BinSlotImm => b.bin_slot_imm_parts().0 == bitand,
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => b.bin_slot_imm_jmpf_parts().0 == bitand,
        _ => false,
    }
}

fn call_count(bc: &[Byte]) -> usize {
    bc.iter().filter(|b| matches!(b.bytecode(), Instruction::CALL)).count()
}

const HIR_INLINE_METHODS: &str = r#"
class Point {
    pub x: int,
    pub y: int,
}
impl Point {
    pub fn sum() -> int {
        return self.x + self.y;
    }
}
fn scale(int a, int k) -> int {
    let t = a * k;
    return t + 1;
}
fn hot() -> int {
    let p = new Point(3, 4);
    let acc = 0;
    let i = 0;
    while i < 10 {
        acc = acc + p.sum() + scale(i, 2);
        i = i + 1;
    }
    return acc;
}
fn main() {
    return hot();
}
"#;

const HIR_INLINE_GUARDS: &str = r#"
fn clamp(int x, int lo, int hi) -> int {
    if x < lo {
        return lo;
    }
    if x > hi {
        return hi;
    }
    return x;
}

fn sign(int x) -> int {
    if x < 0 {
        return -1;
    } else if x == 0 {
        return 0;
    } else {
        return 1;
    }
}

fn countdown(int n) -> int {
    if n == 0 {
        return 0;
    }
    return countdown(n - 1);
}

fn hot(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + clamp(i, 2, 7) + sign(i) + countdown(3);
        i = i + 1;
    }
    return acc;
}

fn main() {
    let x = hot(10);
}
"#;

const HIR_INLINE_INSTANCES: &str = r#"
class Point {
    pub x: int,
    pub y: int,
}
trait Measure<S> {
    fn size(S s, int k) -> int {}

    fn twice(S s, int k) -> int {
        return s.size(k) * 2;
    }
}
impl Measure for int {
    pub fn size(int s, int k) -> int {
        let t = s * k;
        return t + 1;
    }
}
impl Measure for Point {
    pub fn size(Point p, int k) -> int {
        let t = p.x * k;
        return t + p.y;
    }
}
fn hot() -> int {
    let p = new Point(3, 4);
    let acc = 0;
    let i = 0;
    while i < 10 {
        acc = acc + i.size(3) + p.size(i) + i.twice(2);
        i = i + 1;
    }
    return acc;
}
fn main() {
    return hot();
}
"#;

const HIR_INLINE_HEAP: &str = r#"
class Node {
    pub v: int,
    pub next: Option<Node>,
}
fn pair_sum(int a, int b) -> int {
    let m = new Node(b, Option::None);
    let n = new Node(a, Option::Some(m));
    return n.v + m.v;
}
fn hot(int k) -> int {
    let acc = 0;
    let i = 0;
    while i < k {
        acc = acc + pair_sum(i, 10);
        i = i + 1;
    }
    return acc;
}
fn main() {
    return hot(100);
}
"#;

/// A generic class's method clones are kept with their calls resolved
/// and their types qualified (cross-module inlining): a body that only
/// calls, reads fields and indexes is kept; one holding a lambda is not.
#[test]
fn method_clones_keep_resolved_bodies() {
    use crate::hir::HirKind;
    let src = r#"
class Bag<T> {
    pub items: Vec<T>,
}

impl Bag<T> {
    pub fn add(T x) {
        self.items.push(x);
    }
    pub fn at_or(int i, T d) -> T {
        if i < self.items.len() {
            return self.items[i];
        }
        return d;
    }
    pub fn count_if(T -> bool keep) -> int {
        let n = 0;
        for x in self.items {
            if keep(x) {
                n = n + 1;
            }
        }
        return n;
    }
}

fn main() {
    let b: Bag<int> = new Bag(Vec::new());
    b.add(3);
    b.add(4);
    if b.at_or(1, 0) != 4 || b.at_or(9, -1) != -1 {
        panic "at_or";
    }
    if b.count_if(fn (int x) => x > 3) != 1 {
        panic "count_if";
    }
}
"#;
    let mut pipeline = crate::Pipeline::new();
    // Same-module calls would splice the generic bodies and leave no clone.
    pipeline.set_inline_max_cost(0);
    if pipeline.compile_src(src).is_err() {
        panic!("compile failed: {:?}", pipeline.messages().iter().map(|m| m.message()).collect::<Vec<_>>());
    }
    let kept = &pipeline.compiler().hir_resolved_clones;
    let key = |m: &str| kept.keys().find(|k| k.starts_with(&format!("Bag::{m}$"))).cloned();
    let add = key("add").unwrap_or_else(|| panic!("Bag::add not kept: {:?}", kept.keys().collect::<Vec<_>>()));
    let at_or = key("at_or").expect("Bag::at_or kept");
    let resolved = |k: &str| kept[k].exprs.iter().filter(|e| matches!(e.kind, HirKind::Call { callee: crate::hir::Callee::Resolved(_), .. })).count();
    assert!(resolved(&add) >= 1, "Bag::add keeps its push call resolved");
    for k in [&add, &at_or] {
        let b = &kept[k];
        assert!(b.exprs.iter().all(|e| e.node.is_none()), "{k} keeps node ids");
        assert!(
            b.exprs.iter().all(|e| !matches!(e.kind, HirKind::Call { callee: crate::hir::Callee::Named { .. } | crate::hir::Callee::Method { .. }, .. })
                || matches!(&e.kind, HirKind::Call { callee: crate::hir::Callee::Method { name }, .. } if name == "len")),
            "{k} keeps an unresolved call"
        );
    }
    assert!(key("count_if").is_none(), "a body calling a function value is not kept");
}

/// A callee body well over the inline cost limit (100): 80 `let`s.
fn over_limit_body() -> String {
    let mut body = String::from("{ let v0 = x + 1; ");
    for k in 1..80 {
        body.push_str(&format!("let v{k} = v{} % 1000 + {k}; ", k - 1));
    }
    body.push_str("return v79; }");
    body
}

/// A body of `n` dependent `let`s over `x`, returning the last.
fn lets_body(n: usize) -> String {
    let mut body = String::from("{ let v0 = x + 1; ");
    for k in 1..n {
        body.push_str(&format!("let v{k} = v{} % 1000 + {k}; ", k - 1));
    }
    body.push_str(&format!("return v{}; }}", n - 1));
    body
}

/// A loop in `main` calling `big`, tagged `tag`, with a body of `n` lets.
fn hot_fixture(tag: &str, n: usize) -> String {
    format!(
        "{tag} fn big(int x) -> int {} \
         fn main() -> int {{ let s = 0; let i = 0; while i < 50 {{ s = (s + big(i)) % 1000; i = i + 1; }} return s; }}",
        lets_body(n)
    )
}

/// A compile under `--clone-budget 1`.
fn budget_one(c: &mut Compiler) {
    let mut grants = crate::HostGrants::deny_all();
    grants.clone_budget = Some(1);
    c.set_host_grants(grants, Vec::new());
}

/// A callee whose failing arm builds a panic message: the arm is cut out
/// into a cold helper and the rest is spliced into the caller.
fn partial_fixture(arm: &str) -> String {
    format!(
        "fn at(int x, int n) -> int {{ if x >= n {{ {arm} }} return x * 3 + 1; }} \
         fn main() -> int {{ let s = 0; let i = 0; while i < 50 {{ s = (s + at(i, 64)) % 1000; i = i + 1; }} return s; }}"
    )
}
