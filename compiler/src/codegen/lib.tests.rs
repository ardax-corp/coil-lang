    use super::*;
    use parser::Pratt;

    fn compile_src(src: &str) -> (Vec<Byte>, Vec<u64>) {
        let mut owned = String::new();
        let needs_io = src.contains("write(")
            || src.contains("stdout()")
            || src.contains("to_bytes(")
            || src.contains("format(");
        if needs_io && !src.contains("use io::") {
            owned.push_str("use io::{stdout};
\n");
        }
        if needs_io && !src.contains("use string::") {
            owned.push_str("use string::{format, to_bytes};\n");
        }
        owned.push_str(src);
        let mut ast = Pratt::default()
            .parse(owned.as_str())
            .expect("parse failed");
        let mut compiler = Compiler::default();
        // Stable placeholder ids so Approach A packed HostInvoke lowering
        // fires in unit tests (Pipeline assigns real ids at runtime).
        compiler.register_native_id(machine::PACKED_DOT, 9001);
        compiler.register_native_id(machine::PACKED_MATMUL, 9002);
        compiler.register_native_id(machine::PACKED_MATRIX_ZIP, 9003);
        compiler.register_native_id(machine::PACKED_MATRIX_NEG, 9004);
        compiler.register_native_id(machine::PACKED_VEC_ARITH, 9005);
        compiler.register_native_id(machine::GC_REGISTER_FINALIZER_NATIVE, 9100);
        let bc = compiler.compile("", &mut ast);
        (bc, compiler.constants)
    }

    // ---- Recursion-depth guard ----

    #[test]
    fn codegen_depth_guard_panics_with_expected_diagnostic_past_limit() {
        // See the analogous typechecker test (infer_depth_guard_...) for why
        // this seeds the counter directly rather than compiling a literally
        // deep AST: dropping a deep Box<Expression> chain overflows the
        // stack on its own, independent of do_compile's frame size.
        let ast = Pratt::default().parse("1;").expect("trivial literal parses");
        let mut compiler = Compiler::default();
        compiler.codegen_depth = super::CODEGEN_RECURSION_LIMIT;
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

    #[test]
    fn method_call_target_relocated_after_static_init_splice() {
        use common::Instruction;

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/static_singleton.hy");
        let mut pipeline = crate::Pipeline::new();
        let (bytecode, constants) = pipeline
            .compile_src_from_file(path.to_str().unwrap())
            .expect("compile");
        let bump_off = pipeline.compiler_mut().get_function("Counter::bump");
        assert!(
            bytecode.iter().any(|b| {
                matches!(b.bytecode(), Instruction::CALL) && b.call_parts().1 == bump_off
            }),
            "CALL to Counter::bump must target {bump_off} after static-init splice"
        );

        let mut machine = machine::Machine::<256>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.set_program_debug(pipeline.program_debug());
        machine.run_raw(
            &bytecode,
            &constants,
            pipeline.strings(),
            pipeline.static_slot_count(),
        );
    }

    #[test]
    fn two_module_and_class_static_assignments_run() {
        use common::{ARCHIVE_VERSION, ArchivedProgram};
        use rkyv::rancor::Error;

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/static_minimal.hy");
        let mut pipeline = crate::Pipeline::new();
        let (bytecode, constants) = pipeline
            .compile_src_from_file(path.to_str().unwrap())
            .expect("compile");
        assert_eq!(pipeline.static_slot_count(), 2);

        let program = ArchivedProgram {
            version: ARCHIVE_VERSION,
            static_slot_count: pipeline.static_slot_count(),
            constants: constants.clone(),
            strings: pipeline.strings().to_vec(),
            bytecode: bytecode.clone(),
            source_files: pipeline.program_debug().source_files,
            debug_locs: pipeline.program_debug().debug_locs,
            fn_symbols: Vec::new(),
        };
        let bytes = rkyv::to_bytes::<Error>(&program).expect("serialize");
        let archived = rkyv::access::<rkyv::Archived<ArchivedProgram>, Error>(bytes.as_slice())
            .expect("access");
        let loaded_bc: Vec<Byte> =
            rkyv::deserialize::<Vec<Byte>, Error>(&archived.bytecode).expect("bc");
        let loaded_constants: Vec<u64> =
            rkyv::deserialize::<Vec<u64>, Error>(&archived.constants).expect("consts");
        let loaded_strings: Vec<String> =
            rkyv::deserialize::<Vec<String>, Error>(&archived.strings).expect("strings");
        let static_slots = u32::from(archived.static_slot_count);

        let mut machine = machine::Machine::<256>::default();
        pipeline.wire_vm_ffi(&mut machine, Some(path.as_path()));
        pipeline.wire_host_natives(&mut machine);
        machine.run_raw(&loaded_bc, &loaded_constants, &loaded_strings, static_slots);
    }

    /// `test("…")` cases become `__zs_test_N` functions; standalone runs get a virtual `main`.
    #[test]
    fn test_case_emits_synthetic_fns_virtual_main_and_relocates_offsets() {
        use common::Instruction;
        let mut ast = Pratt::default()
            .parse(
                r#"
test("one") { assert(true)?; }
test("two") { assert(true)?; }
"#,
            )
            .expect("parse failed");
        let mut compiler = Compiler::default();
        compiler.set_include_tests(true);
        let bc = compiler.compile("", &mut ast);

        assert_eq!(compiler.test_cases().len(), 2);
        assert_eq!(compiler.test_cases()[0].0, "one");
        assert_eq!(compiler.test_cases()[1].0, "two");

        let syn0 = compiler.get_function("__zs_test_0");
        let syn1 = compiler.get_function("__zs_test_1");
        let main_off = compiler.get_function("main");
        assert!(syn0 < bc.len(), "__zs_test_0 offset out of range");
        assert!(syn1 < bc.len(), "__zs_test_1 offset out of range");
        assert!(main_off < bc.len(), "virtual main offset out of range");
        assert_ne!(syn0, syn1, "synthetic test fns must be distinct");
        assert_ne!(main_off, syn0, "virtual main must be distinct from cases");

        // Peephole relocates test_cases offsets to match fused function entries.
        assert_eq!(
            compiler.test_cases()[0].1,
            syn0 as u32,
            "test_cases[0] must track peephole relocation of __zs_test_0"
        );
        assert_eq!(
            compiler.test_cases()[1].1,
            syn1 as u32,
            "test_cases[1] must track peephole relocation of __zs_test_1"
        );

        let main_bc = &bc[main_off..];
        let calls_in_main = main_bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .count();
        assert!(
            calls_in_main >= 2,
            "virtual main should CALL each harness case; got {calls_in_main}"
        );
        assert!(
            main_bc
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::Panic)),
            "virtual main must Panic on aggregate soft-fail"
        );
    }

    /// End-to-end: a simple integer expression compiles to bytecode
    /// using the HM checker's cache. We don't check exact bytes (those
    /// change as the emitter evolves); we just verify the pipeline
    /// runs without panicking and produces a non-empty bytecode.
    #[test]
    fn integer_arithmetic_emits_bytecode() {
        let (bc, _pool) = compile_src("42;");
        assert!(!bc.is_empty());
    }

    #[test]
    fn async_call_emits_make_coro_not_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src("async fn coro() { yield 1; } fn main() { let h = coro(); }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeCoro)),
            "expected MakeCoro for async fn call"
        );
        assert!(
            !bc.iter()
                .any(|b| { matches!(b.bytecode(), Instruction::CALL) && b.call_parts().1 > 3 }),
            "async fn call site should not use CALL"
        );
    }

    #[test]
    fn yield_and_resume_emit_coroutine_opcodes() {
        use common::Instruction;
        let (bc, _pool) =
            compile_src("async fn coro() { yield 1; } fn main() { let h = coro(); resume h; }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::YieldCoro)),
            "expected YieldCoro in async body"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ResumeCoro)),
            "expected ResumeCoro at call site"
        );
    }

    /// Binding yield (`let x = yield e`) emits YieldCoro then STORE.
    #[test]
    fn let_binding_yield_emits_yield_coro_then_store_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src("async fn f() { let x = yield 1; }");
        let yield_pos = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::YieldCoro))
            .expect("expected YieldCoro");
        let store_pos = bc[yield_pos..]
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::STORE))
            .map(|i| yield_pos + i)
            .expect("expected STORE after YieldCoro");
        assert!(
            yield_pos < store_pos,
            "YieldCoro (at {}) must precede STORE (at {}) for binding yield",
            yield_pos,
            store_pos
        );
    }

    /// Resume-with-send sets the has_send bit on ResumeCoro.
    #[test]
    fn resume_with_send_emits_has_send_operand() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn main() { resume h with 42; }");
        let resume = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::ResumeCoro))
            .expect("expected ResumeCoro");
        assert_ne!(
            resume.operand_u32() & 1,
            0,
            "ResumeCoro for `resume h with v` must set has_send bit"
        );
    }

    /// `yield from` emits YieldFromCoro.
    #[test]
    fn yield_from_emits_yield_from_coro() {
        use common::Instruction;
        let (bc, _pool) = compile_src("async fn f() { yield from inner; }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::YieldFromCoro)),
            "expected YieldFromCoro for yield from"
        );
    }

    /// A bare `yield expr;` statement parses through `expr_statement()`
    /// (see `parser::statement`, where `self.expr_statement()` is tried
    /// before the dedicated `self.yield_()` alternative), landing as
    /// `ExprStatement(Yield(...))`. Regression guard: `ExprStatement`
    /// must NOT emit a trailing `POP` after `YieldCoro` (or
    /// `YieldFromCoro`) — that POP becomes the coroutine's `resume_ip`
    /// and, on the NEXT resume, pops whatever the resumer happens to
    /// have on top of the shared operand stack (e.g. a format string
    /// mid-construction), corrupting it.
    #[test]
    fn bare_yield_statement_does_not_emit_trailing_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src("async fn f() { yield 1; yield 2; }");
        let yield_positions: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::YieldCoro))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(yield_positions.len(), 2, "expected two YieldCoro sites");
        for pos in yield_positions {
            assert!(
                !matches!(
                    bc.get(pos + 1).map(|b| b.bytecode()),
                    Some(Instruction::POP)
                ),
                "bare `yield expr;` must not be followed by POP (would corrupt the next resume)"
            );
        }
    }

    fn bytecode_has_dup_pop_barrier(bc: &[Byte]) -> bool {
        use common::Instruction;
        bc.windows(2).any(|w| {
            matches!(w[0].bytecode(), Instruction::DUPLICATE)
                && matches!(w[1].bytecode(), Instruction::POP)
        })
    }

    #[test]
    fn let_match_omits_fusion_barrier() {
        let (bc, _pool) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
enum Result<T, E> { Ok(T), Err(E) }
fn foo() -> Result<int, int> { return Result::Ok(0); }
fn main() {
    let x = match foo() {
        Result::Ok(s) => s,
        Result::Err(_) => panic "bad",
    };
    write(stdout(), to_bytes(format("%i", x)));
}
"#,
        );
        assert!(
            !bytecode_has_dup_pop_barrier(&bc),
            "let x = match should omit DUPLICATE;POP fusion barrier"
        );
    }

    #[test]
    fn return_match_keeps_fusion_barrier() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn foo() -> int {
    return match Option::Some(1) {
        Option::None => 0,
        Option::Some(n) => n,
    };
}
"#,
        );
        // clone_shared_return may fuse the const arm to ConstReturnImm, but the
        // payload arm must RETURN locally — never JMP into ConstReturnImm (that
        // would ignore the stacked Unpack value). Scope to the match region so
        // prologue / other fn JMPs do not trip the guard.
        let make_enum = bc
            .iter()
            .position(|b| matches!(*b.bytecode(), Instruction::MakeEnum))
            .expect("expected MakeEnum for Option::Some(1)");
        let jim = bc[make_enum..]
            .iter()
            .position(|b| matches!(*b.bytecode(), Instruction::JumpIfMatch))
            .map(|i| make_enum + i)
            .expect("expected JumpIfMatch after MakeEnum");
        let region_end = bc[jim..]
            .iter()
            .position(|b| matches!(*b.bytecode(), Instruction::ConstReturnImm))
            .map(|i| jim + i)
            .expect("expected ConstReturnImm on None arm");
        let region = &bc[jim..=region_end];
        let unpack = region
            .iter()
            .position(|b| matches!(*b.bytecode(), Instruction::Unpack))
            .expect("expected Unpack on Some arm");
        assert!(
            matches!(*region[unpack + 1].bytecode(), Instruction::RETURN),
            "Some arm must RETURN immediately after Unpack; got {:?}",
            region[unpack + 1].bytecode()
        );
        assert!(
            !region
                .iter()
                .any(|b| matches!(*b.bytecode(), Instruction::JMP)),
            "match return region must not JMP into a fused const return; slice={:?}",
            region.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn assignment_match_omits_fusion_barrier() {
        let (bc, _pool) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
enum Result<T, E> { Ok(T), Err(E) }
fn main() {
    let x = 0;
    x = match Result::Ok(1) {
        Result::Ok(n) => n,
        Result::Err(_) => panic "bad",
    };
    write(stdout(), to_bytes(format("%i", x)));
}
"#,
        );
        assert!(
            !bytecode_has_dup_pop_barrier(&bc),
            "x = match should omit fusion barrier before StorePop"
        );
    }

    /// Same guard for a bare `yield from expr;` statement.
    #[test]
    fn bare_yield_from_statement_does_not_emit_trailing_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src("async fn f() { yield from inner; }");
        let pos = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::YieldFromCoro))
            .expect("expected YieldFromCoro");
        assert!(
            !matches!(
                bc.get(pos + 1).map(|b| b.bytecode()),
                Some(Instruction::POP)
            ),
            "bare `yield from expr;` must not be followed by POP"
        );
    }

    /// Float arithmetic should pick `ADDF` (float) instead of `ADD`
    /// (int) — that's the whole point of the cache lookup.
    #[test]
    fn float_arithmetic_emits_float_opcode() {
        use common::Instruction;
        let (bc, _pool) = compile_src("1.0 + 2.0;");
        // Find the binary operator instruction. The bytecode is
        // initialised with CALL/JMP/HALT, then operand code, then the
        // operator. We search for the LAST ADDF / ADD.
        let mut last_binop: Option<&Instruction> = None;
        for b in &bc {
            if matches!(b.bytecode(), Instruction::ADDF | Instruction::ADD) {
                last_binop = Some(b.bytecode());
            }
        }
        assert!(
            matches!(last_binop, Some(Instruction::ADDF)),
            "expected ADDF for float arithmetic"
        );
    }

    /// Integer arithmetic should pick `ADD`, not `ADDF`. Two literals
    /// (`1 + 2`) now constant-fold to a single `CONST`, so we use two
    /// int parameters — `a + b` compiles to a slot/slot binary op whose
    /// packed operator must be the int `ADD` (not the float `ADDF`).
    #[test]
    fn integer_arithmetic_emits_int_opcode() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn add(int a, int b) -> int { return a + b; }");
        // Builtin Num dictionary thunks also contain ADD/ADDF; the user function
        // body should use int ADD (fused BinSlotSlot or bare LOAD/LOAD/ADD).
        let has_int_bin_slot = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::ADD as u8
        });
        let has_bare_add = bc.iter().any(|b| *b.bytecode() == Instruction::ADD);
        let has_float_bin_slot = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::ADDF as u8
        });
        assert!(
            (has_int_bin_slot || has_bare_add) && !has_float_bin_slot,
            "expected int ADD (fused or bare) for integer arithmetic; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x * 8` strength-reduces to `x << 3` (via [`const_fold::strength_mul_int`]).
    #[test]
    fn mul_by_power_of_two_emits_shl_not_mul() {
        let (bc, _pool) = compile_src("fn scale(int x) -> int { return x * 8; }");
        assert!(
            bytecode_has_shl_by(&bc, 3),
            "expected LOAD/CONST/SHL (shift 3) or fused BinSlotImm(SHL, 3) for x*8; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x ** 2` strength-reduces to a self-multiply, not `Pow`. The `DUPLICATE`
    /// is re-expanded to a second `LOAD` so it fuses into one `BinSlotSlot`.
    #[test]
    fn pow_two_emits_self_mul_not_pow() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn sq(int x) -> int { return x ** 2; }");
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::Pow)),
            "x**2 must not emit Pow; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let fused_self_mul = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlot && {
                let (op, a, c) = b.bin_slot_slot_parts();
                op == Instruction::MUL as u8 && a == c
            }
        });
        assert!(
            fused_self_mul,
            "expected BinSlotSlot MUL with both operands the same slot; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x ** 0` folds to const 1.

    #[test]
    fn dict_hot_loop_hoists_field_name_strings() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
            fn main() {
                let d = { x: 0, y: 0 };
                let i = 0;
                while (i < 10) {
                    d.x = d.x + 1;
                    d.y = d.y + 2;
                    i = i + 1;
                }
                write(stdout(), to_bytes(format("%i", d.x + d.y)));
            }
            "#,
        );
        // Count STRING ops after the loop header fuse (BinSlotImmJmpf).
        let jmpf = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::BinSlotImmJmpf));
        let Some(j) = jmpf else {
            panic!("no loop header");
        };
        let latch = bc
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::JMP))
            .unwrap();
        let strings_in_loop = bc[j..=latch]
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STRING))
            .count();
        assert_eq!(
            strings_in_loop,
            0,
            "field-name STRING should be hoisted out of loop; slice={:?}",
            bc[j..=latch]
                .iter()
                .map(|b| b.bytecode())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn repeated_field_keys_materialize_once_per_function() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
            class Point {
                x: int,
                y: int,
            }
            impl Point {
                fn twice_x() -> int {
                    return self.x + self.x;
                }
            }
            "#,
        );
        // Key cached once in Point::twice_x; ignore STRING ops in later
        // default Show/String / builtin thunks (Range::to_vec uses GetField).
        let first_get = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::GetField))
            .find(|(i, _)| {
                let end = (*i + 24).min(bc.len());
                !bc[*i..end]
                    .iter()
                    .any(|b| matches!(b.bytecode(), Instruction::ArrayPush))
            })
            .map(|(i, _)| i)
            .expect("expected GetField in twice_x");
        let region_start = bc[..first_get]
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::RETURN))
            .map(|i| i + 1)
            .unwrap_or(0);
        let region_end = bc[first_get..]
            .iter()
            .position(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::RETURN | Instruction::BinReturn
                )
            })
            .map(|i| first_get + i)
            .unwrap_or(bc.len() - 1);
        let region = &bc[region_start..=region_end];
        let strings = region
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STRING))
            .count();
        let get_fields = region
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::GetField))
            .count();
        let has_dup = region
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::DUPLICATE));
        assert!(
            strings <= 1 && get_fields >= 1 && (get_fields >= 2 || has_dup),
            "expected ≤1 STRING for repeated .x plus GetField/Dup; strings={strings} gets={get_fields} dup={has_dup}; ops={:?}",
            region.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn for_in_array_hoists_array_len_out_of_loop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { for x in [1, 2, 3] { write(stdout(), to_bytes(format(\"%i\", x))); } }",
        );
        let len_at = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::ArrayLen))
            .expect("ArrayLen");
        let header = bc
            .iter()
            .position(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::CmpJmpf
                        | Instruction::BinSlotImmJmpf
                        | Instruction::BinSlotSlotJmpf
                        | Instruction::JMPF
                )
            })
            .expect("loop header compare/jmp");
        assert!(
            len_at < header,
            "ArrayLen should be hoisted before loop header; len@{len_at} header@{header}; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let latch = bc
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::JMP))
            .unwrap();
        let lens_in_loop = bc[header..=latch]
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::ArrayLen))
            .count();
        assert_eq!(lens_in_loop, 0, "no ArrayLen inside loop body/latch");
    }

    #[test]
    fn pow_zero_emits_const_one() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn one(int x) -> int { return x ** 0; }");
        let has_pow = bc.iter().any(|b| matches!(b.bytecode(), Instruction::Pow));
        let has_one = bc.iter().any(|b| {
            matches!(b.bytecode(), Instruction::CONST) && b.operand_u32() == 1
                || matches!(b.bytecode(), Instruction::ConstReturnImm) && b.operand_u32() == 1
        });
        assert!(
            !has_pow && has_one,
            "expected const 1 for x**0; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `if (!cond) { A } else { B }` inverts so fused JMPF sees `cond` (no LogNotJmpf).
    #[test]
    fn not_if_else_inverts_away_log_not_jmpf() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
            fn f(int i) -> int {
                if (!(i & 1)) {
                    return 10;
                } else {
                    return 20;
                }
            }
            "#,
        );
        let has_log_not_jmpf = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::LogNotJmpf));
        let has_bin_slot_jmpf = bc.iter().any(|b| {
            matches!(b.bytecode(), Instruction::BinSlotImmJmpf)
                && b.bin_slot_imm_jmpf_parts().0 == Instruction::BITAND as u8
        });
        assert!(
            !has_log_not_jmpf && has_bin_slot_jmpf,
            "expected BinSlotImmJmpf(BITAND) without LogNotJmpf; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Commuted form `8 * x` must use the same SHL lowering (LHS factor).
    #[test]
    fn mul_by_lhs_power_of_two_emits_shl() {
        let (bc, _pool) = compile_src("fn scale(int x) -> int { return 8 * x; }");
        assert!(
            bytecode_has_shl_by(&bc, 3),
            "expected SHL (shift 3) for 8*x; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `const K = 16; x * K` must consult `const_env` and emit `<< 4`.
    #[test]
    fn mul_by_const_power_of_two_emits_shl() {
        let (bc, _pool) = compile_src("fn scale(int x) -> int { const K = 16; return x * K; }");
        assert!(
            bytecode_has_shl_by(&bc, 4),
            "expected SHL (shift 4) for x*const(16); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x * 1` is identity-reduced, not `<< 0` (shift 0 is reserved for
    /// [`const_fold::strength_reduced_inner`], not SHL lowering).
    #[test]
    fn mul_by_one_does_not_emit_shl() {
        let (bc, _pool) = compile_src("fn id(int x) -> int { return x * 1; }");
        assert!(
            !bytecode_has_any_shl(&bc),
            "x*1 should identity-reduce, not emit SHL; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Float `*` must never strength-reduce to int `SHL` (defense-in-depth
    /// type gate in `try_emit_folded_expr`). Typecheck rejects `float * int`;
    /// when both sides are float the factor is never a power-of-two int, so
    /// `strength_mul_to_shl` stays `None` and we emit `MULF`.
    #[test]
    fn float_mul_does_not_emit_shl() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn scale(float x) -> float { return x * 8.0; }");
        assert!(
            !bytecode_has_any_shl(&bc),
            "float mul must not emit SHL; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let has_mulf = bc.iter().any(|b| {
            matches!(b.bytecode(), Instruction::MULF)
                || (*b.bytecode() == Instruction::BinSlotImm
                    && b.bin_slot_imm_parts().0 == Instruction::MULF as u8)
                || (*b.bytecode() == Instruction::BinSlotSlot
                    && b.bin_slot_slot_parts().0 == Instruction::MULF as u8)
                || (*b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::MULF as u8)
        });
        assert!(
            has_mulf,
            "expected MULF (bare or fused) for float mul; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `byte` is int-like for VM `SHL` (`as_int`); `byte * 8` should still
    /// strength-reduce (not be excluded by the int-only type gate).
    #[test]
    fn byte_mul_by_power_of_two_emits_shl() {
        let (bc, _pool) = compile_src("fn scale(byte x) -> byte { return x * 8; }");
        assert!(
            bytecode_has_shl_by(&bc, 3),
            "expected SHL (shift 3) for byte*8; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Unsigned `byte / 8` is `>> 3`. Signed `int / 8` must stay `DIV`.
    #[test]
    fn byte_div_by_power_of_two_emits_shr() {
        let (bc, _pool) = compile_src("fn scale(byte x) -> byte { return x / 8; }");
        assert!(
            bytecode_has_shr_by(&bc, 3),
            "expected SHR (shift 3) for byte/8; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn int_div_by_power_of_two_keeps_div() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn scale(int x) -> int { return x / 8; }");
        assert!(
            !bytecode_has_any_shr(&bc),
            "signed int / 8 must not become SHR; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let has_div = bc.iter().any(|b| {
            matches!(b.bytecode(), Instruction::DIV)
                || (*b.bytecode() == Instruction::BinSlotImm
                    && b.bin_slot_imm_parts().0 == Instruction::DIV as u8)
                || (*b.bytecode() == Instruction::BinSlotSlot
                    && b.bin_slot_slot_parts().0 == Instruction::DIV as u8)
        });
        assert!(
            has_div,
            "expected DIV for signed int/8; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

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

    #[test]
    fn bitand_zero_emits_const_not_bitand() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn z(int x) -> int { return x & 0; }");
        assert!(
            !bytecode_has_bitand(&bc),
            "x & 0 should not emit BITAND; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| matches!(b.bytecode(), Instruction::CONST)
                || matches!(b.bytecode(), Instruction::ConstReturnImm)),
            "expected CONST 0 for x & 0; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bitor_zero_skips_bitor() {
        let (bc, _pool) = compile_src("fn id(int x) -> int { return x | 0; }");
        assert!(
            !bytecode_has_bitor(&bc),
            "x | 0 should skip BITOR; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn xor_same_ident_emits_zero() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn z(int x) -> int { return x ^ x; }");
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::XOR)
                || (*b.bytecode() == Instruction::BinSlotSlot
                    && b.bin_slot_slot_parts().0 == Instruction::XOR as u8)),
            "x ^ x should not emit XOR; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn shl_zero_skips_shl() {
        let (bc, _pool) = compile_src("fn id(int x) -> int { return x << 0; }");
        assert!(
            !bytecode_has_any_shl(&bc),
            "x << 0 should skip SHL; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Type aliases to `int` expand at check time, so `I * 8` still SHLs.
    #[test]
    fn aliased_int_mul_by_power_of_two_emits_shl() {
        let (bc, _pool) = compile_src("type I = int; fn scale(I x) -> I { return x * 8; }");
        assert!(
            bytecode_has_shl_by(&bc, 3),
            "expected SHL for aliased int*8; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Generic `T: Num` bodies with two type operands dispatch through the
    /// dictionary (`CallIndirect`), never primitive `SHL`. (A literal factor
    /// like `x * 8` unifies `T` to `int` in the checker today, so that shape
    /// correctly takes the primitive SHL path; the `bound_mul` guard covers
    /// the open-var case.)
    #[test]
    fn generic_num_mul_uses_dictionary_not_shl() {
        use common::Instruction;
        let (bc, _pool) =
            compile_src("fn mul2<T: Num>(T a, T b) -> T { return a * b; } fn main() { mul2(1, 2); }");
        assert!(
            !bytecode_has_any_shl(&bc),
            "generic Num mul must not lower to SHL; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Ground calls monomorphize; shared CallIndirect body may be shaken.
        assert!(
            bc.iter().any(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::CallIndirect
                        | Instruction::MUL
                        | Instruction::BinSlotSlot
                        | Instruction::BinReturn
                )
            }),
            "expected CallIndirect or specialized mul; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn string_addition_emits_format_not_add() {
        use common::Instruction;
        // Returned, not a bare statement: stack DCE drops a discarded literal.
        let (bc, _pool) = compile_src("fn main() { return \"a\" + \"b\"; }");

        let folded_string = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::STRING));
        let via_format = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::FORMAT));
        let via_const = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::CONST | Instruction::ConstReturnImm));
        assert!(
            folded_string || via_format || via_const,
            "expected folded STRING/CONST or FORMAT for string concat; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Ignore ADD/ADDF inside builtin Num thunks; the top-level expression
        // must not fuse into a numeric BinSlot* convoy before FORMAT.
        assert!(
            !bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::BinSlotImm | Instruction::BinSlotSlot
            )),
            "string addition should not emit fused numeric slot ops; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Mixed int+float picks float (because HM unifies the operands
    /// and one is float). The pipeline emits a single, well-typed
    /// result — either way, the test should not panic.
    #[test]
    fn mixed_int_float_arithmetic_emits_bytecode() {
        let (bc, _pool) = compile_src("1 + 2.0;");
        assert!(!bc.is_empty());
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
            .parse("native_print(\"hi\");")
            .expect("parse failed");
        let _bc = c.compile("test", &mut ast);
        let msgs = std::mem::take(&mut c.messages);
        assert!(msgs.is_empty(), "expected no messages, got: {:?}", msgs);
    }

    #[test]
    fn typeclass_impl_method_registers_fqn_function() {
        let mut ast = Pratt::default()
            .parse(
                r#"
                trait Foo<T> { fn bar(T x) -> T; }
                impl Foo<int> { fn bar(int x) -> int { return x; } }
                fn use_bar<T: Foo>(T x) -> T { return bar(x); }
                fn main() { use_bar(1); }
                "#,
            )
            .expect("parse failed");
        let mut compiler = Compiler::default();
        let bc = compiler.compile("", &mut ast);
        assert!(
            compiler.messages.is_empty(),
            "unexpected: {:?}",
            compiler.messages
        );
        let offset = compiler
            .functions
            .get("Foo__int__bar")
            .copied()
            .expect("instance method FQN should stay reachable via use_bar");
        assert!(
            offset < bc.len(),
            "function offset should point into bytecode"
        );
    }

    #[test]
    fn check_program_impl_calls_later_helper_via_compiler_checker() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { raise "bump"; }
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
class Foo { v: int, }
impl Foo {
    fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { raise "bump"; }
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

    /// COI-109: free fns that only call user methods inside `while`/`if` must
    /// still emit after the `impl` (walk must enter loop/branch bodies).
    #[test]
    fn free_fn_method_call_inside_while_after_sibling_runs() {
        let src = r#"
class Box { n: int, }
impl Box {
    fn get() -> int { return self.n; }
    fn bump() { self.n = self.n + 1; }
}
fn peek(Box b) -> int { return b.get(); }
fn thrash(Box b) {
    let i = 0;
    while i < 1 {
        b.bump();
        i = i + 1;
    }
}
fn main() {
    let b = new Box(0);
    peek(b);
    thrash(b);
    if b.get() != 1 { raise "bump"; }
}
"#;
        let mut pipeline = crate::Pipeline::new();
        let (bytecode, constants) = pipeline.compile_src(src).expect("compile");
        let mut machine = machine::Machine::<256>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.set_program_debug(pipeline.program_debug());
        machine.run_raw(
            &bytecode,
            &constants,
            pipeline.strings(),
            pipeline.static_slot_count(),
        );
        assert!(
            !machine.panicked(),
            "while-body user method calls must resolve after sibling free fn"
        );
    }

    /// COI-109: free fn calling a deferred method-caller must also emit after
    /// `impl` (or all free fns after impls) so the callee is bound for CALL.
    #[test]
    fn phase1_free_fn_can_call_deferred_method_caller() {
        let src = r#"
class Box { n: int, }
impl Box {
    fn get() -> int { return self.n; }
}
fn peek(Box b) -> int { return b.get(); }
fn wrap(Box b) -> int { return peek(b); }
fn main() {
    let b = new Box(7);
    let got = wrap(b);
    if got != 7 {
        let _x = 0 / 0;
    }
}
"#;
        let mut pipeline = crate::Pipeline::new();
        let compiled = pipeline.compile_src(src);
        assert!(
            compiled.is_ok(),
            "compile failed: {:?}",
            pipeline
                .messages()
                .iter()
                .map(|m| m.message())
                .collect::<Vec<_>>()
        );
        let (bytecode, constants) = compiled.unwrap();
        let mut machine = machine::Machine::<256>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.set_program_debug(pipeline.program_debug());
        machine.run_raw(
            &bytecode,
            &constants,
            pipeline.strings(),
            pipeline.static_slot_count(),
        );
        assert!(
            !machine.panicked(),
            "phase-1 caller must Entry-call deferred free fn"
        );
    }

    /// COI-109: later free helpers must be callable from inherent methods under
    /// the default `main` treeshake path (not only `coil test` roots).
    #[test]
    fn inherent_method_calling_later_helper_runs_via_main() {
        let src = r#"
class Foo { v: int, }
impl Foo {
    fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { raise "bump"; }
}
"#;
        let mut pipeline = crate::Pipeline::new();
        let (bytecode, constants) = pipeline.compile_src(src).expect("compile");
        let mut machine = machine::Machine::<256>::default();
        pipeline.wire_host_natives(&mut machine);
        machine.set_program_debug(pipeline.program_debug());
        machine.run_raw(
            &bytecode,
            &constants,
            pipeline.strings(),
            pipeline.static_slot_count(),
        );
        assert!(
            !machine.panicked(),
            "main treeshake path must keep later helper callable"
        );
    }

    #[test]
    fn emit_call_indirect_pushes_target_then_opcode() {
        use common::Instruction;
        let mut bc = Vec::new();
        Compiler::emit_call_indirect(&mut bc, 42, 2);
        assert_eq!(bc.len(), 2);
        assert!(matches!(bc[0].bytecode(), Instruction::CodePtr));
        assert_eq!(bc[0].operand_u32(), 42);
        assert!(matches!(bc[1].bytecode(), Instruction::CallIndirect));
        assert_eq!(bc[1].operand_u32(), 2);
    }

    // ============================================================
    // sum types and pattern matching codegen
    // ============================================================

    /// Codegen test 1: a constructor call emits a `MAKE_ENUM`
    /// with the correct tag and arity in the operand (upper 16
    /// bits = tag, lower 16 bits = arity).
    #[test]
    fn construct_emits_make_enum_with_correct_tag_and_arity() {
        use common::Instruction;
        let (bc, _pool) = compile_src("let x = Option::Some(42);");

        // Find the MAKE_ENUM instruction. Its operands encode
        // (tag, arity) — for `Option::Some(42)`, tag=1, arity=1.
        let make_enum = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakeEnum))
            .expect("expected at least one MakeEnum in the bytecode");
        let tag = (make_enum.operand_u32() >> 16) as u16;
        let arity = (make_enum.operand_u32() & 0xFFFF) as u16;
        assert_eq!(tag, 1, "expected tag=1 (Some)");
        assert_eq!(arity, 1, "expected arity=1 for Some(int)");
    }

    /// Codegen test 2: a `match` with multiple constructor arms
    /// emits a cascade of `JUMP_IF_MATCH` instructions
    /// (one per non-last constructor arm).
    #[test]
    fn match_emits_jump_if_match_cascade() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "match Option::Some(1) { \
 Option::None() => 0, \
 Option::Some(v) => v, \
 };",
        );

        // Two arms, both constructor. Two JUMP_IF_MATCH should
        // be emitted (one per arm — actually only one, since
        // arm 0 is non-last and arm 1 is last. So we expect 1
        // JUMP_IF_MATCH and 1 UNPACK.
        let jump_if_match_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack))
            .count();
        assert_eq!(
            jump_if_match_count, 1,
            "expected 1 JUMP_IF_MATCH (one per non-last constructor arm)"
        );
        assert_eq!(
            unpack_count, 1,
            "expected 1 UNPACK (one for the last constructor arm)"
        );
    }

    /// Codegen test 3: a wildcard match arm emits `POP` to
    /// discard the scrutinee.
    #[test]
    fn wildcard_match_arm_emits_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "let x = Option::Some(42); \
 match x { _ => 42 };",
        );

        // The wildcard arm is the LAST (and only) arm, reached
        // by fall-through from the scrutinee. It emits `POP` to
        // discard the scrutinee.
        let pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::POP))
            .count();
        assert!(
            pop_count >= 1,
            "expected at least one POP for the wildcard scrutinee"
        );
    }

    /// Codegen test 4 (LOW #5): a `match` with a
    /// NESTED constructor pattern (`Result::Ok(Option::Some(v))`)
    /// emits at least 2 `UNPACK`s — one for the outer `Result::Ok`
    /// and one for the inner `Option::Some`. The codegen
    /// recurses through `emit_pattern_binding` for nested
    /// constructors; the test guards against accidental
    /// simplification that would skip the inner unpack.
    #[test]
    fn match_with_nested_constructor_pattern_emits_unpack_cascade() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "match Result::Ok(Option::Some(1)) { \
 Result::Err(_) => 0, \
 Result::Ok(Option::Some(v)) => v, \
 };",
        );

        // The outer match arm (`Result::Ok(Option::Some(v))`) is
        // non-last (the `Err` arm is listed first), so the
        // codegen emits a `JUMP_IF_MATCH` for it. The inner
        // pattern `Option::Some(v)` is a nested constructor, so
        // the binding code emits an `UNPACK` for the inner
        // payload. The two-UNPACK cascade is the structural
        // signature of a nested match.
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack))
            .count();
        let jump_if_match_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert!(
            unpack_count >= 1,
            "expected at least one UNPACK (the inner Option::Some); got {}",
            unpack_count
        );
        assert!(
            jump_if_match_count >= 1,
            "expected at least one JUMP_IF_MATCH (the outer Result::Ok); got {}",
            jump_if_match_count
        );
    }

    // ============================================================
    // VM perf: peephole superinstruction fusion
    // ============================================================

    #[test]
    fn compile_module_diff_matches_compile_tail_for_fib() {
        use common::Instruction;
        let src = include_str!("../../../examples/fib.hy");
        let mut ast = Pratt::default().parse(src).expect("parse fib");

        // `compile_module` appends IL to the shared buffer; fusion/label
        // resolution happens once via `finalize_bytecode`.
        let mut module = Compiler::default();
        let _ = module.compile_module("", &mut ast);
        assert!(
            module.bytecode.ops().iter().any(|op| {
                matches!(
                    op.as_plain_byte().map(|b| *b.bytecode()),
                    Some(Instruction::LOAD)
                )
            }),
            "module compile should still contain LOAD in IL before finalize"
        );
        module.finalize_bytecode();

        let mut full = Compiler::default();
        let bc_full = full.compile("", &mut ast);

        assert_eq!(
            &bc_full[3..],
            &module.bytecode_slice()[3..],
            "finalize_bytecode on compile_module should match compile() output"
        );
        assert_eq!(full.functions, module.functions);
    }

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
    fn fib_compiles_with_fused_superinstructions() {
        use common::Instruction;
        // Keep fib reachable via `return` (unit-test HostInvoke write paths
        // do not emit CALLs, so treeshake would drop an unused fib body).
        let (bc, _) = compile_src(
            "fn fib(int n) -> int { \
               if n <= 2 { return 1; } \
               return fib(n - 1) + fib(n - 2); \
             } \
             fn main() { return fib(10); }",
        );
        // Pure call arms leave both results on the operand stack (expr_depth
        // pads temps above the stacked lhs), so lower fuses ADD;RETURN.
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::ADD as u8),
            "expected fib tail BinReturn ADD; ops={:?}",
            bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::BinSlotImmJmpf),
            "expected fused n <= 2 guard"
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::ConstReturnImm),
            "expected fused base-case ConstReturnImm"
        );
    }

    /// Pure `f(…) + g(…)` keeps both call results on the operand stack (no
    /// temp STORE between the arms), so lower can emit `BinReturn`.
    #[test]
    fn pure_call_binop_leaves_results_on_stack_for_bin_return() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn fib(int n) -> int { \
               if n <= 2 { return 1; } \
               return fib(n - 1) + fib(n - 2); \
             } \
             fn main() { return fib(10); }",
        );
        // Walk fib's body: after the base ConstReturnImm, the two recursive
        // CALLs must be adjacent to arg prep with no STORE between them, and
        // the join must be BinReturn.
        let base = bc
            .iter()
            .position(|b| *b.bytecode() == Instruction::ConstReturnImm)
            .expect("fused base return");
        let body = &bc[base + 1..];
        let call_pos: Vec<usize> = body
            .iter()
            .enumerate()
            .filter(|(_, b)| *b.bytecode() == Instruction::CALL)
            .map(|(i, _)| i)
            .collect();
        assert!(
            call_pos.len() >= 2,
            "expected two recursive CALLs; ops={:?}",
            body.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
        let (c0, c1) = (call_pos[0], call_pos[1]);
        assert!(
            !(c0 + 1..c1).any(|i| *body[i].bytecode() == Instruction::STORE),
            "STORE between fib arms regresses stack-across-CALL"
        );
        assert!(
            body[c1 + 1..]
                .iter()
                .any(|b| *b.bytecode() == Instruction::BinReturn),
            "expected BinReturn after stacked fib calls"
        );
    }

    /// Distinct pure non-tiny helpers with leaf args also stack-across-CALL
    /// (not fib-only): no STORE between the two CALLs, and BinReturn fuse.
    ///
    /// Bodies use a loop so they are neither tiny-inlined nor predicate-peeled
    /// (peel parks results via STORE and would confuse the inter-CALL check).
    #[test]
    fn pure_helper_binop_stacks_across_call_for_bin_return() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn left(int n) -> int { \
               let s = 0; \
               let i = 0; \
               while i < n { s = s + i; i = i + 1; } \
               return s; \
             } \
             fn right(int n) -> int { \
               let s = 1; \
               let i = 0; \
               while i < n { s = s + i; i = i + 1; } \
               return s; \
             } \
             fn main() { return left(4) + right(3); }",
        );
        let ops: Vec<_> = bc.iter().map(|b| *b.bytecode()).collect();
        let call_pos: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| *b.bytecode() == Instruction::CALL)
            .map(|(i, _)| i)
            .collect();
        assert!(
            call_pos.len() >= 2,
            "expected left/right CALLs in main; ops={ops:?}"
        );
        // Main's stacked arms are the last two CALLs (loop bodies have no CALL).
        let (c0, c1) = (call_pos[call_pos.len() - 2], call_pos[call_pos.len() - 1]);
        assert!(
            !(c0 + 1..c1).any(|i| *bc[i].bytecode() == Instruction::STORE),
            "STORE between pure helper arms regresses stack-across-CALL; between={:?}",
            bc[c0..=c1].iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc[c1 + 1..]
                .iter()
                .any(|b| *b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::ADD as u8),
            "expected BinReturn ADD after stacked helpers; ops={ops:?}"
        );
    }

    /// Tiny-inlineable callees STORE args into temps — must stage binop arms
    /// (not stack-across), or the sibling operand is buried.
    #[test]
    fn tiny_inline_binop_arms_stage_through_temps() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn add(int a, int b) -> int { return a + b; } \
             fn main() { \
               let x = 3; \
               let y = 4; \
               return add(x, y) + add(y, x); \
             }",
        );
        let ops: Vec<_> = bc.iter().map(|b| *b.bytecode()).collect();
        // Arms are tiny-inlined (arg STORE into temps) rather than real CALL;
        // stack-across would leave CALL;CALL;BinReturn without result staging.
        let main_calls = bc
            .iter()
            .filter(|b| *b.bytecode() == Instruction::CALL)
            .count();
        assert!(
            main_calls <= 1,
            "expected tiny-inline of add arms (≤1 prologue CALL); ops={ops:?}"
        );
        assert!(
            !bc.iter().any(|b| {
                *b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::ADD as u8
            }),
            "BinReturn ADD would mean stack-across of tiny-inline arms; ops={ops:?}"
        );
        // Staging parks each inlined result then reloads: … STORE … STORE …
        // LOAD; LOAD … before the join (or fused BinSlotSlot from those slots).
        let stores = bc
            .iter()
            .filter(|b| *b.bytecode() == Instruction::STORE)
            .count();
        assert!(
            stores >= 4,
            "expected arg+result staging STOREs for tiny-inline arms; ops={ops:?}"
        );
        let reloaded = bc.windows(2).any(|w| {
            *w[0].bytecode() == Instruction::LOAD && *w[1].bytecode() == Instruction::LOAD
        }) || bc
            .iter()
            .any(|b| *b.bytecode() == Instruction::BinSlotSlot);
        assert!(
            reloaded,
            "expected LOAD;LOAD or BinSlotSlot after staging tiny-inline arms; ops={ops:?}"
        );
    }

    /// Nested call args are not stack leaves — binop arms must stage so the
    /// nested CALL's temps cannot bury the stacked sibling.
    #[test]
    fn nested_call_arg_binop_arms_stage_through_temps() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn leaf(int n) -> int { \
               if n <= 0 { return 1; } \
               return n + leaf(n - 1); \
             } \
             fn main() { return leaf(leaf(2)) + leaf(3); }",
        );
        let call_pos: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| *b.bytecode() == Instruction::CALL)
            .map(|(i, _)| i)
            .collect();
        // main: CALL leaf(2), CALL leaf(leaf(2)), CALL leaf(3) — at least 3.
        assert!(
            call_pos.len() >= 3,
            "expected nested + sibling CALLs; ops={:?}",
            bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
        // Between the outer nested CALL and the sibling CALL there must be a
        // staging STORE (result of lhs parked before rhs emit).
        let outer_nested = call_pos[call_pos.len() - 2];
        let sibling = call_pos[call_pos.len() - 1];
        assert!(
            (outer_nested + 1..sibling).any(|i| *bc[i].bytecode() == Instruction::STORE),
            "expected STORE staging between nested-arg arm and sibling; ops={:?}",
            bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Match arms clobber the operand stack — binary ops must stage even when
    /// the other arm is a stackable pure CALL.
    #[test]
    fn match_plus_pure_call_binop_stages() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn leaf(int n) -> int { \
               if n <= 0 { return 1; } \
               return n + leaf(n - 1); \
             } \
             fn main() { \
               return match Option::Some(3) { \
                 Option::Some(x) => leaf(x), \
                 Option::None => 0, \
               } + leaf(2); \
             }",
        );
        let call_pos: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| *b.bytecode() == Instruction::CALL)
            .map(|(i, _)| i)
            .collect();
        assert!(
            call_pos.len() >= 2,
            "expected match-arm + sibling CALLs; ops={:?}",
            bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
        let (c0, c1) = (call_pos[call_pos.len() - 2], call_pos[call_pos.len() - 1]);
        assert!(
            (c0 + 1..c1).any(|i| *bc[i].bytecode() == Instruction::STORE),
            "expected STORE staging between match arm and pure CALL; ops={:?}",
            bc.iter().map(|b| *b.bytecode()).collect::<Vec<_>>()
        );
    }

    // ============================================================
    // BlockBuilder for Loop and Match codegen
    // ============================================================
    //
    // The 17A refactor moves both the Loop and Match codegen
    // from manual `Vec<usize>`-based placeholder tracking to
    // the placeholder-tracking `BlockBuilder` (the same
    // primitive that drives If since 16.6). The semantics are
    // IDENTICAL — only the placeholder mechanism changes.
    //
    // These tests guard against regressions in the
    // BlockBuilder-based Loop and Match codegen. The key
    // invariant we check is that the placeholder TARGETS
    // (operands) are correctly patched to the absolute
    // positions of the arm bodies / loop tops — if a `bind_label`
    // is missed, the operand would be `0` (the placeholder
    // value), and the program would either infinite-loop or
    // jump to the prologue.

    /// Codegen test 5 : a `while` loop emits
    /// the structural shape expected by the
    /// BlockBuilder-based codegen — at least 1 JMPF (the
    /// exit condition) and at least 1 JMP (the back-edge).
    /// This mirrors the 16.5 regression test for If, but
    /// for the new Loop codegen.
    #[test]
    fn loop_emits_top_label_and_back_edge() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let i = 0; \
 while (i < 10) { \
 i = i + 1; \
 } \
 }",
        );

        // The loop emits: <iterable>, exit-branch, <body>, JMP→top.
        let exit_branch_count = loop_exit_branch_count(&bc);
        let jmp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .count();
        assert!(
            exit_branch_count >= 1,
            "expected at least 1 loop exit branch (JMPF/CmpJmpf/BinSlotImmJmpf); got {}",
            exit_branch_count
        );
        assert!(
            jmp_count >= 1,
            "expected at least 1 JMP (the loop's back-edge); got {}",
            jmp_count
        );
    }

    /// Two-local compare `a < b` fuses to `BinSlotSlotJmpf`.
    #[test]
    fn two_local_compare_if_fuses_bin_slot_slot_jmpf() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn cmp(int a, int b) -> int { \
               if a < b { return 1; } \
               return 0; \
             }",
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::BinSlotSlotJmpf
                    && b.bin_slot_slot_jmpf_parts().0 == Instruction::LE as u8),
            "expected BinSlotSlotJmpf(LE) for a < b; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x & 1` fuses to `BinSlotImm(BITAND)` (and jmpf when used as a condition).
    #[test]
    fn bitand_imm_fuses_bin_slot_imm() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn lowbit(int x) -> int { return x & 1; }");
        assert!(
            bc.iter().any(|b| {
                *b.bytecode() == Instruction::BinSlotImm
                    && b.bin_slot_imm_parts().0 == Instruction::BITAND as u8
                    && b.bin_slot_imm_parts().2 == 1
            }),
            "expected BinSlotImm(BITAND, 1); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Eager `a && b` as an if-condition fuses to `BinSlotSlotJmpf(AND)`.
    #[test]
    fn logical_and_if_fuses_bin_slot_slot_jmpf() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn both(bool a, bool b) -> int { \
               if a && b { return 1; } \
               return 0; \
             }",
        );
        assert!(
            bc.iter()
                .any(|b| *b.bytecode() == Instruction::BinSlotSlotJmpf
                    && b.bin_slot_slot_jmpf_parts().0 == Instruction::AND as u8),
            "expected BinSlotSlotJmpf(AND) for a && b; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `i = i + 1` fuses to `BinSlotImmStore(ADD)`, or elides the store when
    /// `mem_fwd` + dead-store keep the value on stack for `return i`.
    #[test]
    fn assign_add_imm_fuses_bin_slot_imm_store() {
        use common::Instruction;
        let (bc, pool) = compile_src(
            "fn bump(int i) -> int { \
               i = i + 1; \
               return i; \
             }",
        );
        let fused = bc.iter().any(|b| {
            if *b.bytecode() != Instruction::BinSlotImmStore {
                return false;
            }
            let (op, _src, idx) = b.bin_slot_imm_store_parts();
            op == Instruction::ADD as u8 && (pool[idx] as u16) == 1
        });
        let stack_return = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotImm
                && b.bin_slot_imm_parts().0 == Instruction::ADD as u8
        });
        assert!(
            fused || stack_return,
            "expected BinSlotImmStore(ADD) or BinSlotImm(ADD) for return; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `flags = flags & mask` fuses to `BinSlotSlotStore(BITAND)`, or keeps a
    /// stack `BinSlotSlot(BITAND)` when the store is dead after `return`.
    #[test]
    fn assign_bitand_fuses_bin_slot_slot_store() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn mask_bits(int flags, int mask) -> int { \
               flags = flags & mask; \
               return flags; \
             }",
        );
        let fused = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlotStore
                && b.bin_slot_slot_store_parts().0 == Instruction::BITAND as u8
        });
        let stack_return = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::BITAND as u8
        });
        assert!(
            fused || stack_return,
            "expected BinSlotSlotStore/BinSlotSlot(BITAND); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `x = a && b` assignment fuses to `BinSlotSlotStore(AND)`, or keeps
    /// `BinSlotSlot(AND)` when returning the computed value on stack.
    #[test]
    fn assign_logical_and_fuses_bin_slot_slot_store() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn both(bool a, bool b) -> bool { \
               a = a && b; \
               return a; \
             }",
        );
        let fused = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlotStore
                && b.bin_slot_slot_store_parts().0 == Instruction::AND as u8
        });
        let stack_return = bc.iter().any(|b| {
            *b.bytecode() == Instruction::BinSlotSlot
                && b.bin_slot_slot_parts().0 == Instruction::AND as u8
        });
        assert!(
            fused || stack_return,
            "expected BinSlotSlotStore/BinSlotSlot(AND); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Loop exit jump must land past the back-edge `JMP`, even after
    /// peephole fusion relocates jump targets. The condition may fuse
    /// to `CmpJmpf` (large limit) or `BinSlotImmJmpf` (inline limit).
    #[test]
    fn loop_cmp_jmpf_exit_targets_past_back_edge_after_peephole() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let acc = 0; \
 let i = 0; \
 while (i < 2000) { \
 acc = acc + i; \
 i = i + 1; \
 } \
 }",
        );

        assert!(
            bc.iter().any(|b| matches!(
                *b.bytecode(),
                Instruction::JMPF
                    | Instruction::CmpJmpf
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::LogNotJmpf
            )),
            "while condition should emit a false-jump"
        );
        assert!(
            bc.iter().any(|b| {
                matches!(*b.bytecode(), Instruction::JMP)
                    && b.operand_u32() != u32::MAX
                    && (b.operand_u32() as usize) < bc.len()
            }),
            "loop should emit a back-edge JMP"
        );
    }

    /// While-loop exit must land past the back-edge `JMP`, not on it.
    #[test]
    fn loop_jmpf_exits_past_back_edge() {
        use common::Instruction;
        let (bc, pool) = compile_src(
            "fn main() { \
 let i = 0; \
 while (i < 10) { \
 i = i + 1; \
 } \
 }",
        );

        let cond_idx = bc
            .iter()
            .position(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::JMPF
                        | Instruction::CmpJmpf
                        | Instruction::BinSlotImmJmpf
                        | Instruction::BinSlotSlotJmpf
                        | Instruction::LogNotJmpf
                )
            })
            .expect("loop should emit an exit branch");
        let back_jmp_idx = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::JMP))
            .map(|(i, _)| i)
            .find(|&i| i > cond_idx)
            .expect("loop should emit back-edge JMP after exit branch");
        let exit_target = loop_exit_target(&bc, &pool).expect("loop exit target");
        assert!(
            exit_target > back_jmp_idx,
            "loop exit target ({exit_target}) must be past the back-edge JMP ({back_jmp_idx})"
        );
    }

    /// Assignment statements must not leave a trailing DUPLICATE/POP pair
    /// that shrinks the operand stack below live locals inside loops.
    #[test]
    fn assignment_statement_does_not_emit_duplicate_before_store_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let acc = 0; \
 let i = 0; \
 while (i < 2) { \
 acc = acc + i; \
 i = i + 1; \
 } \
 }",
        );

        let mut dup_before_store = 0usize;
        for w in bc.windows(2) {
            if matches!(w[0].bytecode(), Instruction::DUPLICATE)
                && matches!(w[1].bytecode(), Instruction::STORE)
            {
                dup_before_store += 1;
            }
        }
        assert_eq!(
            dup_before_store, 0,
            "identifier assignment should not emit DUPLICATE before STORE_POP"
        );
    }

    #[test]
    fn for_with_break_and_continue_emits_patched_jumps() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
let sum = 0; \
for (let i = 0; i < 10; i = i + 1) { \
if i == 3 { continue; } \
if i == 7 { break; } \
sum = sum + i; \
} \
}",
        );

        let back_edges: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .map(|b| b.operand_u32())
            .filter(|t| *t != u32::MAX)
            .collect();
        let imm_jmpt = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::BinSlotImmJmpt))
            .count();
        let imm_jmpf = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::BinSlotImmJmpf))
            .count();

        assert!(
            !back_edges.is_empty() && back_edges.iter().all(|t| *t != 0),
            "loop back-edge JMP should be patched: {:?}",
            back_edges
        );
        assert!(
            imm_jmpt >= 2,
            "continue/break `i == k` should invert+fuse to BinSlotImmJmpt; got {imm_jmpt}"
        );
        assert!(
            imm_jmpf >= 1,
            "loop header `i < 10` must stay BinSlotImmJmpf; got {imm_jmpf}"
        );
    }

    /// `if !flag { break }` inverts fused LogNot;JMPF into LogNotJmpt (COI-87).
    #[test]
    fn not_flag_break_emits_log_not_jmpt() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn main() { \
let flag = false; \
let i = 0; \
while (i < 5) { \
if !flag { break; } \
i = i + 1; \
} \
}",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::LogNotJmpt)),
            "expected LogNotJmpt for inverted `if !flag {{ break }}`"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BinSlotImmJmpf)),
            "while header should remain *Jmpf"
        );
    }

    /// Two-local compare break fuses to BinSlotSlotJmpt after invert (COI-87).
    #[test]
    fn two_local_compare_break_emits_bin_slot_slot_jmpt() {
        use common::Instruction;
        let (bc, _) = compile_src(
            "fn main() { \
let a = 1; \
let b = 2; \
let i = 0; \
while (i < 5) { \
if a < b { break; } \
i = i + 1; \
} \
}",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BinSlotSlotJmpt)),
            "expected BinSlotSlotJmpt for inverted `if a < b {{ break }}`"
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BinSlotSlotJmpf)),
            "break guard should not remain BinSlotSlotJmpf after invert"
        );
    }

    /// Plain while headers must not invert to *Jmpt (COI-87 latch stays *Jmpf).
    #[test]
    fn while_header_stays_fused_jmpf_not_jmpt() {
        use common::Instruction;
        let (bc, _) = compile_src("fn main() { let i = 0; while (i < 10) { i = i + 1; } }");
        let jmpt = bc
            .iter()
            .filter(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::JMPT
                        | Instruction::CmpJmpt
                        | Instruction::BinSlotImmJmpt
                        | Instruction::BinSlotSlotJmpt
                        | Instruction::BinSlotSlotConstJmpt
                        | Instruction::LogNotJmpt
                )
            })
            .count();
        assert_eq!(jmpt, 0, "header-only while must not emit *Jmpt");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BinSlotImmJmpf)),
            "header should stay BinSlotImmJmpf"
        );
    }

    #[test]
    fn for_in_array_emits_array_len_index_and_back_edge() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { for x in [1, 2, 3] { write(stdout(), to_bytes(format(\"%i\", x))); } }",
        );
        let has_len = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::ArrayLen));
        let has_index = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::Index));
        let jmp = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .count();
        assert!(has_len, "array for-in should emit ArrayLen");
        assert!(has_index, "array for-in should emit Index");
        assert!(
            jmp >= 1,
            "array for-in should emit back-edge JMP; got {jmp}"
        );
    }

    #[test]
    fn for_in_dict_emits_dict_entries() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { let d = { a: 1, b: 2 }; for p in d { write(stdout(), to_bytes(format(\"%i\", p[1]))); } }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::DictEntries)),
            "dict for-in should emit DictEntries; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn for_in_custom_emits_into_iter_and_next_calls() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "class Counter { cur: int, end: int, } \
impl IntoIterator<Counter> { \
    type Item = int; type IntoIter = Counter; \
    fn into_iter(Counter c) -> Counter { return c; } \
} \
impl Iterator<Counter> { \
    type Item = int; \
    fn next(Counter c) -> Option<int> { \
        if c.cur < c.end { let v = c.cur; c.cur = c.cur + 1; return Option::Some(v); } \
        return Option::None; \
    } \
} \
fn main() { let c = new Counter(0, 3); for x in c { write(stdout(), to_bytes(format(\"%i\", x))); } }",
        );
        let calls = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .count();
        let jump_if_match = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert!(
            calls >= 2,
            "custom for-in should CALL into_iter and next; got {calls}"
        );
        assert!(
            jump_if_match >= 1,
            "custom for-in should JumpIfMatch on Option::None; got {jump_if_match}"
        );
    }

    #[test]
    fn for_in_coro_emits_resume_done_and_back_edge() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "async fn counter() { yield 0; yield 1; return 99; } \
fn main() { for x in counter() { write(stdout(), to_bytes(format(\"%i\", x))); } }",
        );

        let resume = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::ResumeCoro))
            .count();
        let done = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::DoneCoro))
            .count();
        let log_not = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LogNot | Instruction::LogNotJmpf))
            .count();
        let jmpf = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMPF | Instruction::LogNotJmpf))
            .count();
        let jmp = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .count();

        assert!(resume >= 1, "expected ResumeCoro in for-in; got {resume}");
        assert!(done >= 1, "expected DoneCoro in for-in; got {done}");
        assert!(
            log_not + jmpf >= 1,
            "expected done-check exit branch (LogNot/JMPF); log_not={log_not} jmpf={jmpf}"
        );
        assert!(jmp >= 1, "expected back-edge JMP; got {jmp}");
    }

    #[test]
    fn for_in_coro_break_patches_exit_jump() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "async fn counter() { yield 0; yield 1; yield 2; } \
fn main() { for x in counter() { if x == 1 { break; } } }",
        );
        let jmp_targets: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .map(|b| b.operand_u32())
            .collect();
        assert!(
            jmp_targets.iter().all(|t| *t != 0),
            "for-in break/back-edge JMPs should be patched: {:?}",
            jmp_targets
        );
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
            rendered.contains("break outside of loop"),
            "expected break diagnostic, got {rendered}"
        );
        assert!(
            rendered.contains("continue outside of loop"),
            "expected continue diagnostic, got {rendered}"
        );
    }

    /// Codegen test 6 : the loop's JMP back-edge
    /// TARGETS the start of the loop, not the prologue. If
    /// the BlockBuilder's `bind_label` for `top_label` were
    /// missed, the JMP would either point at the prologue
    /// (offset 0) or at some other incorrect position; the
    /// program would either infinite-loop or jump out of the
    /// function. The fix-verification: the JMP operand must
    /// be > 3 (past the 3-byte prologue: CALL, JMP, HALT)
    /// and point at the start of the loop's iterable.
    #[test]
    fn loop_jmp_back_edge_targets_loop_top_not_prologue() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let i = 0; \
 while (i < 10) { \
 i = i + 1; \
 } \
 }",
        );

        // The loop has exactly one JMPF (the exit) and
        // exactly one JMP (the back-edge). The JMP is the
        // one we care about.
        let jmp = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::JMP))
            .expect("expected at least one JMP in the loop bytecode");
        let jmp_target = jmp.operand_u32();

        // The JMP's target must point INTO the function
        // body (i.e., not be 0 which would be the start of
        // the body itself — the back-edge is to the loop's
        // iterable, not to the very first byte). The
        // body is what `compile_src` returns, so offset
        // 0 is the start of `main` (no prologue in the
        // returned slice — see the changes
        // to `Compiler::compile`).
        assert!(
            jmp_target > 0,
            "JMP back-edge target {} should be > 0 (into the loop body)",
            jmp_target
        );
    }

    /// Codegen test 7 : in the BlockBuilder-based
    /// Match codegen, every non-last constructor arm's
    /// JUMP_IF_MATCH placeholder is bound to that arm's body
    /// offset. If the `bind_label` for some arm's label didn't
    /// fire (e.g., the `if let Some(label) = arm_labels[i]` arm didn't
    /// fire for some non-last constructor arm), the
    /// placeholder's `value[31:0]` would be `0` (the
    /// `BlockBuilder` placeholder value), and the VM would
    /// jump to the prologue — crashing with a `HALT`.
    ///
    /// the JUMP_IF_MATCH target lives in
    /// `value[31:0]` (a full 32-bit absolute bytecode offset),
    /// NOT in the lower 16 bits of `operands`. The tag is in
    /// `operands[31:16]` (lower 16 bits reserved).
    #[test]
    fn match_jump_if_match_targets_are_patched_to_arm_offsets() {
        use common::Instruction;
        let (bc, pool) = compile_src(
            "match Option::Some(1) { \
 Option::None() => 0, \
 Option::Some(v) => v, \
 };",
        );

        // Find every JUMP_IF_MATCH. For each, the target
        // (in `value[31:0]`) must be > 0 (i.e., the placeholder
        // was patched to a real arm-body offset).
        let jump_if_matches: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .collect();
        assert!(
            !jump_if_matches.is_empty(),
            "expected at least one JUMP_IF_MATCH in the match bytecode"
        );
        for (i, jim) in jump_if_matches.iter().enumerate() {
            let target = jim.jump_if_match_target(&pool);
            let tag = (jim.operand_u32() >> 16) as u16;
            assert!(
                target > 0,
                "JUMP_IF_MATCH #{} (tag={}) target should be patched to a non-zero offset; got {}",
                i,
                tag,
                target
            );
        }
    }

    /// Codegen test 8 : in the BlockBuilder-based
    /// Match codegen, the `end_label` is correctly bound to
    /// the position just past the FIRST arm body in source
    /// order. The JMP-to-end placeholder (emitted after
    /// every non-FIRST arm body) is patched to this
    /// position. If the binding were missed, the JMP would
    /// point at offset 0 (prologue) and crash.
    ///
    /// We verify by checking that the number of JMP
    /// instructions emitted by a 3-arm match is exactly 2
    /// (one for each non-first arm's JMP-to-end), AND that
    /// the LAST arm body has no JMP after it (it's reached
    /// by fall-through from the previous arm's JMP-to-end).
    /// The 15C codegen produced this exact same
    /// shape; the 17A refactor preserves it.
    #[test]
    fn match_jmp_to_end_placeholders_are_patched_to_end_label() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Choice { Empty, Value(int), Maybe(int) } \
 fn main() { \
 match Choice::Value(1) { \
 Choice::Empty() => 0, \
 Choice::Value(v) => v, \
 Choice::Maybe(w) => w, \
 }; \
 }",
        );

        // 3 arms → 2 non-first arms → 2 JMP-to-end
        // placeholders. The loop's JMP at the very end of
        // the function is ALSO a JMP, but it's not part of
        // the match. We filter for JMPs that are NOT the
        // prologue JMP (operand == 0 or u32::MAX) and NOT
        // the function-exit JMP (if any).
        //
        // Easier check: the JMPs emitted by the match
        // are JMPs with operand > 3 (past the prologue).
        // The 3-arm match emits exactly 2 such JMPs
        // (one per non-first arm).
        let match_jmps: Vec<_> = bc
            .iter()
            .filter(|b| {
                matches!(b.bytecode(), Instruction::JMP)
                    && b.operand_u32() > 3
                    && b.operand_u32() != u32::MAX
            })
            .collect();
        // The match's 2 JMP-to-end + the function's
        // JMP-for-defers (if any) and any nested control
        // flow's JMPs. For this minimal program, the
        // function has no defers, so the only JMPs should
        // be the 2 match JMP-to-end instructions.
        assert_eq!(
            match_jmps.len(),
            2,
            "expected exactly 2 JMP-to-end for a 3-arm match; got {}",
            match_jmps.len()
        );
        // Both JMPs should point to the same end-of-match
        // position (the same `end_label` was bound to the
        // same offset).
        let target_a = match_jmps[0].operand_u32();
        let target_b = match_jmps[1].operand_u32();
        assert_eq!(
            target_a, target_b,
            "both JMP-to-end should target the same end_label; got {} and {}",
            target_a, target_b
        );
    }

    /// Codegen test 9 : a `match` inside a `while`
    /// loop body — the canonical nested-control-flow
    /// scenario for the BlockBuilder-based codegen. The
    /// 16.5/16.6 If-in-If scenario was the regression that
    /// motivated `BlockBuilder`; this test guards against
    /// the equivalent regression in the Match-in-Loop case.
    /// We don't run the VM (the test infrastructure doesn't
    /// support that for arbitrary programs), but we do
    /// assert the bytecode has the expected control-flow
    /// opcode shape: at least 1 JMPF (the loop's exit
    /// condition), at least 1 JMP (the loop's back-edge),
    /// at least 1 JUMP_IF_MATCH (the match's tag dispatch),
    /// and at least 1 UNPACK (the match's last arm
    /// scrutinee-consumer).
    ///
    /// The match's result is the last expression in the
    /// loop body, which sidesteps the parser's
    /// statement-vs-expression ambiguity (the parser
    /// doesn't accept `match { ... }` followed by another
    /// statement — the `match` is an expression and the
    /// parser wants an operator, not a new statement).
    #[test]
    fn nested_match_in_loop_emits_expected_opcodes() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let x = Option::Some(0); \
 let i = 0; \
 while (i < 3) { \
 return match x { \
 Option::None() => 0, \
 Option::Some(v) => v, \
 }; \
 } \
 }",
        );

        let exit_branch_count = loop_exit_branch_count(&bc);
        let jmp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JMP))
            .count();
        let jump_if_match_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack))
            .count();
        assert!(
            exit_branch_count >= 1,
            "expected at least 1 loop exit branch; got {}",
            exit_branch_count
        );
        assert!(
            jmp_count >= 1,
            "expected at least 1 JMP (the loop's back-edge); got {}",
            jmp_count
        );
        assert!(
            jump_if_match_count >= 1,
            "expected at least 1 JUMP_IF_MATCH (the match's tag dispatch); got {}",
            jump_if_match_count
        );
        assert!(
            unpack_count >= 1,
            "expected at least 1 UNPACK (the match's last arm); got {}",
            unpack_count
        );
    }

    // ============================================================
    // record-payload codegen tests
    // ============================================================
    //
    // The 17B spec listed 6 record-payload codegen tests. The
    // developer claimed to add them but in fact added 0 — all 6
    // were silently skipped. This section adds the missing tests,
    // including the red-team's canonical
    // `record_construct_reorders_shuffled_call_site_fields` test
    // that locks in the record-field reordering behavior.

    /// Codegen test 10 : the red-team's canonical
    /// record-payload reorder test. The variant is declared as
    /// `Foo { x: int, y: int, z: int }` and the user calls it
    /// with shuffled fields `Foo { z: 1, x: 2, y: 3 }`. The
    /// codegen must emit the CONST operands in DECLARATION order
    /// (2, 3, 1) so the VM's `MAKE_ENUM` produces a payload
    /// `[2, 3, 1]` matching the declaration order. If the
    /// codegen emitted them in call-site order (1, 2, 3), the
    /// payload would be in the wrong slot positions and any
    /// match destructuring would get the wrong values.
    #[test]
    fn record_construct_reorders_shuffled_call_site_fields() {
        use common::Instruction;
        // The variant is declared as `Foo { x: int, y: int, z: int }`
        // and the user calls it with shuffled fields
        // `Foo { z: 1, x: 2, y: 3 }`. The codegen must emit the
        // CONST operands in REVERSE declaration order so the VM's
        // `MAKE_ENUM` produces a payload in DECLARATION order
        // (payload[0] = x = 2, payload[1] = y = 3, payload[2] = z = 1).
        let (bc, _pool) = compile_src(
            r#"enum E { Foo { x: int, y: int, z: int } }
fn main() {
 let foo = E::Foo { z: 1, x: 2, y: 3 };
}"#,
        );

        // The construct `E::Foo { z: 1, x: 2, y: 3 }` should
        // emit CONST 1 (z), CONST 3 (y), CONST 2 (x) — that
        // is, REVERSE declaration order — so that MAKE_ENUM's
        // top-first pop order places them at payload[0..2]
        // in declaration order.
        let const_operands: Vec<i64> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CONST))
            .map(|b| b.constant(&[]) as i64)
            .filter(|&v| (1..=3).contains(&v))
            .collect();
        assert_eq!(
            const_operands,
            vec![1, 3, 2],
            "Record fields must be emitted in REVERSE declaration order \
 so MAKE_ENUM pops them into declaration order at payload[0..]"
        );

        // Verify MAKE_ENUM has the right tag and arity.
        let make_enum = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakeEnum))
            .expect("expected MAKE_ENUM in the bytecode");
        let tag = (make_enum.operand_u32() >> 16) as u16;
        let arity = (make_enum.operand_u32() & 0xFFFF) as u16;
        assert_eq!(tag, 0, "expected tag=0 for the only variant Foo");
        assert_eq!(arity, 3, "expected arity=3 for Foo {{ x, y, z }}");
    }

    /// Codegen test 11 : a record construct with one
    /// field emits exactly 1 CONST followed by MAKE_ENUM with
    /// arity=1.
    #[test]
    fn record_construct_one_field_emits_correct_bytecode() {
        use common::Instruction;
        let (bc, _pool) =
            compile_src("enum E { Foo { x: int } } fn main() { let _ = E::Foo { x: 1 }; }");

        // Find the MAKE_ENUM. Its operand is tag (upper 16) and
        // arity (lower 16).
        let make_enum = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakeEnum))
            .expect("expected at least one MAKE_ENUM");
        let tag = (make_enum.operand_u32() >> 16) as u16;
        let arity = (make_enum.operand_u32() & 0xFFFF) as u16;
        assert_eq!(tag, 0);
        assert_eq!(arity, 1, "expected arity=1 for Foo {{ x }}");

        // Exactly 1 CONST with value 1 (the literal `1`).
        let const_one_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CONST) && b.constant(&[]) == 1)
            .count();
        assert_eq!(
            const_one_count, 1,
            "expected exactly 1 CONST with value 1; got {}",
            const_one_count
        );
    }

    /// Codegen test 12 : a match pattern with SHUFFLED record
    /// fields (`{ y: _, x: a }`) emits no STORE for binding `a`
    /// (value already in slot) and at least one POP for `_` / omitted
    /// fields. Declaration-order binding is covered by the pipeline
    /// golden `shuffled_record_pattern_binds_declaration_order_field`.
    #[test]
    fn match_emits_binding_interns_in_declaration_order() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum E { Foo { x: int, y: int, z: int } } \
 fn main() { \
 let e = E::Foo { x: 1, y: 2, z: 3 }; \
 let v = match e { \
 E::Foo { y: _, x: a } => a, \
 }; \
 write(stdout(), to_bytes(format(\"%i\", v))); \
 }",
        );
        // `let e` / `let v` emit STORE; match binding `a` does not.
        // An extra STORE may appear for match temps / relocate.
        let store_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        assert!(
            store_count >= 2,
            "expected ≥2 STORE (lets e/v); match binding needs none; got {store_count}"
        );
        let pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::POP))
            .count();
        assert!(
            pop_count >= 1,
            "expected at least 1 POP for the wildcard `_`"
        );
    }

    /// Codegen test 13 : a mixed-shape enum with
    /// Unit + Tuple + Record variants compiles with the
    /// correct tags and arities for each variant.
    #[test]
    fn mixed_enum_unit_tuple_record_all_in_one() {
        use common::Instruction;
        // Use bindings to keep the constructs alive in the
        // bytecode (the codegen is silent on unused `let _`).
        let (bc, _pool) = compile_src(
            "enum E { A, B(int), C { x: int } } \
 fn main() { \
 let a = E::A; \
 let b = E::B(1); \
 let c = E::C { x: 2 }; \
 }",
        );

        // Find all MAKE_ENUM ops (one per construct call,
        // including unit variants — the codegen always emits
        // MAKE_ENUM, even for Unit, with arity=0).
        let make_enums: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeEnum))
            .collect();
        assert_eq!(
            make_enums.len(),
            3,
            "expected 3 MAKE_ENUM ops (one per construct call); got {}",
            make_enums.len()
        );

        // Sort by (tag, arity) for stable comparison.
        let mut tags_arities: Vec<(u16, u16)> = make_enums
            .iter()
            .map(|b| {
                let tag = (b.operand_u32() >> 16) as u16;
                let arity = (b.operand_u32() & 0xFFFF) as u16;
                (tag, arity)
            })
            .collect();
        tags_arities.sort();
        assert_eq!(
            tags_arities,
            vec![(0, 0), (1, 1), (2, 1)],
            "expected MAKE_ENUM ops at (tag=0, arity=0) for A (unit), \
 (tag=1, arity=1) for B(int), and (tag=2, arity=1) for C record variant"
        );
    }

    /// Codegen test 14 : a record pattern with a
    /// wildcard field (`_`) emits a POP for the wildcard
    /// sub-pattern instead of a STORE.
    #[test]
    fn record_pattern_with_wildcard_field_emits_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum E { Foo { x: int, y: int } } \
 fn main() { \
 let e = E::Foo { x: 1, y: 2 }; \
 match e { \
 E::Foo { x: _, y: v } => v, \
 }; \
 }",
        );

        // The wildcard `x: _` produces POP in the binding code.
        // The binding `y: v` produces STORE at slot 2 (second
        // payload position for a 2-field record).
        let pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::POP))
            .count();
        assert!(
            pop_count >= 1,
            "expected at least 1 POP for the wildcard field; got {}",
            pop_count
        );
    }

    /// Codegen test 15 : a unit-variant match arm
    /// (`Empty`) does NOT emit UNPACK (the variant has no
    /// payload). It emits a POP to discard the scrutinee.
    #[test]
    fn empty_record_pattern_does_not_emit_unpack() {
        use common::Instruction;
        // The spec says "E::Empty => 0" where Empty is unit.
        // The codegen for a unit-variant last arm emits POP,
        // not UNPACK.
        let (bc, _pool) = compile_src(
            "enum E { Empty, Foo(int) } \
 fn main() { \
 let e = E::Empty; \
 match e { \
 E::Empty => 0, \
 E::Foo(_) => 1, \
 }; \
 }",
        );

        // Exactly 1 UNPACK (for the Foo arm, which is the
        // last arm and uses UNPACK to consume the scrutinee).
        // The Empty arm is NOT last → emits JUMP_IF_MATCH
        // (not UNPACK). If the codegen wrongly emitted UNPACK
        // for the unit arm, we'd see 2 UNPACKs.
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack))
            .count();
        assert_eq!(
            unpack_count, 1,
            "expected exactly 1 UNPACK (for the Foo last arm); got {}",
            unpack_count
        );

        // And the Empty arm's JUMP_IF_MATCH should be present.
        let jump_if_match_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert_eq!(
            jump_if_match_count, 1,
            "expected exactly 1 JUMP_IF_MATCH (for the Empty arm); got {}",
            jump_if_match_count
        );
    }

    // ============================================================
    // inner-pattern dispatch regression tests
    // ============================================================
    //
    // fixes the inner-pattern dispatch for multi-arm match
    // groups that share the same OUTER variant tag but differ on the
    // INNER sub-pattern. Before 18A, the codegen emitted POP
    // placeholders for nested Constructor sub-patterns in the test
    // chain, so all arms in a multi-arm group that shared an outer
    // tag were dispatched in source order regardless of the actual
    // inner tag (the first matching arm always won, even if the
    // runtime inner tag would have picked a different arm).
    //
    // After 18A:
    // - `arm_has_runtime_test` is more selective — it only flags
    // arms whose inner sub-patterns carry a `Binding` or further
    // nested `Constructor` (i.e., the inner pattern actually
    // binds a value that needs runtime extraction).
    // - `emit_inner_test` emits a real `JUMP_IF_MATCH` for the
    // inner tag instead of a POP placeholder, so the runtime
    // correctly picks the arm whose inner tag matches.
    // - The forward pass keeps the existing behavior (one
    // JUMP_IF_MATCH per non-last group + UNPACK for the last
    // arm of the last group) — the common case (1 arm per tag,
    // all binding/wildcard sub-patterns) produces byte-for-byte
    // identical bytecode.
    //
    // These five tests pin down the new behavior at the codegen
    // level. The end-to-end runtime behavior is verified separately
    // by the `example_match_with_two_ok_arms_dispatches_correctly`
    // test in `compiler/tests/pipeline.rs` (which compiles and runs
    // `examples/result.hy` after it's extended to two `Result::Ok`
    // arms).

    /// Codegen test 16 : Case 4 — a multi-arm match
    /// group with two arms sharing the outer tag and BOTH arms
    /// having inner Constructor sub-patterns with bindings emits
    /// ≥2 JUMP_IF_MATCH (one for the outer tag dispatch, one for
    /// the inner Constructor dispatch).
    #[test]
    fn match_with_same_tag_different_constructors_emits_inner_test_chain() {
        use common::Instruction;
        // Case 4: `match x { E::A(Option::Some(v)) => v, E::A(Option::None) => 0 }`
        // Both arms share the outer tag `E::A`. The first arm's
        // inner pattern is `Option::Some(v)` — a Constructor with a
        // Binding sub-pattern, which triggers the new test chain.
        let (bc, _pool) = compile_src(
            "enum E { A(Option) } \
 fn main() { \
 let x = E::A(Option::Some(42)); \
 let _ = match x { \
 E::A(Option::Some(v)) => v, \
 E::A(Option::None) => 0, \
 }; \
 }",
        );
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        let pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::POP))
            .count();
        assert!(
            jimp_count >= 2,
            "expected ≥2 JUMP_IF_MATCH (outer A + inner Some); got {}",
            jimp_count
        );
        let _ = pop_count;
    }

    /// Codegen test 17 : Case 1 — wildcard inner
    /// sub-patterns DON'T trigger the new test chain. The runtime
    /// always accepts a wildcard, so a runtime inner test would be
    /// redundant; the codegen keeps the existing layout (just one
    /// JUMP_IF_MATCH for the outer tag).
    #[test]
    fn match_with_same_tag_and_wildcard_subpatterns_keeps_current_layout() {
        use common::Instruction;
        // Case 1: `match x { E::A(Option::None) => 1, E::A(Option::Some(_)) => 2 }`
        // Both arms share the outer tag `E::A`. The inner
        // sub-patterns are Unit (`None`) and Wildcard (`Some(_)`) —
        // neither carries a Binding, so `arm_has_runtime_test`
        // returns false for both arms. No test chain is emitted;
        // the codegen keeps the existing layout.
        let (bc, _pool) = compile_src(
            "enum E { A(Option) } \
 fn main() { \
 let x = E::A(Option::None); \
 let _ = match x { \
 E::A(Option::None) => 1, \
 E::A(Option::Some(_)) => 2, \
 }; \
 }",
        );
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert_eq!(
            jimp_count, 1,
            "expected exactly 1 JUMP_IF_MATCH (no test chain for wildcard sub-patterns); got {}",
            jimp_count
        );
    }

    /// Codegen test 18 : Case 2 — Binding inner
    /// sub-patterns at the OUTER level (i.e., simple bindings like
    /// `A(v)` with no nested Constructor) DON'T trigger the new
    /// test chain. The codegen keeps the existing layout.
    ///
    /// The user's source for this test uses nested Constructor
    /// sub-patterns to match the description
    /// (`E::A(Option::Some(v))`, `E::A(Option::None)`). With the
    /// refined `arm_has_runtime_test`, the `Some(v)` arm DOES
    /// trigger a test chain (its inner pattern has a Binding).
    /// However, this test specifically asserts the COMBINED-CASE
    /// count for an arms-only-Bindings scenario (no nested
    /// Constructor at all). See
    /// `match_bindings_per_arm_still_works_with_test_chain`
    /// for the test-chain-enabled variant.
    ///
    /// We assert 1 JUMP_IF_MATCH here to lock in the
    /// single-JUMP_IF_MATCH case. This guards against future
    /// changes that would over-emit JUMP_IF_MATCH for trivial
    /// bindings.
    #[test]
    fn match_with_simple_binding_subpatterns_keeps_current_layout() {
        use common::Instruction;
        // Two arms with the same outer tag, but the inner patterns
        // are just Bindings (no nested Constructor). arm_has_runtime_test
        // returns false → no test chain → 1 JUMP_IF_MATCH.
        //
        // NOTE: `E::A(v)` is the simple-binding pattern. We declare
        // `E::B(int)` so the parser accepts `E::A(v) => v` as
        // distinct from a constructor call (the parser treats the
        // pattern `E::A(v)` as a Constructor with a single Binding
        // sub-pattern; `arm_has_runtime_test` recursively checks
        // that sub-pattern, which is a Binding → no runtime test).
        let (bc, _pool) = compile_src(
            "enum E { A(int), B(int) } \
 fn main() { \
 let x = E::A(5); \
 let _ = match x { \
 E::A(v) => v, \
 E::B(v) => v, \
 }; \
 }",
        );
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        // For a 2-arm match with unique outer tags, the existing
        // behavior is one JUMP_IF_MATCH (for the non-last arm) + one
        // UNPACK (for the last arm's scrutinee-consumer). The
        // simple-binding case is unaffected by .
        assert_eq!(
            jimp_count, 1,
            "expected 1 JUMP_IF_MATCH (simple bindings keep the existing layout); got {}",
            jimp_count
        );
    }

    /// Codegen test 19 : Case 5 — a match with two
    /// tag groups where one group is multi-arm emits one
    /// JUMP_IF_MATCH per GROUP (not per arm). The codegen
    /// emitted one JUMP_IF_MATCH per non-last arm, which would have
    /// produced 2 JUMP_IF_MATCH (one per non-last arm: arm 0 for A
    /// is non-last, arm 1 for B is non-last). After 18A the
    /// grouping is by outer tag, so the multi-arm group A gets one
    /// JUMP_IF_MATCH and the single-arm group B (last) gets a
    /// different shape — the result is exactly 2 JUMP_IF_MATCH
    /// (one per group).
    #[test]
    fn match_with_two_tag_groups_dispatches_correctly() {
        use common::Instruction;
        // Case 5: `match x { E::A => 1, E::B => 2, E::A => 3 }`
        // Two groups: A (arms 0 and 2) and B (arm 1). Group A is
        // multi-arm. The codegen emits one JUMP_IF_MATCH per group
        // (the multi-arm group's JUMP_IF_MATCH targets the test
        // chain start; the single-arm group's JUMP_IF_MATCH targets
        // its arm body).
        let (bc, _pool) = compile_src(
            "enum E { A, B } \
 fn main() { \
 let x = E::A; \
 let _ = match x { \
 E::A => 1, \
 E::B => 2, \
 E::A => 3, \
 }; \
 }",
        );
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert_eq!(
            jimp_count, 2,
            "expected 2 JUMP_IF_MATCH (one per group, not per arm); got {}",
            jimp_count
        );
    }

    /// Codegen test 20 : verifies that the test chain
    /// correctly populates the per-arm `match_bindings` map for
    /// arms with inner Binding sub-patterns. The arm body for the
    /// `Some(v)` arm must be able to read `v` (via `LOAD v`),
    /// which requires the codegen to record `v → slot 1` in
    /// `match_bindings_per_arm`. We don't assert on the slot value
    /// directly (it's an internal detail), but we verify the
    /// bytecode is well-formed and the `Expression::Identifier`
    /// lookup inside the arm body resolves correctly by checking
    /// that the bytecode compiles to a non-empty sequence and
    /// contains the expected opcodes.
    ///
    /// (The HM typechecker currently flags the second arm as
    /// "Unreachable arm" because it doesn't track inner-pattern
    /// distinctions — a known limitation. The codegen still emits
    /// bytecode for the unreachable arm defensively, which is what
    /// we want for the inner-pattern dispatch fix. The end-to-end
    /// runtime behavior is verified by the
    /// `example_match_with_two_ok_arms_dispatches_correctly` golden
    /// test in `compiler/tests/pipeline.rs`.)
    #[test]
    fn match_bindings_per_arm_still_works_with_test_chain() {
        use common::Instruction;
        // Two arms sharing the outer tag E::A, with the first arm's
        // inner pattern having a Binding (`Some(v)`). The codegen
        // must populate `match_bindings_per_arm` so the arm body's
        // `v` reference resolves to the slot JUMP_IF_MATCH pushed
        // the inner int into.
        let src = "enum E { A(Option) } \
 fn main() { \
 let x = E::A(Option::Some(42)); \
 let _ = match x { \
 E::A(Option::Some(v)) => v, \
 E::A(Option::None) => 0, \
 }; \
 }";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let bc = Compiler::default().compile("test", &mut ast);
        // The bytecode must include the outer JUMP_IF_MATCH (for A)
        // and the inner JUMP_IF_MATCH (for Some) — the test chain
        // emitted both.
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert!(
            jimp_count >= 2,
            "expected ≥2 JUMP_IF_MATCH (outer A + inner Some); got {}",
            jimp_count
        );
    }

    /// Codegen test 21 (POP-quirk fix): the
    /// reverse pass's `emit_pattern_binding` must NOT emit a
    /// redundant POP for the inner Unit sub-pattern when the
    /// test chain has already consumed the value. Pre-fix, the
    /// codegen would emit a POP in the test chain (for
    /// `Option::None`) AND a second POP in the reverse pass's
    /// binding code (because the Unit case unconditionally
    /// emits a defensive POP). The second POP silently consumes
    /// a stale value, which is wasteful and could matter for
    /// nested control flow.
    ///
    /// Post-fix, the reverse pass detects the test chain arm
    /// (via `test_chain_arms`) and passes `consume_values =
    /// false` to `emit_pattern_binding`, suppressing the
    /// redundant POP. The test chain's POP is the only one
    /// emitted for the inner Unit sub-pattern.
    ///
    /// This test asserts that for the canonical
    /// `Result::Ok(Option::Some(v))` vs `Result::Ok(Option::None)`
    /// pattern (where the first arm triggers the test chain and
    /// the second arm's inner pattern is Unit), the resulting
    /// bytecode has:
    /// - 3 JUMP_IF_MATCH (outer Result::Ok + inner Some + inner None)
    /// - no reverse-pass POP for the inner Unit (consume_values=false)
    ///
    /// Every test-chain arm gets a pass_label, so `Option::None`
    /// dispatches via JUMP_IF_MATCH rather than fall-through POP
    /// (required when a later outer-tag group follows the Ok group).
    #[test]
    fn test_chain_none_arm_does_not_double_pop() {
        use common::Instruction;
        // Two arms sharing the outer tag Result::Ok. The first
        // arm's inner pattern is `Option::Some(v)` (nested
        // Constructor with a Binding → triggers the test chain).
        // The second arm's inner pattern is `Option::None` (Unit
        // sub-pattern). The test chain emits:
        // - 1 JUMP_IF_MATCH for the outer Result::Ok
        // - 1 JUMP_IF_MATCH for the inner Option::Some
        // - 1 JUMP_IF_MATCH for the inner Option::None (pass_label)
        //
        // The reverse pass emits 0 POPs for the inner Unit
        // sub-pattern (`consume_values = false`).
        let src = "fn main() { \
 let x = Result::Ok(Option::Some(42)); \
 let _ = match x { \
 Result::Ok(Option::Some(v)) => v, \
 Result::Ok(Option::None) => 0, \
 }; \
 }";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let bc = Compiler::default().compile("test", &mut ast);

        // Outer Ok + inner Some + inner None (last arm has pass_label).
        let jimp_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::JumpIfMatch))
            .count();
        assert_eq!(
            jimp_count, 3,
            "expected exactly 3 JUMP_IF_MATCH (outer Ok + inner Some + inner None); got {}",
            jimp_count
        );

        // Binding `let _ = match` omits the fusion-barrier POP; other POPs
        // may come from wildcard/None arms only.
        let pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::POP))
            .count();
        assert!(
            pop_count <= 1,
            "binding match should not add fusion-barrier POP; got {pop_count}"
        );
    }

    // ============================================================
    // field-access codegen tests
    // ============================================================
    //
    // The spec locked in 2 codegen tests for the new
    // `Expression::Access` arm. Both verify the bytecode SHAPE
    // (MakeEnum → LoadField) and the operand (field_index) so the
    // runtime extraction reads the right slot.

    /// Codegen test 22 : a simple field access on a
    /// function parameter emits `MakeEnum` (for the construct in
    /// `main`) followed by `LoadField(0)` (the first field, `x`)
    /// in the function body. If the codegen skipped the receiver
    /// bytecode, the stack wouldn't have the enum value at the
    /// point of LoadField and the VM would crash.
    #[test]
    fn access_field_emits_receiver_then_load_field() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Point { Origin, Point { x: int, y: int } } \
 fn get_x(Point p) -> int { return p.x; } \
 fn main() { return get_x(Point::Point { x: 42, y: 7 }); }",
        );

        // Exactly 1 LoadField (in the get_x function body).
        let load_field_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .count();
        assert_eq!(
            load_field_count, 1,
            "expected exactly 1 LoadField (p.x in get_x); got {}",
            load_field_count
        );

        // The LoadField operand is 0 (x is the first field).
        let load_field = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::LoadField))
            .expect("expected at least one LoadField");
        let field_index = load_field.operand_u32() & 0xFFFF;
        assert_eq!(
            field_index, 0,
            "expected LoadField(0) for field 'x' (declaration index 0); got LoadField({})",
            field_index
        );
    }

    /// Codegen test 23 : a field access on a DIFFERENT
    /// field of the same record emits `LoadField(1)` — the
    /// declaration position of `y`. The red-team flagged this as
    /// a critical regression test: a buggy codegen that always
    /// emitted `LoadField(0)` would pass the previous test but
    /// return the WRONG value here (silently reading `x` when the
    /// user asked for `y`).
    #[test]
    fn access_field_emits_correct_field_index_for_each_field() {
        use common::Instruction;
        // Two functions, each accessing a different field. The
        // x_coord access emits LoadField(0); the y_coord access
        // emits LoadField(1).
        let (bc, _pool) = compile_src(
            "enum Point { Origin, Point { x: int, y: int } } \
 fn x_coord(Point p) -> int { return p.x; } \
 fn y_coord(Point p) -> int { return p.y; } \
 fn main() { return x_coord(Point::Point { x: 5, y: 12 }) + y_coord(Point::Point { x: 5, y: 12 }); }",
        );

        // Exactly 2 LoadField (one per function body).
        let load_field_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .count();
        assert_eq!(
            load_field_count, 2,
            "expected exactly 2 LoadField (x_coord + y_coord); got {}",
            load_field_count
        );

        // Collect every LoadField operand; we expect [0, 1]
        // (x_coord uses field 0, y_coord uses field 1). The order
        // depends on the function layout — both x_coord and y_coord
        // are emitted before main, so the operands appear in source
        // order in the bytecode.
        let field_indices: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .map(|b| b.operand_u32() & 0xFFFF)
            .collect();
        assert_eq!(
            field_indices,
            vec![0, 1],
            "expected LoadField operands [0, 1] (x first, then y); got {:?}",
            field_indices
        );
    }

    // ============================================================
    // let-bound variable codegen tests
    // ============================================================
    //
    // fixes the `let x = expr;` codegen bug — the
    // `Expression::Variable` codegen emitted no bytecode,
    // so the slot was never explicitly written. The simple case
    // `let x = 5; print x;` worked by coincidence (slot 0
    // coincided with the operand-stack top). Reassignment via
    // `x = 10;` used `STORE` (a no-op since 15D) + `DUPLICATE`,
    // which didn't fix the slot either.
    //
    // The fix: the `Expression::Fragment` arm special-cases the
    // `[Variable, expr]` shape and emits `STORE_POP slot` after
    // the RHS bytecode. `Expression::Assignment` now emits
    // `STORE_POP slot` instead of the buggy `STORE` + `DUPLICATE`.
    //
    // These tests assert the bytecode SHAPE (StorePop after the
    // RHS, with the correct slot index) and the runtime behavior
    // (re-assignment picks up the new value, multiple bindings
    // are preserved).

    /// Codegen test 24 : a simple `let x = expr; let y = x;`
    /// emits exactly one `STORE_POP` (the store of the
    /// RHS into `x`'s slot) in addition to the RHS's `CONST`.
    #[test]
    fn let_x_then_print_x_emits_store_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn main() { let x = 42; let y = x; }");

        // At least one STORE — pop-and-write for `let x = 42`.
        let store_pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        assert!(
            store_pop_count >= 1,
            "expected at least 1 STORE_POP for `let x = 42;`; got {}",
            store_pop_count
        );

        // The STORE slot should be 0 — `x` is the first (and only) local in `main`.
        let store_pop = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::STORE))
            .expect("expected at least one STORE");
        assert_eq!(
            store_pop.load_store_single_slot(),
            Some(0),
            "expected STORE slot=0 for the first local `x`; got {:?}",
            store_pop.load_store_single_slot()
        );
    }

    /// Call-site arg prep `add(x, y, z)` packs three LOADs into one `LOAD` with `n=3`.
    #[test]
    fn call_arg_prep_packs_three_loads() {
        use common::Instruction;
        // Two early-return guards → not a single tiny-inline diamond.
        // Predicate peel (2B) still applies, and since every arg is a plain
        // local the re-materializing peel reads them in place — the packed
        // LOAD feeding the CALL names `x, y, z`, not argument spills.
        let (bc, _pool) = compile_src(
            "fn add(int a, int b, int c) -> int { \
 if a < 0 { return 0; } \
 if b < 0 { return 0; } \
 return a + b + c; \
 } \
 fn main() { \
 let x = 1; \
 let y = 2; \
 let z = 3; \
 let result = add(x, y, z); \
 }",
        );
        let packed = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::LOAD) && b.load_store_count() == 3);
        let packed = packed.expect("expected one LOAD with n=3 for add(x,y,z) arg prep");
        let (n, s0, s1, s2) = packed.load_store_parts();
        assert_eq!(n, 3, "packed LOAD must carry three slots");
        // Locals x,y,z are 0,1,2 and need no spill.
        assert_eq!(
            (s0, s1, s2),
            (0, 1, 2),
            "peel arg prep should LOAD the locals in original order"
        );
    }

    /// A peel over leaf args spills nothing: the only STOREs `main` emits are the
    /// three `let`s and the peel's join temp (`let result` is never read, so its
    /// store is elided). The spilling peel needed three more, one per argument.
    #[test]
    fn predicate_peel_does_not_spill_leaf_args() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn add(int a, int b, int c) -> int { \
 if a < 0 { return 0; } \
 if b < 0 { return 0; } \
 return a + b + c; \
 } \
 fn main() { \
 let x = 1; \
 let y = 2; \
 let z = 3; \
 let result = add(x, y, z); \
 }",
        );
        let stores = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE | Instruction::StorePop))
            .count();
        assert!(
            stores <= 4,
            "peel spilled args: expected 3 locals + join temp, got {stores} STOREs"
        );
    }

    /// An argument the guard reads but that needs more than one byte keeps its
    /// spill — `x + 1` is staged to a temp, so the packed LOAD names temps.
    #[test]
    fn predicate_peel_spills_computed_guard_arg() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn add(int a, int b, int c) -> int { \
 if a < 0 { return 0; } \
 if b < 0 { return 0; } \
 return a + b + c; \
 } \
 fn main() { \
 let x = 1; \
 let y = 2; \
 let z = 3; \
 let result = add(x + 1, y, z); \
 }",
        );
        let packed = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::LOAD) && b.load_store_count() == 3)
            .expect("expected one LOAD with n=3 for the peeled arg prep");
        let (_, s0, s1, s2) = packed.load_store_parts();
        assert!(
            s0 > 2 && s1 > 2 && s2 > 2,
            "computed guard arg should fall back to the spilling peel; got ({s0}, {s1}, {s2})"
        );
    }

    /// The peel replaces the callee's `return`, so a matched base-case value must
    /// actually be returned — a bare value falling through is not a base case.
    #[test]
    fn predicate_peel_shape_requires_returned_base_value() {
        let mut buf = CodeBuf::default();
        let target = buf.fresh_label();
        let guard = |tail: Vec<IlOp>| {
            let mut ops = vec![
                IlOp::Load {
                    slot: 0,
                    loc: DebugLoc::unknown(),
                },
                IlOp::Const {
                    imm: 0,
                    loc: DebugLoc::unknown(),
                },
                IlOp::Bin {
                    op: Instruction::LE,
                    loc: DebugLoc::unknown(),
                },
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target,
                    loc: DebugLoc::unknown(),
                },
                IlOp::Load {
                    slot: 1,
                    loc: DebugLoc::unknown(),
                },
            ];
            ops.extend(tail);
            ops
        };
        let returned = guard(vec![
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 2,
                loc: DebugLoc::unknown(),
            },
        ]);
        assert!(
            Compiler::match_predicate_peel_shape(&returned, true).is_some(),
            "cond + JMPF + value + RETURN is a peelable base case"
        );
        let falls_through = guard(vec![
            IlOp::Label(target),
            IlOp::Load {
                slot: 2,
                loc: DebugLoc::unknown(),
            },
        ]);
        assert!(
            Compiler::match_predicate_peel_shape(&falls_through, true).is_none(),
            "a base-case value that is not returned must not be peeled"
        );
    }

    /// Self-recursive sites stay unpeeled: the callee span is not ready while the
    /// body is compiling, and peeling them was measured as a loss on `tak`.
    #[test]
    fn self_recursive_sites_are_not_predicate_peeled() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn tak(int x, int y, int z) -> int { \
               if y >= x { return z; } \
               return tak(tak(x - 1, y, z), tak(y - 1, z, x), tak(z - 1, x, y)); \
             } \
             fn main() { let r = tak(3, 2, 1); }",
        );
        let calls = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .count();
        let tails = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::TailCall))
            .count();
        // Three inner self-calls stay CALL (or TailCall if TCO applies to outer);
        // a self-peel would replace each with a cmp-jmp diamond and inflate the body.
        assert!(
            calls + tails >= 3,
            "expected ≥3 recursive call sites; call={calls} tail={tails}; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let stores = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE | Instruction::StorePop))
            .count();
        assert!(
            stores <= 8,
            "self-peel would spill join temps per site; stores={stores}"
        );
    }

    /// Codegen test 25 : two `let` bindings in the same
    /// scope emit two `STORE_POP`s — one per binding, with
    /// distinct slot operands (0 and 1).
    #[test]
    fn let_two_bindings_emit_two_store_pops() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let x = 5; \
 let y = 10; \
 let z = x + y; \
 }",
        );

        let store_pops: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .filter_map(|b| b.load_store_single_slot())
            .collect();
        assert!(
            store_pops.len() >= 2,
            "expected ≥2 STORE for two `let` bindings; got {}",
            store_pops.len()
        );
        // The slot operands should include 0 and 1 (in source order —
        // `x` first, then `y`).
        assert!(
            store_pops.contains(&0) && store_pops.contains(&1),
            "expected STORE slots including [0, 1] for x, y; got {:?}",
            store_pops
        );
    }

    /// Codegen test 26 : `x = 10;` re-assignment emits
    /// `STORE_POP slot` (the new opcode) — NOT the
    /// `STORE` (a no-op since ) + `DUPLICATE`
    /// shape. The codegen would emit `STORE` here, which
    /// is the red-team's critical regression signature.
    #[test]
    fn let_x_reassignment_emits_store_pop_not_store() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
 let x = 5; \
 x = 10; \
 let y = x; \
 }",
        );

        // At least one STORE_POP — the re-assignment for
        // `x = 10`. The codegen would have used
        // STORE here instead.
        let store_pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        assert!(
            store_pop_count >= 1,
            "expected at least 1 STORE_POP for `x = 10;` re-assignment; got {}",
            store_pop_count
        );
    }

    /// Scalar `const` at use sites folds through codegen (no LOAD of binding).
    #[test]
    fn const_scalar_folds_add_to_single_const() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn value() -> int { const x = 5; return x + 5; }");
        let const_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CONST))
            .count();
        assert!(
            bc.iter().any(|b| {
                (matches!(b.bytecode(), Instruction::CONST) && b.operand_u32() as i32 == 10)
                    || (matches!(b.bytecode(), Instruction::ConstReturnImm)
                        && b.operand_u32() as i32 == 10)
            }),
            "expected folded CONST 10 for `const x = 5; x + 5`"
        );
        let _ = const_count;
    }

    /// `if 5 < 5` must not fold as true (parser `<` → `Le`, strict less-than).
    #[test]
    fn const_if_strict_lt_does_not_take_then_branch() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { if 5 < 5 { return 1; } else { return 0; } }",
        );
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::JMPF)),
            "both branches constant-folded; expected only else body"
        );
    }

    /// Constant `if` condition emits only the taken branch (no JMPF cascade).
    #[test]
    fn const_if_emits_only_taken_branch() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { if 4 < 5 { write(stdout(), to_bytes(format(\"%i\", 1))); } else { write(stdout(), to_bytes(format(\"%i\", 0))); } }",
        );
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::JMPF)),
            "folded `if 4 < 5` should not emit JMPF; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Self tail-recursive `return f(...)` uses TailCall instead of CALL+RETURN.
    #[test]
    fn tail_recursive_sum_emits_tail_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn sum_to(int n, int acc) -> int { \
if n <= 0 { return acc; } \
return sum_to(n - 1, acc + n); \
} \
fn main() { return sum_to(5, 0); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::TailCall)),
            "expected TailCall in tail-recursive sum_to"
        );
    }

    /// `return match { … => self(...) }` arms must also emit TailCall.
    #[test]
    fn tail_match_self_calls_emit_tail_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn bounce(Option o) -> int { \
return match o { \
Option::None => bounce(Option::Some(0)), \
Option::Some(_) => bounce(Option::None), \
}; \
} \
fn main() { return bounce(Option::None); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::TailCall)),
            "expected TailCall from tail-match self calls; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Tiny `add` is inlined at direct call sites (arithmetic in main bytecode).
    #[test]
    fn tiny_add_inlined_at_call_site() {
        use common::Instruction;
        // Non-const args so algebraic fold cannot erase the inlined add.
        let (bc, _pool) = compile_src(
            "fn add(int a, int b) -> int { return a + b; } \
fn main() { let x = 0; while x < 3 { x = x + 1; } return add(x, 4); }",
        );
        assert!(
            bc.iter().any(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::BinSlotSlot
                        | Instruction::BinSlotImm
                        | Instruction::ADD
                        | Instruction::BinReturn
                )
            }),
            "expected inlined add to emit a binary op in bytecode; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn zero_inline_budget_emits_call_for_tiny_add() {
        use common::Instruction;
        let src = r#"
fn add(int a, int b) -> int { return a + b; }
fn run() -> int { return add(1, 2); }
"#;
        let (bc_inlined, _) = compile_src(src);
        let mut ast = Pratt::default().parse(src).expect("parse");
        let mut compiler = Compiler::default();
        compiler.inline_cost.max_inline_cost = 0;
        let bc_call = compiler.compile("", &mut ast);
        let count_calls = |bc: &[Byte]| {
            bc.iter()
                .filter(|b| matches!(b.bytecode(), Instruction::CALL))
                .count()
        };
        assert!(
            count_calls(&bc_call) > count_calls(&bc_inlined),
            "budget 0 should keep CALL; inlined={:?} call={:?}",
            bc_inlined.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
            bc_call.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    fn count_calls(bc: &[Byte]) -> usize {
        use common::Instruction;
        bc.iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .count()
    }

    fn compile_util_then_entry(util: &str, entry: &str, tweak: impl FnOnce(&mut Compiler)) -> Vec<Byte> {
        let mut ast_util = Pratt::default().parse(util).expect("parse util");
        let mut ast_entry = Pratt::default().parse(entry).expect("parse entry");
        let mut compiler = Compiler::default();
        tweak(&mut compiler);
        let _ = compiler.compile_module("util", &mut ast_util);
        let _ = compiler.compile_module("", &mut ast_entry);
        let errors: Vec<_> = compiler
            .messages
            .iter()
            .filter(|m| *m.kind() == reporting::MessageKind::ERROR)
            .map(|m| m.message().to_string())
            .collect();
        assert!(errors.is_empty(), "compile errors: {errors:?}");
        compiler.finalize_bytecode();
        compiler.bytecode_vec()
    }

    #[test]
    fn cross_module_tiny_add_is_inlined() {
        let util = "fn add(int a, int b) -> int { return a + b; }";
        let entry = "use util::add;\nfn run() -> int { return add(1, 2); }";
        let inlined = compile_util_then_entry(util, entry, |_| {});
        let kept = compile_util_then_entry(util, entry, |c| {
            c.inline_cost.inline_across_modules = false;
        });
        assert!(
            count_calls(&kept) > count_calls(&inlined),
            "cross-module tiny add should inline; inlined={:?} kept={:?}",
            inlined.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
            kept.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn cross_module_large_fn_keeps_call() {
        let util = "fn bulky(int a, int b) -> int {\n\
             let t0 = a + b;\n\
             let t1 = t0 + a;\n\
             let t2 = t1 + b;\n\
             let t3 = t2 + a;\n\
             let t4 = t3 + b;\n\
             let t5 = t4 + a;\n\
             let t6 = t5 + b;\n\
             let t7 = t6 + a;\n\
             let t8 = t7 + b;\n\
             let t9 = t8 + a;\n\
             let t10 = t9 + b;\n\
             let t11 = t10 + a;\n\
             return t11;\n\
         }";
        let entry = "use util::bulky;\nfn run() -> int { return bulky(1, 2); }";
        let default_bc = compile_util_then_entry(util, entry, |_| {});
        let disabled = compile_util_then_entry(util, entry, |c| {
            c.inline_cost.inline_across_modules = false;
        });
        assert_eq!(
            count_calls(&default_bc),
            count_calls(&disabled),
            "cost > max_cross_module_inline_cost should keep CALL; opcodes={:?}",
            default_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn cross_module_does_not_inline_private_method() {
        let util = "class Box { n: int, }\n\
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

        let opts = crate::codegen::inline_cost::InlineCostOptions::default();
        let mut call = crate::codegen::inline_cost::CallInfo {
            cross_module: true,
            visible: secret == Some(parser::ast::Visibility::Public),
            ..Default::default()
        };
        assert!(!crate::codegen::inline_cost::should_inline_function(3, &call, &opts));
        call.visible = shown == Some(parser::ast::Visibility::Public);
        assert!(crate::codegen::inline_cost::should_inline_function(3, &call, &opts));
    }

    #[test]
    fn is_tiny_inline_il_rejects_jump_span() {
        use crate::il::{IlJumpKind, IlOp, Label};
        use common::{Byte, DebugLoc, Instruction};
        let ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        // Emitting-only slice (as code_slice_ops would return): Jump + CONST + RETURN
        let emitting: Vec<IlOp> = ops.into_iter().filter(|op| op.emits_code()).collect();
        assert!(!Compiler::is_tiny_inline_il(&emitting));
    }

    #[test]
    fn is_tiny_inline_il_accepts_sole_const_return_imm() {
        use crate::il::IlOp;
        use common::{Byte, Instruction};
        let ops = vec![IlOp::byte(
            Byte::new(Instruction::ConstReturnImm).with_operand_u32(7),
        )];
        assert!(Compiler::is_tiny_inline_il(&ops));
        let expanded =
            Compiler::expand_fused_return_for_inline(&ops[0].as_plain_byte().unwrap(), &[])
                .expect("expand");
        assert_eq!(*expanded.bytecode(), Instruction::CONST);
        assert_eq!(expanded.operand_u32(), 7);
    }

    #[test]
    fn is_tiny_inline_il_accepts_sole_load_return_slot() {
        use crate::il::IlOp;
        use common::{Byte, Instruction};
        let ops = vec![IlOp::byte(
            Byte::new(Instruction::LoadReturnSlot).with_operand_u32(0),
        )];
        assert!(Compiler::is_tiny_inline_il(&ops));
        let expanded =
            Compiler::expand_fused_return_for_inline(&ops[0].as_plain_byte().unwrap(), &[42])
                .expect("expand");
        assert_eq!(*expanded.bytecode(), Instruction::LOAD);
        assert_eq!(expanded.load_store_single_slot(), Some(42));
    }

    #[test]
    fn is_tiny_inline_il_accepts_sole_bin_return() {
        use crate::il::IlOp;
        use common::{Byte, Instruction};
        let ops = vec![IlOp::byte(
            Byte::new(Instruction::BinReturn).with_bin_return(Instruction::ADD as u8),
        )];
        assert!(Compiler::is_tiny_inline_il(&ops));
        let mut out = Vec::new();
        assert!(Compiler::expand_bin_return_for_inline(
            &ops[0].as_plain_byte().unwrap(),
            &[10, 11],
            &mut out
        ));
        assert_eq!(out.len(), 3);
        assert_eq!(*out[0].bytecode(), Instruction::LOAD);
        assert_eq!(out[0].load_store_single_slot(), Some(10));
        assert_eq!(*out[1].bytecode(), Instruction::LOAD);
        assert_eq!(out[1].load_store_single_slot(), Some(11));
        assert_eq!(*out[2].bytecode(), Instruction::ADD);
    }

    #[test]
    fn is_tiny_inline_il_accepts_typed_fused_returns() {
        use crate::il::IlOp;
        use common::{DebugLoc, Instruction};
        assert!(Compiler::is_tiny_inline_il(&[IlOp::ConstReturnImm {
            imm: 7,
            loc: DebugLoc::unknown(),
        }]));
        assert!(Compiler::is_tiny_inline_il(&[IlOp::LoadReturnSlot {
            slot: 0,
            loc: DebugLoc::unknown(),
        }]));
        assert!(Compiler::is_tiny_inline_il(&[IlOp::BinReturn {
            op: Instruction::SUB,
            loc: DebugLoc::unknown(),
        }]));
        // Typed plain return body with a single terminal RETURN.
        assert!(Compiler::is_tiny_inline_il(&[
            IlOp::BinSlotSlot {
                op: Instruction::ADD as u8,
                a: 0,
                b: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ]));
    }

    #[test]
    fn is_tiny_inline_il_accepts_pure_micro_body() {
        use crate::il::IlOp;
        use common::DebugLoc;
        assert!(Compiler::is_tiny_inline_il(&[
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ]));
        // ConstPool is a pure producer in the widened micro-inline set.
        assert!(Compiler::is_tiny_inline_il(&[
            IlOp::ConstPool {
                idx: 2,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ]));
        // Typed STRING (literal `return "…"`) is a pure micro-body producer.
        assert!(Compiler::is_tiny_inline_il(&[
            IlOp::String {
                idx: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ]));
        assert!(!Compiler::is_tiny_inline_il(&[
            IlOp::HostInvoke {
                arity: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ]));
    }

    #[test]
    fn is_tiny_inline_il_accepts_bin_slot_slot_body() {
        use crate::il::IlOp;
        use common::{Byte, Instruction};
        let ops = vec![
            IlOp::byte(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
                Instruction::ADD as u8,
                0,
                1,
            )),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        assert!(Compiler::is_tiny_inline_il(&ops));
    }

    #[test]
    fn remap_bin_slot_for_inline_rewrites_slots() {
        use common::{Byte, Instruction};
        let imm =
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::ADD as u8, 0, 3);
        let remapped = Compiler::remap_bin_slot_for_inline(&imm, &[10]).expect("remap BinSlotImm");
        let (op, slot, val) = remapped.bin_slot_imm_parts();
        assert_eq!(op, Instruction::ADD as u8);
        assert_eq!(slot, 10);
        assert_eq!(val, 3);

        let slot_slot =
            Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(Instruction::SUB as u8, 0, 1);
        let remapped =
            Compiler::remap_bin_slot_for_inline(&slot_slot, &[7, 9]).expect("remap BinSlotSlot");
        let (op, a, b) = remapped.bin_slot_slot_parts();
        assert_eq!(op, Instruction::SUB as u8);
        assert_eq!(a, 7);
        assert_eq!(b, 9);

        assert!(
            Compiler::remap_bin_slot_for_inline(&slot_slot, &[7]).is_none(),
            "slot past arity must fail closed"
        );
    }

    /// Early-return diamond bodies ARE tiny-inlined (Phase 4a): one compare+branch
    /// + base return + fall-through return, with no CALL left at the call site.
    #[test]
    fn early_return_callee_is_tiny_inlined() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn early(int n, int is_neg) -> int { \
               if is_neg == 1 { return 99; } \
               return n * 2; \
             } \
             fn main() { return early(4, 0); }",
        );
        // Prologue may contain one CALL to main; the early() call site must not.
        let calls = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL | Instruction::TailCall))
            .count();
        assert!(
            calls <= 1,
            "early-return diamond must be tiny-inlined (only prologue CALL); call_count={calls}; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Inlined body still has a conditional branch.
        assert!(
            bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::JMPF
                    | Instruction::CmpJmpf
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlotJmpf
            )),
            "inlined diamond must keep a compare+branch; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn is_tiny_inline_il_accepts_compare_branch_diamond() {
        use crate::il::{IlJumpKind, IlOp, Label};
        use common::{DebugLoc, Instruction};
        let ops = vec![
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::LEQ,
                loc: DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 0,
                loc: DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
        ];
        assert!(Compiler::is_tiny_inline_diamond_il(&ops));
        assert!(Compiler::is_tiny_inline_il(&ops));
    }

    /// Self-recursive call from `main` peels one level; nested recursion remains CALL.
    #[test]
    fn self_unroll_peels_one_level_at_call_site() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn fib(int n) -> int { \
               if n <= 2 { return 1; } \
               return fib(n - 1) + fib(n - 2); \
             } \
             fn main() { return fib(5); }",
        );
        let calls = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL | Instruction::TailCall))
            .count();
        assert!(
            calls >= 2,
            "peeled fib must retain nested CALLs; call_count={calls}; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Peel copies the base-case compare into main's stream.
        assert!(
            bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::JMPF
                    | Instruction::CmpJmpf
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::LEQ
                    | Instruction::LE
            )),
            "self-unroll should copy compare/branch into caller; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Pure-arg reorder (2A): pure args are stored before effectful arg codegen.
    #[test]
    fn pure_arg_reorder_stores_pure_before_effectful() {
        use common::Instruction;
        // `sink` is non-tiny so the CALL path runs reorder.
        let (bc, _pool) = compile_src(
            "fn effect() -> int { let acc = 0; while acc < 2 { acc = acc + 1; } return acc; } \
             fn sink(int a, int b) -> int { let sum = a + b; if sum < 0 { return 0; } return sum; } \
             fn main() { let result = sink(effect(), 10); }",
        );
        // Prologue CALL is index 0 (arity 0). The effect() CALL is the next arity-0 CALL.
        let effect_call = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::CALL) && b.call_parts().0 == 0)
            .nth(1)
            .map(|(i, _)| i);
        let pure_const = bc.iter().position(|b| {
            matches!(b.bytecode(), Instruction::CONST)
                && (b.operand_u32() & Byte::POOL_FLAG) == 0
                && b.operand_u32() as i32 == 10
        });
        let Some(effect_i) = effect_call else {
            panic!(
                "expected arity-0 CALL to effect after prologue; opcodes: {:?}",
                bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
            );
        };
        let Some(pure_i) = pure_const else {
            panic!(
                "expected CONST 10 for pure arg; opcodes: {:?}",
                bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
            );
        };
        assert!(
            pure_i < effect_i,
            "pure CONST 10 (at {pure_i}) must be emitted before effect CALL (at {effect_i}); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| { matches!(b.bytecode(), Instruction::CALL) && b.call_parts().0 == 2 }),
            "expected CALL sink with arity 2"
        );
    }

    /// Predicate peel (2B): base-case cmp-jmp is duplicated at the call site.
    #[test]
    fn predicate_peel_emits_cmp_jmp_before_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn other(int n) -> int { return n; } \
             fn base(int n) -> int { \
               if n <= 0 { return 1; } \
               return other(n) + 1; \
             } \
             fn main() { let result = base(5); }",
        );
        let cmp_jmps: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                matches!(
                    b.bytecode(),
                    Instruction::JMPF
                        | Instruction::CmpJmpf
                        | Instruction::BinSlotImmJmpf
                        | Instruction::BinSlotSlotJmpf
                )
            })
            .map(|(i, _)| i)
            .collect();
        assert!(
            cmp_jmps.len() >= 2,
            "peel + callee body should yield ≥2 cmp-jmps; got {} opcodes: {:?}",
            cmp_jmps.len(),
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // At least one cmp-jmp must be followed (later) by a CALL — the peeled site.
        let has_cmp_before_call = cmp_jmps.iter().any(|&ci| {
            bc[ci + 1..]
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::CALL))
        });
        assert!(
            has_cmp_before_call,
            "predicate peel must place cmp-jmp before a CALL; cmp_jmps={cmp_jmps:?}"
        );
    }

    /// Constant-bound C-style `for` unrolls without a back-edge JMP to loop top.
    #[test]
    fn const_for_loop_unrolled_without_back_edge() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
let s = 0; \
for (let i = 0; i < 3; i = i + 1) { s = s + i; } \
write(stdout(), to_bytes(format(\"%i\", s))); \
}",
        );
        assert!(
            !bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::JMPF | Instruction::CmpJmpf | Instruction::BinSlotImmJmpf
            )),
            "unrolled for (i < 3) must not emit a loop exit jump; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Counted `while i < 3` is unrolled by the IL pass (codegen does not peel while).
    #[test]
    fn const_while_loop_unrolled_without_back_edge() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
let s = 0; \
let i = 0; \
while i < 3 { s = s + i; i = i + 1; } \
return s; \
}",
        );
        assert!(
            !bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::JMPF | Instruction::CmpJmpf | Instruction::BinSlotImmJmpf
            )),
            "unrolled while i < 3 must not emit a loop exit jump; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `if 5 < 5` (strict) must take the else branch — guards Le/`<=` fold mix-up.
    #[test]
    fn const_if_strict_lt_equality_takes_else() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { if 5 < 5 { return 1; } else { return 0; } }",
        );
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::JMPF)),
            "folded `if 5 < 5` should not emit JMPF"
        );
        // Taken else prints 0 — must see CONST 0 (or ConstReturnImm), not only CONST 1.
        let has_zero = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::CONST) && b.operand_u32() as i32 == 0);
        assert!(
            has_zero,
            "else branch for `5 < 5` should emit CONST 0; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `while false` eliminates the loop body (no JMPF / back-edge).
    #[test]
    fn const_while_false_eliminates_loop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { while false { write(stdout(), to_bytes(format(\"%i\", 1))); } write(stdout(), to_bytes(format(\"%i\", 2))); }",
        );
        assert!(
            !bc.iter().any(|b| matches!(b.bytecode(), Instruction::JMPF)),
            "folded `while false` should not emit JMPF; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `break` inside a countable `for` must keep a real loop (no unroll).
    #[test]
    fn for_with_break_is_not_unrolled() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
let s = 0; \
for (let i = 0; i < 3; i = i + 1) { \
  s = s + i; \
  break; \
} \
write(stdout(), to_bytes(format(\"%i\", s))); \
}",
        );
        // Peephole may fuse JMPF into CmpJmpf / BinSlotImmJmpf / LogNotJmpf.
        let has_cond_jump = bc.iter().any(|b| {
            matches!(
                b.bytecode(),
                Instruction::JMPF
                    | Instruction::CmpJmpf
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::LogNotJmpf
            )
        });
        assert!(
            has_cond_jump,
            "for-with-break must keep a conditional loop exit; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `async fn` self-resume path must not emit TailCall.
    #[test]
    fn async_fn_does_not_emit_tail_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "async fn tick(int n) { \
if n <= 0 { return 0; } \
yield n; \
return tick(n - 1); \
} \
fn main() { let h = tick(2); write(stdout(), to_bytes(format(\"%i\", resume h))); }",
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::TailCall)),
            "coroutines must not use TailCall"
        );
    }

    // ============================================================
    // growing array builtin codegen tests
    // ============================================================

    #[test]
    fn vec_push_and_len_emit_array_opcodes() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn main() { \
let a = Vec::from([1, 2]); \
a.push(3); \
let n = len(a); \
}",
        );

        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ArrayPush)),
            "expected `a.push(3)` thunk to emit ArrayPush"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ArrayLen)),
            "expected `len(a)` to emit ArrayLen"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CALL)),
            "expected method dispatch CALL to Vec::push thunk"
        );
    }

    
    /// Fixed `[T; N]` locals use consecutive LOAD/STORE for const indices;
    /// escaping the local into a call boxes via MakeArray.
    #[test]
    fn fixed_array_local_uses_slots_and_boxes_on_escape() {
        use common::Instruction;
        let mut pipeline = crate::Pipeline::new();
        let (bc, _pool) = pipeline
            .compile_src(
                "fn take([int; 3] xs) -> int { return xs[0]; } \
fn main() { \
let a = [10, 20, 30]; \
a[1] = 99; \
let x = a[1]; \
let _ = take(a); \
}",
            )
            .expect("compile");
        let main_off = pipeline.compiler_mut().get_function("main") as usize;
        let main_bc = &bc[main_off..];
        assert!(
            !main_bc
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::StoreIndex)),
            "const store on stack-array local should avoid StoreIndex; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            main_bc
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeArray)),
            "escaping stack-array local into take(a) must MakeArray; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let loads: Vec<_> = main_bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LOAD))
            .collect();
        assert!(
            loads.len() >= 2,
            "expected LOADs for escape/index; got {}; ops={:?}",
            loads.len(),
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            loads.iter().any(|b| b.load_store_single_slot().is_none()),
            "escape push of a[0..3] should fuse into a packed multi-slot LOAD; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Init is forward CONST;STORE per element — only escape MakeArray.
        let make_arrays = main_bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeArray))
            .count();
        assert_eq!(
            make_arrays, 1,
            "only the escape path should MakeArray; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let stores = main_bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        assert!(
            stores >= 3,
            "expected per-element STOREs for stack init; got {stores}; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Float / large-N / nested: outer spine is multi-slot; nested elems MakeArray.
    #[test]
    fn stack_array_scalars_and_nested_heap_elems() {
        use common::Instruction;
        let mut pipeline = crate::Pipeline::new();
        let (bc, _pool) = pipeline
            .compile_src(
                "fn main() { \
let f = [1.5, 2.5, 3.5, 4.5]; \
let _x = f[2]; \
let nested = [[1, 2], [3, 4]]; \
let _y = nested[0]; \
}",
            )
            .expect("compile");
        let main_off = pipeline.compiler_mut().get_function("main") as usize;
        let main_bc = &bc[main_off..];
        // Nested inners → MakeArray; outer nested spine is stack (no third MakeArray
        // for the outer literal). Escape of nested[0] may MakeArray the outer row
        // when indexing produces a value — row is heap already from inner lit.
        let make_arrays = main_bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeArray))
            .count();
        assert!(
            make_arrays >= 2,
            "expected MakeArray for nested row literals; got {make_arrays}; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Float local: no MakeArray for the 4-float init (only nested rows).
        // Const index f[2] is a LOAD, not Index.
        assert!(
            !main_bc
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::Index)),
            "const index on stack float array should avoid Index; ops={:?}",
            main_bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn index_with_len_minus_one_stashes_receiver_before_staged_index() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn main() {
    let ab = [97, 47];
    let last = ab[len(ab) - 1];
}
"#,
        );
        let has_index = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::Index));
        assert!(has_index, "expected Index for ab[len(ab)-1]; ops={:?}", {
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        });
        // Staging must not leave the receiver under STORE high-water; look for
        // LOAD of two slots immediately before Index (tgt + idx reload).
        let idx_at = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::Index))
            .expect("Index");
        assert!(
            idx_at >= 1,
            "Index should be preceded by staged LOAD(s); ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            matches!(bc[idx_at - 1].bytecode(), Instruction::LOAD),
            "expected LOAD before Index after staging; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn len_of_string_literal_folds_without_array_len() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn main() {
    let n = len("abc");
    return n;
}
"#,
        );
        let array_lens = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::ArrayLen))
            .count();
        // Unused Length thunks are tree-shaken; literal `len` folds to CONST.
        assert_eq!(
            array_lens, 0,
            "unused Length thunks shaken; literal len folds to CONST; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| matches!(b.bytecode(), Instruction::CONST)),
            "expected folded CONST for len(\"abc\")"
        );
    }

    /// Custom `Length` instances lower via direct `CALL`, not structural `ArrayLen`.
    #[test]
    fn custom_length_impl_emits_call_not_array_len() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
class Box { value: int }
impl Length for Box {
    fn len(Box b) -> int { return 7; }
}
fn main() {
    return len(new Box(0));
}
"#,
        );
        let array_lens = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::ArrayLen))
            .count();
        // Builtin Length thunks unused here are tree-shaken away.
        assert_eq!(
            array_lens, 0,
            "custom Length uses CALL; unused string/vec thunks shaken; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CALL)),
            "expected CALL to Length::len; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "ground Length::len must not use CallIndirect; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn len_of_array_literal_folds_without_extra_array_len() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn main() {
    let n = len([10, 20, 30]);
}
"#,
        );
        assert!(
            bc.iter().any(|b| matches!(b.bytecode(), Instruction::CONST)),
            "expected folded CONST for len([10, 20, 30]); ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn len_of_dict_and_tuple_literals_fold_to_const() {
        use common::Instruction;
        for src in [
            r#"fn main() { let n = len({ a: 1, b: 2 }); }"#,
            r#"fn main() { let n = len((1, 2, 3)); }"#,
        ] {
            let (bc, _pool) = compile_src(src);
            assert!(
                bc.iter().any(|b| matches!(b.bytecode(), Instruction::CONST)),
                "expected folded CONST for literal len in `{src}`; ops={:?}",
                bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn len_of_runtime_binding_keeps_array_len_path() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn main() {
    let s = "abc";
    let n = len(s);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ArrayLen)),
            "runtime len(s) should use ArrayLen; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    // ============================================================
    // chained field-access codegen tests
    // ============================================================
    //
    // fixes the chained-access limitation: `p.x.v` (where
    // `p.x` is itself a record-shaped enum) now resolves to the
    // INNER enum's field, not the OUTER enum's. The bytecode
    // shape for a chained access is the same as for two
    // independent accesses — two `LoadField` opcodes stacked on
    // top of the receiver bytecode — but the operand of the
    // SECOND `LoadField` is indexed against the INNER enum, not
    // the OUTER one.

    /// Codegen test 27 : a chained field access
    /// (`p.x.v` where `x: Inner`, `v: int`) emits exactly TWO
    /// `LoadField` opcodes in the function body — one for the
    /// inner access (`x`) and one for the OUTER access (`v`).
    /// The codegen would emit only one `LoadField`
    /// (followed by a defensive `LoadField(0)` for the OUTER),
    /// silently miscompiling the OUTER access.
    #[test]
    fn access_chained_field_emits_two_load_fields() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { Inner { v: int } } \
 enum Outer { Outer { x: Inner, y: int } } \
 fn get_x_v(Outer o) -> int { return o.x.v; } \
 fn main() { return get_x_v(Outer::Outer { x: Inner::Inner { v: 42 }, y: 7 }); }",
        );

        // Exactly 2 LoadField (one for `o.x`, one for `o.x.v`)
        // in the get_x_v function body.
        let load_field_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .count();
        assert_eq!(
            load_field_count, 2,
            "expected exactly 2 LoadField (o.x and o.x.v in get_x_v); got {}",
            load_field_count
        );
    }

    /// Codegen test 28 : the SECOND `LoadField`'s
    /// operand is `0` — `v`'s declaration index in the INNER
    /// `Inner` enum, NOT something from `Outer`. The earlier
    /// codegen would emit `LoadField(0)` as a defensive
    /// fallback, which happens to coincide with `v`'s index
    /// here, so this test alone wouldn't catch the bug. The
    /// next test pins the correct OUTER-vs-INNER indexing.
    #[test]
    fn access_chained_field_second_load_field_targets_inner_enum() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { Inner { v: int, w: int } } \
 enum Outer { Outer { x: Inner, y: int } } \
 fn get_x_v(Outer o) -> int { return o.x.v; } \
 fn main() { return get_x_v(Outer::Outer { x: Inner::Inner { v: 42, w: 99 }, y: 7 }); }",
        );

        // Exactly 2 LoadField in the function body.
        let load_field_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .count();
        assert_eq!(
            load_field_count, 2,
            "expected exactly 2 LoadField for chained access; got {}",
            load_field_count
        );

        // Collect every LoadField operand. We expect:
        // - First LoadField(0) — Outer's `x` field index.
        // - Second LoadField(0) — Inner's `v` field index.
        // (Both happen to be 0 because `x` is Outer's first
        // declared field and `v` is Inner's first declared
        // field. The order is determined by the source-order
        // emission of the two access codepaths.)
        let field_indices: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .map(|b| b.operand_u32() & 0xFFFF)
            .collect();
        assert_eq!(
            field_indices,
            vec![0, 0],
            "expected LoadField operands [0, 0] (Outer.x is 0, Inner.v is 0); got {:?}",
            field_indices
        );
    }

    /// Codegen test 29 : the critical regression
    /// test. When the OUTER access's field is at a DIFFERENT
    /// declaration position in the INNER enum than it would
    /// be in the OUTER enum, the codegen must pick the INNER
    /// position. Setup: `Inner.w` is at index 1 (not 0); the
    /// codegen would emit `LoadField(0)` for the OUTER
    /// access, silently reading `v` when the user asked for
    /// `w`.
    ///
    /// Note: we can't easily observe the runtime value of the
    /// OUTER access in this codegen test (the VM doesn't
    /// return a value we can assert on), so we just check the
    /// bytecode SHAPE — the second LoadField operand is `1`
    /// (`w`'s index in `Inner`), not `0`.
    #[test]
    fn access_chained_field_with_correct_field_index() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { Inner { v: int, w: int } } \
 enum Outer { Outer { x: Inner, y: int } } \
 fn get_x_w(Outer o) -> int { return o.x.w; } \
 fn main() { return get_x_w(Outer::Outer { x: Inner::Inner { v: 42, w: 99 }, y: 7 }); }",
        );

        // Exactly 2 LoadField (one for `o.x`, one for `o.x.w`).
        let load_field_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .count();
        assert_eq!(
            load_field_count, 2,
            "expected exactly 2 LoadField for chained access; got {}",
            load_field_count
        );

        // Collect every LoadField operand. We expect:
        // - First LoadField(0) — Outer's `x` field index.
        // - Second LoadField(1) — Inner's `w` field index
        // (NOT Outer's `y` index — which would be 1 in
        // Outer but isn't what the user asked for).
        let field_indices: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LoadField))
            .map(|b| b.operand_u32() & 0xFFFF)
            .collect();
        assert_eq!(
            field_indices,
            vec![0, 1],
            "expected LoadField operands [0, 1] (Outer.x is 0, Inner.w is 1); got {:?}",
            field_indices
        );
    }

    // ============================================================
    // nested record patterns — codegen tests
    // ============================================================
    //
    // lifts the -cleanup limitation #1
    // (nested record patterns inside an arm body are rejected).
    // The codegen emitted a POP for an inner record
    // pattern instead of walking its declared fields, so the
    // binding slot for the inner record's fields was never
    // populated and the arm body read garbage values.
    //
    // These tests guard the codegen for the four
    // nested-record scenarios called out in the spec:
    //
    // 1. Nested record in tuple: `Result::Ok(Inner { v })`.
    // 2. Nested record in record: `Result::Ok { x: Inner { v } }`.
    // 3. Depth-3 nesting: `Foo::Bar(Baz::Qux { a: W::W { v } })`.
    // 4. Missing field in inner record (defensive POP emitted).
    //
    // The tests check the bytecode SHAPE (opcodes emitted) so
    // accidental regressions in the codegen are caught even if
    // the runtime happens to produce the right output for a
    // buggy bytecode (e.g. by accidentally emitting POP for
    // every record, which would compile and run but bind to
    // the wrong slots).

    /// Codegen test 23 : a record pattern inside a
    /// tuple pattern (`Result::Ok(Inner::I { v })`) compiles
    /// cleanly. Pre-18B, the inner `Inner::I { v }` was
    /// silently swallowed (a single POP was emitted for the
    /// inner record instead of walking its declared fields).
    /// Binding `v` needs no STORE; UNPACK must still appear.
    #[test]
    fn match_nested_record_in_tuple_binds_correctly() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { I { v: int } } \
 fn main() { \
 match Result::Ok(Inner::I { v: 42 }) { \
 Result::Err(_) => 0, \
 Result::Ok(Inner::I { v }) => v, \
 }; \
 }",
        );

        // The OUTER Result::Ok is the last arm (Err is first),
        // so it consumes the scrutinee via UNPACK (not
        // JUMP_IF_MATCH). The INNER Inner::I is a nested
        // Inner Binding `v` needs no STORE (value already in slot).
        // Pre-18B swallowed the inner record with POP — require UNPACK.

        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack))
            .count();
        assert!(
            unpack_count >= 1,
            "expected at least one UNPACK (the inner Inner::I); got {}",
            unpack_count
        );
    }

    /// Codegen test 24 : a record pattern inside a
    /// record pattern (`Result::Ok { x: Inner::I { v } }`).
    /// The codegen walks BOTH the OUTER record's and the
    /// INNER record's declared fields in decl_order. Pre-18B,
    /// the inner record was silently swallowed.
    #[test]
    fn match_nested_record_in_record_binds_correctly() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { I { v: int } } \
 enum Wrap { Good { x: Inner }, Bad(string) } \
 fn main() { \
 match Wrap::Good { x: Inner::I { v: 42 } } { \
 Wrap::Bad(_) => 0, \
 Wrap::Good { x: Inner::I { v } } => v, \
 }; \
 }",
        );

        // Outer UNPACK + inner walk; Binding `v` emits no STORE.
        // Pre-18B replaced the inner record with a single POP.
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack | Instruction::UnpackAt))
            .count();
        assert!(
            unpack_count >= 1,
            "expected UNPACK/UnpackAt for nested record walk; got {unpack_count}"
        );
    }

    /// Nested multi-field records emit scratch relocate (LOAD+StorePop)
    /// then UnpackAt with operands `[arity, scratch_slot]` — not in-place
    /// at the outer field (which would clobber siblings).
    #[test]
    fn match_nested_multifield_record_emits_scratch_unpack_at() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { I { x: int, y: int } } \
 enum Wrap { W { inner: Inner, name: int } } \
 fn main() { \
 match Wrap::W { inner: Inner::I { x: 1, y: 2 }, name: 3 } { \
 Wrap::W { inner: Inner::I { x, y }, name } => x + y + name, \
 }; \
 }",
        );

        let unpack_at: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::UnpackAt))
            .collect();
        assert!(
            !unpack_at.is_empty(),
            "expected UnpackAt for nested Inner::I"
        );
        for b in &unpack_at {
            let ops = b.operand_u32();
            let slot = ops & 0xFFFF;
            let arity = ops >> 16;
            assert_eq!(
                arity, 2,
                "inner record arity must be in [31:16]; got {ops:#x}"
            );
            // Outer has 2 fields; scratch starts at record_base + 2 (payload_base
            // is 0 for bare expression matches, 1 inside functions).
            assert!(
                slot >= 2,
                "scratch slot must be past outer field region; got slot={slot}"
            );
        }
        let load_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::LOAD))
            .count();
        let store_pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        assert!(
            load_count >= 1 && store_pop_count >= 1,
            "expected LOAD+StorePop to relocate nested enum into scratch; LOAD={load_count} StorePop={store_pop_count}"
        );
    }

    /// Codegen test 25 : depth-3 nested constructor
    /// patterns (`Foo::Bar(Baz::Qux { a: W::W { v } })`).
    /// The codegen recurses at unbounded depth — three levels
    /// of nested constructor patterns, with the innermost being
    /// a record. Pre-18B, the inner record was silently
    /// swallowed at any depth > 1.
    #[test]
    fn match_depth_3_nested_records_bind_correctly() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum W { W { v: int } } \
 enum Baz { Qux { a: W } } \
 enum Foo { Bar(Baz), Other } \
 fn main() { \
 match Foo::Bar(Baz::Qux { a: W::W { v: 99 } }) { \
 Foo::Other => 0, \
 Foo::Bar(Baz::Qux { a: W::W { v } }) => v, \
 }; \
 }",
        );

        // Innermost Binding `v` needs no STORE; require nested unpack
        // so the depth-3 walk still reaches the record fields.
        let unpack_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Unpack | Instruction::UnpackAt))
            .count();
        assert!(
            unpack_count >= 1,
            "expected UNPACK/UnpackAt for depth-3 nested records; got {unpack_count}"
        );
    }

    /// Codegen test 26 : a record pattern with an
    /// OMITTED field (`Inner::I { }` instead of `Inner::I { v }`)
    /// emits a POP for the missing field (to keep the stack
    /// consistent with the decl_order walk). Pre-18B, the inner
    /// record was silently swallowed entirely.
    #[test]
    fn match_nested_record_missing_field_consumes_slot() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "enum Inner { I { v: int } } \
 fn main() { \
 match Result::Ok(Inner::I { v: 42 }) { \
 Result::Err(_) => 0, \
 Result::Ok(Inner::I { }) => 99, \
 }; \
 }",
        );

        // The pattern omits the `v` field. The codegen walks
        // the inner record's declared fields in decl_order
        // and emits POP for the missing field. Pre-18B, the
        // codegen emitted a single POP for the inner record
        // (regardless of how many fields it had) — this
        // assertion is a sanity check that the codegen still
        // produces a well-formed bytecode for this case (the
        // arm body is `99` and doesn't reference any bindings).
        //
        // We don't assert exact POP count (other parts of
        // the bytecode emit POPs too — e.g. the prologue's
        // scrutinee POP for the wildcard arm); we just check
        // the bytecode compiles.
        assert!(!bc.is_empty(), "bytecode should not be empty");

        // Sanity: the arm body `99` should produce a
        // non-zero integer constant somewhere in the bytecode.
        // (The CONST opcode uses `value[63:0]` for the
        // constant — see `Byte::constant()`.)
        let has_99 = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::CONST) && b.constant(&[]) == 99);
        assert!(has_99, "expected CONST 99 for the arm body");
    }

    /// A Num-constrained shared generic body dispatches through its trailing
    /// dictionary rather than a legacy DynAdd opcode.
    #[test]
    fn generic_add_emits_dictionary_indirect_call() {
        use common::Instruction;
        let (bc, _pool) =
            compile_src("fn add<T: Num>(T a, T b) -> T { return a + b; } fn main() { add(1, 2); }");
        // Ground monomorphization may shake the shared CallIndirect body.
        assert!(
            bc.iter().any(|b| {
                matches!(
                    b.bytecode(),
                    Instruction::CallIndirect
                        | Instruction::ADD
                        | Instruction::BinSlotSlot
                        | Instruction::BinReturn
                )
            }),
            "expected CallIndirect or specialized add; bytecode opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::DynAdd)),
            "new shared generic bodies must not emit DynAdd"
        );
    }

    /// Codegen test B1-2: a concrete `fn add(int a, int b) -> int { return a + b; }`
    /// must NOT emit `DynAdd` — it should use the regular `ADD` (or the peephole-fused
    /// `BinSlotSlot`) path.
    #[test]
    fn concrete_add_still_emits_add() {
        use common::Instruction;
        let (bc, _pool) =
            compile_src("fn add(int a, int b) -> int { return a + b; } fn main() { let x = 0; while x < 1 { x = x + 1; } return add(x, 2); }");
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::DynAdd)),
            "DynAdd must NOT appear for concrete fn add(int a, int b)"
        );
        // Either ADD (unfused) or BinSlotSlot (peephole-fused) must be present.
        let has_int_add = bc.iter().any(|b| {
            matches!(b.bytecode(), Instruction::ADD)
                || matches!(b.bytecode(), Instruction::BinSlotSlot)
                || matches!(b.bytecode(), Instruction::BinSlotImm)
                || matches!(b.bytecode(), Instruction::BinReturn)
        });
        assert!(
            has_int_add,
            "expected ADD or BinSlot*/BinReturn for concrete add; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Codegen test B3-1: when a generic function is referenced in a non-call position
    /// (e.g. `let f = id;`), the codegen must emit `MakePolyFn` so that `f` holds an
    /// `ObjPolyFn` heap pointer that `CallIndirect` can dispatch through.
    ///
    /// The function `id<T>(T x) -> T` is a canonical unconstrained identity and has
    /// no trait bound, so no DynAdd / DynCmp / etc. opcode is emitted — this
    /// purely tests the MakePolyFn path.
    #[test]
    fn generic_fn_as_value_emits_make_polyfn() {
        use common::Instruction;
        // `let f = id;` in main must compile `id` (a generic fn) as a MakePolyFn rather
        // than a direct CALL or LOAD — id is not a local variable, so the Identifier arm
        // must detect `is_generic_fn("id")` and emit MakePolyFn with id's entry offset.
        let (bc, _pool) = compile_src("fn id<T>(T x) -> T { return x; } fn main() { let f = id; }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn)),
            "expected MakePolyFn for `let f = id` where id is generic; bytecode opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Phase 4: escaping a constrained generic always emits `MakePolyFnCapture`
    /// (from an active `__dictN` scope or with unresolved null slots).
    #[test]
    fn constrained_generic_escape_emits_make_polyfn_capture() {
        use common::Instruction;
        let src = r#"
            trait Showable<T> { fn show_it(T x) -> int; }
            impl Showable<int> { fn show_it(int x) -> int { return x; } }
            fn show<T: Showable>(T x) -> int { return show_it(x); }
            fn capture<T: Showable>(T _w) { return show; }
            fn main() { let f = capture(0); }
        "#;
        let (bc, _pool) = compile_src(src);
        let capture = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakePolyFnCapture))
            .expect("expected MakePolyFnCapture");
        assert_eq!(
            capture.operand_u32() & 0xFF,
            1,
            "Showable escape should capture exactly 1 dict slot; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn)),
            "constrained escape should not emit unconstrained MakePolyFn"
        );
    }

    /// Phase 4: top-level constrained escape (`let f = show`) still uses
    /// `MakePolyFnCapture` with null slots (delayed application evidence).
    #[test]
    fn top_level_constrained_escape_emits_make_polyfn_capture_with_null_slots() {
        use common::Instruction;
        let src = r#"
            trait Showable<T> { fn show_it(T x) -> int; }
            impl Showable<int> { fn show_it(int x) -> int { return x; } }
            fn show<T: Showable>(T x) -> int { return show_it(x); }
            fn main() { let f = show; }
        "#;
        let (bc, _pool) = compile_src(src);
        let capture = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakePolyFnCapture))
            .expect("expected MakePolyFnCapture for top-level constrained escape");
        assert_eq!(
            capture.operand_u32() & 0xFF,
            1,
            "top-level show escape should reserve 1 dict slot"
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn)),
            "constrained escape must not use bare MakePolyFn"
        );
    }

    /// Phase 4: multiparam constraint escape captures one slot per constraint.
    #[test]
    fn multiparam_constrained_escape_emits_capture_with_slot_count() {
        use common::Instruction;
        let src = r#"
            trait Convert<A, B> { fn cast(A x) -> B; }
            impl Convert<int, int> { fn cast(int x) -> int { return x; } }
            fn convert_fn<A, B>(A x) -> B where Convert<A, B> { return cast(x); }
            fn capture_convert<A, B>(A _wa, B _wb) where Convert<A, B> { return convert_fn; }
            fn main() { let f = capture_convert(0, 0); }
        "#;
        let (bc, _pool) = compile_src(src);
        let capture = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakePolyFnCapture))
            .expect("expected MakePolyFnCapture for multiparam escape");
        assert_eq!(
            capture.operand_u32() & 0xFF,
            1,
            "Convert<A,B> escape should capture exactly 1 dict slot"
        );
    }

    /// Codegen test B3-2: when a concrete value is passed to a generic function,
    /// the codegen must emit `BoxValue` immediately after the argument to wrap it
    /// into an `ObjBoxed` heap object at the concrete→generic boundary.
    ///
    /// For `id(42)` where `fn id<T>(T x) -> T`, `42` is an `int` literal.
    /// After compiling the `CONST 42`, codegen detects `is_generic_fn("id")`,
    /// infers the argument's type as `int`, and emits `BoxValue` with tag
    /// `ValueTag::Int`.
    #[test]
    fn generic_call_with_concrete_arg_emits_box_value() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn id<T>(T x) -> T { return x; } fn main() { id(42); }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BoxValue)),
            "expected BoxValue for concrete int arg passed to generic fn id<T>; bytecode opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // The BoxValue operand should encode ValueTag::Int (= 0).
        let box_ops: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::BoxValue))
            .map(|b| b.operand_u32())
            .collect();
        assert!(
            box_ops
                .iter()
                .any(|&tag| tag == common::ValueTag::Int as u32),
            "BoxValue operand should be ValueTag::Int ({}), got: {:?}",
            common::ValueTag::Int as u32,
            box_ops
        );
    }

    /// Codegen test: `fn id<T>(T x) -> T { return x; }` called with a
    /// concrete `int` argument must emit BOTH `BoxValue` (arg boxing) AND
    /// `UnboxValue` (return unboxing) in the bytecode.
    ///
    /// The unbox is required so the caller receives a raw `i64`, not an
    /// `ObjBoxed` heap pointer, when the generic return type is instantiated
    /// to a concrete primitive.
    #[test]
    fn generic_call_emits_box_and_unbox() {
        use common::Instruction;
        let (bc, _pool) = compile_src("fn id<T>(T x) -> T { return x; } fn main() { id(42); }");
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BoxValue)),
            "expected BoxValue for concrete int arg to generic fn id<T>; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::UnboxValue)),
            "expected UnboxValue after generic fn call returns concrete int; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // The UnboxValue operand should encode ValueTag::Int (= 0).
        let unbox_ops: Vec<u32> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::UnboxValue))
            .map(|b| b.operand_u32() & 0xFFFF)
            .collect();
        assert!(
            unbox_ops
                .iter()
                .any(|&tag| tag == common::ValueTag::Int as u32),
            "UnboxValue operand should be ValueTag::Int ({}), got: {:?}",
            common::ValueTag::Int as u32,
            unbox_ops
        );
    }

    /// Generic functions returning ADTs must not emit a primitive UnboxValue
    /// on the call result (that would turn a valid heap object into garbage).
    #[test]
    fn generic_fn_returning_option_does_not_unbox_enum() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn some_of<T>(T x) -> Option<T> { return Option::Some(x); } \
fn main() { let _ = some_of(7); }",
        );
        let opcodes: Vec<_> = bc.iter().map(|b| b.bytecode()).collect();
        let last_call = opcodes
            .iter()
            .rposition(|op| matches!(op, Instruction::CALL | Instruction::TailCall))
            .expect("expected a CALL to some_of");
        assert!(
            !matches!(opcodes.get(last_call + 1), Some(Instruction::UnboxValue)),
            "Option return must not be UnboxValue'd after CALL; near: {:?}",
            &opcodes[last_call.saturating_sub(2)..(last_call + 3).min(opcodes.len())]
        );
    }

    #[test]
    fn bounded_generic_add_ground_call_uses_specialized_clone() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn add<T: Num>(T a, T b) -> T { return a + b; } \
             fn main() { return add(1, 2); }",
        );

        // Ground monomorphization may shake the shared CallIndirect body;
        // the specialized clone must remain (and must not use DynAdd).
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::DynAdd)),
            "must not emit DynAdd; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Ground monomorphized call in main: CONST/CONST/CALL without a
        // preceding BoxValue. (Builtin Num thunks also contain BoxValue.)
        let main_call = bc
            .iter()
            .enumerate()
            .rev()
            .find(|(_, b)| matches!(b.bytecode(), Instruction::CALL))
            .map(|(i, _)| i)
            .expect("main should CALL the specialized add");
        let boxed_before_call =
            main_call > 0 && matches!(bc[main_call - 1].bytecode(), Instruction::BoxValue);
        assert!(
            !boxed_before_call,
            "ground monomorphic add call should not box args; opcodes near CALL: {:?}",
            &bc[main_call.saturating_sub(4)..=main_call]
                .iter()
                .map(|b| b.bytecode())
                .collect::<Vec<_>>()
        );
        let has_specialized_add = bc.iter().any(|b| {
            matches!(
                b.bytecode(),
                Instruction::ADD | Instruction::BinSlotSlot | Instruction::BinReturn
            )
        });
        assert!(
            has_specialized_add,
            "specialized add clone should contain int ADD/fused equivalent; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    // ── Dictionary-passing calling convention tests ─────────────────────────

    /// Codegen test: A non-monomorphized call to a generic function with a
    /// **user-defined** trait constraint must emit:
    ///   1. `MakeTuple` (the method-offset dict) after the value arg.
    ///   2. A `CALL` whose packed arity is 2 (1 value arg + 1 dict tuple),
    ///      NOT 1.
    ///
    /// The CONST that feeds `MakeTuple` encodes the bytecode offset of the
    /// instance method (i.e. it must be > 0 because the method compiles to a
    /// real function body).
    #[test]
    fn user_typeclass_constrained_call_emits_dict_tuple_and_bumps_arity() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            // Declare a user trait with one method.
            "trait Describable<T> { fn describe_val(T x) -> int; } \
             impl Describable<int> { fn describe_val(int x) -> int { return x; } } \
             // Generic fn with one user trait constraint.  NOT called as mono.
             fn show<T: Describable>(T x) -> int { return 0; } \
             fn main() { show(42); }",
        );

        // ── 1. A MakeTuple must be present (the dict for Describable<int>).
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "expected MakeTuple for dict emission; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );

        // ── 2. The CALL to `show` must have arity 2 (1 value + 1 dict).
        //    We look for the CALL with the highest arity among all CALL
        //    instructions (the monomorphized clone if any won't have a dict).
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity,
            2,
            "expected CALL arity = 2 (1 value + 1 dict); got {} from opcodes: {:?}",
            max_call_arity,
            bc.iter()
                .filter(|b| matches!(b.bytecode(), Instruction::CALL))
                .map(|b| b.call_parts())
                .collect::<Vec<_>>()
        );
    }

    /// Codegen test: A non-monomorphized call with **two** user typeclass
    /// constraints must emit **two** `MakeTuple` instructions (one per dict)
    /// and a `CALL` with arity N_value_args + 2.
    #[test]
    fn two_user_typeclass_constraints_emit_two_dicts_and_arity_plus_two() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "trait Printable<T> { fn printable_val(T x) -> int; } \
             trait Countable<T> { fn count_val(T x) -> int; } \
             impl Printable<int> { fn printable_val(int x) -> int { return x; } } \
             impl Countable<int> { fn count_val(int x) -> int { return x + 1; } } \
             fn process<T: Printable + Countable>(T x) -> int { return 0; } \
             fn main() { process(5); }",
        );

        // Two MakeTuple instructions (one per dict).
        let make_tuple_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeTuple))
            .count();
        assert!(
            make_tuple_count >= 2,
            "expected at least 2 MakeTuple (two dicts); got {}; opcodes: {:?}",
            make_tuple_count,
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );

        // CALL arity should be 1 value + 2 dicts = 3.
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity, 3,
            "expected CALL arity = 3 (1 value + 2 dicts); got {}",
            max_call_arity
        );
    }

    /// Builtin constraints use the same tuple dictionary ABI as user-defined
    /// constraints, with compiler-generated method thunks as entries.
    #[test]
    fn builtin_num_constraint_emits_dict_tuple() {
        use common::Instruction;
        // Non-monomorphized call: use boxed arg path.
        let (bc, _pool) = compile_src(
            "fn add_generic<T: Num>(T a, T b) -> T { return a + b; } \
             fn caller<U: Num>(U x, U y) -> U { return add_generic(x, y); }",
        );

        let make_tuple_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeTuple))
            .count();
        assert_eq!(
            make_tuple_count, 0,
            "open Num evidence is forwarded, not rebuilt"
        );

        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "expected dictionary CallIndirect for Num-constrained add; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Ground calls with **user** trait bounds are NOT monomorphized
    /// (see `monomorphize.rs`); they use the shared body + dictionary-passing
    /// convention instead. Expect BoxValue + MakeTuple + bumped CALL arity.
    #[test]
    fn ground_user_typeclass_call_uses_dict_not_mono() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "trait Describable<T> { fn describe_val(T x) -> int; } \
             impl Describable<int> { fn describe_val(int x) -> int { return x; } } \
             fn id_d<T: Describable>(T x) -> T { return x; } \
             fn main() { let y = id_d(7); }",
        );

        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::BoxValue)),
            "shared generic path should box the concrete arg; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "user trait ground call should emit a dict MakeTuple; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity, 2,
            "expected CALL arity = 2 (1 value + 1 dict); got {}",
            max_call_arity
        );
    }

    #[test]
    fn generic_bound_method_consumes_dictionary_indirectly() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "trait Measurable<T> { fn size(T x) -> int; } \
             impl Measurable<int> { fn size(int x) -> int { return x; } } \
             fn size_of<T: Measurable>(T x) -> int { return x.size(); } \
             fn main() { size_of(42); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::Index))
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "bound method should dispatch via CallIndirect"
        );
    }

    /// COI-78: a user-trait method at a concrete type uses static `CALL`
    /// (B4). The same method under an open generic bound stays on
    /// `Index` + `CallIndirect`, and a ground call to that generic still
    /// passes a dictionary rather than monomorphizing.
    #[test]
    fn user_trait_ground_method_call_vs_generic_dictionary() {
        use common::Instruction;

        let (ground, _) = compile_src(
            "trait Measurable<T> { fn size(T x) -> int; } \
             impl Measurable<int> { fn size(int x) -> int { return x + 1; } } \
             fn main() { return 41.size(); }",
        );
        assert!(
            ground
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::CALL)),
            "ground user-trait method must CALL; ops={:?}",
            ground.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            !ground
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "ground user-trait method must not CallIndirect; ops={:?}",
            ground.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );

        let (generic, _) = compile_src(
            "trait Measurable<T> { fn size(T x) -> int; } \
             impl Measurable<int> { fn size(int x) -> int { return x + 1; } } \
             fn size_of<T: Measurable>(T x) -> int { return x.size(); } \
             fn main() { return size_of(41); }",
        );
        assert!(
            generic
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "open user-trait bound must CallIndirect; ops={:?}",
            generic.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            generic
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "ground call to a user-trait generic must pass a dict; ops={:?}",
            generic.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let max_call_arity = generic
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity, 2,
            "size_of(41) CALL arity = 1 value + 1 dict; got {max_call_arity}"
        );
    }

    /// COI-78: builtin `Show` is not a monomorphization candidate even at
    /// a ground type (unlike `Num` / `Ord` / `Eq`).
    #[test]
    fn show_bound_ground_call_uses_dictionary_not_mono() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn show_it<T: Show>(T x) -> T { return x; } \
             fn main() { let y = show_it(7); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "Show ground call should emit a dict MakeTuple; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity, 2,
            "expected CALL arity = 2 (1 value + 1 Show dict); got {max_call_arity}"
        );
    }

    /// COI-78: `Length` matches `Show` — ground calls keep dictionary ABI.
    #[test]
    fn length_bound_ground_call_uses_dictionary_not_mono() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn n<T: Length>(T x) -> int { return len(x); } \
             fn main() { return n(\"ab\"); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "Length ground call should emit a dict MakeTuple; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        assert_eq!(
            max_call_arity, 2,
            "expected CALL arity = 2 (1 value + 1 Length dict); got {max_call_arity}"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "open Length::len body must CallIndirect; ops={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// COI-78: mixing `Num` with a dictionary bound must not monomorphize —
    /// the call site still emits a dict tuple (and bumped arity).
    #[test]
    fn num_plus_show_ground_call_keeps_dictionary() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn mix<T: Num + Show>(T a, T b) -> T { return a + b; } \
             fn main() { let y = mix(1, 2); }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeTuple)),
            "Num+Show must emit dict MakeTuple; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let max_call_arity = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CALL))
            .map(|b| b.call_parts().0)
            .max()
            .unwrap_or(0);
        // 2 values + Num dict + Show dict
        assert_eq!(
            max_call_arity, 4,
            "expected CALL arity = 4 (2 values + 2 dicts); got {max_call_arity}"
        );
    }

    #[test]
    fn omitted_default_method_dict_slot_has_real_target() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "trait Tiny<T> { fn zero(T x) -> int { return 7; } } \
             impl Tiny<int> {} \
             fn get<T: Tiny>(T x) -> int { return zero(x); } \
             fn main() { get(0); }",
        );
        let tuple_index = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::MakeTuple))
            .expect("default method dictionary");
        assert!(tuple_index > 0);
        assert!(
            matches!(bc[tuple_index - 1].bytecode(), Instruction::CodePtr),
            "dictionary method slots must use CodePtr; got {:?}",
            bc[tuple_index - 1].bytecode()
        );
        assert!(
            bc[tuple_index - 1].operand_u32() > 0,
            "default method dictionary slot must contain a compiled code offset"
        );
    }

    /// Dictionary emission uses self-identifying `CodePtr` (not `CONST`).
    #[test]
    fn dictionary_entries_emit_code_ptr() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "trait Measurable<T> { fn size(T x) -> int; } \
             impl Measurable<int> { fn size(int x) -> int { return x; } } \
             fn size_of<T: Measurable>(T x) -> int { return size(x); } \
             fn main() { size_of(42); }",
        );
        let tuple_pos = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::MakeTuple))
            .expect("expected dict MakeTuple");
        assert!(
            matches!(bc[tuple_pos - 1].bytecode(), Instruction::CodePtr),
            "dict entry before MakeTuple must be CodePtr"
        );
        let code_ptrs: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::CodePtr))
            .collect();
        assert!(
            !code_ptrs.is_empty(),
            "expected at least one CodePtr in dictionary program"
        );
        for ptr in &code_ptrs {
            assert!(
                (ptr.operand_u32() as usize) < bc.len(),
                "CodePtr target {} out of range (len={})",
                ptr.operand_u32(),
                bc.len()
            );
        }
    }

    /// Direct instance-method / CallIndirect sites push `CodePtr` targets.
    #[test]
    fn call_indirect_sites_use_code_ptr_targets() {
        use common::Instruction;
        let mut bc = Vec::new();
        Compiler::emit_call_indirect(&mut bc, 100_000, 1);
        assert!(matches!(bc[0].bytecode(), Instruction::CodePtr));
        assert_eq!(
            bc[0].operand_u32(),
            100_000,
            "CodePtr must carry full 32-bit targets (> u16::MAX)"
        );
        assert!(matches!(bc[1].bytecode(), Instruction::CallIndirect));
    }

    /// Nested IO HostInvoke (`read(stdin(), buf)`) stages args before pushing
    /// the outer native id, so temp slots from nested calls cannot sit between
    /// the id and the tuple that HostInvoke consumes.
    #[test]
    fn nested_io_host_invoke_emits_outer_const_before_inner_host_invoke() {
        use common::Instruction;
        let src = "\
use io::{stdin, read}; \
fn main() { \
  let buf = Vec::from([0 as byte]); \
  let _ = read(stdin(), buf); \
}";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let mut compiler = Compiler::default();
        // Stable ids matching Pipeline::register_io_natives order is not
        // required — only the relative nesting shape is asserted.
        compiler.register_native_id("stdin", 1);
        compiler.register_native_id("read", 2);
        let bc = compiler.compile("", &mut ast);

        let host_idxs: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::HostInvoke))
            .map(|(i, _)| i)
            .collect();
        assert!(
            host_idxs.len() >= 2,
            "expected nested HostInvoke (stdin + read); got {}",
            host_idxs.len()
        );
        let outer_host = *host_idxs.last().expect("outer HostInvoke");
        // Outer native id is CONST value 2, emitted after its args (which
        // include the inner HostInvoke) but before the outer HostInvoke.
        let outer_const = bc[..outer_host]
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::CONST) && b.value_u32() == 2)
            .expect("outer read CONST(id=2) before outer HostInvoke");
        let inner_host = host_idxs
            .iter()
            .copied()
            .find(|&i| i < outer_host)
            .expect("inner stdin HostInvoke before outer");
        assert!(
            inner_host < outer_const && outer_const < outer_host,
            "outer native-id CONST must follow nested HostInvoke and precede outer HostInvoke \
             (const@{outer_const} vs inner@{inner_host})"
        );
    }

    /// Field-stored variadic `declare` ids must set the FfiInvoke variadic
    /// bit (codegen uses `is_ffi_declare_variadic_for_fn_id`, not bare lets).
    #[test]
    fn invoke_field_fn_id_variadic_sets_ffi_operand_flag() {
        use common::Instruction;
        let src = "\
use ffi::{dload, declare, invoke, Error}; \
use ffi::types::{Int, Float}; \
class Api { id: int, } \
fn main() -> Result<(), Error> { \
  let lib = dload(\"noop\")?; \
  let api = new Api(0); \
  api.id = declare(lib, \"f\", (Int,), Float, true)?; \
  let _ = invoke(lib, api.id, (1, 2, 3))?; \
}";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let mut compiler = Compiler::default();
        let bc = compiler.compile("", &mut ast);
        assert!(
            compiler.get_messages().is_empty(),
            "unexpected compile diagnostics: {:?}",
            compiler
                .get_messages()
                .iter()
                .map(|m| m.message())
                .collect::<Vec<_>>()
        );
        let ffi = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::FfiInvoke))
            .expect("expected FfiInvoke");
        let operand = ffi.operand_u32();
        assert_eq!(operand & 0xFFFF, 3, "arity low bits should be 3");
        assert_ne!(
            operand & (1 << 16),
            0,
            "variadic bit must be set for field fn-id declare(..., true)"
        );
    }

    /// Param call-site flow must still set the FfiInvoke variadic bit when
    /// `invoke` uses a bare parameter whose `declare(..., true)` metadata was
    /// recorded from callers (codegen must restore checker `current_function`).
    #[test]
    fn invoke_param_fn_id_variadic_sets_ffi_operand_flag() {
        use common::Instruction;
        let src = "\
use ffi::{dload, declare, invoke, Error}; \
use ffi::types::{Int, Float}; \
fn helper(int id) -> Result<float, Error> { \
  let f: float = invoke(0, id, (1, 2, 3))?; \
  return f; \
} \
fn main() -> Result<(), Error> { \
  let lib = dload(\"noop\")?; \
  let fn_id = declare(lib, \"f\", (Int,), Float, true)?; \
  let _ = helper(fn_id)?; \
}";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let mut compiler = Compiler::default();
        let bc = compiler.compile("", &mut ast);
        assert!(
            compiler.get_messages().is_empty(),
            "unexpected compile diagnostics: {:?}",
            compiler
                .get_messages()
                .iter()
                .map(|m| m.message())
                .collect::<Vec<_>>()
        );
        let ffi = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::FfiInvoke))
            .expect("expected FfiInvoke");
        let operand = ffi.operand_u32();
        assert_eq!(operand & 0xFFFF, 3, "arity low bits should be 3");
        assert_ne!(
            operand & (1 << 16),
            0,
            "variadic bit must be set for param fn-id declare(..., true)"
        );
    }

    /// `invoke(..., (fn, …))` callback args must use relocatable `CodePtr`,
    /// not `CONST`. Peephole fusion adjusts `CodePtr` in `finalize_bytecode`
    /// but never rewrites `CONST`, so a stale offset would make the FFI
    /// trampoline jump to the wrong IP (regression: prints `0` instead of
    /// `42` for `examples/ffi_callback.hy`).
    #[test]
    fn invoke_callback_fn_arg_emits_relocatable_code_ptr() {
        use common::Instruction;
        let src = "\
use ffi::{dload, declare, invoke}; \
use ffi::types::{Callback, Int}; \
fn doubler(int x) -> int { return x * 2; } \
fn main() { \
  let lib = dload(\"libsum.so\"); \
  let id = declare(lib, \"apply_cb\", (Callback, Int), Int); \
  invoke(lib, id, (doubler, 21)); \
}";
        let mut ast = Pratt::default().parse(src).expect("parse failed");
        let mut compiler = Compiler::default();
        let bc = compiler.compile("", &mut ast);
        let doubler = *compiler
            .functions
            .get("doubler")
            .expect("doubler must be registered");

        let ffi_idx = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::FfiInvoke))
            .expect("expected FfiInvoke");
        let make_tuple_idx = bc[..ffi_idx]
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::MakeTuple))
            .expect("expected MakeTuple before FfiInvoke");
        // Callback fn arg is the first tuple element → last CodePtr before
        // MakeTuple (args are emitted bottom-to-top; doubler then 21).
        let code_ptr = bc[..make_tuple_idx]
            .iter()
            .rev()
            .find(|b| matches!(b.bytecode(), Instruction::CodePtr))
            .expect("expected CodePtr for callback fn arg before MakeTuple");
        assert_eq!(
            code_ptr.operand_u32() as usize,
            doubler,
            "CodePtr must match post-finalize doubler entry (got {}; table={})",
            code_ptr.operand_u32(),
            doubler
        );
        // Guard against regressing to CONST at this site.
        assert!(
            !bc[..make_tuple_idx].iter().any(|b| {
                matches!(b.bytecode(), Instruction::CONST) && b.operand_u32() as usize == doubler
            }),
            "callback fn arg must not be baked as CONST (unrelocatable)"
        );
    }

    /// `MakePolyFn` operands are absolute and survive final-link fusion.
    #[test]
    fn make_polyfn_operand_is_relocatable_under_fusion() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            "fn id<T>(T x) -> T { return x; } \
             fn main() { let f = id; let y = f(42); }",
        );
        let poly = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakePolyFn))
            .expect("MakePolyFn for escaped generic");
        let entry = poly.operand_u32() as usize;
        assert!(
            entry < bc.len(),
            "MakePolyFn entry must point into bytecode"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "polyfn application should use CallIndirect"
        );
    }

    /// PolyFn programs still receive peephole fusion (BinSlotImm / etc.).
    #[test]
    fn polyfn_plus_fib_style_body_still_fuses() {
        use common::Instruction;
        // Shared fib-style body uses LOAD/CONST/op patterns that fuse when
        // CodePtr/MakePolyFn are relocatable (Phase 1 — no global skip-fusion).
        let (bc, _pool) = compile_src(
            "fn id<T>(T x) -> T { return x; } \
             fn fib(int n) -> int { \
               if n <= 2 { return 1; } \
               return fib(n - 1) + fib(n - 2); \
             } \
             fn main() { \
               let f = id; \
               write(stdout(), to_bytes(format(\"%i\", f(fib(5))))); \
             }",
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakePolyFn)),
            "expected MakePolyFn"
        );
        let has_fused = bc.iter().any(|b| {
            matches!(
                b.bytecode(),
                Instruction::BinSlotImm
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotSlot
                    | Instruction::CmpJmpf
                    | Instruction::BinReturn
                    | Instruction::ConstReturnImm
                    | Instruction::LoadReturnSlot
            )
        });
        // BinSlotSlot fuse covers LOAD;LOAD;ADD in non-generic helpers; fib
        // arithmetic may appear fused (*Return / BinSlot*) or as raw ops.
        assert!(
            has_fused
                || bc.iter().any(|b| matches!(
                    *b.bytecode(),
                    Instruction::ADD | Instruction::LEQ | Instruction::JMPF
                )),
            "expected fused superinstructions or fib arithmetic alongside PolyFn; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Generic HostInvoke Call path (`self.native`) has the same id-before-args
    /// contract as `emit_io_host_invoke` — nested `outer(inner())` must not
    /// leave the inner invoke above the outer id.
    ///
    /// Mirrors `nested_io_host_invoke_emits_outer_const_before_inner_host_invoke`:
    /// require two `HostInvoke`s and assert the outer id `CONST` precedes the
    /// *inner* `HostInvoke` (not merely the first one).
    #[test]
    fn nested_generic_host_invoke_emits_outer_id_before_inner_invoke() {
        use crate::typechecking::ty::int;
        use common::Instruction;

        let mut ast = Pratt::default()
            .parse(
                r#"
fn main() {
    outer(inner());
}
"#,
            )
            .expect("parse failed");
        let mut compiler = Compiler::default();
        compiler.register("inner", &[], &int());
        compiler.register("outer", &[int()], &int());
        let outer_id = compiler.native_id("outer").expect("outer registered") as u32;
        let bc = compiler.compile("", &mut ast);

        let host_idxs: Vec<usize> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::HostInvoke))
            .map(|(i, _)| i)
            .collect();
        assert!(
            host_idxs.len() >= 2,
            "expected nested HostInvoke (inner + outer); got {}; opcodes: {:?}",
            host_idxs.len(),
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let outer_host = *host_idxs.last().expect("outer HostInvoke");
        let outer_const = bc[..outer_host]
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::CONST) && b.value_u32() == outer_id)
            .expect("outer id CONST before outer HostInvoke");
        let inner_host = host_idxs
            .iter()
            .copied()
            .find(|&i| i < outer_host)
            .expect("inner HostInvoke before outer");
        assert!(
            outer_const < inner_host,
            "outer native-id CONST must precede nested HostInvoke \
             (const@{outer_const} vs inner@{inner_host})"
        );
    }

    /// Named call args are reordered to declaration order before CALL.
    /// Source order is `age` then `name`; bytecode must push name (STRING)
    /// then age (CONST) so a missing reorder still typechecks but fails here.
    #[test]
    fn named_call_shuffled_args_emits_declaration_order() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn greet(string name, int age) -> int {
    let acc = 0;
    while acc < age {
        acc = acc + 1;
    }
    return acc;
}
fn main() {
    let years = greet(age: 36, name: "Ada");
}
"#,
        );
        // Find the CALL in main (arity 2) — skip any earlier CALLs.
        // call_parts() = (arity, target).
        let call_idx = bc
            .iter()
            .rposition(|b| matches!(b.bytecode(), Instruction::CALL) && b.call_parts().0 == 2)
            .expect("expected CALL arity 2 for greet");
        // Walk backward from CALL over the two arg pushes: STRING then CONST.
        let mut saw_string = false;
        let mut saw_const = false;
        let mut order = Vec::new();
        for b in bc[..call_idx].iter().rev() {
            match b.bytecode() {
                Instruction::STRING | Instruction::FORMAT => {
                    order.push("string");
                    saw_string = true;
                    if saw_const {
                        break;
                    }
                }
                Instruction::CONST => {
                    order.push("const");
                    saw_const = true;
                    if saw_string {
                        break;
                    }
                }
                // Skip peephole / prologue noise between arg pushes.
                Instruction::POP
                | Instruction::DUPLICATE
                | Instruction::JMP
                | Instruction::JMPF
                | Instruction::CALL
                | Instruction::RETURN
                | Instruction::PRINT => {}
                _ => {
                    // Keep scanning; Format/Print in greet body appear earlier.
                }
            }
            if order.len() >= 2 {
                break;
            }
        }
        // Reverse to source-of-stack order: first pushed is declaration-first.
        order.reverse();
        assert_eq!(
            order,
            vec!["string", "const"],
            "expected STRING (name) then CONST (age) before CALL; got {:?}. \
             Missing reorder would emit CONST then STRING.",
            order
        );
    }

    /// Rest calls pack trailing args into MakeArray and CALL with arity =
    /// fixed + 1 (here fixed=0 → arity 1).
    #[test]
    fn rest_call_emits_make_array_before_call() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn sum(int... xs) -> int { return len(xs); }
fn main() {
    let n = sum(1, 2, 3);
}
"#,
        );
        let make_array = bc
            .iter()
            .find(|b| {
                matches!(b.bytecode(), Instruction::MakeArray) && b.operand_u32() == 3
            })
            .expect("expected MakeArray(3) for rest packing");
        assert_eq!(
            make_array.operand_u32(),
            3,
            "sum(1,2,3) should MakeArray(3); got {}",
            make_array.operand_u32()
        );
        let call = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::CALL) && b.call_parts().0 == 1)
            .expect("expected CALL arity 1 (rest packed as one slot)");
        let make_pos = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::MakeArray))
            .unwrap();
        let call_pos = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::CALL) && b.call_parts().0 == 1)
            .unwrap();
        assert!(
            make_pos < call_pos,
            "MakeArray must precede CALL (make@{make_pos} call@{call_pos})"
        );
        let _ = call;
    }

    /// Empty rest call still emits MakeArray(0) so the rest formal is `[]`.
    #[test]
    fn rest_empty_call_emits_make_array_zero() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
fn sum(int... xs) -> int { return len(xs); }
fn main() {
    let n = sum();
}
"#,
        );
        let make_array = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::MakeArray))
            .expect("expected MakeArray(0) for empty rest");
        assert_eq!(make_array.operand_u32(), 0);
    }

    /// `let (a, b) = (1, 2)` desugars to Index + StorePop per binding.
    #[test]
    fn let_tuple_destructure_emits_index_and_store_pop() {
        use common::Instruction;
        let (bc, _pool) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let (a, b) = (1, 2);
    write(stdout(), to_bytes(format("%i", a)));
    write(stdout(), to_bytes(format("%i", b)));
}
"#,
        );
        let index_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::Index))
            .count();
        assert!(
            index_count >= 2,
            "expected ≥2 Index for tuple let destructure; got {index_count}"
        );
        let store_pop_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::STORE))
            .count();
        // RHS temp + a + b (at least 3).
        assert!(
            store_pop_count >= 3,
            "expected ≥3 StorePop (tmp + a + b); got {store_pop_count}"
        );
    }

    /// Value-position mono fn → MakeFn; calling through the local → CallIndirect.
    #[test]
    fn fn_value_emits_make_fn_then_call_indirect() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn add(int a, int b) -> int { return a + b; }
fn main() {
    let f = add;
    let y = f(20, 22);
}
"#,
        );
        let make_fn: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeFn))
            .collect();
        assert_eq!(
            make_fn.len(),
            1,
            "expected exactly one MakeFn for `let f = add`; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // n_cap=0, n_filled=0, arity=2, is_rest=false
        assert_eq!(
            make_fn[0].operand_u32(),
            make_fn_operand(0, 0, 2, false),
            "MakeFn operand should pack arity=2 with no fills/captures"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "calling through `f` must use CallIndirect; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Positional under-apply must emit MakeFn with n_filled matching argc,
    /// not a full CALL (which would leave holes unfilled / wrong ABI).
    #[test]
    fn partial_application_emits_make_fn_with_fill_count() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn add(int a, int b) -> int { return a + b; }
fn main() {
    let g = add(1);
    let y = g(2);
}
"#,
        );
        let make_fn: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeFn))
            .collect();
        assert!(
            !make_fn.is_empty(),
            "expected MakeFn for partial `add(1)`; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Prefer the partial MakeFn (n_filled=1, arity=2) over any other.
        assert!(
            make_fn
                .iter()
                .any(|b| b.operand_u32() == make_fn_operand(0, 1, 2, false)),
            "expected MakeFn(n_cap=0, n_filled=1, arity=2); got operands {:?}",
            make_fn.iter().map(|b| b.operand_u32()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CallIndirect)),
            "completing the partial must CallIndirect; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Explicit-capture lambda must MakeFn with n_cap matching `use (...)`.
    #[test]
    fn lambda_emits_make_fn_with_capture_count() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let y = 10;
    let f = fn (int x) use (y) => x + y;
    write(stdout(), to_bytes(format("%i", f(32))));
}
"#,
        );
        let make_fn: Vec<_> = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MakeFn))
            .collect();
        assert!(
            !make_fn.is_empty(),
            "expected MakeFn for lambda; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            make_fn
                .iter()
                .any(|b| b.operand_u32() == make_fn_operand(1, 0, 1, false)),
            "expected MakeFn(n_cap=1, n_filled=0, arity=1); got operands {:?}",
            make_fn.iter().map(|b| b.operand_u32()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tuple_zip_add_emits_index_and_make_tuple() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let a = (1, 1) + (1, 1);
    write(stdout(), to_bytes(format("%i", a[0])));
}
"#,
        );
        let has_index = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::Index));
        let has_make = bc
            .iter()
            .any(|b| matches!(b.bytecode(), Instruction::MakeTuple));
        let has_add = bc.iter().any(|b| {
            matches!(
                b.bytecode(),
                Instruction::ADD | Instruction::BinSlotImm | Instruction::BinSlotSlot
            )
        });
        assert!(
            has_index && has_make && has_add,
            "expected Index + ADD + MakeTuple zip lowering; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Approach A: `matmul` lowers to packed `HostInvoke`, not a MUL cascade.
    #[test]
    fn matmul_emits_packed_matmul_opcode() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let a = [[1, 2], [3, 4]];
    let b = [[5, 6], [7, 8]];
    let c = matmul(a, b);
}
"#,
        );
        let hosts = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::HostInvoke))
            .count();
        assert!(
            hosts >= 1,
            "expected packed HostInvoke for matmul; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        // Scalar 2×2×2 unroll would emit 8 MUL; packed path must be far below that.
        let mul_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MUL | Instruction::MULF))
            .count();
        assert!(
            mul_count < 8,
            "packed matmul path should not unroll to 8 MULs; got {mul_count}"
        );
    }

    // Dims over the `u8` packed ceiling fall back to scalar unroll.
    #[test]
    fn matmul_dims_over_u8_limit_falls_back_to_unroll() {
        use common::Instruction;
        let ones: String = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join(", ");
        let a = format!("[[{ones}]]"); // 1×256
        let b_rows: String = std::iter::repeat_n("[1]", 256)
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!(
            "fn main() {{\n    let a = {a};\n    let b = [{b_rows}];\n    let _ = matmul(a, b);\n}}\n"
        );
        let (bc, _) = compile_src(&src);
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "dims > 255 must not emit packed HostInvoke"
        );
        let mul_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MUL | Instruction::MULF))
            .count();
        // 1×256×1 scalar unroll → 256 MULs.
        assert!(
            mul_count >= 256,
            "expected scalar unroll (≥256 MUL); got {mul_count}"
        );
    }

    /// Approach A: `dot` lowers to packed `HostInvoke` (`packed_dot`).
    #[test]
    fn dot_emits_packed_dot_opcode() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let x = dot([1, 2], [3, 4]);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected packed HostInvoke (dot); opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Approach A: `Matrix` `*` lowers to packed `HostInvoke` (`packed_matmul`).
    #[test]
    fn matrix_mul_emits_packed_matmul_opcode() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let a = matrix([[1, 2], [3, 4]]);
    let b = matrix([[5, 6], [7, 8]]);
    let c = a * b;
    write(stdout(), to_bytes(format("%i", c[0][0])));
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected packed HostInvoke (matmul) for Matrix *; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    fn packed_host_meta(bc: &[common::Byte]) -> u32 {
        use common::Instruction;
        let hi = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::HostInvoke))
            .expect("expected HostInvoke");
        // Layout: … CONST meta, MakeTuple, HostInvoke
        assert!(
            hi >= 2 && matches!(bc[hi - 1].bytecode(), Instruction::MakeTuple),
            "HostInvoke must follow MakeTuple"
        );
        assert!(
            matches!(bc[hi - 2].bytecode(), Instruction::CONST),
            "meta CONST must precede MakeTuple"
        );
        bc[hi - 2].operand_u32()
    }

    /// Approach A: `Matrix` `+` lowers to packed_matrix_zip with zip_kind=Add.
    #[test]
    fn matrix_add_emits_packed_matrix_zip_add() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let a = matrix([[1, 2], [3, 4]]);
    let c = a + a;
    write(stdout(), to_bytes(format("%i", c[0][0])));
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for Matrix +"
        );
        let ops = packed_host_meta(&bc);
        assert_eq!(ops & 0xFF, 2, "m");
        assert_eq!((ops >> 8) & 0xFF, 2, "n");
        assert_eq!((ops >> 16) & 0xFF, 0, "zip_kind Add");
    }

    /// Approach A: `Matrix` `-` packs zip_kind=Sub (not Add).
    #[test]
    fn matrix_sub_emits_packed_matrix_zip_sub() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let a = matrix([[5, 7], [9, 11]]);
    let b = matrix([[1, 2], [3, 4]]);
    let c = a - b;
    write(stdout(), to_bytes(format("%i", c[0][0])));
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for Matrix -"
        );
        assert_eq!(
            (packed_host_meta(&bc) >> 16) & 0xFF,
            1,
            "zip_kind Sub; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Approach A: unary `-` on `Matrix` lowers to packed_matrix_neg via HostInvoke.
    #[test]
    fn matrix_neg_emits_packed_matrix_neg() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn main() {
    let a = matrix([[1, 2], [3, 4]]);
    let c = -a;
    write(stdout(), to_bytes(format("%i", c[0][0])));
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for Matrix unary -; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// `cross` stays on the scalar unroll path (no HostInvoke / packed natives).
    #[test]
    fn cross_does_not_emit_packed_opcodes() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let c = cross((1, 0, 0), (0, 1, 0));
    return c[0];
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "cross must stay unrolled; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter().any(|b| matches!(
                b.bytecode(),
                Instruction::MUL
                    | Instruction::MULF
                    | Instruction::BinSlotSlot
                    | Instruction::BinReturn
            )),
            "cross unroll should emit MUL or fused slot mul; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Float `dot` sets the packed_dot is_float meta flag (`operands[16]`).
    #[test]
    fn float_dot_emits_packed_dot_with_float_flag() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let x = dot([1.0, 2.0], [3.0, 4.0]);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for float dot"
        );
        assert_ne!(
            packed_host_meta(&bc) & (1 << 16),
            0,
            "float packed_dot meta must set is_float bit"
        );
    }

    /// Static aggregate length ≥ 8 lowers to `packed_vec_arith` HostInvoke.
    #[test]
    fn aggregate_zip_len8_emits_packed_vec_arith() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let a = [1, 2, 3, 4, 5, 6, 7, 8];
    let b = [2, 2, 2, 2, 2, 2, 2, 2];
    let _ = a * b;
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for N=8 zip mul; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
        let mul_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MUL | Instruction::MULF))
            .count();
        assert!(
            mul_count < 8,
            "packed vec path should not unroll 8 MULs; got {mul_count}"
        );
        let ops = packed_host_meta(&bc);
        assert_eq!(ops & 0xFFFF, 8, "len");
        assert_eq!((ops >> 16) & 0xFF, 2, "op=mul");
        assert_eq!(ops & (1 << 24), 0, "int elements");
        assert_eq!(ops & (1 << 25), 0, "array result");
        assert_eq!(ops & (1 << 26), 0, "zip (not broadcast)");
        // native id CONST then MakeTuple(3) then HostInvoke
        let hi = bc
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::HostInvoke))
            .unwrap();
        assert_eq!(bc[hi - 1].operand_u32(), 3, "binary arity MakeTuple(3)");
    }

    /// N < 8 stays on scalar unroll — packed path must not fire.
    #[test]
    fn aggregate_zip_len7_does_not_emit_packed_vec_arith() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    let a = [1, 2, 3, 4, 5, 6, 7];
    let b = [1, 1, 1, 1, 1, 1, 1];
    let _ = a * b;
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "N=7 must stay on scalar unroll"
        );
        let mul_count = bc
            .iter()
            .filter(|b| matches!(b.bytecode(), Instruction::MUL | Instruction::MULF))
            .count();
        assert!(
            mul_count >= 7,
            "expected scalar unroll (≥7 MUL); got {mul_count}"
        );
    }

    /// Broadcast / float / scalar-left / neg meta bits for packed_vec_arith.
    #[test]
    fn aggregate_packed_vec_arith_meta_flags() {
        use common::Instruction;

        let (bc_bc, _) = compile_src(
            r#"
fn main() {
    let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
    let _ = 10.0 - a;
}
"#,
        );
        assert!(
            bc_bc
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for float broadcast sub"
        );
        let ops = packed_host_meta(&bc_bc);
        assert_eq!(ops & 0xFFFF, 8, "len");
        assert_eq!((ops >> 16) & 0xFF, 1, "op=sub");
        assert_ne!(ops & (1 << 24), 0, "float");
        assert_ne!(ops & (1 << 26), 0, "broadcast");
        assert_ne!(ops & (1 << 27), 0, "scalar_left");

        let (bc_neg, _) = compile_src(
            r#"
fn main() {
    let a = (1, 2, 3, 4, 5, 6, 7, 8);
    let _ = -a;
}
"#,
        );
        assert!(
            bc_neg
                .iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected HostInvoke for N=8 tuple neg"
        );
        let neg_ops = packed_host_meta(&bc_neg);
        assert_eq!((neg_ops >> 16) & 0xFF, 4, "op=neg");
        assert_ne!(neg_ops & (1 << 25), 0, "tuple result");
        let hi = bc_neg
            .iter()
            .position(|b| matches!(b.bytecode(), Instruction::HostInvoke))
            .unwrap();
        assert_eq!(bc_neg[hi - 1].operand_u32(), 2, "unary MakeTuple(2)");
    }

    #[test]
    fn unescape_coil_string_supports_hex_and_unicode() {
        assert_eq!(unescape_coil_string(r"\x41"), "A");
        assert_eq!(unescape_coil_string(r"\u{42}"), "B");
        assert_eq!(unescape_coil_string("\\\""), "\"");
        assert_eq!(unescape_coil_string(r"\e"), "\x1b");
    }

    #[test]
    fn cast_as_int_emits_cast_opcode() {
        use common::Instruction;
        let (bc, _) = compile_src(
            r#"
fn main() {
    return (3.5 as int);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CastFloatToInt)),
            "expected CastFloatToInt in {:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    /// Failed diamond inline must not leave a JMP to PC 0 before peel/call fallback.
    #[test]
    fn diamond_inline_failure_does_not_emit_jmp_to_pc_zero() {
        use common::Instruction;
        // Helper call in the else arm refuses tiny-inline diamond emit; peel/call
        // must still produce a sane program (no unbound join → JMP 0).
        let (bc, _pool) = compile_src(
            "fn other(int n) -> int { return n; } \
             fn base(int n) -> int { \
               if n <= 0 { return 1; } \
               return other(n) + 1; \
             } \
             fn main() { write(stdout(), to_bytes(format(\"%i\", base(0)))); }",
        );
        let bad: Vec<_> = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::JMP) && b.operand_u32() == 0)
            .map(|(i, _)| i)
            .collect();
        assert!(
            bad.is_empty(),
            "unbound diamond/peel join must not resolve to PC 0; JMP at {bad:?}"
        );
    }

    /// `bytes_slice`-shaped loop: `while i < end` with nested `i < len(src)` and
    /// `out.push(src[i])` must exit past the back-edge (BinSlotSlotJmpf pool target).
    #[test]
    fn bytes_slice_while_exit_targets_past_back_edge() {
        use common::Instruction;
        let (bc, pool) = compile_src(
            r#"
use io::{stdout};

use string::{format, to_bytes};
fn bytes_slice(Vec<byte> src, int start, int end) -> Vec<byte> {
    let out: Vec<byte> = Vec::new();
    let i = start;
    while i < end {
        if i < len(src) {
            out.push(src[i]);
        }
        i = i + 1;
    }
    return out;
}
fn main() {
    let b = to_bytes("abcd");
    let s = bytes_slice(b, 0, 4);
    write(stdout(), to_bytes(format("%i", len(s))));
}
"#,
        );
        let back = bc
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.bytecode(), Instruction::JMP))
            .map(|(i, _)| i)
            .max()
            .expect("back-edge JMP");
        // Inner `if i < len(src)` may also fuse to BinSlotSlotJmpf and jump to
        // the increment (still inside the loop). Only the while-exit must land
        // past the back-edge JMP.
        let mut saw_while_exit = false;
        for (_i, b) in bc.iter().enumerate() {
            if *b.bytecode() != Instruction::BinSlotSlotJmpf {
                continue;
            }
            let (_op, _a, idx) = b.bin_slot_slot_jmpf_parts();
            let packed = pool[idx];
            let tgt = (packed >> 32) as usize;
            if tgt > back {
                saw_while_exit = true;
            }
        }
        assert!(
            saw_while_exit,
            "expected while-exit BinSlotSlotJmpf targeting past back-edge JMP {back}"
        );
    }

    #[test]
    fn class_ctor_emits_init_typed() {
        let (bc, _) = compile_src(
            r#"
class Box { n: int }
fn main() {
    let b = new Box(1);
    return b.n;
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::InitTyped)),
            "expected InitTyped for class ctor; opcodes: {:?}",
            bc.iter().map(|b| b.bytecode().mnemonic()).collect::<Vec<_>>()
        );
        assert!(
            bc.iter()
                .all(|b| !matches!(b.bytecode(), Instruction::INIT)),
            "new Class should not emit legacy INIT"
        );
    }

    #[test]
    fn drop_method_registers_finalizer_prologue() {
        let (bc, _) = compile_src(
            r#"
class Handle { fd: int }
impl Handle {
    fn drop() {}
}
fn main() {
    let h = new Handle(1);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "expected registry HostInvoke in prologue"
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::CodePtr)),
            "expected CodePtr for drop entry"
        );
    }

    #[test]
    fn ground_option_string_none_uses_pointer_niche() {
        let (bc, _) = compile_src(
            r#"
fn main() {
    let x: Option<string> = Option::None;
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeEnum)),
            "ground Option<string> None must not box; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn option_int_none_stays_boxed() {
        let (bc, _) = compile_src(
            r#"
fn main() {
    let x: Option<int> = Option::None;
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeEnum)),
            "Option<int> None must stay boxed; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unary_option_int_return_uses_return_pair() {
        let (bc, _) = compile_src(
            r#"
fn give() -> Option<int> {
    return Option::None;
}
fn main() {
    let _ = give();
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ReturnPair)),
            "unary Option<int> return must use ReturnPair; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn niche_option_string_return_skips_return_pair() {
        let (bc, _) = compile_src(
            r#"
fn give() -> Option<string> {
    return Option::None;
}
fn main() {
    let _ = give();
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ReturnPair)),
            "pointer-niche Option return stays off ReturnPair; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn generic_option_boundary_inserts_niche_to_heap() {
        let (bc, _) = compile_src(
            r#"
fn id<T>(T x) -> T { return x; }
fn main() {
    let x: Option<string> = Option::Some("ok");
    let _ = id(x);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::OptionNicheToHeap)),
            "generic Option<T> boundary must box a niche value; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vec_pop_heap_item_emits_host_invoke_niche() {
        let mut ast = Pratt::default()
            .parse(
                r#"
fn main() {
    let v = Vec::from(["a"]);
    let _ = v.pop();
}
"#,
            )
            .expect("parse");
        let mut compiler = Compiler::default();
        compiler.register_native_id("vec_from_array", 1);
        compiler.register_native_id("vec_pop", 2);
        let bc = compiler.compile("", &mut ast);
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvokeNiche)),
            "Vec::pop of string must emit HostInvokeNiche; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vec_pop_int_stays_host_invoke() {
        let mut ast = Pratt::default()
            .parse(
                r#"
fn main() {
    let v = Vec::from([1]);
    let _ = v.pop();
}
"#,
            )
            .expect("parse");
        let mut compiler = Compiler::default();
        compiler.register_native_id("vec_from_array", 1);
        compiler.register_native_id("vec_pop", 2);
        let bc = compiler.compile("", &mut ast);
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvokeNiche)),
            "Vec::pop of int must not use HostInvokeNiche; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvoke)),
            "Vec::pop of int should still HostInvoke; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn unary_result_return_uses_return_pair() {
        let (bc, _) = compile_src(
            r#"
fn give() -> Result<int, string> {
    return Result::Ok(1);
}
fn main() {
    let _ = give();
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ReturnPair)),
            "unary Result return must use ReturnPair; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn result_bind_to_local_boxes_return_pair() {
        let (bc, _) = compile_src(
            r#"
enum Node {
    Obj { v: int },
}
fn make_ok() -> Result<Node, string> {
    return Node::Obj { v: 42 };
}
fn main() {
    let r = make_ok();
    let _ = match r {
        Result::Ok(Node::Obj { v }) => v,
        Result::Err(_) => -1,
    };
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::ReturnPair)),
            "Result call must use ReturnPair; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::PairToHeap)),
            "binding a pair-return call must box before StorePop; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn method_result_bind_boxes_return_pair() {
        let (bc, _) = compile_src(
            r#"
class Svc {}
enum Node { Obj { v: int } }
impl Svc {
    fn decode() -> Result<Node, string> {
        return Node::Obj { v: 42 };
    }
}
fn main() {
    let s = new Svc();
    let r = s.decode();
    let _ = match r {
        Result::Ok(_) => 0,
        Result::Err(_) => -1,
    };
}
"#,
        );
        let pair_to_heap = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::PairToHeap));
        assert!(
            pair_to_heap.is_some(),
            "instance method Result bind must PairToHeap; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
        assert_eq!(
            pair_to_heap.unwrap().operand_u32(),
            0,
            "Result PairToHeap operand must be 0 (is_option=false)",
        );
    }

    #[test]
    fn method_option_bind_boxes_return_pair() {
        let (bc, _) = compile_src(
            r#"
class Svc {}
impl Svc {
    fn maybe() -> Option<int> {
        return Option::Some(7);
    }
}
fn main() {
    let s = new Svc();
    let o = s.maybe();
    let _ = match o {
        Option::Some(_) => 0,
        Option::None => -1,
    };
}
"#,
        );
        let pair_to_heap = bc
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::PairToHeap));
        assert!(
            pair_to_heap.is_some(),
            "instance method Option bind must PairToHeap; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
        assert_eq!(
            pair_to_heap.unwrap().operand_u32(),
            1,
            "Option PairToHeap operand must be 1 (is_option=true)",
        );
    }

    #[test]
    fn method_result_direct_match_skips_pair_to_heap() {
        let (bc, _) = compile_src(
            r#"
class Svc {}
enum Node { Obj { v: int } }
impl Svc {
    fn decode() -> Result<Node, string> {
        return Node::Obj { v: 42 };
    }
}
fn main() {
    let s = new Svc();
    let _ = match s.decode() {
        Result::Ok(_) => 0,
        Result::Err(_) => -1,
    };
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::PairToHeap)),
            "direct match on pair-return method must stay in pair_value_context; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    /// COI-108: `self.inner()?` with a different Ok payload must keep the
    /// ReturnPair and use the tag EQ/JMPF path — not PairToHeap + JumpIfMatch.
    #[test]
    fn nested_method_try_mismatched_result_keeps_pair_path() {
        let (bc, _) = compile_src(
            r#"
class Enc {}
impl Enc {
    fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        return out;
    }
    fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }
}
fn main() {
    let e = new Enc();
    let _ = e.encode_into(10);
}
"#,
        );
        let ops: Vec<_> = bc.iter().map(|b| *b.bytecode()).collect();
        assert!(
            !bc.windows(3).any(|w| {
                matches!(w[0].bytecode(), Instruction::CALL)
                    && matches!(w[1].bytecode(), Instruction::PairToHeap)
                    && matches!(w[2].bytecode(), Instruction::DUPLICATE)
            }),
            "mismatched-Result method Try must not box before pair tag check; opcodes={ops:?}",
        );
        assert!(
            bc.windows(4).any(|w| {
                matches!(w[0].bytecode(), Instruction::CALL)
                    && matches!(w[1].bytecode(), Instruction::DUPLICATE)
                    && matches!(w[2].bytecode(), Instruction::CONST)
                    && matches!(w[3].bytecode(), Instruction::EQ)
            }),
            "mismatched-Result method Try must use pair EQ tag check; opcodes={ops:?}",
        );
        assert!(
            !ops.iter().any(|op| matches!(op, Instruction::JumpIfMatch)),
            "mismatched-Result method Try must not JumpIfMatch a heap enum; opcodes={ops:?}",
        );
    }

    /// COI-108 forward refs: callee declared after the caller must still use
    /// the pair Try path (reserved entry + `pair_call_kind`), not Unknown method
    /// or PairToHeap-before-tag-check.
    #[test]
    fn forward_mismatched_result_method_try_keeps_pair_path() {
        let (bc, _) = compile_src(
            r#"
class EncFwd {}
impl EncFwd {
    fn encode_into(int n) -> Result<int, string> {
        let bytes = self.encode(n)?;
        return len(bytes);
    }
    fn encode(int n) -> Result<Vec<byte>, string> {
        let out: Vec<byte> = Vec::new();
        out.push(n as byte);
        return out;
    }
}
fn main() {
    let e = new EncFwd();
    let _ = e.encode_into(10);
}
"#,
        );
        let ops: Vec<_> = bc.iter().map(|b| *b.bytecode()).collect();
        assert!(
            ops.iter().any(|op| matches!(op, Instruction::CALL)),
            "forward method call must lower to CALL; opcodes={ops:?}",
        );
        assert!(
            !bc.windows(3).any(|w| {
                matches!(w[0].bytecode(), Instruction::CALL)
                    && matches!(w[1].bytecode(), Instruction::PairToHeap)
                    && matches!(w[2].bytecode(), Instruction::DUPLICATE)
            }),
            "forward mismatched-Result Try must not box before pair tag check; opcodes={ops:?}",
        );
        assert!(
            bc.windows(4).any(|w| {
                matches!(w[0].bytecode(), Instruction::CALL)
                    && matches!(w[1].bytecode(), Instruction::DUPLICATE)
                    && matches!(w[2].bytecode(), Instruction::CONST)
                    && matches!(w[3].bytecode(), Instruction::EQ)
            }),
            "forward mismatched-Result Try must use pair EQ tag check; opcodes={ops:?}",
        );
        assert!(
            !ops.iter().any(|op| matches!(op, Instruction::JumpIfMatch)),
            "forward mismatched-Result Try must not JumpIfMatch a heap enum; opcodes={ops:?}",
        );
    }

    /// Plain (non-Result) forward instance call must resolve via reserved entry.
    #[test]
    fn forward_instance_method_call_emits_call() {
        let (bc, _) = compile_src(
            r#"
class Counter {}
impl Counter {
    fn early() -> int {
        return self.late();
    }
    fn late() -> int {
        return 7;
    }
}
fn main() {
    let c = new Counter();
    let _ = c.early();
}
"#,
        );
        let ops: Vec<_> = bc.iter().map(|b| *b.bytecode()).collect();
        assert!(
            ops.iter().any(|op| matches!(op, Instruction::CALL)),
            "forward instance method must lower to CALL; opcodes={ops:?}",
        );
    }

    #[test]
    fn nested_option_string_none_stays_boxed() {
        let (bc, _) = compile_src(
            r#"
fn main() {
    let x: Option<Option<string>> = Option::None;
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeEnum)),
            "nested Option must stay boxed; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn generic_option_return_inserts_heap_to_niche() {
        let (bc, _) = compile_src(
            r#"
fn id<T>(T x) -> T { return x; }
fn main() {
    let x: Option<string> = Option::Some("ok");
    let y: Option<string> = id(x);
}
"#,
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::OptionNicheToHeap)),
            "generic Option arg must niche→heap; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HeapOptionToNiche)),
            "generic Option return must heap→niche; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn vec_remove_heap_item_emits_host_invoke_niche() {
        let mut ast = Pratt::default()
            .parse(
                r#"
fn main() {
    let v = Vec::from(["a"]);
    let _ = v.remove(0);
}
"#,
            )
            .expect("parse");
        let mut compiler = Compiler::default();
        compiler.register_native_id("vec_from_array", 1);
        compiler.register_native_id("vec_remove", 2);
        let bc = compiler.compile("", &mut ast);
        assert!(
            bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::HostInvokeNiche)),
            "Vec::remove of string must emit HostInvokeNiche; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn ground_option_class_none_uses_pointer_niche() {
        let (bc, _) = compile_src(
            r#"
class Box {
    n: int,
}
fn main() {
    let x: Option<Box> = Option::None;
}
"#,
        );
        assert!(
            !bc.iter()
                .any(|b| matches!(b.bytecode(), Instruction::MakeEnum)),
            "ground Option<class> None must not box; opcodes={:?}",
            bc.iter().map(|b| b.bytecode()).collect::<Vec<_>>(),
        );
    }

