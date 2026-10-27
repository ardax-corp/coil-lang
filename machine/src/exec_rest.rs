impl<const S: usize> Machine<S> {
    /// Every opcode the giant match in `execute` leaves out. `ip` / `sp` go
    /// in and come back by value (#1882). Not `#[cold]`: that cost `fib`
    /// 1.8% and a closure-call loop 0.3% (callgrind).
    fn exec_rest(
        &mut self,
        opcode: &Byte,
        mut ip: usize,
        mut sp: usize,
        code: &[Byte],
        constants: &[u64],
        stack_cap: usize,
    ) -> dispatch::RestExit {
        let bc = opcode.bytecode();
        // Step budget charge, as `charge_step!` in `execute`.
        macro_rules! charge_step {
            ($back:expr) => {
                self.fuel = self.fuel.wrapping_sub(($back) as u64);
                if unlikely(self.fuel == 0) {
                    return dispatch::RestExit::done(self.step_budget_out(ip.saturating_sub(1)));
                }
            };
        }
        // A Coil int op: the exact result, or a panic.
        macro_rules! int_trap {
            ($trap:expr) => {{
                return dispatch::RestExit::done(self.runtime_panic($trap.message(), ip.saturating_sub(1)));
            }};
        }
        // A plain binary op: [`crate::fused::eval_bin`] with a constant op,
        // so the stack handlers and the fused ops share one table.
        macro_rules! bin {
            ($op:ident) => {{
                let top = self.stack.tell();
                promise!(top >= 2);
                let rhs = self.stack[top - 1];
                let lhs = self.stack[top - 2];
                match crate::fused::eval_bin(common::Instruction::$op as u8, lhs, rhs, &self.heap) {
                    Ok(r) => {
                        self.stack[top - 2] = r;
                        self.stack.seek(top - 1);
                    }
                    Err(t) => int_trap!(t),
                }
            }};
        }
        match bc {
                Instruction::POP => {
                    self.stack.pop();
                }
                Instruction::DUPLICATE => {
                    self.stack.duplicate();
                }
                Instruction::CONST => {
                    let op = opcode.operand_u32();
                    let raw = if unlikely(op & Byte::POOL_FLAG != 0) {
                        let pool_idx = (op & !Byte::POOL_FLAG) as usize;
                        promise!(pool_idx < constants.len());
                        unsafe { *constants.get_unchecked(pool_idx) }
                    } else {
                        op as i32 as i64 as u64
                    };
                    self.stack.push(Value::from(raw));
                }
                Instruction::CodePtr => {
                    // Absolute CodePtr entry; same stack form as CALL target.
                    let offset = opcode.operand_u32() as i64;
                    self.stack.push(Value::from(offset));
                }
                Instruction::OptionNicheToHeap => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode OptionNicheToHeap", ip.saturating_sub(1)));
                }
                Instruction::HeapOptionToNiche => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode HeapOptionToNiche", ip.saturating_sub(1)));
                }
                Instruction::PairJumpIfTag => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode PairJumpIfTag", ip.saturating_sub(1)));
                }
                Instruction::PairToHeap => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode PairToHeap", ip.saturating_sub(1)));
                }
                Instruction::HeapToPair => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode HeapToPair", ip.saturating_sub(1)));
                }
                Instruction::ReturnPair => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode ReturnPair", ip.saturating_sub(1)));
                }
                Instruction::INC => {
                    let (slot, prefix, is_float) = opcode.inc_dec_parts();
                    promise!(sp + slot < stack_cap);
                    let idx = sp + slot;
                    let old = self.stack[idx];
                    let new_val = if is_float {
                        Value::from(old.as_float() + 1.0)
                    } else {
                        match int_arith::add(old.as_int(), 1) {
                            Ok(v) => Value::from(v),
                            Err(t) => int_trap!(t),
                        }
                    };
                    self.stack[idx] = new_val;
                    self.stack.push(if prefix { new_val } else { old });
                }
                Instruction::DEC => {
                    let (slot, prefix, is_float) = opcode.inc_dec_parts();
                    promise!(sp + slot < stack_cap);
                    let idx = sp + slot;
                    let old = self.stack[idx];
                    let new_val = if is_float {
                        Value::from(old.as_float() - 1.0)
                    } else {
                        match int_arith::sub(old.as_int(), 1) {
                            Ok(v) => Value::from(v),
                            Err(t) => int_trap!(t),
                        }
                    };
                    self.stack[idx] = new_val;
                    self.stack.push(if prefix { new_val } else { old });
                }
                Instruction::NOT => unary!(self.stack, !, as_int),
                Instruction::LogNot => {
                    let val = self.stack.pop();
                    self.stack.push(Value::from(!(val.as_int() != 0)));
                }
                Instruction::NEG => {
                    let top = self.stack.tell();
                    promise!(top >= 1);
                    match int_arith::neg(self.stack[top - 1].as_int()) {
                        Ok(r) => self.stack[top - 1].replace(r as _),
                        Err(t) => int_trap!(t),
                    }
                }
                // IEEE negate: flip sign bit (preserves NaN payload).
                Instruction::NEGF => {
                    let sp = self.stack.tell();
                    promise!(sp >= 1);
                    let idx = sp - 1;
                    let bits = self.stack[idx].bits();
                    self.stack[idx].replace((bits ^ (1u64 << 63)) as _);
                }
                Instruction::AND => bin!(AND),
                Instruction::OR => bin!(OR),
                Instruction::ADD => bin!(ADD),
                Instruction::SUB => bin!(SUB),
                Instruction::MUL => bin!(MUL),
                Instruction::DIV => bin!(DIV),
                Instruction::MOD => bin!(MOD),
                Instruction::LE => bin!(LE),
                Instruction::LEQ => bin!(LEQ),
                Instruction::GT => bin!(GT),
                Instruction::GEQ => bin!(GEQ),
                Instruction::EQ => bin!(EQ),
                Instruction::NEQ => bin!(NEQ),
                Instruction::ADDF => bin!(ADDF),
                Instruction::SUBF => bin!(SUBF),
                Instruction::MULF => bin!(MULF),
                Instruction::DIVF => bin!(DIVF),
                Instruction::MODF => bin!(MODF),
                Instruction::SHL => bin!(SHL),
                Instruction::SHR => bin!(SHR),
                Instruction::XOR => bin!(XOR),
                Instruction::BITAND => bin!(BITAND),
                Instruction::BITOR => bin!(BITOR),
                Instruction::Pow => bin!(Pow),
                Instruction::PowF => bin!(PowF),
                Instruction::LEF => bin!(LEF),
                Instruction::LEQF => bin!(LEQF),
                Instruction::EQF => bin!(EQF),
                Instruction::NEQF => bin!(NEQF),
                Instruction::GTF => bin!(GTF),
                Instruction::GEQF => bin!(GEQF),
                Instruction::FORMAT => {
                    let params_count = opcode.operand_u32();
                    if params_count != 0 {
                        let mut params = ArrayVec::<Value, 8>::default();

                        for _ in 0..params_count as usize {
                            params.push(self.stack.pop());
                        }

                        let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                        let format_string = (unsafe { &*ptr }).as_ref().data.as_str();
                        // `a + b` on strings lowers to `FORMAT "%s%s"`. With a
                        // leading `%s`, format only the rest and append it to
                        // that first string, in place when it ends at its
                        // buffer's tail, so `s = s + x` loops stay linear.
                        let (head, format_rest) = match format_string.strip_prefix("%s") {
                            Some(rest) => (Some(*params.pop()), rest),
                            None => (None, format_string),
                        };

                        let mut message = String::default();

                        let mut chars = format_rest.chars().peekable();
                        while let Some(ch) = chars.next() {
                            if ch == '%' {
                                match chars.peek() {
                                    Some('%') => {
                                        chars.next();
                                        message.push('%');
                                    }
                                    Some('i') => {
                                        chars.next();
                                        message.push_str(&params.pop().as_int().to_string());
                                    }
                                    Some('f') => {
                                        chars.next();
                                        // message
                                        //     .push_str(&format!("{:.?}", params.pop().as_float()));
                                        let _ =
                                            write!(&mut message, "{:.?}", params.pop().as_float());
                                    }
                                    Some('b') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:0b}",
                                            params.pop().bits()
                                        );
                                    }
                                    Some('s') => {
                                        chars.next();
                                        let string_val = (unsafe {
                                            &*params.pop().as_ptr::<GcData<ObjString>>()
                                        })
                                        .as_ref()
                                        .data
                                        .as_str();
                                        // Allocated::<crate::String>::new(params.pop().as_ptr());
                                        message.push_str(string_val);
                                    }
                                    Some('x') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:0x}",
                                            params.pop().bits()
                                        );
                                    }
                                    Some('z') => {
                                        chars.next();
                                        message.push_str(if params.pop().bits() > 0 {
                                            "true"
                                        } else {
                                            "false"
                                        });
                                    }
                                    Some('u') => {
                                        chars.next();
                                        message.push_str(&params.pop().bits().to_string());
                                    }
                                    Some('p') => {
                                        chars.next();
                                        let _ = write!(
                                            &mut message,
                                            "{:08x}",
                                            params.pop().as_ptr::<bool>().addr()
                                        );
                                    }
                                    _ => {
                                        message.push('%');
                                    }
                                }
                            } else {
                                message.push(ch);
                            }
                        }

                        match head {
                            Some(head) => self.push_concat_str(head, &message, ip),
                            None => self.push_new_string(message, ip),
                        }
                    } else {
                        // No arguments: the only specifier left to apply is
                        // `%%`, so the literal is reused unless it has one.
                        let fmt = self.stack.pop();
                        let ptr = fmt.as_ptr::<GcData<ObjString>>();
                        let format_string = (unsafe { &*ptr }).as_ref().data.as_str();
                        if format_string.contains("%%") {
                            let message = format_string.replace("%%", "%");
                            self.push_new_string(message, ip);
                        } else {
                            self.stack.push(fmt);
                        }
                    }
                }
                Instruction::STRINGIFY => {
                    // Shared primitive conversion for Show thunks / `%v`.
                    // Accepts a boxed value (preferred), a heap string, or a
                    // raw immediate (treated as int).
                    let v = self.stack.pop();
                    let text = Self::stringify_value(&self.heap, v);
                    self.push_new_string(text, ip);
                }
                Instruction::PRINT => {
                    let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                    let s = unsafe { (*ptr).as_ref() };
                    if crate::io::capture_write(false, s.data.as_bytes()) {
                        // `io::capture_begin()` keeps it.
                    } else if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "{}", s);
                        let _ = out.flush();
                        self.mirror_stdout(s.data.as_str());
                    } else {
                        print!("{}", s);
                        let _ = io::stdout().flush();
                    }
                }
                Instruction::CastIntToFloat => {
                    let v = self.stack.pop().as_int() as f64;
                    self.stack.push(Value::from(v));
                }
                Instruction::CastFloatToInt => {
                    // Truncate toward zero (`3.9 as int` → `3`); not floor/round.
                    let v = self.stack.pop().as_float() as i64;
                    self.stack.push(Value::from(v));
                }
                Instruction::CastIntToByte => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from((v as u8) as i64));
                }
                Instruction::CastByteToInt => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from(v & 0xff));
                }
                Instruction::CastIntToBool => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from((v != 0) as i64));
                }
                Instruction::CastBoolToInt => {
                    let v = self.stack.pop().as_int();
                    self.stack.push(Value::from(if v != 0 { 1 } else { 0 }));
                }
                Instruction::INIT => {
                    let (_, mut r) = self.heap.alloc(ObjInstance::default(), Object::Instance);
                    let _ = r.as_mut();
                    // Root before GC, same rule as `push_new_string`.
                    self.stack.push(Value::from(r.as_ptr().addr() as u64));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::InitTyped => {
                    let (type_id, nfields) = unpack_init_typed(opcode.operand_u32());
                    let (_, mut r) = self.heap.alloc(
                        ObjInstance::with_type_id_and_fields(type_id, nfields as usize),
                        Object::Instance,
                    );
                    let _ = r.as_mut();
                    self.stack.push(Value::from(r.as_ptr().addr() as u64));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::BinSlotSlot => {
                    let (op, a, b) = opcode.bin_slot_slot_parts();
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    let va = self.stack[sp + a];
                    let vb = self.stack[sp + b];
                    match crate::fused::eval_bin(op, va, vb, &self.heap) {
                        Ok(result) => self.stack.push(result),
                        Err(t) => int_trap!(t),
                    }
                }
                Instruction::NATIVE => {
                    #[cfg(debug_assertions)]
                    eprintln!("FFI: deprecated NATIVE opcode — recompile from source");
                }
                Instruction::FfiLoad => {
                    // Inlined to split-borrow `heap`/`libraries` from `frames`.
                    let path_val = self.stack.pop();
                    let path = {
                        let addr = path_val.bits();
                        match Self::find_object_by_addr(&self.heap, addr) {
                            Some(crate::memory::Object::String(gc)) => gc.as_ref().data.to_string(),
                            _ => String::new(),
                        }
                    };
                    match crate::ffi::resolve_library(
                        &path,
                        self.base_dir.as_deref(),
                        &self.ffi_search_paths,
                        &self.dload_gate,
                    ) {
                        Ok(lib_arc) => {
                            self.libraries
                                .entry(path.clone())
                                .or_insert_with(|| lib_arc.clone());
                            let (object, _gc) = self.heap.alloc_library(lib_arc);
                            let addr = object.addr();
                            self.userland_libraries.insert(addr, object);
                            self.push_result_ok(Value::from(addr));
                        }
                        Err(e) => {
                            self.push_ffi_error(e);
                        }
                    }
                }
                Instruction::FfiInvoke => {
                    let raw = opcode.operand_u32();
                    let _arity = (raw & 0xFFFF) as usize;
                    let has_arg_tags = (raw & (1 << 16)) != 0;

                    // Stack (bottom → top): lib, fn_id, args_tuple [, tags_tuple].
                    let arg_types = if has_arg_tags {
                        let tags_val = self.stack.pop();
                        let tags_addr = tags_val.bits();
                        let tags: Vec<crate::memory::FfiType> =
                            match Self::find_object_by_addr(&self.heap, tags_addr) {
                                Some(crate::memory::Object::Tuple(gc)) => gc
                                    .as_ref()
                                    .elements()
                                    .iter()
                                    .map(|v| Self::ffi_type_from_value(v, &self.heap))
                                    .collect(),
                                _ => Vec::new(),
                            };
                        Some(tags)
                    } else {
                        None
                    };

                    let tuple_val = self.stack.pop();
                    let tuple_addr = tuple_val.bits();

                    let function_id_val = self.stack.pop();
                    let function_id = function_id_val.as_int() as usize;

                    let lib_val = self.stack.pop();
                    let lib_addr = lib_val.bits();

                    let args: Vec<Value> = match Self::find_object_by_addr(&self.heap, tuple_addr) {
                        Some(crate::memory::Object::Tuple(gc)) => gc.as_ref().elements().to_vec(),
                        _ => Vec::new(),
                    };

                    self.frames.get_mut().set(sp);
                    self.pending_ffi = Some(PendingFfiInvoke {
                        lib_addr,
                        function_id,
                        args,
                        arg_types,
                        resume_ip: ip,
                        resume_sp: sp,
                    });
                    return dispatch::RestExit::done(true);
                }
                Instruction::DeclareFFI => {
                    let raw = opcode.operand_u32();
                    let _arity = (raw & 0xFFFF) as usize;
                    let variadic = (raw & (1 << 16)) != 0;

                    // Stack (bottom → top): lib, name, args_tuple, ret_tag.
                    let ret_tag_val = self.stack.pop();
                    let ret_type = Self::ffi_type_from_value(&ret_tag_val, &self.heap);

                    // Pop args tuple, then name, then lib handle.
                    let args_tuple_val = self.stack.pop();
                    let args_tuple_addr = args_tuple_val.bits();

                    let arg_types: Vec<crate::memory::FfiType> =
                        match Self::find_object_by_addr(&self.heap, args_tuple_addr) {
                            Some(crate::memory::Object::Tuple(gc)) => gc
                                .as_ref()
                                .elements()
                                .iter()
                                .map(|v| Self::ffi_type_from_value(v, &self.heap))
                                .collect(),
                            _ => Vec::new(),
                        };
                    let name_val = self.stack.pop();
                    let name = Self::object_string_value(&self.heap, &name_val);
                    let lib_val = self.stack.pop();
                    let lib_addr = lib_val.bits();
                    let lib_obj = self.userland_libraries.get(&lib_addr).copied();
                    match lib_obj {
                        Some(mut owned) => {
                            let ffi_sig = crate::ffi::FfiSignature {
                                name,
                                args: arg_types,
                                ret: ret_type,
                                variadic,
                            };
                            match Self::register_signature_on_object(
                                &mut owned,
                                ffi_sig,
                                &self.struct_layouts,
                            ) {
                                Ok(id) => {
                                    self.userland_libraries.insert(lib_addr, owned);
                                    self.push_result_ok(Value::from(id as i64));
                                }
                                Err(e) => {
                                    self.push_ffi_error(e);
                                }
                            }
                        }
                        None => {
                            self.push_result_err(
                                crate::ffi::FfiErrorKindTag::InvalidHandle,
                                format!("FFI declare: library at 0x{:x} is not loaded", lib_addr),
                            );
                        }
                    }
                }
                Instruction::HostInvoke => {
                    let arity = (opcode.operand_u32() & 0xFFFF) as usize;
                    let tell = self.stack.tell();
                    let consume = arity + 1;
                    promise!(tell >= consume);
                    let fn_id = self.stack.top_window(consume)[0].as_int() as usize;
                    // Packed LA (and other host natives) allocate via
                    // `heap.alloc` inside the closure; count those so GC
                    // pressure still fires when HostInvoke is the only
                    // allocator on a hot path.
                    let live_before = self.heap.live_object_count();
                    let host_op = match self.natives.get_by_id(fn_id) {
                        Some(native) => native.host_op(),
                        None => {
                    return dispatch::RestExit::done(self.runtime_panic(
                                &format!("HostInvoke: unknown native id {fn_id}"),
                                ip.saturating_sub(1),
                            ));

                        }
                    };
                    match host_op {
                        crate::HostOp::Collect => {
                            self.stack.seek(tell - consume);
                            let before = self.heap.size();
                            if unlikely(!self.stack_maps.is_empty()) {
                                self.gc_ip = ip;
                            }
                            self.gc_top_ip = Some(ip);
                            self.gc_collect();
                            self.gc_top_ip = None;
                            let freed = before.saturating_sub(self.heap.size());
                            self.stack.push(Value::from(freed as i64));
                        }
                        crate::HostOp::RegisterFinalizer => {
                            let args = self.stack.top_window(consume);
                            let type_id = args.get(1).map(|v| v.as_int() as u32).unwrap_or(0);
                            let pc = args.get(2).map(|v| v.as_int() as u32).unwrap_or(0);
                            self.register_finalizer(type_id, pc);
                            self.stack.seek(tell - consume);
                            self.stack.push(Value::from(0i64));
                        }
                        crate::HostOp::Free => {
                            let args = self.stack.top_window(consume);
                            let (v, deep) = (args[1], args[2].as_int() != 0);
                            // The window's copy of the reference is dead.
                            self.stack[tell - 2] = Value::default();
                            self.stack.seek(tell - consume);
                            self.free_early(v, deep);
                            self.stack.push(Value::from(0i64));
                        }
                        crate::HostOp::Unwind => {
                            self.stack.seek(tell - consume);
                            self.unwind_resume();
                            return dispatch::RestExit::done(false);
                        }
                        crate::HostOp::Task => {
                            let mut args = [Value::default(); 2];
                            let window = &self.stack.top_window(consume)[1..];
                            let n = window.len().min(2);
                            args[..n].copy_from_slice(&window[..n]);
                            self.stack.seek(tell - consume);
                            match self.task_host_op(fn_id, &args[..n], &mut ip, &mut sp) {
                                TaskFlow::Value(v) => self.stack.push(v),
                                TaskFlow::Switched => {
                                    return dispatch::RestExit::resume(ip, sp);
                                }
                                TaskFlow::Panic(msg) => {
                                    return dispatch::RestExit::done(
                                        self.runtime_panic(&msg, ip.saturating_sub(1)),
                                    );
                                }
                            }
                        }
                        crate::HostOp::Ordinary => {
                            if unlikely(
                                fn_id == common::CLOCK_SLEEP_MS_ID as usize
                                    && self.tasks_can_switch(),
                            ) {
                                // Inside a scope, `clock::sleep_ms` suspends the task.
                                let ms = self.stack.top_window(consume)[1].as_int();
                                self.stack.seek(tell - consume);
                                if let TaskFlow::Value(v) = self.task_sleep(ms, &mut ip, &mut sp) {
                                    self.stack.push(v);
                                }
                                return dispatch::RestExit::resume(ip, sp);
                            }
                            let native = self.natives.get_by_id(fn_id).expect("id checked above");
                            let args = &self.stack.top_window(consume)[1..];
                            let layout = crate::host_enum::HostEnumLayout::from_operand(
                                opcode.operand_u32(),
                            );
                            // Under a scheduler, natives that would block the thread
                            // (`stream_park`, `thread` recv / join / locks) suspend
                            // the task instead.
                            let park_tasks = self.tasks_can_switch();
                            if unlikely(park_tasks) {
                                crate::task::set_task_waiter(self.task_waiter());
                            }
                            let invoked = crate::host_enum::with_host_enum_layout(layout, || {
                                native.invoke(&mut self.heap, args)
                            });
                            if unlikely(park_tasks) {
                                crate::task::set_task_waiter(None);
                            }
                            match invoked {
                                Ok(Some(v)) => {
                                    self.stack.seek(tell - consume);
                                    self.stack.push(v);
                                }
                                Ok(None) if park_tasks && crate::task::take_thread_parked() => {
                                    // The arguments stay: the HostInvoke runs again
                                    // when another thread posts the wake.
                                    self.task_suspend_thread(&mut ip, &mut sp);
                                    return dispatch::RestExit::resume(ip, sp);
                                }
                                Ok(None) if park_tasks && crate::task::io_retry_parked() => {
                                    // A connect in progress: the arguments stay and
                                    // the HostInvoke runs again on readiness.
                                    let req = crate::task::take_io_retry_park().expect("checked");
                                    self.task_suspend_io_retry(req, &mut ip, &mut sp);
                                    return dispatch::RestExit::resume(ip, sp);
                                }
                                Ok(None) => {
                                    self.stack.seek(tell - consume);
                                    if let Some(req) = crate::io::take_pending_io_park() {
                                        if self.tasks_can_switch() {
                                            // Suspend this task (with any generator it
                                            // is resuming); another task runs meanwhile.
                                            self.task_suspend_io(req, layout, &mut ip, &mut sp);
                                            return dispatch::RestExit::resume(ip, sp);
                                        } else {
                                            // No other task: park the whole VM, even
                                            // inside a generator (it does not yield).
                                            self.frames.get_mut().set(sp);
                                            self.pending_io = Some(PendingIoWait {
                                                request: req,
                                                resume_ip: ip,
                                                resume_sp: sp,
                                                layout,
                                            });
                    return dispatch::RestExit::done(true);
                                        }
                                    } else {
                                        // Void natives must still leave a defined TOS.
                                        self.stack.push(Value::default());
                                    }
                                }
                                Err(crate::FfiError::IntTrap(t)) => int_trap!(t),
                                Err(crate::FfiError::Panic(msg)) => {
                                    return dispatch::RestExit::done(self.runtime_panic(&msg, ip.saturating_sub(1)));
                                }
                                Err(e) => {
                                    let name = native.name();
                    return dispatch::RestExit::done(self.runtime_panic(
                                        &format!("HostInvoke failed for `{name}`: {e}"),
                                        ip.saturating_sub(1),
                                    ));

                                }
                            }
                        }
                    }
                    let allocated = self.heap.live_object_count().saturating_sub(live_before);
                    if allocated > 0 {
                        self.maybe_gc_after_alloc(ip);
                    }
                }
                Instruction::HostInvokeNiche => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode HostInvokeNiche", ip.saturating_sub(1)));
                }
                Instruction::FloatChainStore => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode FloatChainStore", ip.saturating_sub(1)));
                }
                // Fused `BinSlotSlot <arith>; CONST pool; CmpJmpf/CmpJmpt`, no stack traffic.
                Instruction::BinSlotSlotConstJmpf => {
                    return dispatch::RestExit::done(self.runtime_panic(
                        "retired opcode BinSlotSlotConstJmpf",
                        ip.saturating_sub(1),
                    ));

                }
                Instruction::BinSlotSlotConstJmpt => {
                    return dispatch::RestExit::done(self.runtime_panic(
                        "retired opcode BinSlotSlotConstJmpt",
                        ip.saturating_sub(1),
                    ));
                }
                Instruction::HALT => {
                    if let Some(out) = self.output.as_mut() {
                        let _ = out.flush();
                    } else {
                        let _ = io::stdout().flush();
                    }
                    return dispatch::RestExit::done(false);
                }
                Instruction::Panic => {
                    let panic_ip = ip.saturating_sub(1);
                    // Record where the frame stopped: backtraces and the
                    // debugger read each frame's saved ip.
                    if !self.frames.is_empty() {
                        self.frames.get_mut().seek(panic_ip);
                    }
                    let ptr = self.stack.pop().as_ptr::<GcData<ObjString>>();
                    let s = unsafe { (*ptr).as_ref() };
                    self.arm_unwind(&s.to_string());
                    if self.task_panic_is_caught() {
                        // A child task's panic fails its scope, which reports it.
                        self.task_panic_message = Some(s.to_string());
                        self.panicked = true;
                        return dispatch::RestExit::done(false);
                    }
                    let loc_suffix = self
                        .format_panic_location(panic_ip)
                        .map(|loc| format!(" at {loc}"))
                        .unwrap_or_default();
                    let backtrace = self
                        .explicit_panic_backtrace(panic_ip)
                        .filter(|bt| !bt.is_empty())
                        .map(|bt| format!("\n{bt}"))
                        .unwrap_or_default();
                    if let Some(out) = self.output.as_mut() {
                        let text = format!("panic: {}{}{}", s, loc_suffix, backtrace);
                        let _ = write!(out, "{text}");
                        let _ = out.flush();
                        self.mirror_stderr(&text);
                    } else {
                        eprintln!("panic: {}{}{}", s, loc_suffix, backtrace);
                        let _ = io::stderr().flush();
                    }
                    self.panicked = true;
                    return dispatch::RestExit::done(false);
                }
                Instruction::STRING => {
                    let idx = opcode.operand_u32() as usize;
                    promise!(idx < self.program_strings.len());
                    self.push_program_string(idx, ip);
                }
                Instruction::NOOP => {
                    return dispatch::RestExit::resume(ip, sp);
                }
                Instruction::MakeEnum | Instruction::MakeEnumK => {
                    self.push_make_enum(opcode, ip);
                }
                Instruction::MakeTuple
                | Instruction::MakeArray
                | Instruction::MakeTupleK
                | Instruction::MakeArrayK => {
                    let arity = opcode.make_arity() as usize;
                    let sp = self.stack.tell();
                    promise!(sp >= arity);
                    let n = arity;
                    let base = sp - n;
                    if n <= 3 {
                        note_make_fast();
                    }
                    // Declaration order; keep args on stack through alloc for rooting.
                    let array = matches!(
                        opcode.bytecode(),
                        Instruction::MakeArray | Instruction::MakeArrayK
                    );
                    let addr = if !array && n == 0 {
                        self.heap.immortal_empty_tuple().addr()
                    } else if !array {
                        let tuple = ObjTuple::from_slice(&self.stack[base..base + n])
                            .with_kinds(opcode.make_kinds());
                        let (object, _) = self.heap.alloc(tuple, Object::Tuple);
                        object.addr()
                    } else {
                        let values = Self::stack_copy_decl(&self.stack, base, n);
                        let mut arr = ObjArray::from_values(values, &self.heap);
                        if matches!(opcode.bytecode(), Instruction::MakeArrayK) {
                            arr.set_elem_kind((opcode.operand_u32() >> 16) as u8 & 0x3);
                            // Not allocated yet: the allocation counts the
                            // packed size.
                            let _ = arr.take_resize();
                        }
                        let (object, _) = self.heap.alloc(arr, Object::Array);
                        object.addr()
                    };
                    self.stack.seek(base);
                    self.stack.push(Value::from(addr));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::ArrayPin => {
                    let slot = opcode.operand_u32();
                    let arr_val = self.stack.pop();
                    let addr = arr_val.bits();
                    if let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, addr) {
                        self.pin_current_array(slot, Object::Array(gc));
                    }
                }
                Instruction::Index | Instruction::IndexUnchecked => {
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.bits();
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::IndexUnchecked);
                    // Arrays dominate Index traffic (Vec); check Array before Tuple.
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => {
                            gc.as_ref().read_indexed(index, unchecked)
                        }
                        Some(crate::memory::Object::Tuple(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                    return dispatch::RestExit::done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                    };
                    self.stack.push(result);
                }
                Instruction::IndexPin | Instruction::IndexPinUnchecked => {
                    let slot = opcode.operand_u32();
                    let index = self.stack.pop().as_int();
                    let unchecked = matches!(*bc, Instruction::IndexPinUnchecked);
                    let result = match self.pinned_object(slot) {
                        Some(Object::Array(gc)) => gc.as_ref().read_indexed(index, unchecked),
                        Some(Object::Tuple(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                    return dispatch::RestExit::done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                    };
                    self.stack.push(result);
                }
                Instruction::MakeDict => {
                    let arity = (opcode.operand_u32() & 0xFFFF) as usize;
                    let mut pairs: Vec<(crate::memory::RefString, Value)> =
                        Vec::with_capacity(arity);
                    for _ in 0..arity {
                        let name_val = self.stack.pop();
                        let value = self.stack.pop();
                        pairs.push((Self::intern_key(&mut self.heap, name_val), value));
                    }
                    pairs.reverse();
                    // Allocate the instance and populate.
                    let (object, mut gc) =
                        self.heap.alloc(ObjInstance::default(), Object::Instance);
                    {
                        let instance: &mut ObjInstance = gc.as_mut();
                        for (key, value) in pairs {
                            let member = if let Some(obj) =
                                Self::find_object_by_addr(&self.heap, value.bits())
                            {
                                crate::memory::Member::Object(obj)
                            } else {
                                crate::memory::Member::Value(value)
                            };
                            instance.set(key, member);
                        }
                    }
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::GetField => {
                    let name_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let key = Self::intern_key(&mut self.heap, name_val);
                    let target_addr = target_val.bits();
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Instance(gc)) => match gc.as_ref().get(key) {
                            Some(v) => v,
                            None => {
                    return dispatch::RestExit::done(self.runtime_panic("no such field", ip.saturating_sub(1)));
                            }
                        },
                        _ => {
                    return dispatch::RestExit::done(self.runtime_panic("no such field", ip.saturating_sub(1)));
                        }
                    };
                    self.stack.push(result);
                }
                Instruction::SetField => {
                    if let Some(slot) = set_field_slot_index(opcode.operand_u32()) {
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let target_addr = target_val.bits();
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let idx = slot as usize;
                            promise!(gc.as_ref().slot_len().is_some_and(|n| idx < n));
                            gc.as_mut().set_slot(idx, value);
                        } else {
                    return dispatch::RestExit::done(self.runtime_panic("SetField on non-instance", ip.saturating_sub(1)));
                        }
                        self.stack.push(value);
                    } else {
                        let name_val = self.stack.pop();
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let key = Self::intern_key(&mut self.heap, name_val);
                        let target_addr = target_val.bits();
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let member = Self::value_as_member(&self.heap, value);
                            gc.as_mut().set(key, member);
                        } else {
                    return dispatch::RestExit::done(self.runtime_panic("SetField on non-instance", ip.saturating_sub(1)));
                        }
                        self.stack.push(value);
                    }
                }
                Instruction::StoreIndex | Instruction::StoreIndexUnchecked => {
                    let value = self.stack.pop();
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.bits();
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::StoreIndexUnchecked);
                    if let Some(crate::memory::Object::Array(mut gc)) =
                        Self::find_object_by_addr(&self.heap, target_addr)
                    {
                        if !gc.as_mut().store_indexed(index, value, unchecked) {
                    return dispatch::RestExit::done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                        }
                    } else {
                    return dispatch::RestExit::done(self.runtime_panic("StoreIndex on non-array", ip.saturating_sub(1)));
                    }
                    self.stack.push(value);
                }
                Instruction::StoreIndexPin | Instruction::StoreIndexPinUnchecked => {
                    let slot = opcode.operand_u32();
                    let value = self.stack.pop();
                    let index = self.stack.pop().as_int();
                    let unchecked = matches!(*bc, Instruction::StoreIndexPinUnchecked);
                    if let Some(Object::Array(mut gc)) = self.pinned_object(slot) {
                        if !gc.as_mut().store_indexed(index, value, unchecked) {
                    return dispatch::RestExit::done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                        }
                    } else {
                    return dispatch::RestExit::done(self.runtime_panic("StoreIndexPin on non-array", ip.saturating_sub(1)));
                    }
                    self.stack.push(value);
                }
                Instruction::VLoad => {
                    let (ty, vdest, arr, idx) = opcode.dense_abc_parts();
                    promise!(vdest < common::simd::NREGS);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + idx < stack_cap);
                    let index = self.stack[sp + idx].as_int();
                    let addr = self.stack[sp + arr].bits();
                    if !self.vload(vdest, addr, index, ty) {
                    return dispatch::RestExit::done(self.runtime_panic("VLoad out of bounds", ip.saturating_sub(1)));
                    }
                }
                Instruction::VStore => {
                    let (ty, vsrc, arr, idx) = opcode.dense_abc_parts();
                    promise!(vsrc < common::simd::NREGS);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + idx < stack_cap);
                    let index = self.stack[sp + idx].as_int();
                    let addr = self.stack[sp + arr].bits();
                    if !self.vstore(vsrc, addr, index, ty) {
                    return dispatch::RestExit::done(self.runtime_panic("VStore out of bounds", ip.saturating_sub(1)));
                    }
                }
                Instruction::VBin if common::simd::is_byte_kind(opcode.dense_abc_parts().0) => {
                    let (kind, dest, a, b) = opcode.dense_abc_parts();
                    promise!(dest < common::simd::NREGS);
                    promise!(b < common::simd::NREGS);
                    // `BS_CMP` reads a frame slot in `a`; the others a register.
                    let scalar = if kind >= common::simd::BS_CMP {
                        promise!(sp + a < stack_cap);
                        self.stack[sp + a]
                    } else {
                        promise!(a < common::simd::NREGS);
                        Value::from(0i64)
                    };
                    crate::simd::vbin_bytes_in(&mut self.vregs, kind, dest, a, b, scalar);
                }
                Instruction::VScan => {
                    let (n, dest, from, bound) = opcode.dense_abc_parts();
                    let inclusive = n & common::simd::SCAN_INCLUSIVE != 0;
                    let backward = n & common::simd::SCAN_BACKWARD != 0;
            let n = usize::from(n & common::simd::SCAN_LEN);
                    promise!(sp + dest < stack_cap);
                    promise!(sp + from < stack_cap);
                    promise!(sp + bound < stack_cap);
                    let (from, bound) = (self.stack[sp + from].as_int(), self.stack[sp + bound].as_int());
                    let found = match code.get(ip..ip + n) {
                        Some(body) => vscan(&self.heap, &self.stack, sp, &mut self.vregs, body, from, bound, inclusive, backward),
                        None => Err("VScan body past the code"),
                    };
                    match found {
                        Ok(p) => self.stack[sp + dest] = Value::from(p),
                        Err(msg) => return dispatch::RestExit::done(self.runtime_panic(msg, ip.saturating_sub(1))),
                    }
                    ip += n;
                }
                Instruction::VMask => {
                    let (dest, src) = opcode.dense_move_parts();
                    promise!(src < common::simd::NREGS);
                    promise!(sp + dest < stack_cap);
                    self.stack[sp + dest] = Value::from(crate::simd::movemask(&self.vregs[src]));
                }
                Instruction::VBin => {
                    let (kind, dest, a, b) = opcode.dense_abc_parts();
                    promise!(dest < common::simd::NREGS);
                    let splat = matches!(kind, common::simd::SPLAT_I64 | common::simd::SPLAT_F64);
                    let scalar = if splat {
                        promise!(sp + a < stack_cap);
                        self.stack[sp + a]
                    } else {
                        Value::from(0i64)
                    };
                    let z = [0u64; common::simd::LANES];
                    let lhs = if splat
                        || matches!(kind, common::simd::IOTA_I64 | common::simd::IOTA_F64)
                    {
                        &z
                    } else {
                        promise!(a < common::simd::NREGS);
                        &self.vregs[a]
                    };
                    let rhs = if splat
                        || matches!(
                            kind,
                            common::simd::IOTA_I64
                                | common::simd::IOTA_F64
                                | common::simd::INEG
                                | common::simd::FNEG
                        ) {
                        &z
                    } else {
                        promise!(b < common::simd::NREGS);
                        &self.vregs[b]
                    };
                    match crate::simd::eval_vbin(kind, lhs, rhs, scalar) {
                        Ok(out) => self.vregs[dest] = out,
                        Err(t) => int_trap!(t),
                    }
                }
                Instruction::VMove => {
                    let (dest, src) = opcode.dense_move_parts();
                    promise!(dest < common::simd::NREGS);
                    promise!(src < common::simd::NREGS);
                    self.vregs[dest] = self.vregs[src];
                }
                Instruction::VReduce => {
                    let (ty, dest, vsrc, fold) = opcode.dense_abc_parts();
                    promise!(vsrc < common::simd::NREGS);
                    promise!(sp + dest < stack_cap);
                    let acc = self.stack[sp + dest];
                    match crate::simd::eval_vreduce(ty, acc, &self.vregs[vsrc], fold as u8) {
                        Ok(v) => self.stack[sp + dest] = v,
                        Err(t) => int_trap!(t),
                    }
                }
                Instruction::VFma => {
                    let (ty, dest, a, b) = opcode.dense_abc_parts();
                    promise!(dest < common::simd::NREGS);
                    promise!(a < common::simd::NREGS);
                    promise!(b < common::simd::NREGS);
                    match crate::simd::eval_vfma(ty, &self.vregs[a], &self.vregs[b], &self.vregs[dest]) {
                        Ok(out) => self.vregs[dest] = out,
                        Err(t) => int_trap!(t),
                    }
                }
                Instruction::DenseArrayPush => {
                    // A push the storage has room for runs in the dense
                    // streak (`dispatch::dense_array_push`); this one grows.
                    let (_, dest, arr, val) = opcode.dense_abc_parts();
                    promise!(sp + dest < stack_cap);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + val < stack_cap);
                    let target_val = self.stack[sp + arr];
                    let value = self.stack[sp + val];
                    if !self.array_push_value(target_val, value) {
                    return dispatch::RestExit::done(self.runtime_panic("ArrayPush on non-array", ip.saturating_sub(1)));
                    }
                    self.stack[sp + dest] = target_val;
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::DenseMakeObject => {
                    let (dest, nfields, type_id) =
                        common::dense::unpack_make_object(opcode.operand_u32());
                    let dest = dest as usize;
                    promise!(sp + dest < stack_cap);
                    let (_, mut r) = self.heap.alloc(
                        ObjInstance::with_type_id_and_fields(type_id, nfields as usize),
                        Object::Instance,
                    );
                    let _ = r.as_mut();
                    self.stack[sp + dest] = Value::from(r.as_ptr().addr() as u64);
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::ArrayPush => {
                    // Stack discipline matches `StoreIndex`: codegen emits
                    // `array` then `value`, so dispatch pops value first,
                    // mutates the heap array in place, and returns the array
                    // address for chaining (`push(push(a, 1), 2)`).
                    let value = self.stack.pop();
                    let target_val = self.stack.pop();
                    if !self.array_push_value(target_val, value) {
                    return dispatch::RestExit::done(self.runtime_panic("ArrayPush on non-array", ip.saturating_sub(1)));
                    }
                    self.stack.push(target_val);
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::ArrayLen => {
                    let target_val = self.stack.pop();
                    let target_addr = target_val.bits();
                    let len = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => gc.as_ref().len(),
                        Some(crate::memory::Object::Tuple(gc)) => gc.as_ref().elements().len(),
                        Some(crate::memory::Object::String(gc)) => gc.as_ref().data.len(),
                        Some(crate::memory::Object::Instance(gc)) => gc
                            .as_ref()
                            .slot_len()
                            .unwrap_or_else(|| gc.as_ref().iter_fields().count()),
                        _ => 0,
                    };
                    self.stack.push(Value::from(len as i64));
                }
                Instruction::DictEntries => {
                    // Pop dict; push ObjArray of ObjTuple(2) (key, value).
                    let dict_val = self.stack.pop();
                    let dict_addr = dict_val.bits();
                    let mut pair_addrs: Vec<Value> = Vec::new();
                    if let Some(crate::memory::Object::Instance(gc)) =
                        Self::find_object_by_addr(&self.heap, dict_addr)
                    {
                        let entries: Vec<(crate::memory::RefString, Member)> =
                            gc.as_ref().iter_fields().collect();
                        for (key, member) in entries {
                            let key_val = Value::from(key.as_ptr() as u64);
                            let val = match member {
                                Member::Value(v) => v,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                            let (tuple_obj, _) = self.heap.alloc(
                                ObjTuple::new(vec![key_val, val]),
                                Object::Tuple,
                            );
                            pair_addrs.push(Value::from(tuple_obj.addr()));
                        }
                    }
                    let (array_obj, _) = self.heap.alloc(
                        ObjArray::new(pair_addrs),
                        Object::Array,
                    );
                    self.stack.push(Value::from(array_obj.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::JumpIfMatch => {
                    // Tag in operands[31:16]; pool index in operands[15:0]
                    // (`constants[idx]` holds the absolute jump target).
                    let operands = opcode.operand_u32();
                    let expected_tag = operands >> 16;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.peek().bits();

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        if enum_ref.tag == expected_tag {
                            let pool_idx = (operands & 0xFFFF) as usize;
                            promise!(pool_idx < constants.len());
                            let target_offset = opcode.jump_if_match_target(constants);
                            let _ = self.stack.pop();
                            let payload: &[Value] = &enum_ref.payload;
                            // The frame reserve counts the widest payload the program builds.
                            let wide = payload.len() > self.match_payload_bound;
                            if unlikely(wide) && !self.reserve_wide_payload(payload.len()) {
                                return dispatch::RestExit::done(
                                    self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                                );
                            }
                            for member in payload {
                                let value = *member;
                                self.stack.push(value);
                            }
                            if unlikely(wide) {
                                self.rearm_call_window();
                            }
                            charge_step!(target_offset < ip);
                            set_jump_target(&mut ip, target_offset, code);
                        } else if ip < code.len()
                            && *unsafe { code.get_unchecked(ip) }.bytecode() as u8 == Instruction::Unpack as u8
                            && unsafe { code.get_unchecked(ip) }.operand_u32() as usize == enum_ref.payload.len()
                            && !self.exec_hooks_attached()
                        {
                            // A two-arm match: the other arm's `Unpack` of the
                            // same scrutinee is next. Run it here on the enum
                            // this op already resolved, as `Unpack` would. A
                            // debugger / coverage still sees the `Unpack`.
                            let _ = self.stack.pop();
                            for member in enum_ref.payload.iter() {
                                self.stack.push(*member);
                            }
                            ip += 1;
                        }
                    }
                }
                Instruction::Unpack => {
                    // Pops enum scrutinee; pushes payload in declaration order
                    // (stack/locals overlap, see STORE).
                    let arity = opcode.operand_u32() as usize;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.pop().bits();

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        promise!(arity == enum_ref.payload.len());
                        for i in 0..arity {
                            let member = unsafe { enum_ref.payload.get_unchecked(i) };
                            let value = *member;
                            self.stack.push(value);
                        }
                    }
                }
                Instruction::LoadField => {
                    let field_index = (opcode.operand_u32() & 0xFFFF) as usize;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.pop().bits();

                    match Self::find_object_by_addr(&self.heap, scrutinee_addr) {
                        Some(Object::Enum(enum_ref)) => {
                            let enum_ref = enum_ref.as_ref();
                            promise!(field_index < enum_ref.payload.len());
                            let member = unsafe { enum_ref.payload.get_unchecked(field_index) };
                            let value = *member;
                            self.stack.push(value);
                        }
                        Some(Object::Instance(gc)) => {
                            if let Some(n) = gc.as_ref().slot_len() {
                                promise!(field_index < n);
                                self.stack
                                    .push(gc.as_ref().slot(field_index).unwrap_or_default());
                            } else {
                                self.stack.push(Value::default());
                            }
                        }
                        _ => {
                            self.stack.push(Value::default());
                        }
                    }
                }
                Instruction::UnpackAt => {
                    // Unpack enum at `sp + slot_offset` in place (nested record patterns).
                    // Scratch-area codegen may unpack past the current cursor; extend
                    // `tell` so subsequent LOAD/StorePop see the written slots.
                    let operands = opcode.operand_u32();
                    let slot_offset = (operands & 0xFFFF) as usize;
                    let arity = (operands >> 16) as usize;

                    let slot = sp + slot_offset;
                    promise!(slot < self.stack.tell());
                    let scrutinee_addr = self.stack[slot].bits();

                    let obj_enum = Self::find_enum_exact(&self.heap, scrutinee_addr);

                    if let Some(enum_ref) = obj_enum {
                        let enum_ref = enum_ref.as_ref();
                        promise!(arity == enum_ref.payload.len());
                        for i in 0..arity {
                            let member = unsafe { enum_ref.payload.get_unchecked(i) };
                            let value = *member;
                            self.stack[slot + i] = value;
                        }
                        let end = slot + arity;
                        if self.stack.tell() < end {
                            self.stack.seek(end);
                        }
                    }
                }
                // Deprecated STORE discriminant alias (same handler).
                // Compiler never emits StorePop; kept for archived bytecode.
                Instruction::MakeCoro => {
                    let (arity, target) = opcode.call_parts();
                    promise!(self.stack.tell() >= arity);
                    let mut values: Vec<Value> = Vec::with_capacity(arity);
                    for _ in 0..arity {
                        values.push(self.stack.pop());
                    }
                    values.reverse();

                    let live_mask = Self::saved_stack_live_mask(&self.heap, &values);
                    let obj_coro = ObjCoroutine {
                        state: CoroState::Suspended,
                        resume_ip: target,
                        saved_stack: values,
                        saved_live_mask: live_mask,
                        saved_frames: vec![(target, 0)],
                        pending_send: Value::from(0_i64),
                        yield_from: None,
                        delegator: None,
                        yield_from_resume_ip: 0,
                    };
                    let (object, _) = self.heap.alloc(obj_coro, Object::Coroutine);

                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::ResumeCoro => {
                    promise!(self.stack.tell() > 0);
                    let has_send = opcode.operand_u32() & 1 != 0;
                    let handle = self.stack.pop();
                    let send_val = if has_send {
                        promise!(self.stack.tell() > 0);
                        self.stack.pop()
                    } else {
                        Value::from(0_i64)
                    };
                    let addr = handle.bits();
                    if let Some(Object::Coroutine(gc)) = Self::find_object_by_addr(&self.heap, addr)
                    {
                        if gc.as_ref().state == CoroState::Done {
                    return dispatch::RestExit::done(self.runtime_panic("resumed after completion", ip.saturating_sub(1)));
                        }
                        // A delegating coroutine resumes its innermost
                        // `yield from` delegate.
                        let mut target = gc;
                        while let Some(sub) = target.as_ref().yield_from {
                            Self::with_coroutine_mut(target, |c| {
                                c.pending_send = send_val;
                            });
                            target = sub;
                        }
                        if !self.resume_coroutine(&mut ip, &mut sp, target, send_val, code, true) {
                            return dispatch::RestExit::done(
                                self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                            );
                        }
                    } else {
                    return dispatch::RestExit::done(self.runtime_panic(
                            "resumed invalid coroutine handle",
                            ip.saturating_sub(1),
                        ));

                    }
                }
                Instruction::YieldCoro => {
                    promise!(self.stack.tell() > 0);
                    let yield_val = self.stack.pop();
                    self.yield_coroutine(&mut ip, &mut sp, yield_val);
                }
                Instruction::YieldFromCoro => {
                    promise!(self.stack.tell() > 0);
                    let handle = self.stack.pop();
                    let addr = handle.bits();
                    if let Some(Object::Coroutine(sub)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        if !self.start_yield_from(&mut ip, &mut sp, sub, code) {
                            return dispatch::RestExit::done(
                                self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                            );
                        }
                    } else {
                    return dispatch::RestExit::done(self.runtime_panic(
                            "yield from invalid coroutine handle",
                            ip.saturating_sub(1),
                        ));

                    }
                }
                Instruction::DoneCoro => {
                    promise!(self.stack.tell() > 0);
                    let handle = self.stack.pop();
                    let addr = handle.bits();
                    let is_done = matches!(
                        Self::find_object_by_addr(&self.heap, addr),
                        Some(Object::Coroutine(gc)) if gc.as_ref().state == CoroState::Done
                    );
                    self.stack.push(Value::from(is_done));
                }
                Instruction::CallIndirect => {
                    // Stack: [value_args..., app_dicts..., target]
                    // operands[15:0] = value_arity; [31:16] = app_dict_arity
                    let packed = opcode.operand_u32();
                    let value_arity = (packed & 0xFFFF) as usize;
                    let app_dict_arity = ((packed >> 16) & 0xFFFF) as usize;
                    charge_step!(true);
                    promise!(self.stack.tell() > value_arity + app_dict_arity);
                    let raw = self.stack.pop();
                    let (next, flow) = self.call_indirect(raw, value_arity, app_dict_arity, ip, &mut sp, code);
                    return match flow {
                        dispatch::RestFlow::Continue => dispatch::RestExit::resume(next, sp),
                        dispatch::RestFlow::Done(paused) => dispatch::RestExit::done(paused),
                    };
                }
                Instruction::MakeFn => {
                    // Stack (bottom → TOS):
                    //   [captures..., filled_param_values..., filled_mask, entry]
                    // Operand packing:
                    //   [7:0]=n_captures [15:8]=n_filled [23:16]=arity [24]=is_rest
                    let op = opcode.operand_u32();
                    let n_captures = (op & 0xFF) as usize;
                    let n_filled = ((op >> 8) & 0xFF) as usize;
                    let arity = (op >> 16) & 0xFF;
                    let is_rest = (op & (1 << 24)) != 0;

                    let entry = self.stack.pop().as_int() as u32;
                    let filled_mask = self.stack.pop().as_int() as u64;

                    let mut filled_vals = Vec::with_capacity(n_filled);
                    for _ in 0..n_filled {
                        filled_vals.push(self.stack.pop());
                    }
                    filled_vals.reverse();

                    let mut captures = Vec::with_capacity(n_captures);
                    for _ in 0..n_captures {
                        captures.push(self.stack.pop());
                    }
                    captures.reverse();

                    let pfn = ObjFn {
                        entry,
                        arity,
                        is_rest,
                        filled_mask,
                        captured_args: filled_vals,
                        captures,
                        dicts: None,
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::Fn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::LoadStatic => {
                    let slot = opcode.operand_u32() as usize;
                    debug_assert!(slot < self.statics.len());
                    // Checked in release too: an out-of-range slot must not touch
                    // memory past the statics buffer (#1744).
                    let val = self.statics.get(slot).copied().unwrap_or_default();
                    self.stack.push(val);
                }
                Instruction::StoreStatic => {
                    let slot = opcode.operand_u32() as usize;
                    debug_assert!(slot < self.statics.len());
                    let val = self.stack.pop();
                    if let Some(s) = self.statics.get_mut(slot) {
                        *s = val;
                    }
                }
                Instruction::BoxValue => {
                    let tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.bits();
                    let payload = if addr == 0 {
                        Member::Value(v)
                    } else if let Some(obj) = Self::find_object_by_addr(&self.heap, addr) {
                        Member::Object(obj)
                    } else {
                        Member::Value(v)
                    };
                    let boxed = ObjBoxed { tag, payload };
                    let (object, _) = self.heap.alloc(boxed, Object::Boxed);
                    // Root before GC: an unpushed fresh box would be swept.
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::RestoreSnapshot => {
                    let at = opcode.operand_u32() as usize;
                    let snapshot = constants
                        .get(at..)
                        .ok_or(common::SnapshotDecodeError::Truncated)
                        .and_then(common::HeapSnapshot::decode);
                    let snapshot = match snapshot {
                        Ok(s) if s.statics.iter().all(|(slot, _)| (*slot as usize) < self.statics.len()) => s,
                        Ok(_) => {
                            return dispatch::RestExit::done(
                                self.runtime_panic("RestoreSnapshot: static slot out of range", ip.saturating_sub(1)),
                            );
                        }
                        Err(e) => {
                            let message = format!("RestoreSnapshot: {e}");
                            return dispatch::RestExit::done(self.runtime_panic(&message, ip.saturating_sub(1)));
                        }
                    };
                    for (slot, value) in self.heap.restore_snapshot(&snapshot) {
                        self.statics[slot as usize] = value;
                    }
                }
                Instruction::TagEnumType => {
                    // Payload variants only: unit variants are shared immortals.
                    let v = *self.stack.top();
                    if let Some(Object::Enum(gc)) = Self::find_object_by_addr(&self.heap, v.bits())
                        && !gc.as_ref().payload.is_empty()
                    {
                        gc.payload_mut().type_id = opcode.operand_u32();
                        self.heap.note_enum_type(Object::Enum(gc), opcode.operand_u32());
                    }
                }
                Instruction::TagArrayKind => {
                    let v = *self.stack.top();
                    if let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, v.bits()) {
                        let arr = gc.payload_mut();
                        arr.set_elem_kind(opcode.operand_u32() as u8);
                        self.heap.account_array(arr);
                    }
                }
                Instruction::UnboxValue => {
                    let expected_tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.bits();
                    let result = if let Some(Object::Boxed(gc)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        let b = gc.as_ref();
                        if b.tag == expected_tag || u32::from(expected_tag) == common::UNBOX_ANY_TAG {
                            match &b.payload {
                                Member::Value(inner) => *inner,
                                Member::Object(o) => Value::from(o.addr()),
                            }
                        } else {
                            // The box was made for another static type than
                            // the unboxer's: a representation bug (debug and
                            // gc-stress builds only).
                            #[cfg(any(debug_assertions, feature = "gc-stress"))]
                            {
                                let message = format!("UnboxValue: boxed tag {} where tag {expected_tag} is expected", b.tag);
                                return dispatch::RestExit::done(self.runtime_panic(&message, ip.saturating_sub(1)));
                            }
                            #[cfg(not(any(debug_assertions, feature = "gc-stress")))]
                            Value::default()
                        }
                    } else {
                        // Already unboxed (e.g. raw enum passed to a Show
                        // thunk that still emits UnboxValue). Pass through.
                        v
                    };
                    self.stack.push(result);
                }
                Instruction::MakePolyFn => {
                    let entry = opcode.operand_u32();
                    let (object, _) = self.heap.alloc(ObjFn::poly(entry, 0, Vec::new()), Object::Fn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::MakePolyFnCapture => {
                    let count = (opcode.operand_u32() & 0xFF) as usize;
                    let arity = (opcode.operand_u32() >> 8) & 0xFF;
                    let entry = self.stack.pop().as_int() as u32;
                    let mut captured_dicts = vec![None; count];
                    for slot in (0..count).rev() {
                        let value = self.stack.pop();
                        let addr = value.bits();
                        captured_dicts[slot] = if addr == 0 {
                            // Unresolved evidence, filled at CallIndirect.
                            None
                        } else if let Some(obj) = Self::find_object_by_addr(&self.heap, addr) {
                            Some(Member::Object(obj))
                        } else {
                            Some(Member::Value(value))
                        };
                    }
                    let (object, _) = self.heap.alloc(ObjFn::poly(entry, arity, captured_dicts), Object::Fn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::DynAdd
                | Instruction::DynSub
                | Instruction::DynMul
                | Instruction::DynDiv
                | Instruction::DynMod
                | Instruction::DynCmp
                | Instruction::DynEq
                | Instruction::DynNe
                | Instruction::DynPrint => {
                    return dispatch::RestExit::done(self.runtime_panic("retired opcode Dyn*", ip.saturating_sub(1)));
                }
                _ => {
                    return dispatch::RestExit::done(self.runtime_panic("unknown opcode", ip.saturating_sub(1)));
                }
        }
        dispatch::RestExit::resume(ip, sp)
    }
}

impl<const S: usize> Machine<S> {
    /// `CallIndirect` of the function word `raw`, the stack holding
    /// `[value_args..., app_dicts...]`; the call returns to `ip`. Returns the
    /// next `ip` and how dispatch continues.
    fn call_indirect(
        &mut self,
        raw: Value,
        value_arity: usize,
        app_dict_arity: usize,
        ip: usize,
        sp: &mut usize,
        code: &[Byte],
    ) -> (usize, dispatch::RestFlow) {
        // One probe: a function object, or a raw code pointer.
        let fn_obj = if raw.is_zero() {
            None
        } else {
            match self.heap.find_object_by_addr(raw.bits()) {
                Some(Object::Fn(gc)) => Some(gc),
                _ => None,
            }
        };
        let mut value_arity = value_arity;
        let mut return_ip = ip;
        let (target, captured) = match &fn_obj {
            Some(gc) => {
                let base = gc.as_ref();
                match &base.dicts {
                    Some(dicts) => {
                        // A `PolyFn` given more arguments than it declares
                        // (#1589): it runs on the ones it takes, and its
                        // result is called with the rest, as a closure's
                        // over-application (`call_fn_object`). An arity of 0
                        // is unrecorded (an older archive): every argument
                        // goes to the body.
                        let arity = base.arity as usize;
                        if arity > 0 && value_arity > arity {
                            match self.split_poly_over_apply(arity, value_arity, app_dict_arity, ip) {
                                Some(stub_ip) => return_ip = stub_ip,
                                None => {
                                    return (ip, dispatch::RestFlow::Done(self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1))));
                                }
                            }
                            value_arity = arity;
                        }
                        (base.entry as usize, dicts.as_slice())
                    }
                    None => {
                        return self.call_fn_object(base, value_arity, app_dict_arity, ip, sp, code);
                    }
                }
            }
            // A raw code offset. Anything else (another object, an
            // out-of-range integer) is a compiler bug: fail loudly rather
            // than jump into the weeds (#1570). One compare, off the
            // function-object path.
            None if (raw.as_int() as u64) < code.len() as u64 => (raw.as_int() as usize, &[][..]),
            None => return (ip, dispatch::RestFlow::Done(self.bad_call_target(raw, ip))),
        };

        // Merge captured evidence over the application dictionaries
        // in place: slot `k` is the captured dictionary, else the
        // `k`-th application dictionary. A `PolyFn` callee takes
        // exactly its captured slots, none for a `MakePolyFn` of an
        // unbounded function: application dictionaries a `forall`
        // constraint passes beyond them are dropped, so the callee
        // frame opens at its declared height (its precise map
        // assumes it, #1530). A raw code pointer takes them all.
        let dicts_base = self.stack.tell() - app_dict_arity;
        let dict_arity = if fn_obj.is_some() { captured.len() } else { app_dict_arity };
        if self.frames.len() >= crate::MAX_CALL_FRAMES
            || !self.reserve_operand_from(dicts_base + dict_arity)
        {
            return (ip, dispatch::RestFlow::Done(self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1))));
        }
        if fn_obj.is_some() {
            self.stack.seek(dicts_base);
            for (k, slot) in captured.iter().enumerate() {
                let dict = match slot {
                    Some(Member::Value(v)) => *v,
                    Some(Member::Object(o)) => Value::from(o.addr()),
                    None if k < app_dict_arity => self.stack.buffer()[dicts_base + k],
                    None => Value::default(),
                };
                self.stack.push(dict);
            }
        }

        let callee_sp = self.stack.tell() - (value_arity + dict_arity);
        (self.enter_frame(callee_sp, target, return_ip, sp, code), dispatch::RestFlow::Continue)
    }

    /// Over-application of a `PolyFn` declaring `arity` parameters, the
    /// stack holding `[value_args(value_arity)..., app_dicts...]`: the
    /// arguments past `arity` move into an over-apply stub frame under the
    /// call's own `[args(arity)..., app_dicts...]`. Returns the ip the
    /// callee returns to (the stub's), or `None` out of frames or stack.
    #[cold]
    #[inline(never)]
    fn split_poly_over_apply(&mut self, arity: usize, value_arity: usize, app_dict_arity: usize, ip: usize) -> Option<usize> {
        let base = self.stack.tell() - (value_arity + app_dict_arity);
        let words = self.stack.buffer()[base..base + value_arity + app_dict_arity].to_vec();
        let (args, rest) = words.split_at(arity);
        let (extras, dicts) = rest.split_at(value_arity - arity);
        self.stack.seek(base);
        let stub_ip = self.push_over_apply_stub(extras, ip)?;
        for w in args.iter().chain(dicts) {
            self.stack.push(*w);
        }
        Some(stub_ip)
    }

    /// Push an over-apply stub frame holding `extras` (#1550), the caller's
    /// frame resuming at `ip`: the function called next returns into it,
    /// and `finish_over_apply` calls its result with `extras`. Returns the
    /// stub's resume ip, or `None` out of frames or stack.
    fn push_over_apply_stub(&mut self, extras: &[Value], ip: usize) -> Option<usize> {
        if self.frames.len() + 1 >= crate::MAX_CALL_FRAMES || !self.reserve_operand_words(extras.len()) {
            return None;
        }
        let stub_sp = self.stack.tell();
        for a in extras {
            self.stack.push(*a);
        }
        self.frames.get_mut().seek(ip);
        self.frames.setup_current_and_advance(|frame| frame.set(stub_sp));
        self.over_apply_frames += 1;
        self.return_bookkeeping = true;
        Some(crate::vm::OVER_APPLY_IP + extras.len())
    }

    /// `CallIndirect` of a word that is neither a function object nor a
    /// code offset: an internal error, reported as a VM panic.
    #[cold]
    #[inline(never)]
    fn bad_call_target(&mut self, raw: Value, ip: usize) -> bool {
        let what = if self.heap.find_object_by_addr(raw.bits()).is_some() {
            "a heap object that is not a function"
        } else {
            "not a code offset"
        };
        let message = format!(
            "internal error: CallIndirect target {:#x} is {what} (compiler bug)",
            raw.bits()
        );
        self.runtime_panic(&message, ip.saturating_sub(1))
    }

    /// `CallIndirect` on a monomorphic [`ObjFn`]: the stack holds
    /// `[value_args..., app_dicts...]` (the target already popped). An exact
    /// call reuses the arguments in place under the captures; otherwise the
    /// new arguments fill the next holes, giving a fresh partial or, once
    /// every fixed slot is filled, a call (rest extras packed into an array).
    /// Returns the next `ip` and how dispatch continues.
    #[inline]
    fn call_fn_object(
        &mut self,
        base: &ObjFn,
        value_arity: usize,
        app_dict_arity: usize,
        ip: usize,
        sp: &mut usize,
        code: &[Byte],
    ) -> (usize, dispatch::RestFlow) {
        // A monomorphic function takes no dictionaries.
        self.stack.seek(self.stack.tell() - app_dict_arity);
        let arity = base.arity as usize;
        let n_captures = base.captures.len();

        if base.filled_mask == 0 && !base.is_rest && value_arity == arity {
            // Frame: [captures..., params...], the params already pushed.
            if self.frames.len() >= crate::MAX_CALL_FRAMES || !self.reserve_operand_words(n_captures) {
                return (ip, dispatch::RestFlow::Done(self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1))));
            }
            let callee_sp = self.stack.tell() - value_arity;
            if n_captures > 0 {
                self.stack.copy_slots(callee_sp + n_captures, callee_sp, value_arity);
                self.stack.seek(callee_sp);
                for c in &base.captures {
                    self.stack.push(*c);
                }
                self.stack.seek(callee_sp + n_captures + value_arity);
            }
            return (self.enter_frame(callee_sp, base.entry as usize, ip, sp, code), dispatch::RestFlow::Continue);
        }

        let mut new_args = Vec::with_capacity(value_arity);
        for _ in 0..value_arity {
            new_args.push(self.stack.pop());
        }
        new_args.reverse();

        let is_rest = base.is_rest;
        let mut filled_mask = base.filled_mask;
        let entry = base.entry;

        // Expand existing filled values into per-slot slots (decl order),
        // then fill the next unfilled holes positionally from `new_args`.
        let mut slot_vals: Vec<Option<Value>> = vec![None; arity];
        {
            let mut old_i = 0usize;
            for (slot, slot_val) in slot_vals.iter_mut().enumerate() {
                if filled_mask & (1u64 << slot) != 0 && old_i < base.captured_args.len() {
                    *slot_val = Some(base.captured_args[old_i]);
                    old_i += 1;
                }
            }
        }
        let mut arg_i = 0usize;
        for (slot, slot_val) in slot_vals.iter_mut().enumerate() {
            if filled_mask & (1u64 << slot) != 0 {
                continue;
            }
            if arg_i >= new_args.len() {
                break;
            }
            *slot_val = Some(new_args[arg_i]);
            filled_mask |= 1u64 << slot;
            arg_i += 1;
        }

        let mut captured_args: Vec<Value> = Vec::with_capacity(arity);
        for (slot, slot_val) in slot_vals.iter().enumerate() {
            if filled_mask & (1u64 << slot) != 0
                && let Some(v) = *slot_val
            {
                captured_args.push(v);
            }
        }

        let fixed_filled = filled_mask.count_ones() as usize;
        let remaining_new = &new_args[arg_i..];

        if fixed_filled < arity {
            let partial = ObjFn {
                entry,
                arity: base.arity,
                is_rest,
                filled_mask,
                captured_args,
                captures: base.captures.clone(),
                dicts: None,
            };
            let (object, _) = self.heap.alloc(partial, Object::Fn);
            self.stack.push(Value::from(object.addr()));
            self.maybe_gc_after_call_alloc(ip);
            return (ip, dispatch::RestFlow::Continue);
        }

        // Fixed slots complete. Rest extras → array (including an empty rest
        // when `is_rest` and no extras); extras to a fixed fn are applied to
        // its result (below).
        let mut call_args = captured_args;
        if is_rest {
            let rest_val = if remaining_new.len() == 1
                && !remaining_new[0].is_zero()
                && matches!(
                    Self::find_object_by_addr(&self.heap, remaining_new[0].bits()),
                    Some(Object::Array(_))
                ) {
                remaining_new[0]
            } else {
                let arr = crate::memory::ObjArray::new(remaining_new.to_vec());
                let (object, _) = self.heap.alloc(arr, Object::Array);
                Value::from(object.addr())
            };
            call_args.push(rest_val);
        }

        // Over-application (#1550): more arguments than a fixed function
        // takes, as `f(a, b)` on a curried `fn (a) => fn (b) => ..` of type
        // `A -> B -> C`. The function runs on the ones it takes and its
        // result is called with the rest, which wait on the stack in a stub
        // frame that resumes at an over-apply mark (`finish_over_apply`).
        let extras = if is_rest { &[][..] } else { remaining_new };
        let stub = usize::from(!extras.is_empty());
        if self.frames.len() + stub >= crate::MAX_CALL_FRAMES
            || !self.reserve_operand_words(extras.len() + n_captures + call_args.len())
        {
            return (ip, dispatch::RestFlow::Done(self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1))));
        }
        let mut return_ip = ip;
        if !extras.is_empty() {
            match self.push_over_apply_stub(extras, ip) {
                Some(stub_ip) => return_ip = stub_ip,
                None => {
                    return (ip, dispatch::RestFlow::Done(self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1))));
                }
            }
        }
        // Frame: [captures..., params...]
        for c in &base.captures {
            self.stack.push(*c);
        }
        for a in &call_args {
            self.stack.push(*a);
        }
        let callee_sp = self.stack.tell() - (n_captures + call_args.len());
        (self.enter_frame(callee_sp, entry as usize, return_ip, sp, code), dispatch::RestFlow::Continue)
    }

    /// Push a call frame whose words start at `callee_sp`, returning to `ip`,
    /// and jump to `target`. Returns the callee's first `ip`.
    #[inline]
    fn enter_frame(&mut self, callee_sp: usize, target: usize, ip: usize, sp: &mut usize, code: &[Byte]) -> usize {
        self.frames.get_mut().seek(ip);
        self.frames.setup_current_and_advance(|frame| frame.set(callee_sp));
        self.keep_call_window(self.frames.len() - 1);
        *sp = callee_sp;
        let mut next = ip;
        set_jump_target(&mut next, target, code);
        next
    }
}
