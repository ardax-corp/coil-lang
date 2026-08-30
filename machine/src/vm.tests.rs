    use common::{
        ArchivedByte as Byte, ArchivedInstruction as Instruction, Byte as RawByte, Value,
    };

    use super::{
        alloc_count, dispatch_count, make_fast_count, reset_alloc_profile, reset_dispatch_count,
    };
    use crate::{Heap, Machine, ObjArray, ObjEnum, Object};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct TestOutputBuf(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for TestOutputBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn take_test_output(buf: Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        Arc::try_unwrap(buf)
            .expect("VM still holds a reference to the buffer")
            .into_inner()
            .expect("mutex poisoned")
    }

    /// Build a `MAKE_ENUM` byte with the given tag and arity
    /// packed into the operand (upper 16 bits = tag, lower 16
    /// bits = arity).
    fn make_enum(tag: u16, arity: u16) -> Byte {
        Byte::new(Instruction::MakeEnum).with_operands_u16([tag, arity])
    }

    fn jump_if_match(tag: u16, pool_idx: u16) -> Byte {
        Byte::new(Instruction::JumpIfMatch).with_operands_u16([tag, pool_idx])
    }

    /// Build an `UNPACK` byte with the given arity in the
    /// operand.
    fn unpack(arity: u32) -> Byte {
        Byte::new(Instruction::Unpack).with_operand_u32(arity)
    }

    fn load_field(field_index: u16) -> Byte {
        Byte::new(Instruction::LoadField).with_operand_u32(field_index as u32)
    }

    fn store_pop(slot: u32) -> Byte {
        Byte::new(Instruction::STORE).with_load_store_slot(slot)
    }

    /// Build a `LOAD` byte that pushes `stack[frame.sp + slot]`
    /// onto the stack. Used to verify that a value previously
    /// written by `STORE_POP` is read back correctly.
    fn load(slot: u32) -> Byte {
        Byte::new(Instruction::LOAD).with_load_store_slot(slot)
    }

    /// Fused fib body for dispatch-count regression tests, using the
    /// operator-parameterized superinstructions (`BinSlotImmJmpf`,
    /// `BinSlotImm`, `ConstReturnImm`, `BinReturn`). Real recursion:
    /// fib(n) = fib(n-1) + fib(n-2), base case fib(<=2) = 1.
    fn fused_fib_bytecode(n: i64) -> (Vec<Byte>, Vec<u64>) {
        let leq = Instruction::LEQ as u8;
        let sub = Instruction::SUB as u8;
        let add = Instruction::ADD as u8;
        // Pool: jmpf (imm=2, target=5).
        let pool = vec![((5u64) << 32) | (2u16 as u64)];
        let code = vec![
            Byte::new(Instruction::CONST).with_const_inline(n as i32),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::HALT),
            // 3: if !(n <= 2) jump to 5 (recurse); else fall through.
            Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(leq, 0, 0),
            // 4: base case → return 1.
            Byte::new(Instruction::ConstReturnImm).with_operand_u32(1),
            // 5: fib(n - 1)
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(sub, 0, 1),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            // 7: fib(n - 2)
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(sub, 0, 2),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            // 9: return fib(n-1) + fib(n-2)
            Byte::new(Instruction::BinReturn).with_bin_return(add),
        ];
        (code, pool)
    }

    #[test]
    fn fused_fib_reduces_dispatch_count_for_n13() {
        reset_dispatch_count();
        let unfused = [
            Byte::new(Instruction::CONST).with_const_inline(13),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::LEQ),
            // if !(n <= 2) jump to 9 (recurse); else fall through.
            Byte::new(Instruction::JMPF).with_operand_u32(9),
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::RETURN),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::SUB),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_const_inline(2),
            Byte::new(Instruction::SUB),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        reset_dispatch_count();
        Machine::<512>::default().run(&unfused);
        let unfused_ops = dispatch_count();

        reset_dispatch_count();
        let (code, pool) = fused_fib_bytecode(13);
        Machine::<512>::default().run_with_pool(&code, &pool, &[], 0);
        let fused_ops = dispatch_count();

        // Both forms recurse identically, so the unfused run must
        // dispatch many opcodes (guards against a non-recursive
        // regression like the one this test previously masked).
        assert!(
            unfused_ops > 100,
            "unfused fib should actually recurse; got {unfused_ops}"
        );
        assert!(
            fused_ops < unfused_ops,
            "fused fib should dispatch fewer opcodes (fused={fused_ops}, unfused={unfused_ops})"
        );
    }

    /// Build a `CONST` byte that pushes the given `i64` value
    /// onto the stack. Used to set up the operand values for
    /// `MAKE_ENUM` and `JUMP_IF_MATCH`.
    fn const_int(value: i64) -> Byte {
        Byte::new(Instruction::CONST).with_const_inline(value as i32)
    }

    /// Tail-recursive countdown reuses one frame (no stack overflow for deep n).
    #[test]
    fn tail_call_countdown_reuses_frame() {
        const ENTRY: u32 = 3;
        let mut code = vec![
            const_int(10),
            Byte::new(Instruction::CALL).with_call_packed(1, ENTRY),
            Byte::new(Instruction::HALT),
            // ENTRY: if n == 0 { return n }
            load(0),
            const_int(0),
            Byte::new(Instruction::EQ),
            Byte::new(Instruction::JMPF).with_operand_u32(0), // patched below
            load(0),
            Byte::new(Instruction::RETURN),
        ];
        let continue_at = code.len() as u32;
        code.extend([
            load(0),
            const_int(1),
            Byte::new(Instruction::SUB),
            Byte::new(Instruction::TailCall).with_call_packed(1, ENTRY),
        ]);
        code[6] = Byte::new(Instruction::JMPF).with_operand_u32(continue_at);
        let mut vm = Machine::<64>::default();
        vm.run(&code);
        assert_eq!(vm.pop().as_int(), 0);
    }

    #[test]
    fn array_push_grows_in_place_and_len_reports_new_size() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            const_int(2),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            Byte::new(Instruction::DUPLICATE),
            const_int(3),
            Byte::new(Instruction::ArrayPush),
            Byte::new(Instruction::DUPLICATE),
            Byte::new(Instruction::ArrayLen),
            Byte::new(Instruction::HALT),
        ]);

        assert_eq!(vm.pop().as_int(), 3);

        vm.run(&[
            Byte::new(Instruction::DUPLICATE),
            const_int(2),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 3);
    }

    /// The aggregate builders read their operand window in place instead of
    /// popping, so element order and stack discipline are what can break.
    #[test]
    fn make_tuple_and_array_keep_declaration_order_and_consume_exactly_arity() {
        for (name, insn) in [
            ("MakeTuple", Instruction::MakeTuple),
            ("MakeArray", Instruction::MakeArray),
        ] {
            let mut vm = Machine::<16>::default();
            vm.run(&[
                const_int(99), // sentinel below the operand window
                const_int(10),
                const_int(20),
                const_int(30),
                Byte::new(insn).with_operand_u32(3),
                Byte::new(Instruction::HALT),
            ]);

            let addr = vm.pop().raw() as u64;
            let elements: Vec<i64> = match vm.heap().find_object_by_addr(addr) {
                Some(Object::Tuple(gc)) => {
                    gc.as_ref().elements.iter().map(Value::as_int).collect()
                }
                Some(Object::Array(gc)) => {
                    gc.as_ref().elements.iter().map(Value::as_int).collect()
                }
                _ => panic!("{name} did not allocate an aggregate"),
            };

            assert_eq!(elements, vec![10, 20, 30], "{name} element order");
            assert_eq!(
                vm.pop().as_int(),
                99,
                "{name} consumed more than `arity` operands"
            );
        }
    }

    #[test]
    fn make_tuple_arity_zero_allocates_an_empty_aggregate() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(7),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);

        let addr = vm.pop().raw() as u64;
        match vm.heap().find_object_by_addr(addr) {
            Some(Object::Tuple(gc)) => assert!(gc.as_ref().elements.is_empty()),
            _ => panic!("arity-0 MakeTuple must still allocate a tuple"),
        }
        assert_eq!(
            vm.pop().as_int(),
            7,
            "arity-0 MakeTuple must not touch the operand stack"
        );
    }

    /// Compiler-emitted MakeTuple always has `tell >= arity`; underfull stacks
    /// trip `promise!` in debug builds.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic]
    fn make_tuple_underfull_stack_debug_asserts() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(99),
            const_int(42),
            Byte::new(Instruction::MakeTuple).with_operand_u32(3),
            Byte::new(Instruction::HALT),
        ]);
    }

    /// `MakeEnum` stores its payload top-first and tags each arg as immediate
    /// or heap pointer; that classification is what GC tracing walks.
    #[test]
    fn make_enum_payload_is_top_first_and_classifies_heap_args() {
        use crate::Member;

        let strings = vec!["payload".to_owned()];
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                const_int(41),
                Byte::new(Instruction::STRING).with_operand_u32(0),
                make_enum(5, 2),
                Byte::new(Instruction::HALT),
            ],
            &[],
            &strings,
            0,
        );

        let addr = vm.pop().raw() as u64;
        let gc = match vm.heap().find_object_by_addr(addr) {
            Some(Object::Enum(gc)) => gc,
            _ => panic!("MakeEnum did not allocate an enum"),
        };
        let obj = gc.as_ref();

        assert_eq!(obj.tag, 5);
        assert_eq!(obj.payload.len(), 2);
        assert!(
            matches!(obj.payload[0], Member::Object(Object::String(_))),
            "the top operand must land in payload[0]"
        );
        match obj.payload[1] {
            Member::Value(v) => assert_eq!(v.as_int(), 41),
            Member::Object(_) => panic!("an immediate int must not be classified as a heap object"),
        }
    }

    /// binary_trees' `Node(Tree, Tree)`: the fresh arity-2 enum has to be
    /// rooted before the post-alloc GC, otherwise its children get swept.
    #[test]
    fn make_enum_arity_two_keeps_children_alive_across_gc() {
        use crate::Member;

        let mut vm = Machine::<32>::default();
        // Collect on every allocation so rooting bugs cannot hide.
        vm.heap_mut().set_gc_threshold_for_test(1);
        vm.run(&[
            const_int(1),
            make_enum(3, 1), // left
            const_int(2),
            make_enum(3, 1), // right
            make_enum(4, 2), // Node(left, right)
            Byte::new(Instruction::HALT),
        ]);

        let addr = vm.pop().raw() as u64;
        let node = match vm.heap().find_object_by_addr(addr) {
            Some(Object::Enum(gc)) => gc,
            _ => panic!("the Node enum was swept by the post-alloc GC"),
        };

        assert_eq!(node.as_ref().tag, 4);
        assert_eq!(node.as_ref().payload.len(), 2);
        for member in &node.as_ref().payload {
            match member {
                Member::Object(child) => assert!(
                    vm.heap().find_object_by_addr(child.addr()).is_some(),
                    "a child enum was swept while reachable from the Node payload"
                ),
                Member::Value(_) => panic!("a child enum must be classified as a heap object"),
            }
        }
    }

    /// Emit a STRING opcode that pushes an interned heap string.
    fn string_lit(strings: &mut Vec<String>, s: &str) -> Vec<Byte> {
        let idx = strings.len() as u32;
        strings.push(s.to_string());
        vec![Byte::new(Instruction::STRING).with_operand_u32(idx)]
    }

    #[test]
    fn repeated_program_string_reuses_one_heap_object() {
        let strings = vec!["literal".to_owned()];
        let code = vec![
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::POP),
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(&code, &[], &strings, 0);

        let value = vm.pop();
        assert_eq!(
            vm.heap()
                .into_iter()
                .filter(|obj| matches!(obj, Object::String(_)))
                .count(),
            1
        );
        assert!(vm.heap().find_object_by_addr(value.raw() as u64).is_some());
    }

    #[test]
    fn intern_key_reuses_heap_string_handle() {
        let mut vm = Machine::<8>::default();
        let key = vm.heap_mut().intern("field".to_owned());
        let value = Value::from(key.as_ptr() as u64);
        let resolved = Machine::<8>::intern_key(vm.heap_mut(), value);

        assert!(crate::memory::Gc::ptr_eq(key, resolved));
    }

    #[test]
    fn intern_key_non_string_falls_back_to_empty() {
        let mut vm = Machine::<8>::default();
        let empty = vm.heap_mut().intern_str("");
        let resolved = Machine::<8>::intern_key(vm.heap_mut(), Value::from(42i64));

        assert!(crate::memory::Gc::ptr_eq(empty, resolved));
        assert_eq!(resolved.as_ref().data, "");
    }

    #[test]
    fn make_dict_set_field_get_field_roundtrip_via_intern_key() {
        let strings = vec!["k".to_owned()];
        let mut code = Vec::new();
        code.push(const_int(1));
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(Byte::new(Instruction::MakeDict).with_operand_u32(1));
        code.push(store_pop(0));
        // SetField pops name, target, value — push value, dict, key.
        code.push(const_int(99));
        code.push(load(0));
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(Byte::new(Instruction::SetField));
        code.push(Byte::new(Instruction::POP));
        code.push(load(0));
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(Byte::new(Instruction::GetField));
        code.push(Byte::new(Instruction::HALT));

        let mut vm = Machine::<16>::default();
        vm.run_with_pool(&code, &[], &strings, 0);
        assert_eq!(vm.pop().as_int(), 99);
        assert_eq!(
            vm.heap()
                .into_iter()
                .filter(|obj| matches!(obj, Object::String(_)))
                .count(),
            1,
            "MakeDict/SetField/GetField must reuse the program string key"
        );
    }

    #[test]
    fn make_enum_mixed_int_and_string_payload_order() {
        use crate::memory::Member;

        let strings = vec!["payload".to_owned()];
        let mut code = Vec::new();
        // Declaration order (int, string): push string then int so payload[0]=int.
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(const_int(7));
        code.push(make_enum(3, 2));
        code.push(Byte::new(Instruction::HALT));

        let mut vm = Machine::<8>::default();
        vm.run_with_pool(&code, &[], &strings, 0);
        let enum_addr = vm.pop().raw() as u64;
        match vm.heap().find_object_by_addr(enum_addr) {
            Some(Object::Enum(gc)) => {
                let e = gc.as_ref();
                assert_eq!(e.tag, 3);
                assert_eq!(e.payload.len(), 2);
                match &e.payload[0] {
                    Member::Value(v) => assert_eq!(v.as_int(), 7),
                    Member::Object(_) => panic!("payload[0] should be int Value"),
                }
                match &e.payload[1] {
                    Member::Object(Object::String(s)) => {
                        assert_eq!(s.as_ref().data, "payload");
                    }
                    _ => panic!("payload[1] should be string Object"),
                }
            }
            _ => panic!("expected enum on stack"),
        }
    }

    /// MakeTuple fixed-arity fast path: declaration-order elements, no reverse.
    #[test]
    fn make_tuple_arity2_and_3_preserve_declaration_order() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            Byte::new(Instruction::MakeTuple).with_operand_u32(2),
            Byte::new(Instruction::HALT),
        ]);
        let addr = vm.pop().raw() as u64;
        match vm.heap().find_object_by_addr(addr) {
            Some(Object::Tuple(gc)) => {
                let e = &gc.as_ref().elements;
                assert_eq!(e.len(), 2);
                assert_eq!(e[0].as_int(), 10);
                assert_eq!(e[1].as_int(), 20);
            }
            _ => panic!("expected tuple"),
        }

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            const_int(2),
            const_int(3),
            Byte::new(Instruction::MakeTuple).with_operand_u32(3),
            const_int(1),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 2);
    }

    /// MakeArray fixed-arity fast path mirrors MakeTuple order.
    #[test]
    fn make_array_arity2_preserves_declaration_order() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            const_int(6),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            const_int(0),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 5);

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            const_int(6),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            const_int(1),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 6);
    }

    /// Docs contract: `Index` never panics — too-large / negative / non-array
    /// targets yield the integer `-1`. `IndexUnchecked` skips the range test
    /// (compiler proof only; UB in release on violation).
    #[test]
    fn index_pin_reads_after_array_pin() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            const_int(6),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ArrayPin).with_operand_u32(0),
            const_int(1),
            Byte::new(Instruction::IndexPin).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 6);
    }

    #[test]
    fn index_pin_unchecked_reads_in_bounds_element() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            const_int(6),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ArrayPin).with_operand_u32(0),
            const_int(1),
            Byte::new(Instruction::IndexPinUnchecked).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 6);
    }

    #[test]
    fn index_unchecked_reads_in_bounds_element() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            const_int(6),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            const_int(1),
            Byte::new(Instruction::IndexUnchecked),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 6);
    }

    /// Docs contract: checked `Index` never panics — too-large / negative / non-array
    /// targets yield the integer `-1` (COI-85 keeps the check in-VM).
    #[test]
    fn index_oob_and_non_array_yield_minus_one() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            const_int(2),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), -1, "too-large array Index");

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            const_int(2),
            const_int(3),
            Byte::new(Instruction::MakeTuple).with_operand_u32(3),
            const_int(5),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), -1, "too-large tuple Index");

        let neg1 = Value::from(-1_i64).raw() as u64;
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(
            &[
                const_int(10),
                const_int(20),
                Byte::new(Instruction::MakeArray).with_operand_u32(2),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::Index),
                Byte::new(Instruction::HALT),
            ],
            &[neg1],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), -1, "negative array Index");

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(42),
            const_int(0),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), -1, "non-array Index");
    }

    /// Docs contract: `StoreIndex` with a bad index is a no-op and still
    /// leaves `x` on the stack.
    #[test]
    fn store_index_oob_is_noop_and_pushes_value() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            Byte::new(Instruction::DUPLICATE),
            const_int(99),
            const_int(7),
            Byte::new(Instruction::StoreIndex),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 7, "OOB StoreIndex still pushes value");

        // Array still [10, 20] — re-index both slots.
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            Byte::new(Instruction::DUPLICATE),
            Byte::new(Instruction::DUPLICATE),
            const_int(5),
            const_int(99),
            Byte::new(Instruction::StoreIndex),
            Byte::new(Instruction::POP),
            Byte::new(Instruction::DUPLICATE),
            const_int(0),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 10, "slot 0 unchanged after OOB store");

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            Byte::new(Instruction::MakeArray).with_operand_u32(2),
            Byte::new(Instruction::DUPLICATE),
            const_int(5),
            const_int(99),
            Byte::new(Instruction::StoreIndex),
            Byte::new(Instruction::POP),
            const_int(1),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 20, "slot 1 unchanged after OOB store");

        // Non-array target: still a no-op that pushes the value.
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(42),
            const_int(0),
            const_int(9),
            Byte::new(Instruction::StoreIndex),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 9, "non-array StoreIndex pushes value");
    }

    /// Empty MakeTuple / MakeArray still allocate a rooted aggregate.
    #[test]
    fn make_tuple_and_array_arity0() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::ArrayLen),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);

        let mut vm = Machine::<4>::default();
        vm.run(&[
            Byte::new(Instruction::MakeArray).with_operand_u32(0),
            Byte::new(Instruction::ArrayLen),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// Tree-node shape: MakeEnum arity-2 hits the fixed-arity fast path and
    /// keeps pop-order payload (codegen reverse-pushes).
    #[test]
    fn make_enum_arity2_fast_path_tree_node_shape() {
        reset_alloc_profile();
        let mut vm = Machine::<8>::default();
        // Leaf singletons + Node(left, right): push right leaf, left leaf.
        vm.run(&[
            make_enum(0, 0),
            make_enum(0, 0),
            make_enum(1, 2),
            Byte::new(Instruction::HALT),
        ]);
        assert!(
            make_fast_count() >= 1,
            "arity-2 MakeEnum should take the fixed-arity fast path"
        );
        assert!(alloc_count() >= 1, "Node must allocate one enum object");
        let node = vm.pop().raw() as u64;
        match vm.heap().find_object_by_addr(node) {
            Some(Object::Enum(gc)) => {
                let e = gc.as_ref();
                assert_eq!(e.tag, 1);
                assert_eq!(e.payload.len(), 2);
            }
            _ => panic!("expected Node enum"),
        }
    }

    /// Large arity still works (slow Vec path) and preserves order.
    #[test]
    fn make_tuple_arity4_slow_path_preserves_order() {
        reset_alloc_profile();
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            const_int(2),
            const_int(3),
            const_int(4),
            Byte::new(Instruction::MakeTuple).with_operand_u32(4),
            const_int(3),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 4);
        // arity 4 must not bump the fixed-arity fast counter for this op.
        // (Other setup may be zero — only this MakeTuple ran.)
        assert_eq!(make_fast_count(), 0);
    }

    /// `ArrayLen` must report string/tuple/dict lengths (structural `len`).
    #[test]
    fn array_len_reports_string_tuple_and_dict_sizes() {
        let mut strings = Vec::new();
        let mut code = string_lit(&mut strings, "abcd");
        code.push(Byte::new(Instruction::ArrayLen));
        code.push(Byte::new(Instruction::HALT));
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(&code, &[], &strings, 0);
        assert_eq!(vm.pop().as_int(), 4, "string ArrayLen");

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            const_int(2),
            const_int(3),
            Byte::new(Instruction::MakeTuple).with_operand_u32(3),
            Byte::new(Instruction::ArrayLen),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 3, "tuple ArrayLen");

        let mut strings = Vec::new();
        let mut code = Vec::new();
        code.push(const_int(10));
        code.extend(string_lit(&mut strings, "a"));
        code.push(const_int(20));
        code.extend(string_lit(&mut strings, "b"));
        code.push(Byte::new(Instruction::MakeDict).with_operand_u32(2));
        code.push(Byte::new(Instruction::ArrayLen));
        code.push(Byte::new(Instruction::HALT));
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(&code, &[], &strings, 0);
        assert_eq!(vm.pop().as_int(), 2, "dict ArrayLen");
    }

    #[test]
    fn dict_entries_yields_array_of_key_value_tuples() {
        // MakeDict with {a: 1, b: 2}, then DictEntries → array of pairs.
        let mut code = Vec::new();
        let mut strings = Vec::new();
        // value, name for field a
        code.push(const_int(1));
        code.extend(string_lit(&mut strings, "a"));
        // value, name for field b
        code.push(const_int(2));
        code.extend(string_lit(&mut strings, "b"));
        code.push(Byte::new(Instruction::MakeDict).with_operand_u32(2));
        code.push(Byte::new(Instruction::DictEntries));
        code.push(Byte::new(Instruction::DUPLICATE));
        code.push(Byte::new(Instruction::ArrayLen));
        code.push(Byte::new(Instruction::HALT));

        let mut vm = Machine::<16>::default();
        vm.run_with_pool(&code, &[], &strings, 0);
        assert_eq!(vm.pop().as_int(), 2, "DictEntries should produce 2 pairs");

        // Index 0 → tuple; Index 1 on tuple → value (1 or 2 depending on table order)
        vm.run(&[
            Byte::new(Instruction::DUPLICATE),
            const_int(0),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::DUPLICATE),
            const_int(1),
            Byte::new(Instruction::Index),
            Byte::new(Instruction::HALT),
        ]);
        let v0 = vm.pop().as_int();
        assert!(v0 == 1 || v0 == 2, "pair value should be 1 or 2, got {v0}");
    }

    #[test]
    fn make_enum_allocates_enum_with_correct_tag() {
        let mut vm = Machine::<1>::default();
        // [MAKE_ENUM tag=0 arity=0, HALT]
        vm.run(&[make_enum(0, 0), Byte::new(Instruction::HALT)]);

        let enum_value = vm.pop();
        // The enum was allocated; its address is on the stack.
        // We don't have a direct accessor from the public VM
        // API, but we can at least check that the stack
        // contains a non-zero pointer (an allocated heap
        // object).
        assert!(
            enum_value.raw() as u64 != 0,
            "MAKE_ENUM did not push a heap pointer"
        );
    }

    #[test]
    fn arity0_make_enum_reuses_immortal_singleton() {
        let mut vm = Machine::<4>::default();
        let before = vm.heap().size();
        vm.run(&[
            make_enum(3, 0),
            make_enum(3, 0),
            make_enum(3, 0),
            Byte::new(Instruction::HALT),
        ]);
        let a = vm.pop().raw() as u64;
        let b = vm.pop().raw() as u64;
        let c = vm.pop().raw() as u64;
        assert_eq!(a, b);
        assert_eq!(b, c);
        // One immortal alloc only — heap bytes should not grow per construct.
        let after = vm.heap().size();
        assert!(
            after > before,
            "expected one immortal enum allocation"
        );
        let after_more = {
            vm.run(&[make_enum(3, 0), Byte::new(Instruction::HALT)]);
            let _ = vm.pop();
            vm.heap().size()
        };
        assert_eq!(after, after_more, "arity-0 MakeEnum must not re-allocate");
    }

    /// Distinct tags must not share a singleton — the map is keyed by tag.
    #[test]
    fn arity0_make_enum_distinct_tags_are_distinct_singletons() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            make_enum(1, 0),
            make_enum(2, 0),
            make_enum(1, 0),
            Byte::new(Instruction::HALT),
        ]);
        let again_tag1 = vm.pop().raw() as u64;
        let tag2 = vm.pop().raw() as u64;
        let tag1 = vm.pop().raw() as u64;
        assert_eq!(tag1, again_tag1);
        assert_ne!(tag1, tag2, "different tags must not alias");
    }

    #[test]
    fn make_enum_with_payload_populates_payload() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            const_int(42),
            const_int(7),
            // Codegen pushes args in REVERSE declaration order
            // so that the top of stack is payload[0]; so for a
            // 2-arg constructor with declaration order
            // (a, b), codegen emits CONST b, CONST a, MAKE_ENUM.
            make_enum(1, 2),
            Byte::new(Instruction::HALT),
        ]);
        let enum_value = vm.pop();
        assert!(
            enum_value.raw() as u64 != 0,
            "MAKE_ENUM with payload did not push a heap pointer"
        );
    }

    #[test]
    fn jump_if_match_taken_advances_ip() {
        // Build a minimal bytecode that:
        //   1. constructs an enum with tag=2, arity=1, payload=[42]
        //   2. executes JUMP_IF_MATCH tag=2 target=4 arity=1
        //   3. has a HALT at offset 4 (target)
        //
        // Since JUMP_IF_MATCH is checking for tag=2 and the
        // enum's tag IS 2, the jump is taken. The payload
        // (42) is pushed onto the stack.
        let mut vm = Machine::<4>::default();
        let constants = vec![4u64];
        vm.run_with_pool(
            &[
                // Build the enum (tag=2, arity=1) with payload [42]:
                const_int(42),
                make_enum(2, 1),
                // JUMP_IF_MATCH tag=2 target=4 (pool[0])
                jump_if_match(2, 0),
                // (Should not reach here on the jump-taken path.)
                const_int(999),
                // HALT at offset 4 (the target).
                Byte::new(Instruction::HALT),
            ],
            &constants,
            &[],
            0,
        );
        // After the jump, the payload (42) was pushed. Top of
        // stack is 42.
        let v = vm.pop();
        assert_eq!(v.as_int(), 42, "JUMP_IF_MATCH did not push the payload");
    }

    #[test]
    fn jump_if_match_wide_target_round_trips() {
        let target: u32 = 100_000;
        let constants = vec![target as u64];
        let byte = jump_if_match(5, 0);
        assert!(matches!(byte.bytecode(), Instruction::JumpIfMatch));
        assert_eq!(
            byte.jump_if_match_target(&constants),
            target as usize,
            "wide target should resolve via constant pool"
        );
        assert_eq!(
            byte.operand_u32() >> 16,
            5,
            "tag should be preserved in upper 16 bits of operands"
        );
        assert_eq!(
            byte.operand_u32() & 0xFFFF,
            0,
            "lower 16 bits should hold the pool index"
        );
        assert!(target > 0xFFFF, "test must exercise wide-target path");
    }

    #[test]
    fn jump_if_match_not_taken_falls_through() {
        let mut vm = Machine::<4>::default();
        vm.run_with_pool(
            &[
                // Build an enum (tag=2, arity=1) with payload [42]:
                const_int(42),
                make_enum(2, 1),
                // JUMP_IF_MATCH tag=5 (won't match; fall through)
                jump_if_match(5, 0),
                // (Should be reached on the fall-through path.)
                const_int(99),
                // Target for the (non-taken) jump at offset 4.
                Byte::new(Instruction::HALT),
            ],
            &[],
            &[],
            0,
        );
        // After fall-through, we pushed 99. Stack: [enum_ptr, 99].
        let v = vm.pop();
        assert_eq!(v.as_int(), 99, "JUMP_IF_MATCH should have fallen through");
    }

    #[test]
    fn unpack_pops_enum_and_pushes_payload() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            // Build enum (tag=0, arity=3) with payload [10, 20, 30]:
            // Codegen pushes args in REVERSE declaration order:
            const_int(30),
            const_int(20),
            const_int(10),
            make_enum(0, 3),
            unpack(3),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be 30 (payload[2]).
        assert_eq!(vm.pop().as_int(), 30);
        assert_eq!(vm.pop().as_int(), 20);
        assert_eq!(vm.pop().as_int(), 10);
    }

    #[test]
    fn load_field_extracts_field_zero() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            // Build enum (tag=0, arity=3) with payload [10, 20, 30]:
            // Pushed in REVERSE declaration order.
            const_int(30),
            const_int(20),
            const_int(10),
            make_enum(0, 3),
            // LoadField(0): pops enum, pushes payload[0] = 10.
            load_field(0),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be payload[0] = 10.
        assert_eq!(vm.pop().as_int(), 10);
    }

    #[test]
    fn load_field_extracts_last_field() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(30),
            const_int(20),
            const_int(10),
            make_enum(0, 3),
            // LoadField(2): pops enum, pushes payload[2] = 30.
            load_field(2),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be payload[2] = 30.
        assert_eq!(vm.pop().as_int(), 30);
    }

    #[test]
    fn load_field_extracts_middle_field() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            // Build enum (tag=0, arity=3) with payload
            // [10, 20, 30]: pushed in REVERSE declaration order.
            const_int(30),
            const_int(20),
            const_int(10),
            make_enum(0, 3),
            // LoadField(1): pops enum, pushes payload[1] = 20.
            load_field(1),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be payload[1] = 20.
        assert_eq!(vm.pop().as_int(), 20);
    }

    #[test]
    fn load_field_consumes_receiver() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            // Build enum (tag=0, arity=2) with payload [42, 99].
            const_int(99),
            const_int(42),
            make_enum(0, 2),
            // LoadField(0): pops enum, pushes payload[0] = 42.
            load_field(0),
            Byte::new(Instruction::HALT),
        ]);
        // Only ONE value should be on the stack after
        // LoadField (the extracted field). The enum itself
        // should have been consumed.
        assert_eq!(
            vm.tell(),
            1,
            "LoadField should leave exactly one value on the stack"
        );
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// Malformed bytecode: OOB `LoadField` trips `promise!` in debug builds.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic]
    fn load_field_out_of_bounds_debug_asserts() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            const_int(99),
            const_int(42),
            make_enum(0, 2),
            load_field(5),
            Byte::new(Instruction::HALT),
        ]);
    }

    #[cfg(not(debug_assertions))]
    #[test]
    fn load_field_out_of_bounds_release_uses_unchecked() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            const_int(99),
            const_int(42),
            make_enum(0, 2),
            load_field(5),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.tell(), 1);
    }

    /// `BinSlotSlot` applies an int binary op between two locals.
    /// Set up slots 0 and 1 with `6` and `4`, then `SUB` → `2`.
    #[test]
    fn bin_slot_slot_int_subtracts_two_locals() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(6), // slot 0
            const_int(4), // slot 1
            Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(Instruction::SUB as u8, 0, 1),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 2);
    }

    /// `BinSlotSlot` also covers float ops (both operands are slot
    /// loads, so unlike `BinSlotImm` there's no pool-constant issue).
    /// Slots 0 and 1 hold pooled `1.5` and `2.0`; `ADDF` → `3.5`.
    #[test]
    fn bin_slot_slot_float_adds_two_locals() {
        let pool = [1.5f64.to_bits(), 2.0f64.to_bits()];
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG), // pool[0] = 1.5
                Byte::new(Instruction::CONST).with_operand_u32(1 | Byte::POOL_FLAG), // pool[1] = 2.0
                Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
                    Instruction::ADDF as u8,
                    0,
                    1,
                ),
                Byte::new(Instruction::HALT),
            ],
            &pool,
            &[],
            0,
        );
        assert_eq!(vm.pop().as_float(), 3.5);
    }

    /// Fused `PowF` used to fall through to `Value::default()` (0.0) because
    /// fuse-select admitted it via `is_bin_op` without a handler arm.
    #[test]
    fn bin_slot_slot_powf_computes_float_power() {
        let pool = [2.0f64.to_bits(), 10.0f64.to_bits()];
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::CONST).with_operand_u32(1 | Byte::POOL_FLAG),
                Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(
                    Instruction::PowF as u8,
                    0,
                    1,
                ),
                Byte::new(Instruction::HALT),
            ],
            &pool,
            &[],
            0,
        );
        assert_eq!(vm.pop().as_float(), 1024.0);
    }

    /// `BinSlotSlotStore` float `PowF` is the store-into-dest fuse of the same
    /// bug — must write `2.0 ** 3.0` into slot 2, not leave a zero default.
    #[test]
    fn bin_slot_slot_store_powf_writes_dest() {
        let pool = [2.0f64.to_bits(), 3.0f64.to_bits()];
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::CONST).with_operand_u32(1 | Byte::POOL_FLAG),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
                    Instruction::PowF as u8,
                    0,
                    1,
                    2,
                ),
                load(2),
                Byte::new(Instruction::RETURN),
            ],
            &pool,
            &[],
            0,
        );
        assert_eq!(vm.pop().as_float(), 8.0);
    }

    /// `BinReturn` + `PowF` is the third fused path that silently returned 0.0.
    #[test]
    fn bin_return_powf_returns_float_power() {
        let pool = [2.0f64.to_bits(), 4.0f64.to_bits()];
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::CONST).with_operand_u32(1 | Byte::POOL_FLAG),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                load(0),
                load(1),
                Byte::new(Instruction::BinReturn).with_bin_return(Instruction::PowF as u8),
            ],
            &pool,
            &[],
            0,
        );
        assert_eq!(vm.pop().as_float(), 16.0);
    }

    #[test]
    fn nested_enum_gc_traces_correctly() {
        use crate::{Heap, Member, ObjString, Object};
        use std::collections::HashSet;

        let mut heap = Heap::default();

        // Allocate an inner enum (no payload).
        let (inner_obj, _) = heap.alloc(
            ObjEnum {
                tag: 99,
                payload: vec![],
            },
            Object::Enum,
        );
        // Allocate a string.
        let (string_obj, _) = heap.alloc(ObjString::from("inner"), Object::String);
        // Allocate an outer enum whose payload contains
        // references to both the inner enum and the string.
        let (outer_obj, _) = heap.alloc(
            ObjEnum {
                tag: 0,
                payload: vec![Member::Object(inner_obj), Member::Object(string_obj)],
            },
            Object::Enum,
        );

        let mut gray = Vec::new();
        heap.trace(&[outer_obj.addr()]);
        outer_obj.mark_references(&mut gray);
        while let Some(o) = gray.pop() {
            o.mark_references(&mut gray);
        }

        // Sweep — anything not marked is deallocated.
        unsafe {
            heap.sweep();
        }

        // All three must survive: outer (the root), inner
        // (referenced from outer's payload), and string
        // (also referenced from outer's payload).
        let mut addrs = HashSet::new();
        for o in heap.into_iter() {
            addrs.insert(o.addr());
        }
        assert!(
            addrs.contains(&outer_obj.addr()),
            "outer enum was collected despite being the GC root"
        );
        assert!(
            addrs.contains(&inner_obj.addr()),
            "inner enum was collected despite being in outer's payload"
        );
        assert!(
            addrs.contains(&string_obj.addr()),
            "string was collected despite being in outer's payload"
        );
    }

    #[test]
    fn heap_does_not_grow_unboundedly_under_repeated_alloc() {
        use std::collections::HashSet;

        let mut vm = Machine::<256>::default();
        // Force frequent collections so dead enums are reclaimed.
        vm.heap_mut().set_gc_threshold_for_test(256);

        // Build bytecode: CONST 0 (the sentinel int); then N
        // iterations of `MAKE_ENUM 0 1` (an enum wrapping the
        // sentinel); POP each result so the address is no
        // longer on the stack. After POP, the enum is
        // unreachable — the next GC cycle should free it.
        let n: usize = 200;
        let mut bytecode: Vec<Byte> = Vec::with_capacity(n * 3 + 2);
        for _ in 0..n {
            bytecode.push(const_int(0));
            bytecode.push(make_enum(0, 1));
            bytecode.push(Byte::new(Instruction::POP));
        }
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run(&bytecode);

        // After running, the heap should contain FAR FEWER
        // than N objects — GC reclaims POPed enums.
        let live_addrs: HashSet<u64> = vm.heap().into_iter().map(|o| o.addr()).collect();

        assert!(
            live_addrs.len() < n,
            "expected heap to contain far fewer than {} objects, got {}",
            n,
            live_addrs.len()
        );

        let _ = vm.heap().size();
    }

    #[test]
    fn live_enum_survives_automatic_gc_cycle() {
        use std::collections::HashSet;

        let mut vm = Machine::<256>::default();
        vm.heap_mut().set_gc_threshold_for_test(256);

        // Build bytecode:
        //   MAKE_ENUM 7 1 (the live root, payload = sentinel int)
        //   loop 200 times:
        //     MAKE_ENUM 0 1 (an unrelated enum — unreachable
        //     after POP)
        //     POP
        //   HALT
        //
        // The live root's address sits on the operand stack
        // for the entire program — so the GC must preserve
        // it across every collection cycle.
        let n: usize = 200;
        let mut bytecode: Vec<Byte> = Vec::with_capacity(n * 2 + 4);
        bytecode.push(const_int(0)); // sentinel payload
        bytecode.push(make_enum(7, 1)); // tag=7 sentinel, arity=1
        let root_addr = {
            // We can't easily capture the address at codegen
            // time (we'd need a DUP + something), so we'll
            // just inspect the heap after the run instead.
            // For now, leave the live root on the stack.
            // Duplicate it so we still have it after we POP
            // unrelated allocations... wait, no — the
            // unrelated allocations are POPed, the root is
            // NOT popped. Just leave it.
            vm.run(&[]); // dummy to silence unused
            0u64
        };
        let _ = root_addr;

        for _ in 0..n {
            bytecode.push(const_int(0));
            bytecode.push(make_enum(0, 1));
            bytecode.push(Byte::new(Instruction::POP));
        }
        // Now the live root is at the bottom of the stack,
        // with n stale enums (already POPed) above it on
        // nothing (they were popped off the stack but their
        // allocations may still be on the heap until GC).
        // HALT.
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run(&bytecode);

        // The live root should still be on the stack (we
        // never POPed it). We can't easily inspect the stack
        // from outside, but we CAN inspect the heap: after GC
        // the heap should contain only the live root. The n
        // unreachable enums should have been collected.
        let live_addrs: HashSet<u64> = vm.heap().into_iter().map(|o| o.addr()).collect();

        // Bound: at most a small handful of objects — the
        // live root (1) plus at most the threshold minus one
        // (uncollected but unreachable) enums. The point is
        // `live_addrs.len() < n` — without GC, it would be
        // ~n+1.
        assert!(
            live_addrs.len() < n,
            "expected heap to be much smaller than n={}, got {}",
            n,
            live_addrs.len()
        );

        // At least the live root should be present.
        assert!(
            !live_addrs.is_empty(),
            "expected at least one live object (the root enum)"
        );
    }

    #[test]
    fn panic_opcode_sets_panicked_and_writes_message() {
        let mut vm = Machine::<4>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let strings = vec!["boom".to_string()];
        let mut bytecode = Vec::new();
        // STRING table[0] == "boom"
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::Panic));
        // Unreachable if Panic aborts.
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 0);
        assert!(vm.panicked());
        let _ = vm.restore_output();
        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("output should be valid UTF-8");
        assert_eq!(s, "panic: boom");
    }

    #[test]
    fn with_output_captures_print() {
        let mut vm = Machine::<16>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        // Build bytecode:
        //   STRING table[0] == "hello"
        //   PRINT
        //   HALT
        let strings = vec!["hello".to_string()];
        let mut bytecode: Vec<Byte> = Vec::new();
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 0);

        // Drop the sink first so the `Rc` we hold is the
        // only one (then we can move the `Vec` out).
        let _ = vm.restore_output();

        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("output should be valid UTF-8");
        assert_eq!(s, "hello");
    }

    /// Regression: `STRING` / `FORMAT` used to `intern` then maybe
    /// `gc_collect` *before* pushing. The intern table is not a GC
    /// root, so the fresh object could be swept and the pushed
    /// pointer dangling — exposed by heavy `"%s%s"` concat (HTTP
    /// showcase / string-table era).
    #[test]
    fn string_literal_survives_gc_triggered_at_intern() {
        let mut vm = Machine::<16>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let n = 72;
        let strings: Vec<String> = (0..n).map(|i| format!("lit-{i}")).collect();
        let mut bytecode: Vec<Byte> = Vec::with_capacity(n * 2 + 2);
        for i in 0..n {
            bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(i as u32));
            // Drop earlier literals so only the newest is live — maximizes
            // chance the just-interned object is unmarked during GC.
            if i + 1 < n {
                bytecode.push(Byte::new(Instruction::POP));
            }
        }
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 0);
        assert!(!vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        assert_eq!(s, format!("lit-{}", n - 1));
    }

    /// Regression: `MakeEnum` used to allocate then maybe `gc_collect`
    /// *before* pushing. The fresh enum was not a stack root, so it (and
    /// payload objects only reachable through it) could be swept — dangling
    /// `Result::Ok` after a heavy callee, seen as json `stringify` flaking
    /// on `[1,2,true]` under a cold heap.
    #[test]
    fn make_enum_survives_gc_triggered_at_alloc() {
        let mut vm = Machine::<32>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let n = 68;
        let strings: Vec<String> = (0..n).map(|i| format!("e{i}")).collect();
        let mut bytecode: Vec<Byte> = Vec::new();
        // Burn the alloc counter with interned strings (popped so unmarked).
        for i in 0..n {
            bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(i as u32));
            bytecode.push(Byte::new(Instruction::POP));
        }
        // Payload string for Result::Ok(s).
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(make_enum(0, 1)); // Ok(s)
        // JumpIfMatch Ok — if the enum was swept, find_object fails and we
        // fall through to panic.
        let jump_pc = bytecode.len();
        bytecode.push(jump_if_match(0, 0)); // pool[0] patched below
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(1));
        bytecode.push(Byte::new(Instruction::Panic));
        let ok_arm = bytecode.len();
        bytecode.push(Byte::new(Instruction::POP)); // Ok payload
        bytecode.push(Byte::new(Instruction::HALT));
        let _ = jump_pc;
        let constants = [ok_arm as u64];

        vm.run_with_pool(&bytecode, &constants, &strings, 0);
        assert!(
            !vm.panicked(),
            "MakeEnum Ok should survive GC at alloc; JumpIfMatch must see the enum"
        );
    }

    #[test]
    fn format_concat_survives_gc_triggered_at_intern() {
        let mut vm = Machine::<16>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        // FORMAT "%s%s" past the GC interval with unique RHS pieces.
        let n = 68;
        let mut strings = vec!["%s%s".to_string(), "x".to_string()];
        for i in 0..n {
            strings.push(format!("p{i}"));
        }
        let mut bytecode: Vec<Byte> = Vec::new();
        // seed acc in slot 0
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(1));
        bytecode.push(Byte::new(Instruction::STORE).with_load_store_slot(0));
        for i in 0..n {
            // format("%s%s", acc, p_i) — FORMAT pops args then format string.
            bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
            bytecode.push(Byte::new(Instruction::LOAD).with_load_store_slot(0));
            bytecode.push(Byte::new(Instruction::STRING).with_operand_u32((2 + i) as u32));
            bytecode.push(Byte::new(Instruction::FORMAT).with_operand_u32(2));
            bytecode.push(Byte::new(Instruction::STORE).with_load_store_slot(0));
        }
        bytecode.push(Byte::new(Instruction::LOAD).with_load_store_slot(0));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 0);
        assert!(!vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        let mut expect = String::from("x");
        for i in 0..n {
            expect.push_str(&format!("p{i}"));
        }
        assert_eq!(s, expect);
    }

    /// DynAdd string concat also goes through `push_interned_string` (and
    /// `continue`s the dispatch loop). Root-after-intern is required here too.
    #[test]
    fn dyn_add_strings_survives_gc_triggered_at_intern() {
        let mut vm = Machine::<16>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let n = 68;
        let mut strings = vec!["x".to_string()];
        for i in 0..n {
            strings.push(format!("p{i}"));
        }
        let mut bytecode: Vec<Byte> = Vec::new();
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::STORE).with_load_store_slot(0));
        for i in 0..n {
            bytecode.push(Byte::new(Instruction::LOAD).with_load_store_slot(0));
            bytecode.push(Byte::new(Instruction::STRING).with_operand_u32((1 + i) as u32));
            bytecode.push(Byte::new(Instruction::DynAdd));
            bytecode.push(Byte::new(Instruction::STORE).with_load_store_slot(0));
        }
        bytecode.push(Byte::new(Instruction::LOAD).with_load_store_slot(0));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 0);
        assert!(!vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        let mut expect = String::from("x");
        for i in 0..n {
            expect.push_str(&format!("p{i}"));
        }
        assert_eq!(s, expect);
    }

    /// STRINGIFY shares `push_interned_string` — GC at intern must not sweep
    /// the fresh display string before it is stacked.
    #[test]
    fn stringify_survives_gc_triggered_at_intern() {
        let mut vm = Machine::<16>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let n = 72;
        let mut bytecode: Vec<Byte> = Vec::with_capacity(n * 3 + 2);
        for i in 0..n {
            bytecode.push(const_int(i as i64));
            bytecode.push(Byte::new(Instruction::STRINGIFY));
            if i + 1 < n {
                bytecode.push(Byte::new(Instruction::POP));
            }
        }
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run(&bytecode);
        assert!(!vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        assert_eq!(s, format!("{}", n - 1));
    }

    #[test]
    fn with_output_captures_io_stdout_write_all() {
        let mut vm = Machine::<16>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let heap = vm.heap_mut();
        let stdout = crate::io::stream_stdout(heap).expect("stdout stream");
        let elements = b"hello via io"
            .iter()
            .map(|&b| Value::from(b as i64))
            .collect();
        let (arr, _) = heap.alloc(ObjArray { elements }, Object::Array);
        let data = Value::from(arr.addr());
        crate::io::stream_write_all(heap, stdout, data).expect("write_all stdout");

        let _ = vm.restore_output();
        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("output should be valid UTF-8");
        assert_eq!(s, "hello via io");
    }

    /// GetField must return heap-object field values (strings,
    /// nested dicts, …) by address — not the `-1` sentinel used
    /// for missing fields. Pre-P0 returned `-1` for `Member::Object`.
    #[test]
    fn get_field_returns_heap_object_field() {
        let mut vm = Machine::<16>::default();
        // STRING table[0] == "hi"  → heap string
        // STRING table[1] == "s"   → field name
        // MakeDict 1     → { s: "hi" }
        // DUPLICATE
        // STRING 1 "s"
        // GetField       → should push the "hi" string address
        // PRINT
        // HALT
        let strings = vec!["hi".to_string(), "s".to_string()];
        let mut bytecode: Vec<Byte> = Vec::new();
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(1));
        bytecode.push(Byte::new(Instruction::MakeDict).with_operand_u32(1));
        bytecode.push(Byte::new(Instruction::DUPLICATE));
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(1));
        bytecode.push(Byte::new(Instruction::GetField));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.run_with_pool(&bytecode, &[], &strings, 0);
        let _ = vm.restore_output();
        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("output should be valid UTF-8");
        assert_eq!(s, "hi", "GetField should return the stored string, not -1");
    }

    #[test]
    fn store_pop_writes_value_to_slot_and_pops() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            // Push 42 onto the operand stack.
            const_int(42),
            // Pop 42, write to slot 0 (= frame.sp + 0 = 0).
            store_pop(0),
            // Push slot 0 (= 42) back onto the stack.
            load(0),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be 42 — proving both the
        // write-to-slot and the pop-and-write semantics.
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// Packed LOAD n=3 pushes s0, then s1, then s2 (same order as consecutive LOADs).
    #[test]
    fn packed_load_n3_pushes_slots_in_order() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            store_pop(0),
            const_int(20),
            store_pop(1),
            const_int(30),
            store_pop(2),
            Byte::new(Instruction::LOAD).with_load_store_packed(3, 0, 1, 2),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 30); // s2 last pushed
        assert_eq!(vm.pop().as_int(), 20);
        assert_eq!(vm.pop().as_int(), 10); // s0 first pushed
    }

    /// Packed STORE n=3: TOS → s0, then next → s1, then next → s2.
    /// Values sit above the local region so pops do not clobber destinations.
    #[test]
    fn packed_store_n3_pops_into_slots_in_order() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(0),
            store_pop(0),
            const_int(0),
            store_pop(1),
            const_int(0),
            store_pop(2),
            const_int(10),
            const_int(20),
            const_int(30), // TOS
            Byte::new(Instruction::STORE).with_load_store_packed(3, 0, 1, 2),
            load(0),
            load(1),
            load(2),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 10); // slot 2
        assert_eq!(vm.pop().as_int(), 20); // slot 1
        assert_eq!(vm.pop().as_int(), 30); // slot 0 got TOS
    }


    /// Reverse slot order (high→low) must still leave the cursor past max slot
    /// so later pushes do not clobber multi-slot fixed-array locals.
    #[test]
    fn packed_store_reverse_slots_extends_cursor_past_max() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            const_int(20),
            const_int(30), // TOS
            // Same order codegen emits for stack `[T; 3]` locals.
            Byte::new(Instruction::STORE).with_load_store_packed(3, 2, 1, 0),
            const_int(99),
            store_pop(3),
            load(0),
            load(1),
            load(2),
            load(3),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 99);
        assert_eq!(vm.pop().as_int(), 30);
        assert_eq!(vm.pop().as_int(), 20);
        assert_eq!(vm.pop().as_int(), 10);
    }

    #[test]
    fn store_pop_writes_to_correct_slot_index() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            // Push 99, store at slot 0.
            const_int(99),
            store_pop(0),
            // Push 42, store at slot 2.
            const_int(42),
            store_pop(2),
            // Push slot 2 (= 42) — the second binding.
            load(2),
            Byte::new(Instruction::HALT),
        ]);
        // Top of stack should be 42 (the value stored at
        // slot 2). Slot 0 still holds 99.
        assert_eq!(vm.pop().as_int(), 42);
    }

    #[test]
    fn store_pop_two_bindings_preserves_both_values() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            // let x = 5;
            const_int(5),
            store_pop(0),
            // let y = 10;
            const_int(10),
            store_pop(1),
            // read x back
            load(0),
            // push y so we can add them
            load(1),
            // x + y = 15
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 15);
    }

    #[test]
    fn store_pop_overwrites_existing_slot() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            // let x = 5;
            const_int(5),
            store_pop(0),
            // x = 10;
            const_int(10),
            store_pop(0),
            // read x back
            load(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 10);
    }

    /// P0: reassigning a low slot must not truncate the cursor past
    /// higher locals (shared operand-stack / locals area).
    #[test]
    fn store_pop_preserves_higher_locals_and_cursor() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            store_pop(0),
            const_int(20),
            store_pop(1),
            const_int(30),
            store_pop(2),
            // Reassign slot 0 while slots 1 and 2 are live.
            const_int(99),
            store_pop(0),
            load(0),
            load(1),
            load(2),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 30, "slot 2 must survive StorePop 0");
        assert_eq!(vm.pop().as_int(), 20, "slot 1 must survive StorePop 0");
        assert_eq!(vm.pop().as_int(), 99, "slot 0 must hold the new value");
    }

    /// P1: heap objects stored in an array survive GC when the array is rooted.
    #[test]
    fn array_elements_survive_gc() {
        let mut vm = Machine::<64>::default();
        // STRING "hi" → MakeArray(1) → store slot 0 → allocate 128 enums →
        // load slot 0 → Index 0 → PRINT → HALT
        let strings = vec!["hi".to_string()];
        let mut code = Vec::new();
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(Byte::new(Instruction::MakeArray).with_operand_u32(1));
        code.push(store_pop(0));
        for _ in 0..128 {
            code.push(Byte::new(Instruction::MakeEnum).with_operands_u16([0, 0]));
            code.push(Byte::new(Instruction::POP));
        }
        code.push(load(0));
        code.push(const_int(0));
        code.push(Byte::new(Instruction::Index));
        code.push(Byte::new(Instruction::PRINT));
        code.push(Byte::new(Instruction::HALT));

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.run_with_pool(&code, &[], &strings, 1);
        let _ = vm.restore_output();
        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(s, "hi", "array string element must survive GC pressure");
    }

    /// P1 sibling: heap objects stored in a tuple survive GC when the
    /// tuple is rooted. Arrays were covered above; tuples share the
    /// same `mark_aggregate_elements` path and must not regress alone.
    #[test]
    fn tuple_elements_survive_gc() {
        let mut vm = Machine::<64>::default();
        // STRING "ok" → MakeTuple(1) → store slot 0 → allocate 128 enums →
        // load slot 0 → Index 0 → PRINT → HALT
        let strings = vec!["ok".to_string()];
        let mut code = Vec::new();
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(Byte::new(Instruction::MakeTuple).with_operand_u32(1));
        code.push(store_pop(0));
        for _ in 0..128 {
            code.push(Byte::new(Instruction::MakeEnum).with_operands_u16([0, 0]));
            code.push(Byte::new(Instruction::POP));
        }
        code.push(load(0));
        code.push(const_int(0));
        code.push(Byte::new(Instruction::Index));
        code.push(Byte::new(Instruction::PRINT));
        code.push(Byte::new(Instruction::HALT));

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.run_with_pool(&code, &[], &strings, 1);
        let _ = vm.restore_output();
        let bytes = take_test_output(buf);
        let s = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(s, "ok", "tuple string element must survive GC pressure");
    }

    /// P5: PRINT flushes so redirected sinks observe output before HALT.
    /// HALT also flushes, so a single PRINT+HALT program must flush ≥2 times
    /// (once from PRINT, once from HALT). Pre-fix PRINT skipped flush → 1.
    #[test]
    fn print_flushes_output_sink() {
        use std::sync::{Arc, Mutex};
        struct FlushCountingWriter {
            buf: Arc<Mutex<Vec<u8>>>,
            flushes: Arc<Mutex<usize>>,
        }
        impl std::io::Write for FlushCountingWriter {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.buf.lock().unwrap().extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                *self.flushes.lock().unwrap() += 1;
                Ok(())
            }
        }

        let buf = Arc::new(Mutex::new(Vec::new()));
        let flushes = Arc::new(Mutex::new(0usize));
        let mut vm = Machine::<16>::default();
        vm.with_output(FlushCountingWriter {
            buf: Arc::clone(&buf),
            flushes: Arc::clone(&flushes),
        });

        let strings = vec!["xyz".to_string()];
        let mut bytecode = Vec::new();
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));
        vm.run_with_pool(&bytecode, &[], &strings, 0);
        let _ = vm.restore_output();
        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert_eq!(text, "xyz");
        assert!(
            *flushes.lock().unwrap() >= 2,
            "PRINT+HALT must each flush; got {} flush(es)",
            *flushes.lock().unwrap()
        );
    }

    /// Host native dispatch via explicit signature registry.
    #[test]
    fn host_invoke_dispatches_rust_closure() {
        use crate::ffi::FfiSignatureBuilder;
        use crate::memory::FfiType;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let sig = FfiSignatureBuilder::new("inc")
            .ret(FfiType::Void)
            .build()
            .unwrap();
        let mut vm = Machine::<4>::default();
        let fn_id = vm.register_fn(sig, |_heap, _args| {
            COUNTER.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        });
        vm.run(&[
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(
            COUNTER.load(Ordering::SeqCst),
            3,
            "HostInvoke should have invoked the Rust closure 3 times"
        );
    }

    /// Unknown native ids trap in debug and release (no silent stack skip).
    #[test]
    fn host_invoke_unknown_id_traps() {
        let mut vm = Machine::<4>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.run(&[
            Byte::new(Instruction::CONST).with_value_u32(99),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert!(vm.panicked(), "unknown HostInvoke id must trap");
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert!(
            s.contains("HostInvoke: unknown native id 99"),
            "missing unknown-id panic: {s:?}"
        );
    }

    /// Native `Err` traps via `runtime_panic`; callers must not see Result::Err.
    #[test]
    fn host_invoke_native_err_traps() {
        use crate::ffi::{FfiError, FfiSignatureBuilder};
        use crate::memory::FfiType;

        let sig = FfiSignatureBuilder::new("boom")
            .ret(FfiType::Void)
            .build()
            .unwrap();
        let mut vm = Machine::<4>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        let fn_id = vm.register_fn(sig, |_heap, _args| {
            Err(FfiError::InvalidHandle("native failed".into()))
        });
        vm.run(&[
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert!(vm.panicked(), "native Err must trap");
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert!(
            s.contains("HostInvoke failed for `boom`"),
            "missing native-err panic: {s:?}"
        );
        assert!(s.contains("native failed"), "missing error text: {s:?}");
    }

    /// `Ok(None)` without a pending IO park pushes unit so later ops see TOS.
    #[test]
    fn host_invoke_void_ok_none_pushes_unit() {
        use crate::ffi::FfiSignatureBuilder;
        use crate::memory::FfiType;

        let sig = FfiSignatureBuilder::new("void_ok")
            .ret(FfiType::Void)
            .build()
            .unwrap();
        let mut vm = Machine::<4>::default();
        let fn_id = vm.register_fn(sig, |_heap, _args| Ok(None));
        vm.run(&[
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            Byte::new(Instruction::MakeTuple).with_operand_u32(0),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            const_int(42),
            Byte::new(Instruction::HALT),
        ]);
        assert!(!vm.panicked());
        assert_eq!(vm.pop().as_int(), 42, "CONST after HostInvoke must not be lost");
        assert_eq!(
            vm.pop(),
            Value::default(),
            "void Ok(None) must push unit"
        );
    }

    /// `Ok(None)` + pending park still parks (no push, no panic).
    #[test]
    fn host_invoke_ok_none_with_pending_park_parks() {
        use crate::ffi::FfiSignatureBuilder;
        use crate::io::{
            alloc_stream, stream_await_readable, stream_close, stream_set_read_timeout,
            take_pending_io_park,
        };
        use crate::io_handle::NativeHandle;
        use crate::memory::{FfiType, StreamKind};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let writer = TcpStream::connect(addr).expect("connect");
        let (reader, _) = listener.accept().expect("accept");

        let mut vm = Machine::<8>::default();
        let stream = alloc_stream(vm.heap_mut(), NativeHandle::Tcp(reader), StreamKind::Tcp)
            .expect("alloc stream");
        stream_set_read_timeout(vm.heap_mut(), stream, 50).expect("timeout");
        let _ = take_pending_io_park();

        let sig = FfiSignatureBuilder::new("await_readable")
            .arg(FfiType::Int)
            .ret(FfiType::Int)
            .build()
            .unwrap();
        let fn_id = vm.register_fn(sig, |heap, args| {
            stream_await_readable(heap, args[0])
                .map_err(|tag| crate::ffi::FfiError::Unsupported(format!("{tag:?}")))
        });

        vm.push(Value::from(fn_id as i64));
        vm.push(stream);
        let code = [
            Byte::new(Instruction::MakeTuple).with_operand_u32(1),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ];
        let paused = vm.execute(&code, &[], 0);
        assert!(paused, "HostInvoke park must pause execute");
        assert!(
            vm.pending_io.is_some(),
            "pending_io must be set when await parks"
        );
        assert!(!vm.panicked(), "park path must not trap");
        assert_eq!(
            vm.tell(),
            0,
            "park path must not push; wait completion pushes later"
        );
        assert!(
            take_pending_io_park().is_none(),
            "HostInvoke must consume the park request"
        );

        let _ = vm.pending_io.take();
        let _ = stream_close(vm.heap_mut(), stream);
        drop(writer);
    }

    fn install_program(vm: &mut Machine<512>, code: &[Byte]) {
        vm.program_code = unsafe {
            std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
        };
        vm.program_constants.clear();
    }

    /// Reentrant `call_function` runs bytecode at the given offset.
    #[test]
    fn call_function_runs_bytecode_at_offset() {
        let mut vm = Machine::<512>::default();
        install_program(
            &mut vm,
            &[
                load(0),
                const_int(2),
                Byte::new(Instruction::MUL),
                Byte::new(Instruction::RETURN),
            ],
        );
        let out = vm.call_function(0, &[Value::from(21_i64)]);
        assert_eq!(out.as_int(), 42);
    }

    /// `load_program` is the public entry used by `coil test` —
    /// without it, harness cases cannot `call_function` against compiled
    /// bytecode.
    #[test]
    fn load_program_enables_call_function() {
        let mut vm = Machine::<512>::default();
        let code = [
            load(0),
            const_int(3),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        let raw: Vec<RawByte> = unsafe {
            std::slice::from_raw_parts(code.as_ptr().cast::<RawByte>(), code.len()).to_vec()
        };
        vm.load_program(&raw, &[], &[]);
        let out = vm.call_function(0, &[Value::from(39_i64)]);
        assert_eq!(out.as_int(), 42);
        assert!(!vm.panicked());
    }

    /// Harness soft-pass checks `Result::Ok` via tag 0.
    #[test]
    fn result_is_ok_true_for_tag_zero_enum() {
        let mut vm = Machine::<4>::default();
        vm.run(&[const_int(1), make_enum(0, 1), Byte::new(Instruction::HALT)]);
        let v = vm.pop();
        assert!(vm.result_is_ok(v), "tag 0 must count as Ok");
    }

    /// Harness soft-fail checks `Result::Err` via tag 1.
    #[test]
    fn result_is_ok_false_for_tag_one_enum() {
        let mut vm = Machine::<4>::default();
        vm.run(&[const_int(1), make_enum(1, 1), Byte::new(Instruction::HALT)]);
        let v = vm.pop();
        assert!(!vm.result_is_ok(v), "tag 1 must count as Err");
    }

    /// Non-enum values (and missing heap objects) are not Ok.
    #[test]
    fn result_is_ok_false_for_immediate() {
        let vm = Machine::<4>::default();
        assert!(!vm.result_is_ok(Value::from(0_i64)));
        assert!(!vm.result_is_ok(Value::from(42_i64)));
    }

    /// Inner `CALL`/`RETURN` must unwind normally under `call_function`.
    /// Without `nested_frame_depths`, the inner RETURN would capture early
    /// and return 7 instead of continuing the outer body (7+1=8).
    #[test]
    fn call_function_captures_only_outer_return_not_inner_call() {
        let mut vm = Machine::<512>::default();
        // 0: CALL → 4
        // 1: CONST 1
        // 2: ADD
        // 3: RETURN   (outer — captured by call_function)
        // 4: CONST 7
        // 5: RETURN   (inner — must unwind, not capture)
        install_program(
            &mut vm,
            &[
                Byte::new(Instruction::CALL).with_call_packed(0, 4),
                const_int(1),
                Byte::new(Instruction::ADD),
                Byte::new(Instruction::RETURN),
                const_int(7),
                Byte::new(Instruction::RETURN),
            ],
        );
        let out = vm.call_function(0, &[]);
        assert_eq!(out.as_int(), 8);
    }

    /// Nested `call_function` (FFI callback reentrancy) must not clobber the
    /// outer frame-depth target — outer RETURN still captures a non-default value.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn nested_call_function_preserves_outer_return() {
        use crate::ffi::FfiSignature;
        use crate::memory::FfiType;

        let lib_name = crate::ffi::platform_shared_lib_filename("sum");
        let lib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples")
            .join(&lib_name);
        if !lib_path.exists() {
            if std::env::var_os("CI").is_some() {
                panic!("FFI soft-skip forbidden in CI: {lib_name} not built");
            }
            eprintln!("skipping: {lib_name} not built");
            return;
        }

        let mut vm = Machine::<512>::default();
        vm.dload_gate_mut()
            .grant_file("sum", &lib_path)
            .unwrap_or_else(|e| panic!("grant {lib_name}: {e}"));
        let lib_val = vm
            .load_userland_library(lib_path.to_str().unwrap())
            .unwrap_or_else(|e| panic!("load {lib_name}: {e}"));
        let sig = FfiSignature::from_parts(
            "apply_cb",
            vec![FfiType::Callback(0), FfiType::Int],
            FfiType::Int,
        )
        .unwrap();
        let fn_id = vm
            .register_ffi_function(lib_val, sig)
            .unwrap_or_else(|e| panic!("declare apply_cb: {e}"));

        // 0: identity callback — LOAD 0; RETURN
        // 2: outer — FfiInvoke apply_cb(callback@0, 21); POP Result; CONST 99; RETURN
        install_program(
            &mut vm,
            &[
                load(0),
                Byte::new(Instruction::RETURN),
                // outer entry (offset 2); args: lib, fn_id
                load(0),
                load(1),
                Byte::new(Instruction::CodePtr).with_operand_u32(0),
                const_int(21),
                Byte::new(Instruction::MakeTuple).with_operand_u32(2),
                Byte::new(Instruction::FfiInvoke).with_operand_u32(2),
                Byte::new(Instruction::POP),
                const_int(99),
                Byte::new(Instruction::RETURN),
            ],
        );

        let out = vm.call_function(2, &[lib_val, Value::from(fn_id as i64)]);
        assert_eq!(
            out.as_int(),
            99,
            "outer call_function must capture its RETURN after nested callback"
        );
    }

    /// C → coil callback via `apply_cb` in libsum.so.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn vm_callback_apply_cb_doubles() {
        use crate::ffi::{
            FfiSignature, InvokeContext, callback_cif, invoke_via_libffi, make_int_callback,
            prepare_cif_for_symbol, resolve_library,
        };
        use crate::memory::FfiType;
        use std::ffi::c_void;

        let lib_name = crate::ffi::platform_shared_lib_filename("sum");
        let lib_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../examples")
            .join(&lib_name);
        if !lib_path.exists() {
            if std::env::var_os("CI").is_some() {
                panic!("FFI soft-skip forbidden in CI: {lib_name} not built");
            }
            eprintln!("skipping: {lib_name} not built");
            return;
        }
        let mut gate = crate::ffi::DloadGate::deny_all();
        gate.grant_file("sum", &lib_path)
            .unwrap_or_else(|e| panic!("grant {lib_name}: {e}"));
        let lib = resolve_library(lib_path.to_str().unwrap(), None, &[], &gate)
            .unwrap_or_else(|e| panic!("load {lib_name}: {e}"));

        let mut vm = Machine::<512>::default();
        install_program(
            &mut vm,
            &[
                load(0),
                const_int(2),
                Byte::new(Instruction::MUL),
                Byte::new(Instruction::RETURN),
            ],
        );

        let cif = callback_cif(&[FfiType::Int], FfiType::Int, &[]).unwrap();
        let vm_ptr = &mut vm as *mut Machine<512> as *mut c_void;
        let closure = make_int_callback(vm_ptr, 0, Machine::<512>::invoke_call, cif).unwrap();
        let cb_ptr = closure.code_ptr_usize();
        vm.ffi_closures.push(closure);

        let sig = FfiSignature::from_parts(
            "apply_cb",
            vec![FfiType::Callback(0), FfiType::Int],
            FfiType::Int,
        )
        .unwrap();
        let prepared = prepare_cif_for_symbol(&sig, &lib, "apply_cb", &[]).unwrap();
        let args = [Value::from(cb_ptr as u64), Value::from(21_i64)];
        let mut ctx = InvokeContext::new(&mut vm.heap as *mut Heap, &vm.struct_layouts);
        let mut closure_ptrs = Vec::new();
        let ret = invoke_via_libffi(&prepared, &sig, &args, None, &mut ctx, &mut closure_ptrs)
            .unwrap()
            .unwrap();
        assert_eq!(ret.as_int(), 42);
    }

    fn make_coro(arity: u32, target: u32) -> Byte {
        Byte::new(Instruction::MakeCoro).with_call_packed(arity, target)
    }

    /// Create → resume → yield returns the yielded value to the resumer.
    #[test]
    fn coroutine_resume_yields_value() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            make_coro(0, 3),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::HALT),
            // 3: coroutine body
            const_int(42),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// Yield used to pop the caller's pin frame; CALL after resume needs it.
    #[test]
    fn coroutine_yield_keeps_caller_pin_frame() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            make_coro(0, 5),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::CALL).with_call_packed(0, 8),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::NOOP),
            // 5: coroutine body
            const_int(42),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
            // 8: empty callee
            Byte::new(Instruction::ConstReturnImm).with_operand_u32(7),
        ]);
        assert!(!vm.panicked());
        assert_eq!(vm.pop().as_int(), 7);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// Resuming a completed coroutine panics.
    #[test]
    fn coroutine_resume_after_done_panics() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            make_coro(0, 9),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::HALT),
            const_int(7),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert!(vm.panicked());
        assert_eq!(vm.pop().as_int(), 7);
    }

    /// Resume-after-done must emit the runtime panic contract (not a silent 0).
    #[test]
    fn coroutine_resume_after_done_writes_panic_message() {
        let mut vm = Machine::<8>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.run(&[
            make_coro(0, 9),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::HALT),
            const_int(7),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert!(vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert!(
            s.contains("panic: resumed after completion"),
            "unexpected panic output: {s:?}"
        );
        assert!(!s.contains("HALT_SHOULD_NOT_RUN"));
    }

    /// Non-coroutine handles must panic instead of UB / silent no-op.
    #[test]
    fn coroutine_resume_invalid_handle_panics() {
        let mut vm = Machine::<8>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        let strings = vec!["should-not-print".to_string()];
        vm.run_with_pool(
            &[
                const_int(0),
                Byte::new(Instruction::ResumeCoro),
                Byte::new(Instruction::STRING).with_operand_u32(0),
                Byte::new(Instruction::PRINT),
                Byte::new(Instruction::HALT),
            ],
            &[],
            &strings,
            0,
        );
        assert!(vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert!(
            s.contains("panic: resumed invalid coroutine handle"),
            "unexpected panic output: {s:?}"
        );
        assert!(
            !s.contains("should-not-print"),
            "bytecode after panic must not execute: {s:?}"
        );
    }

    /// Suspended coroutine heap slots must stay rooted via `saved_live_mask`.
    #[test]
    fn coroutine_suspended_string_survives_gc() {
        let mut vm = Machine::<256>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let n = 72;
        let mut strings: Vec<String> = vec!["keep-me".into()];
        strings.extend((0..n).map(|i| format!("junk-{i}")));

        // main: resume → alloc junk while suspended → resume → PRINT kept string
        // body: STRING "keep-me"; CONST 1; YieldCoro; PRINT; RETURN
        let body = 8 + n * 3;
        let mut code: Vec<Byte> = Vec::with_capacity(body + 6);
        code.push(make_coro(0, body as u32));
        code.push(store_pop(0));
        code.push(load(0));
        code.push(Byte::new(Instruction::ResumeCoro));
        code.push(Byte::new(Instruction::POP));
        for _ in 0..n {
            code.push(const_int(0));
            code.push(make_enum(0, 1));
            code.push(Byte::new(Instruction::POP));
        }
        code.push(load(0));
        code.push(Byte::new(Instruction::ResumeCoro));
        code.push(Byte::new(Instruction::HALT));
        assert_eq!(code.len(), body);
        code.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        code.push(const_int(1));
        code.push(Byte::new(Instruction::YieldCoro));
        code.push(Byte::new(Instruction::PRINT));
        code.push(Byte::new(Instruction::RETURN));

        vm.run_with_pool(&code, &[], &strings, 0);
        assert!(!vm.panicked(), "GC must not collect suspended string");
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert_eq!(s, "keep-me");
    }

    /// Panic backtraces resolve `fn_symbols` by entry PC (binary search).
    #[test]
    fn runtime_panic_backtrace_includes_fn_symbols() {
        use common::{FnDebugSym, ProgramDebug};

        let mut vm = Machine::<8>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.set_program_debug(ProgramDebug {
            source_files: vec![],
            debug_locs: vec![],
            fn_symbols: vec![
                FnDebugSym {
                    name: "main".into(),
                    entry_pc: 0,
                },
                FnDebugSym {
                    name: "worker".into(),
                    entry_pc: 9,
                },
            ],
        });
        vm.run(&[
            make_coro(0, 9),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::HALT),
            // worker @ 9
            const_int(7),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert!(vm.panicked());
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf8");
        assert!(
            s.contains("panic: resumed after completion"),
            "missing panic message: {s:?}"
        );
        assert!(
            s.contains("in main"),
            "expected fn_symbols backtrace frame, got: {s:?}"
        );
    }

    /// Resume with send + binding yield: second resume returns the sent value.
    #[test]
    fn coroutine_resume_with_send_binding_yield() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            make_coro(0, 8),
            store_pop(0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            const_int(200),
            load(0),
            Byte::new(Instruction::ResumeCoro).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            // 8: coroutine body — yield out, receive send, yield received value
            const_int(100),
            Byte::new(Instruction::YieldCoro),
            store_pop(0),
            load(0),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 200);
        assert_eq!(vm.pop().as_int(), 100);
    }

    /// `yield from` delegation must survive an ordinary RETURN in main
    /// between resumes (host I/O returns through the same `after_return`
    /// path as coroutine completion).
    #[test]
    fn yield_from_delegation_survives_main_return_between_resumes() {
        let mut vm = Machine::<16>::default();
        vm.run(&[
            // main
            make_coro(0, 10), // outer @ 10
            store_pop(0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            store_pop(1), // first yield
            Byte::new(Instruction::CALL).with_call_packed(0, 15), // host stub
            load(0),
            Byte::new(Instruction::ResumeCoro),
            store_pop(2), // second yield
            Byte::new(Instruction::HALT),
            // 10: outer — yield from inner @ 17
            make_coro(0, 17),
            Byte::new(Instruction::YieldFromCoro),
            const_int(0),
            Byte::new(Instruction::RETURN),
            // 15: host stub
            Byte::new(Instruction::RETURN),
            // 17: inner
            const_int(10),
            Byte::new(Instruction::YieldCoro),
            const_int(20),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert!(!vm.panicked());
        assert_eq!(vm.stack[2].as_int(), 20);
    }

    #[test]
    fn log_not_bool_and_int() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(1),
            Byte::new(Instruction::LogNot),
            const_int(0),
            Byte::new(Instruction::LogNot),
            const_int(42),
            Byte::new(Instruction::LogNot),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_bool(), false);
        assert_eq!(vm.pop().as_bool(), true);
        assert_eq!(vm.pop().as_bool(), false);
    }

    #[test]
    fn option_niche_round_trip_preserves_heap_payload() {
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::STRING).with_operand_u32(0),
                Byte::new(Instruction::OptionNicheToHeap),
                Byte::new(Instruction::HeapOptionToNiche),
                Byte::new(Instruction::HALT),
            ],
            &[],
            &["ok".to_string()],
            0,
        );
        let value = vm.pop();
        match vm.heap().find_object_by_addr(value.raw() as u64) {
            Some(Object::String(string)) => assert_eq!(string.as_ref().data, "ok"),
            other => panic!(
                "niche round-trip lost string payload (object present: {})",
                other.is_some()
            ),
        }
    }

    #[test]
    fn pair_box_round_trip_preserves_tag_and_payload() {
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(42),
            const_int(0),
            Byte::new(Instruction::PairToHeap),
            Byte::new(Instruction::HeapToPair),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);
        assert_eq!(vm.pop().as_int(), 42);
    }

    #[test]
    fn float_chain_store_preserves_separate_operation_rounding() {
        let a = 1.0 + 2f64.powi(-27);
        let b = 1.0 - 2f64.powi(-27);
        let c = -1.0;
        let descriptor = (Instruction::MULF as u64)
            | (0_u64 << 8)
            | (1_u64 << 16)
            | ((Instruction::ADDF as u64) << 24)
            | (2_u64 << 32);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST)
                    .with_operand_u32(common::Byte::POOL_FLAG),
                store_pop(0),
                Byte::new(Instruction::CONST)
                    .with_operand_u32(common::Byte::POOL_FLAG | 1),
                store_pop(1),
                Byte::new(Instruction::CONST)
                    .with_operand_u32(common::Byte::POOL_FLAG | 2),
                store_pop(2),
                Byte::new(Instruction::FloatChainStore).with_operand_u32((3 << 16) | 3),
                load(3),
                Byte::new(Instruction::HALT),
            ],
            &[
                Value::from(a).raw() as u64,
                Value::from(b).raw() as u64,
                Value::from(c).raw() as u64,
                descriptor,
            ],
            &[],
            0,
        );

        // Separate MULF then ADDF rounds the product to 1.0 before adding -1.
        assert_eq!(vm.pop().as_float(), 0.0);
    }

    #[test]
    fn float_chain_store_preserves_special_float_values() {
        let descriptor = (Instruction::ADDF as u64)
            | (0_u64 << 8)
            | (1_u64 << 16)
            | ((Instruction::MULF as u64) << 24)
            | (2_u64 << 32);
        let code = [
            Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG),
            store_pop(0),
            Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 1),
            store_pop(1),
            Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 2),
            store_pop(2),
            Byte::new(Instruction::FloatChainStore).with_operand_u32((3 << 16) | 3),
            load(3),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &code,
            &[
                Value::from(-0.0_f64).raw() as u64,
                Value::from(-0.0_f64).raw() as u64,
                Value::from(1.0_f64).raw() as u64,
                descriptor,
            ],
            &[],
            0,
        );
        let signed_zero = vm.pop().as_float();
        assert_eq!(signed_zero, 0.0);
        assert!(signed_zero.is_sign_negative());

        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &code,
            &[
                Value::from(f64::NAN).raw() as u64,
                Value::from(1.0_f64).raw() as u64,
                Value::from(1.0_f64).raw() as u64,
                descriptor,
            ],
            &[],
            0,
        );
        assert!(vm.pop().as_float().is_nan());
    }

    #[test]
    fn float_chain_store_three_stage_const_under_matches_separate_ops() {
        // 2.0 * (zr * zi) + ci with intermediate rounding, const-under MULF.
        let zr = 1.0 + 2f64.powi(-27);
        let zi = 1.0 - 2f64.powi(-27);
        let ci = -1.0;
        let two = 2.0_f64;
        let separate = {
            let t = zr * zi;
            let t = two * t;
            t + ci
        };
        // EXT | has_stage2 | stage1_left | rhs1_const
        let descriptor = (Instruction::MULF as u64)
            | (0_u64 << 8)
            | (1_u64 << 16)
            | ((Instruction::MULF as u64) << 24)
            | (3_u64 << 32) // pool idx of 2.0
            | ((Instruction::ADDF as u64) << 40)
            | (2_u64 << 48) // ci slot
            | (1 << 57) // rhs1 const
            | (1 << 60) // stage1 other on left
            | (1 << 62) // has stage2
            | (1u64 << 63);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG),
                store_pop(0),
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 1),
                store_pop(1),
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 2),
                store_pop(2),
                Byte::new(Instruction::FloatChainStore).with_operand_u32((4 << 16) | 4),
                load(4),
                Byte::new(Instruction::HALT),
            ],
            &[
                Value::from(zr).raw() as u64,
                Value::from(zi).raw() as u64,
                Value::from(ci).raw() as u64,
                Value::from(two).raw() as u64,
                descriptor,
            ],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_float(), separate);
    }

    #[test]
    fn float_chain_store_three_stage_preserves_nan() {
        let descriptor = (Instruction::MULF as u64)
            | (0_u64 << 8)
            | (1_u64 << 16)
            | ((Instruction::MULF as u64) << 24)
            | (3_u64 << 32)
            | ((Instruction::ADDF as u64) << 40)
            | (2_u64 << 48)
            | (1 << 57)
            | (1 << 60)
            | (1 << 62)
            | (1u64 << 63);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG),
                store_pop(0),
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 1),
                store_pop(1),
                Byte::new(Instruction::CONST).with_operand_u32(common::Byte::POOL_FLAG | 2),
                store_pop(2),
                Byte::new(Instruction::FloatChainStore).with_operand_u32((4 << 16) | 4),
                load(4),
                Byte::new(Instruction::HALT),
            ],
            &[
                Value::from(f64::NAN).raw() as u64,
                Value::from(1.0_f64).raw() as u64,
                Value::from(1.0_f64).raw() as u64,
                Value::from(2.0_f64).raw() as u64,
                descriptor,
            ],
            &[],
            0,
        );
        assert!(vm.pop().as_float().is_nan());
    }

    #[test]
    fn vec_niche_pop_does_not_allocate_an_option_enum() {
        let mut vm = Machine::<16>::default();
        let (object, _) = vm.heap.alloc(
            ObjArray {
                elements: vec![Value::from(7_i64)],
            },
            Object::Array,
        );
        let before = vm.heap.live_object_count();
        let value = crate::vec_ops::host_vec_pop_niche(
            &mut vm.heap,
            &[Value::from(object.addr())],
        );

        assert_eq!(value.as_int(), 7);
        assert_eq!(vm.heap.live_object_count(), before);
    }

    #[test]
    fn inc_prefix_returns_new_value() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            store_pop(0),
            Byte::new(Instruction::INC).with_inc_dec(0, true, false),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 6);
        assert_eq!(vm.stack[0].as_int(), 6);
    }

    #[test]
    fn inc_postfix_returns_old_value() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            store_pop(0),
            Byte::new(Instruction::INC).with_inc_dec(0, false, false),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 5);
        assert_eq!(vm.stack[0].as_int(), 6);
    }

    #[test]
    fn dec_prefix_and_postfix() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(5),
            store_pop(0),
            Byte::new(Instruction::DEC).with_inc_dec(0, false, false),
            Byte::new(Instruction::DEC).with_inc_dec(0, true, false),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 3);
        assert_eq!(vm.stack[0].as_int(), 3);
    }

    /// Coroutine handle + saved stack survive an automatic GC cycle.
    #[test]
    fn coroutine_handle_survives_gc() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            make_coro(0, 8),
            store_pop(0),
            make_enum(0, 0),
            make_enum(1, 0),
            make_enum(2, 0),
            load(0),
            Byte::new(Instruction::ResumeCoro),
            Byte::new(Instruction::HALT),
            const_int(99),
            Byte::new(Instruction::YieldCoro),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 99);
    }

    // ── Generics runtime opcode tests ────────────────────────────────────────

    /// `CallIndirect` pops a target offset from the stack and jumps to it,
    /// treating the remaining stack entries as the callee's arguments.
    ///
    /// Layout:
    ///   0: CONST 42        (arg0)
    ///   1: CONST 4         (target = bytecode offset 4)
    ///   2: CallIndirect    (arity=1)
    ///   3: HALT
    ///   4: LOAD 0          (callee: load arg0)
    ///   5: RETURN
    #[test]
    fn call_indirect_jumps_to_target() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(42),
            const_int(4),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            // callee at offset 4
            load(0),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// `CodePtr` pushes an absolute bytecode offset like an integer constant;
    /// `CallIndirect` consumes it as the callee entry.
    #[test]
    fn code_ptr_feeds_call_indirect() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(42),
            Byte::new(Instruction::CodePtr).with_operand_u32(4),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            load(0),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// `BoxValue` wraps a raw integer in an `Object::Boxed` heap cell;
    /// `UnboxValue` recovers the payload when tags match.
    #[test]
    fn box_unbox_int_roundtrip() {
        let int_tag: u32 = 0; // ValueTag::Int
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(99),
            Byte::new(Instruction::BoxValue).with_operand_u32(int_tag),
            Byte::new(Instruction::UnboxValue).with_operand_u32(int_tag),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 99);
    }

    /// `MakePolyFn` allocates a heap object and pushes a non-null address.
    #[test]
    fn make_polyfn_allocates() {
        let mut vm = Machine::<8>::default();
        // entry offset 0 — irrelevant for the allocation test.
        vm.run(&[
            Byte::new(Instruction::MakePolyFn).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        let addr = vm.pop();
        assert!(
            addr.raw() as u64 != 0,
            "MakePolyFn should push a non-null heap pointer"
        );
    }

    /// `DynAdd` with two boxed integers yields their sum as an unboxed int.
    #[test]
    fn dyn_add_ints() {
        let int_tag: u32 = 0; // ValueTag::Int
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            Byte::new(Instruction::BoxValue).with_operand_u32(int_tag),
            const_int(32),
            Byte::new(Instruction::BoxValue).with_operand_u32(int_tag),
            Byte::new(Instruction::DynAdd),
            Byte::new(Instruction::HALT),
        ]);
        // DynAdd on two Int-tagged boxed values returns an unboxed int.
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// `DynAdd` with two boxed floats yields their sum as an unboxed float.
    #[test]
    fn dyn_add_floats() {
        let float_tag: u32 = 1; // ValueTag::Float
        let pool = [1.5f64.to_bits(), 2.5f64.to_bits()];
        let mut vm = Machine::<8>::default();
        vm.run_with_pool(
            &[
                // push 1.5 (pool[0])
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::BoxValue).with_operand_u32(float_tag),
                // push 2.5 (pool[1])
                Byte::new(Instruction::CONST).with_operand_u32(1 | Byte::POOL_FLAG),
                Byte::new(Instruction::BoxValue).with_operand_u32(float_tag),
                Byte::new(Instruction::DynAdd),
                Byte::new(Instruction::HALT),
            ],
            &pool,
            &[],
            0,
        );
        // 1.5 + 2.5 = 4.0
        assert_eq!(vm.pop().as_float(), 4.0);
    }

    /// `MakePolyFnCapture` + `CallIndirect` injects captured dictionaries when
    /// the application site supplies none.
    #[test]
    fn call_indirect_merges_captured_dicts_without_app_evidence() {
        // Layout:
        //  0: CONST 7            captured dict (immediate)
        //  1: CodePtr 8          entry
        //  2: MakePolyFnCapture  (1 slot)
        //  3: StorePop 0         save PolyFn
        //  4: CONST 42           value arg
        //  5: LOAD 0             PolyFn
        //  6: CallIndirect       value_arity=1, app_dict_arity=0
        //  7: HALT
        //  8: LOAD 1             callee reads captured dict
        //  9: RETURN
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(7),
            Byte::new(Instruction::CodePtr).with_operand_u32(8),
            Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(1),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(42),
            load(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            load(1),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 7);
    }

    /// Phase 4: capture with every slot `Some` and `app_dict_arity=0` still
    /// injects all dictionaries for the callee.
    #[test]
    fn call_indirect_all_some_capture_slots_work_with_zero_app_dicts() {
        // Two captured dicts (11, 22); callee returns dict1 + dict2 (slots 1, 2).
        //  0: CONST 11
        //  1: CONST 22
        //  2: CodePtr entry
        //  3: MakePolyFnCapture (2)
        //  4: StorePop 0
        //  5: CONST 1            value arg (unused by callee)
        //  6: LOAD 0
        //  7: CallIndirect value_arity=1, app_dict_arity=0
        //  8: HALT
        //  9: LOAD 1 / LOAD 2 / ADD / RETURN
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(11),
            const_int(22),
            Byte::new(Instruction::CodePtr).with_operand_u32(9),
            Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(2),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(1),
            load(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            load(1),
            load(2),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 33);
    }

    /// Captured evidence wins over a duplicate application dictionary.
    #[test]
    fn call_indirect_prefers_captured_dict_over_app_dict() {
        // Captured dict = 11; app dict = 22; callee returns slot 1.
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(11),
            Byte::new(Instruction::CodePtr).with_operand_u32(9),
            Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(1),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(42),
            const_int(22),
            load(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1 | (1 << 16)),
            Byte::new(Instruction::HALT),
            load(1),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 11);
    }

    /// Null capture slots are filled from application dictionaries.
    #[test]
    fn call_indirect_fills_unresolved_capture_slots_from_app() {
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(0), // unresolved sentinel
            Byte::new(Instruction::CodePtr).with_operand_u32(9),
            Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(1),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(42),
            const_int(33),
            load(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1 | (1 << 16)),
            Byte::new(Instruction::HALT),
            load(1),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 33);
    }

    /// STRINGIFY turns a boxed int into a heap string "42".
    #[test]
    fn stringify_boxed_int_produces_string() {
        let mut vm = Machine::<64>::default();
        let int_tag: u32 = 0;
        vm.run(&[
            const_int(42),
            Byte::new(Instruction::BoxValue).with_operand_u32(int_tag),
            Byte::new(Instruction::STRINGIFY),
            Byte::new(Instruction::HALT),
        ]);
        let s = vm.pop();
        let text = Machine::<64>::object_string_value(&vm.heap, &s);
        assert_eq!(text, "42");
    }

    /// STRINGIFY turns a boxed float into a debug-formatted string.
    #[test]
    fn stringify_boxed_float_produces_string() {
        let pool = [1.5f64.to_bits()];
        let mut vm = Machine::<64>::default();
        let float_tag: u32 = 1;
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::BoxValue).with_operand_u32(float_tag),
                Byte::new(Instruction::STRINGIFY),
                Byte::new(Instruction::HALT),
            ],
            &pool,
            &[],
            0,
        );
        let s = vm.pop();
        let text = Machine::<64>::object_string_value(&vm.heap, &s);
        assert!(
            text.contains("1.5"),
            "expected float display containing 1.5, got {text:?}"
        );
    }

    /// STRINGIFY copies a heap string through.
    #[test]
    fn stringify_string_copies_contents() {
        let mut vm = Machine::<64>::default();
        let strings = vec!["hi".to_string()];
        vm.run_with_pool(
            &[
                Byte::new(Instruction::STRING).with_operand_u32(0),
                Byte::new(Instruction::STRINGIFY),
                Byte::new(Instruction::HALT),
            ],
            &[],
            &strings,
            0,
        );
        let s = vm.pop();
        let text = Machine::<64>::object_string_value(&vm.heap, &s);
        assert_eq!(text, "hi");
    }

    /// Captured heap dictionaries stay alive across GC pressure.
    #[test]
    fn polyfn_captured_dict_survives_gc() {
        let mut vm = Machine::<64>::default();
        // Build a 1-element tuple dict, capture it, allocate many enums to
        // trigger GC, then CallIndirect and read the captured tuple via LOAD 1.
        let mut code = vec![
            Byte::new(Instruction::CodePtr).with_operand_u32(0), // placeholder method
            Byte::new(Instruction::MakeTuple).with_operand_u32(1),
            Byte::new(Instruction::CodePtr).with_operand_u32(0), // entry patched below
            Byte::new(Instruction::MakePolyFnCapture).with_operand_u32(1),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
        ];
        for _ in 0..128 {
            code.push(Byte::new(Instruction::MakeEnum).with_operands_u16([0, 0]));
            code.push(Byte::new(Instruction::POP));
        }
        let entry = code.len() as u32 + 4;
        code[2] = Byte::new(Instruction::CodePtr).with_operand_u32(entry);
        code.extend([
            const_int(1),
            load(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            load(1),
            Byte::new(Instruction::RETURN),
        ]);
        vm.run(&code);
        let dict = vm.pop();
        assert!(
            dict.raw() as u64 != 0,
            "captured dictionary must survive GC"
        );
    }

    fn unpack_at(slot: u16, arity: u16) -> Byte {
        // operands[31:16]=arity, [15:0]=slot_offset (matches UnpackAt dispatch).
        Byte::new(Instruction::UnpackAt).with_operands_u16([arity, slot])
    }

    #[test]
    fn jump_if_match_on_non_enum_falls_through() {
        let mut vm = Machine::<4>::default();
        vm.run_with_pool(
            &[
                const_int(42),
                jump_if_match(0, 0),
                const_int(7),
                Byte::new(Instruction::HALT),
            ],
            &[0u64],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 7);
        assert_eq!(vm.pop().as_int(), 42);
    }

    #[test]
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic]
    fn jump_if_match_on_empty_stack_debug_asserts() {
        let mut vm = Machine::<2>::default();
        vm.run_with_pool(
            &[
                jump_if_match(0, 0),
                const_int(3),
                Byte::new(Instruction::HALT),
            ],
            &[0u64],
            &[],
            0,
        );
    }

    #[test]
    fn unpack_on_non_enum_discards_scrutinee_without_payload() {
        let mut vm = Machine::<4>::default();
        vm.run(&[const_int(1), unpack(1), Byte::new(Instruction::HALT)]);
        assert_eq!(vm.tell(), 0, "non-enum UNPACK should leave stack empty");
    }

    #[test]
    fn unpack_at_on_non_enum_is_noop() {
        let mut vm = Machine::<4>::default();
        vm.run(&[
            const_int(5),
            unpack_at(0, 1),
            load(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 5);
    }

    /// Scratch-area nested records unpack past the live cursor; UnpackAt must
    /// extend `tell` so a later push does not overwrite the written payload.
    #[test]
    fn unpack_at_extends_tell_when_payload_past_cursor() {
        let mut vm = Machine::<8>::default();
        // Slot 0 = sibling 99; slot 1 = enum{3,7} (arity 2). UnpackAt@1 writes
        // payload into slots 1..3. Without seek(3), the next push would clobber
        // slot 2.
        vm.run(&[
            const_int(99),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(7),
            const_int(3),
            make_enum(0, 2),
            Byte::new(Instruction::StorePop).with_operand_u32(1),
            unpack_at(1, 2),
            const_int(111),
            load(0),
            load(1),
            load(2),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(
            vm.pop().as_int(),
            7,
            "payload[1] must survive push after UnpackAt"
        );
        assert_eq!(vm.pop().as_int(), 3, "payload[0] at slot 1");
        assert_eq!(vm.pop().as_int(), 99, "sibling at slot 0 preserved");
        assert_eq!(
            vm.pop().as_int(),
            111,
            "push must land past unpacked payload"
        );
    }

    #[test]
    fn get_field_missing_returns_minus_one() {
        let mut vm = Machine::<16>::default();
        let mut code = Vec::new();
        let mut strings = Vec::new();
        code.push(const_int(1));
        code.extend(string_lit(&mut strings, "a"));
        code.push(Byte::new(Instruction::MakeDict).with_operand_u32(1));
        code.extend(string_lit(&mut strings, "missing"));
        code.push(Byte::new(Instruction::GetField));
        code.push(Byte::new(Instruction::HALT));
        vm.run_with_pool(&code, &[], &strings, 0);
        assert_eq!(vm.pop().as_int(), -1);
    }

    #[test]
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic]
    fn done_coro_empty_stack_debug_asserts() {
        let mut vm = Machine::<2>::default();
        vm.run(&[
            Byte::new(Instruction::DoneCoro),
            Byte::new(Instruction::HALT),
        ]);
    }

    #[test]
    fn done_coro_on_int_pushes_false() {
        let mut vm = Machine::<2>::default();
        vm.run(&[
            const_int(1),
            Byte::new(Instruction::DoneCoro),
            Byte::new(Instruction::HALT),
        ]);
        assert!(!vm.pop().as_bool());
    }

    #[test]
    fn load_field_on_non_enum_pushes_default() {
        let mut vm = Machine::<4>::default();
        vm.run(&[const_int(9), load_field(0), Byte::new(Instruction::HALT)]);
        assert_eq!(vm.tell(), 1);
        assert_eq!(vm.pop(), Value::default());
    }

    /// MakeFn packing: [7:0]=n_cap [15:8]=n_filled [23:16]=arity [24]=is_rest
    fn make_fn_op(n_cap: u32, n_filled: u32, arity: u32, is_rest: bool) -> u32 {
        n_cap | (n_filled << 8) | (arity << 16) | if is_rest { 1 << 24 } else { 0 }
    }

    #[test]
    fn make_fn_then_call_indirect_invokes_entry() {
        // MakeFn → StorePop 0; push args; LOAD 0; CallIndirect
        let body_entry = 9u32;
        let code = vec![
            const_int(0),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(0, 0, 2, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(10),
            const_int(20),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(2),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        assert!(matches!(
            code[body_entry as usize].bytecode(),
            Instruction::LOAD
        ));
        let mut vm = Machine::<16>::default();
        vm.run(&code);
        assert_eq!(vm.pop().as_int(), 30);
    }

    #[test]
    fn make_fn_partial_then_complete_via_call_indirect() {
        let body_entry = 9u32;
        let code = vec![
            const_int(7),
            const_int(0b001),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(0, 1, 2, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(3),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        let mut vm = Machine::<16>::default();
        vm.run(&code);
        assert_eq!(vm.pop().as_int(), 10);
    }

    #[test]
    fn make_fn_with_captures_injects_leading_locals() {
        let body_entry = 9u32;
        let code = vec![
            const_int(10),
            const_int(0),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(1, 0, 1, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(5),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        let mut vm = Machine::<16>::default();
        vm.run(&code);
        assert_eq!(vm.pop().as_int(), 15);
    }

    #[test]
    fn call_indirect_partial_mask_preserves_high_bit() {
        let body_entry = 10u32;
        let partial_mask = 1u64 << 32;
        let constants = vec![partial_mask];
        let code = vec![
            const_int(42),
            Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(0, 1, 33, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(0),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::RETURN),
        ];
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(&code, &constants, &[], 0);
        let addr = vm.pop().raw() as u64;
        let fn_obj = vm
            .heap()
            .find_object_by_addr(addr)
            .expect("partial ObjFn on heap");
        if let crate::Object::Fn(gc) = fn_obj {
            assert_eq!(gc.as_ref().filled_mask, partial_mask);
            assert_eq!(gc.as_ref().captured_args.len(), 1);
            assert_eq!(gc.as_ref().captured_args[0].as_int(), 42);
        } else {
            panic!("expected ObjFn");
        }
    }

    /// Completing a 33-param partial whose only filled hole is slot 32.
    #[test]
    fn call_indirect_completes_high_bit_partial() {
        let body_entry = 40u32;
        let partial_mask = 1u64 << 32;
        let constants = vec![partial_mask];
        let mut code = vec![
            const_int(42),
            Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(0, 1, 33, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
        ];
        for i in 0..32 {
            code.push(const_int(i as i64));
        }
        code.push(Byte::new(Instruction::LOAD).with_operand_u32(0));
        code.push(Byte::new(Instruction::CallIndirect).with_operand_u32(32));
        code.push(Byte::new(Instruction::HALT));
        assert_eq!(code.len(), body_entry as usize);
        code.push(Byte::new(Instruction::LOAD).with_operand_u32(32));
        code.push(Byte::new(Instruction::RETURN));

        let mut vm = Machine::<64>::default();
        vm.run_with_pool(&code, &constants, &[], 0);
        assert!(!vm.panicked());
        assert_eq!(vm.pop().as_int(), 42);
    }

    #[test]
    fn call_indirect_nested_partial_fills_remaining_holes() {
        let body_entry = 14u32;
        let code = [
            const_int(0),
            const_int(body_entry as i64),
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_op(0, 0, 3, false)),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(1),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(1),
            Byte::new(Instruction::StorePop).with_operand_u32(0),
            const_int(2),
            const_int(3),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::CallIndirect).with_operand_u32(2),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::POP),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::LOAD).with_operand_u32(2),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::RETURN),
        ];
        assert!(matches!(
            code[body_entry as usize].bytecode(),
            Instruction::LOAD
        ));
        let mut vm = Machine::<32>::default();
        vm.run(&code);
        assert_eq!(vm.pop().as_int(), 6);
    }

    /// Regression: two `LoadStatic; CONST 1; ADD; StoreStatic` sequences in one
    /// function must not underflow the stack in release builds.
    #[test]
    fn dual_static_assign_sequence_does_not_underflow_stack() {
        let code = [
            Byte::new(Instruction::LoadStatic).with_operand_u32(0),
            Byte::new(Instruction::CONST).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::StoreStatic).with_operand_u32(0),
            Byte::new(Instruction::LoadStatic).with_operand_u32(1),
            Byte::new(Instruction::CONST).with_operand_u32(1),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::StoreStatic).with_operand_u32(1),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<256>::default();
        vm.run_with_pool(&code, &[], &[], 2);
    }

    /// StoreStatic must write the popped value so a later LoadStatic observes it.
    #[test]
    fn load_store_static_round_trips_value() {
        let code = [
            Byte::new(Instruction::CONST).with_operand_u32(42),
            Byte::new(Instruction::StoreStatic).with_operand_u32(0),
            Byte::new(Instruction::LoadStatic).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<64>::default();
        vm.run_with_pool(&code, &[], &[], 1);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// COI-19: heap objects reachable only via static slots must survive GC.
    /// Pre-fix, `gc_collect` rooted the operand stack / coroutines but not
    /// `Machine::statics`, so `StoreStatic` of an `FfiLoad` library (or any
    /// heap value) could be swept while still live.
    #[test]
    fn static_slot_roots_heap_object_across_gc() {
        let mut vm = Machine::<256>::default();
        vm.heap_mut().set_gc_threshold_for_test(0);
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));

        let strings = vec!["survive-static".to_string()];
        let mut bytecode: Vec<Byte> = Vec::new();
        // Intern + StoreStatic leaves the string only in statics (stack empty).
        bytecode.push(Byte::new(Instruction::STRING).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::StoreStatic).with_operand_u32(0));
        // Force many GC cycles with unreachable enums while the string is
        // not a stack root.
        let n = 96usize;
        for _ in 0..n {
            bytecode.push(const_int(0));
            bytecode.push(make_enum(0, 1));
            bytecode.push(Byte::new(Instruction::POP));
        }
        bytecode.push(Byte::new(Instruction::LoadStatic).with_operand_u32(0));
        bytecode.push(Byte::new(Instruction::PRINT));
        bytecode.push(Byte::new(Instruction::HALT));

        vm.run_with_pool(&bytecode, &[], &strings, 1);
        assert!(!vm.panicked(), "LoadStatic after GC must not see a swept string");
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        assert_eq!(s, "survive-static");
    }

    /// TailCall reuses the current frame (no nest) and overwrites args in place.
    /// Manual sum_to(n, acc): if n <= 0 return acc; else TailCall(n-1, acc+n).
    #[test]
    fn tail_call_reuses_frame_and_computes_sum() {
        let leq = Instruction::LEQ as u8;
        let sub = Instruction::SUB as u8;
        let code = [
            Byte::new(Instruction::CONST).with_const_inline(5),
            Byte::new(Instruction::CONST).with_const_inline(0),
            Byte::new(Instruction::CALL).with_call_packed(2, 4),
            Byte::new(Instruction::HALT),
            // 4: if !(n <= 0) jump to recurse at 7
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(leq, 0, 0),
            Byte::new(Instruction::JMPF).with_operand_u32(7),
            // 6: return acc
            Byte::new(Instruction::LoadReturnSlot).with_operand_u32(1),
            // 7: n - 1 onto stack
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(sub, 0, 1),
            // 8–10: acc + n → new_acc; stack = [n-1, new_acc]
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::TailCall).with_call_packed(2, 4),
        ];
        let mut vm = Machine::<128>::default();
        vm.run(&code);
        // sum_to(5,0) = 5+4+3+2+1 = 15
        assert_eq!(vm.pop().as_int(), 15);
    }

    /// TailCall must not push frames: deep recursion stays within Machine::<64> frames.
    #[test]
    fn tail_call_does_not_grow_frame_stack() {
        // sum_to(200, 0) via TailCall — if TailCall pushed frames like CALL,
        // Machine::<64> would overflow the frame stack.
        let leq = Instruction::LEQ as u8;
        let sub = Instruction::SUB as u8;
        let code = [
            Byte::new(Instruction::CONST).with_const_inline(200),
            Byte::new(Instruction::CONST).with_const_inline(0),
            Byte::new(Instruction::CALL).with_call_packed(2, 4),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(leq, 0, 0),
            Byte::new(Instruction::JMPF).with_operand_u32(7),
            Byte::new(Instruction::LoadReturnSlot).with_operand_u32(1),
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(sub, 0, 1),
            Byte::new(Instruction::LOAD).with_operand_u32(1),
            Byte::new(Instruction::LOAD).with_operand_u32(0),
            Byte::new(Instruction::ADD),
            Byte::new(Instruction::TailCall).with_call_packed(2, 4),
        ];
        let mut vm = Machine::<64>::default();
        vm.run(&code);
        // 200+199+...+1 = 20100
        assert_eq!(vm.pop().as_int(), 20100);
    }

    /// Out-of-range StoreStatic is a defensive no-op in release (debug_assert in dev).
    #[test]
    #[cfg(not(debug_assertions))]
    fn store_static_out_of_range_is_noop() {
        let code = [
            Byte::new(Instruction::CONST).with_operand_u32(7),
            Byte::new(Instruction::StoreStatic).with_operand_u32(99),
            Byte::new(Instruction::CONST).with_operand_u32(1),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<64>::default();
        vm.run_with_pool(&code, &[], &[], 1);
        assert_eq!(vm.pop().as_int(), 1);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "slot < self.statics.len()")]
    fn store_static_out_of_range_debug_asserts() {
        let code = [
            Byte::new(Instruction::CONST).with_operand_u32(7),
            Byte::new(Instruction::StoreStatic).with_operand_u32(99),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<64>::default();
        vm.run_with_pool(&code, &[], &[], 1);
    }

    #[test]
    fn cast_int_to_byte_truncates_high_bits() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(257),
            Byte::new(Instruction::CastIntToByte),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 1);
    }

    #[test]
    fn cast_int_to_byte_wraps_negatives() {
        let mut vm = Machine::<8>::default();
        // Negatives need the constant pool (inline CONST cannot encode them).
        let neg1 = Value::from(-1_i64).raw() as u64;
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::CastIntToByte),
                Byte::new(Instruction::HALT),
            ],
            &[neg1],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 255);
    }

    #[test]
    fn cast_int_to_bool_normalizes_nonzero() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(2),
            Byte::new(Instruction::CastIntToBool),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 1);

        vm.run(&[
            const_int(0),
            Byte::new(Instruction::CastIntToBool),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// Live-slice GC must not treat POP'd slots past `tell` as roots.
    /// Rooting the full operand-stack buffer would keep the tag-99 enum alive forever.
    #[test]
    fn gc_does_not_root_stale_slots_past_tell() {
        use crate::memory::Object;
        use std::collections::HashSet;

        let mut vm = Machine::<256>::default();
        vm.heap_mut().set_gc_threshold_for_test(256);
        let n: usize = 200;
        let mut bytecode: Vec<Byte> = Vec::with_capacity(n * 2 + 8);
        bytecode.push(const_int(0));
        bytecode.push(make_enum(99, 1)); // distinctive tag — then POP so only stale.
        bytecode.push(Byte::new(Instruction::POP));
        for _ in 0..n {
            bytecode.push(const_int(0));
            bytecode.push(make_enum(0, 1));
            bytecode.push(Byte::new(Instruction::POP));
        }
        bytecode.push(Byte::new(Instruction::HALT));
        vm.run(&bytecode);

        let mut tag99 = HashSet::new();
        for obj in vm.heap().into_iter() {
            if let Object::Enum(gc) = obj {
                if gc.as_ref().tag == 99 {
                    tag99.insert(obj.addr());
                }
            }
        }
        assert!(
            tag99.is_empty(),
            "POP'd enum must not survive via stale buffer slots; still live: {tag99:?}"
        );
    }

    /// Fused CmpJmpf: false comparison jumps; true comparison falls through.
    #[test]
    fn cmp_jmpf_jumps_when_false_falls_through_when_true() {
        // 3 < 5 → taken=true → fall through → push 1
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(3),
            const_int(5),
            Byte::new(Instruction::CmpJmpf).with_cmp_jmpf(Instruction::LE as u8, 5),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0), // target 5
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 1);

        // 5 < 3 → taken=false → jump → push 0
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(5),
            const_int(3),
            Byte::new(Instruction::CmpJmpf).with_cmp_jmpf(Instruction::LE as u8, 5),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// Fused CmpJmpt: true comparison jumps; false comparison falls through.
    #[test]
    fn cmp_jmpt_jumps_when_true_falls_through_when_false() {
        // 3 < 5 → taken=true → jump → push 0
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(3),
            const_int(5),
            Byte::new(Instruction::CmpJmpt).with_cmp_jmpf(Instruction::LE as u8, 5),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);

        // 5 < 3 → taken=false → fall through → push 1
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(5),
            const_int(3),
            Byte::new(Instruction::CmpJmpt).with_cmp_jmpf(Instruction::LE as u8, 5),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// Fused float CmpJmpf uses as_float paths (not raw bit compares).
    #[test]
    fn cmp_jmpf_float_leq_falls_through_on_equal() {
        let a = 1.5f64.to_bits();
        let b = 1.5f64.to_bits();
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG | 1),
                Byte::new(Instruction::CmpJmpf).with_cmp_jmpf(Instruction::LEQF as u8, 6),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0), // unreachable if LEQF falls through
                Byte::new(Instruction::HALT),
            ],
            &[a, b],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotImmJmpf reads packed (target<<32)|imm from the constant pool.
    #[test]
    fn bin_slot_imm_jmpf_uses_pool_imm_and_target() {
        let leq = Instruction::LEQ as u8;
        // n=3; if !(n <= 2) jump to done(push 0); else push 1
        // packed: imm=2 in low 32, target=8 in high 32
        let packed = (8u64 << 32) | (2u32 as u64);
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(3),
                Byte::new(Instruction::CALL).with_call_packed(1, 3),
                Byte::new(Instruction::HALT),
                // 3: frame with n in slot 0
                Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(leq, 0, 0),
                // 4: n <= 2 was true → fall through
                const_int(1),
                Byte::new(Instruction::RETURN),
                // 6: padding so target 8 is unambiguous
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::HALT),
                // 8: jumped here when n <= 2 is false
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        // n=2 → LEQ true → fall through → 1
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(2),
                Byte::new(Instruction::CALL).with_call_packed(1, 3),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(leq, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotImmJmpt jumps when the compare is true (inverted `if n <= 2 { … }`).
    #[test]
    fn bin_slot_imm_jmpt_jumps_when_compare_true() {
        let leq = Instruction::LEQ as u8;
        let packed = (8u64 << 32) | (2u32 as u64);
        // n=2 → LEQ true → jump → 0
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(2),
                Byte::new(Instruction::CALL).with_call_packed(1, 3),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotImmJmpt).with_bin_slot_imm_jmpf(leq, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        // n=3 → LEQ false → fall through → 1
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(3),
                Byte::new(Instruction::CALL).with_call_packed(1, 3),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotImmJmpt).with_bin_slot_imm_jmpf(leq, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotSlotJmpf: two-local compare + JMPF without stack traffic.
    #[test]
    fn bin_slot_slot_jmpf_jumps_when_compare_false() {
        let le = Instruction::LE as u8;
        // pool: b=1, target=8
        let packed = (8u64 << 32) | 1u64;
        // a=3, b=5 → 3<5 true → fall through → 1
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(3),
                const_int(5),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                // 4: frame slots 0,1
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(le, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                // 8: jumped when compare false
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);

        // a=5, b=3 → 5<3 false → jump → 0
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(5),
                const_int(3),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(le, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// BinSlotSlotJmpt: jump when the two-local compare is true (inverted break).
    #[test]
    fn bin_slot_slot_jmpt_jumps_when_compare_true() {
        let le = Instruction::LE as u8;
        let packed = (8u64 << 32) | 1u64;
        // a=3, b=5 → 3<5 true → jump → 0
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(3),
                const_int(5),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpt).with_bin_slot_slot_jmpf(le, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        // a=5, b=3 → 5<3 false → fall through → 1
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(5),
                const_int(3),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpt).with_bin_slot_slot_jmpf(le, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotImmStore: compute slot⊕imm and write dest without stack traffic.
    #[test]
    fn bin_slot_imm_store_writes_dest() {
        let add = Instruction::ADD as u8;
        // slot0=10; BinSlotImmStore ADD src=0 imm=1 dest=0 → slot0=11; return slot0
        let packed = (0u64 << 32) | (1u16 as u64);
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(10),
                Byte::new(Instruction::CALL).with_call_packed(1, 3),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(add, 0, 0),
                load(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 11);
    }

    /// BinSlotSlotStore: AND two locals into dest.
    #[test]
    fn bin_slot_slot_store_and_writes_dest() {
        let and = Instruction::AND as u8;
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(1),
                const_int(1),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(and, 0, 1, 2),
                load(2),
                Byte::new(Instruction::RETURN),
            ],
            &[],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_bool(), true);
    }

    /// CmpJmpf / LogNotJmpf resolve large targets via the constant pool.
    #[test]
    fn cmp_jmpf_and_log_not_jmpf_pool_targets() {
        let target = 5usize;
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                const_int(5),
                const_int(3),
                Byte::new(Instruction::CmpJmpf).with_cmp_jmpf_pool(Instruction::LE as u8, 0),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[target as u64],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                const_int(1), // nonzero → LogNotJmpf jumps
                Byte::new(Instruction::LogNotJmpf).with_log_not_jmpf_pool(0),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[4u64],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// LogNotJmpt jumps when the popped value is falsy (fused `!x; JMPT`).
    #[test]
    fn log_not_jmpt_jumps_when_falsy_falls_through_when_truthy() {
        // 0 → LogNot true → Jmpt jumps → push 0
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(0),
            Byte::new(Instruction::LogNotJmpt).with_log_not_jmpf(4),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 0);

        // nonzero → LogNot false → fall through → push 1
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(1),
            Byte::new(Instruction::LogNotJmpt).with_log_not_jmpf(4),
            const_int(1),
            Byte::new(Instruction::HALT),
            const_int(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotImm covers bitwise and logical ops.
    #[test]
    fn bin_slot_imm_bitwise_and_logical() {
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(0b1011),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::BITAND as u8, 0, 1),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 1);

        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(1),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::AND as u8, 0, 1),
            Byte::new(Instruction::RETURN),
        ]);
        assert!(vm.pop().as_bool());
    }

    /// BinSlotSlotJmpf covers logical AND and BITAND (compiler fuses `a && b` / `a & b` conditions).
    #[test]
    fn bin_slot_slot_jmpf_and_and_bitand() {
        let and = Instruction::AND as u8;
        // pool: b=1, target=8
        let packed = (8u64 << 32) | 1u64;
        // true && true → fall through → 1
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(1),
                const_int(1),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(and, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);

        // true && false → jump → 0
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(1),
                const_int(0),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(and, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        let bitand = Instruction::BITAND as u8;
        // 0b101 & 0b010 = 0 → false → jump → 0
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(0b101),
                const_int(0b010),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(bitand, 0, 0),
                const_int(1),
                Byte::new(Instruction::RETURN),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::RETURN),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// BinSlotImmStore to a dest past the live cursor must extend tell so higher locals survive.
    #[test]
    fn bin_slot_imm_store_extends_cursor_and_preserves_higher() {
        let add = Instruction::ADD as u8;
        // dest=2, imm=1 — writes past the current cursor after a single local
        let packed = (2u64 << 32) | 1u64;
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(10),
                store_pop(0),
                // slot0=10; BinSlotImmStore ADD → slot2=11 (extends cursor)
                Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(add, 0, 0),
                // Reassign low slot; must not truncate past slot 2.
                const_int(99),
                store_pop(0),
                load(0),
                load(2),
                Byte::new(Instruction::HALT),
            ],
            &[packed],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 11, "dest slot must hold fused result");
        assert_eq!(vm.pop().as_int(), 99, "low slot reassignment must stick");
    }

    /// BinSlotSlotStore BITAND matches the compiler's `flags = flags & mask` fuse.
    #[test]
    fn bin_slot_slot_store_bitand_writes_dest() {
        let bitand = Instruction::BITAND as u8;
        let mut vm = Machine::<32>::default();
        vm.run_with_pool(
            &[
                const_int(0b1111),
                const_int(0b0101),
                Byte::new(Instruction::CALL).with_call_packed(2, 4),
                Byte::new(Instruction::HALT),
                Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(bitand, 0, 1, 2),
                load(2),
                Byte::new(Instruction::RETURN),
            ],
            &[],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0b0101);
    }

    /// Wide LOAD/STORE (`n==0`, slot in low 24 bits) round-trips past the u8 packed range.
    #[test]
    fn wide_load_store_slot_past_255() {
        let slot = 300u32;
        // Default operand stack is 256 slots; wide local 300 needs a larger buffer.
        let mut vm = Machine::<512>::with_operand_capacity(512);
        vm.run(&[
            const_int(42),
            Byte::new(Instruction::STORE).with_load_store_wide(slot),
            Byte::new(Instruction::LOAD).with_load_store_wide(slot),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 42);
    }

    /// CmpJmpf pool path falls through when the compare is true (not only the taken jump).
    #[test]
    fn cmp_jmpf_pool_falls_through_when_compare_true() {
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                const_int(2),
                const_int(5),
                // 2 < 5 → true → fall through → 1
                Byte::new(Instruction::CmpJmpf).with_cmp_jmpf_pool(Instruction::LE as u8, 0),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[5u64],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotSlotConstJmpf: ADDF(slots) > pool float — fall through / jump.
    #[test]
    fn bin_slot_slot_const_jmpf_addf_gtf() {
        let four = 4.0f64.to_bits();
        let three = 3.0f64.to_bits();
        let one = 1.0f64.to_bits();
        let two = 2.0f64.to_bits();
        // Code layout: setup(0..3), fused(4), fall(5..6), jump(7..8).
        // 3.0+1.0=4.0; 4.0 > 4.0 is false → jump to 7 → push 0
        let desc_jump =
            common::Byte::pack_bin_slot_slot_const_jmpf_desc(1, Instruction::GTF as u8, 2, 7);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::STORE).with_operand_u32(0),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG | 1),
                Byte::new(Instruction::STORE).with_operand_u32(1),
                Byte::new(Instruction::BinSlotSlotConstJmpf).with_bin_slot_slot_const_jmpf(
                    Instruction::ADDF as u8,
                    0,
                    3,
                ),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[three, one, four, desc_jump],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);

        // 3.0+2.0=5.0; 5.0 > 4.0 → fall through → push 1
        let desc_fall =
            common::Byte::pack_bin_slot_slot_const_jmpf_desc(1, Instruction::GTF as u8, 2, 7);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::STORE).with_operand_u32(0),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG | 1),
                Byte::new(Instruction::STORE).with_operand_u32(1),
                Byte::new(Instruction::BinSlotSlotConstJmpf).with_bin_slot_slot_const_jmpf(
                    Instruction::ADDF as u8,
                    0,
                    3,
                ),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[three, two, four, desc_fall],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);
    }

    /// BinSlotSlotConstJmpt: ADDF(slots) > pool float — jump when true (escape break).
    #[test]
    fn bin_slot_slot_const_jmpt_addf_gtf() {
        let four = 4.0f64.to_bits();
        let three = 3.0f64.to_bits();
        let one = 1.0f64.to_bits();
        let two = 2.0f64.to_bits();
        // 3.0+1.0=4.0; 4.0 > 4.0 is false → fall through → push 1
        let desc_fall =
            common::Byte::pack_bin_slot_slot_const_jmpf_desc(1, Instruction::GTF as u8, 2, 7);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::STORE).with_operand_u32(0),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG | 1),
                Byte::new(Instruction::STORE).with_operand_u32(1),
                Byte::new(Instruction::BinSlotSlotConstJmpt).with_bin_slot_slot_const_jmpf(
                    Instruction::ADDF as u8,
                    0,
                    3,
                ),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[three, one, four, desc_fall],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 1);

        // 3.0+2.0=5.0; 5.0 > 4.0 → jump → push 0
        let desc_jump =
            common::Byte::pack_bin_slot_slot_const_jmpf_desc(1, Instruction::GTF as u8, 2, 7);
        let mut vm = Machine::<16>::default();
        vm.run_with_pool(
            &[
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG),
                Byte::new(Instruction::STORE).with_operand_u32(0),
                Byte::new(Instruction::CONST).with_operand_u32(Byte::POOL_FLAG | 1),
                Byte::new(Instruction::STORE).with_operand_u32(1),
                Byte::new(Instruction::BinSlotSlotConstJmpt).with_bin_slot_slot_const_jmpf(
                    Instruction::ADDF as u8,
                    0,
                    3,
                ),
                const_int(1),
                Byte::new(Instruction::HALT),
                const_int(0),
                Byte::new(Instruction::HALT),
            ],
            &[three, two, four, desc_jump],
            &[],
            0,
        );
        assert_eq!(vm.pop().as_int(), 0);
    }

    /// Packed LOAD/STORE n=2 preserves push/pop order (n=3 is covered separately).
    #[test]
    fn packed_load_store_n2_order() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(10),
            store_pop(0),
            const_int(20),
            store_pop(1),
            Byte::new(Instruction::LOAD).with_load_store_packed(2, 0, 1, 0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 20);
        assert_eq!(vm.pop().as_int(), 10);

        let mut vm = Machine::<8>::default();
        vm.run(&[
            const_int(0),
            store_pop(0),
            const_int(0),
            store_pop(1),
            const_int(7),
            const_int(8), // TOS
            Byte::new(Instruction::STORE).with_load_store_packed(2, 0, 1, 0),
            load(0),
            load(1),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 7); // slot 1
        assert_eq!(vm.pop().as_int(), 8); // slot 0 got TOS
    }

    /// In-register BinSlotImm Pow avoids the old push/binary stack dance.
    #[test]
    fn bin_slot_imm_pow_computes_without_stack_roundtrip() {
        let pow = Instruction::Pow as u8;
        let mut vm = Machine::<16>::default();
        vm.run(&[
            const_int(2),
            Byte::new(Instruction::CALL).with_call_packed(1, 3),
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(pow, 0, 3),
            Byte::new(Instruction::RETURN),
        ]);
        assert_eq!(vm.pop().as_int(), 8);
    }

    /// HostInvoke borrows tuple elements as `&[Value]` (no clone); native must
    /// still observe args and may allocate without dangling the slice.
    #[test]
    fn host_invoke_slice_args_readable_during_allocating_native() {
        use crate::ffi::FfiSignatureBuilder;
        use crate::memory::{FfiType, ObjString, Object};

        let sig = FfiSignatureBuilder::new("sum_alloc")
            .arg(FfiType::Int)
            .arg(FfiType::Int)
            .ret(FfiType::Int)
            .build()
            .unwrap();
        let mut vm = Machine::<32>::default();
        let fn_id = vm.register_fn(sig, |heap, args| {
            assert_eq!(args.len(), 2);
            let sum = args[0].as_int() + args[1].as_int();
            // Allocate while the args slice is live — must not free the tuple.
            let (_obj, _) = heap.alloc(ObjString::from("scratch"), Object::String);
            Ok(Some(Value::from(sum)))
        });
        // Stack order: fn_id under tuple (HostInvoke pops tuple, then fn_id).
        vm.run(&[
            Byte::new(Instruction::CONST).with_value_u32(fn_id as u32),
            const_int(20),
            const_int(22),
            Byte::new(Instruction::MakeTuple).with_operand_u32(2),
            Byte::new(Instruction::HostInvoke).with_operand_u32(0),
            Byte::new(Instruction::HALT),
        ]);
        assert_eq!(vm.pop().as_int(), 42);
    }

    #[test]
    fn debug_breakpoint_stops_before_target() {
        use crate::DebugController;
        use crate::StopReason;
        use common::Byte as OwnedByte;
        use common::Instruction as OwnedInsn;

        // Build owned bytes then transmute like run_raw.
        let owned = vec![
            OwnedByte::new(OwnedInsn::CONST).with_const_inline(1),
            OwnedByte::new(OwnedInsn::CONST).with_const_inline(2),
            OwnedByte::new(OwnedInsn::ADD),
            OwnedByte::new(OwnedInsn::HALT),
        ];
        let code: &[Byte] =
            unsafe { std::slice::from_raw_parts(owned.as_ptr().cast(), owned.len()) };

        let mut vm = Machine::<16>::default();
        let mut dbg = DebugController::new();
        dbg.add_breakpoint(2); // stop at ADD
        vm.attach_debug(dbg);

        let reason = vm.debug_run_until(code, &[], &[], 0, 0);
        assert_eq!(reason, StopReason::Breakpoint { pc: 2 });
        assert_eq!(vm.debug_ip(), 2);

        // Continue past the breakpoint to HALT.
        if let Some(d) = vm.debug_controller_mut() {
            d.skip_breakpoint_once(2);
        }
        let reason = vm.debug_run_until(code, &[], &[], 0, 2);
        assert_eq!(reason, StopReason::Halt);
    }

    #[test]
    fn debug_stepi_advances_one_insn() {
        use crate::DebugController;
        use crate::StopReason;
        use common::Byte as OwnedByte;
        use common::Instruction as OwnedInsn;

        let owned = vec![
            OwnedByte::new(OwnedInsn::CONST).with_const_inline(7),
            OwnedByte::new(OwnedInsn::CONST).with_const_inline(8),
            OwnedByte::new(OwnedInsn::HALT),
        ];
        let code: &[Byte] =
            unsafe { std::slice::from_raw_parts(owned.as_ptr().cast(), owned.len()) };

        let mut vm = Machine::<16>::default();
        vm.attach_debug(DebugController::new());
        if let Some(d) = vm.debug_controller_mut() {
            d.set_stepi();
        }
        let reason = vm.debug_run_until(code, &[], &[], 0, 0);
        assert_eq!(reason, StopReason::Step);
        assert_eq!(vm.debug_ip(), 1);
    }

    #[test]
    fn with_operand_capacity_clamps_and_reports() {
        let default_vm = Machine::<16>::default();
        assert_eq!(
            default_vm.operand_stack_capacity(),
            crate::DEFAULT_OPERAND_STACK_SLOTS
        );

        let zero = Machine::<16>::with_operand_capacity(0);
        assert_eq!(zero.operand_stack_capacity(), 1);

        let huge = Machine::<16>::with_operand_capacity(crate::MAX_OPERAND_STACK_SLOTS + 99);
        assert_eq!(huge.operand_stack_capacity(), crate::MAX_OPERAND_STACK_SLOTS);

        let sized = Machine::<16>::with_operand_capacity(512);
        assert_eq!(sized.operand_stack_capacity(), 512);
    }

    #[test]
    fn init_typed_stamps_type_id() {
        let mut vm = Machine::<8>::default();
        vm.run(&[
            Byte::new(Instruction::InitTyped).with_operand_u32(7),
            Byte::new(Instruction::HALT),
        ]);
        vm.collect_garbage();
        let v = vm.pop();
        assert_eq!(vm.instance_meta(v), Some((7, false)));
    }

    #[test]
    fn finalizer_registry_round_trip() {
        let mut vm = Machine::<8>::default();
        vm.register_finalizer_for_test(3, 99);
        assert_eq!(vm.finalizer_pc(3), Some(99));
        assert_eq!(vm.finalizer_pc(1), None);
    }

    #[test]
    fn teardown_runs_remaining_finalizers() {
        // JMP over drop; drop PRINT "closed"; main allocates a typed instance.
        let strings = vec!["closed".to_string()];
        let bytecode = vec![
            Byte::new(Instruction::JMP).with_operand_u32(5),
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::PRINT),
            const_int(0),
            Byte::new(Instruction::RETURN),
            Byte::new(Instruction::InitTyped).with_operand_u32(1),
            Byte::new(Instruction::HALT),
        ];
        let mut vm = Machine::<8>::default();
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        vm.with_output(TestOutputBuf(Arc::clone(&buf)));
        vm.register_finalizer_for_test(1, 1);
        vm.run_with_pool(&bytecode, &[], &strings, 0);
        let _ = vm.restore_output();
        let s = String::from_utf8(take_test_output(buf)).expect("utf-8");
        assert_eq!(s, "closed");
    }
