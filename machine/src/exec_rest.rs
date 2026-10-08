impl<const S: usize> Machine<S> {
    fn exec_rest(
        &mut self,
        opcode: &Byte,
        ip_out: &mut usize,
        sp_out: &mut usize,
        code: &[Byte],
        constants: &[u64],
        stack_cap: usize,
    ) -> dispatch::RestFlow {
        let mut ip = *ip_out;
        let mut sp = *sp_out;
        let bc = opcode.bytecode();
        // Step budget charge, as `charge_step!` in `execute`.
        macro_rules! charge_step {
            ($back:expr) => {
                self.fuel = self.fuel.wrapping_sub(($back) as u64);
                if unlikely(self.fuel == 0) {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.step_budget_out(ip.saturating_sub(1)));
                }
            };
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
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode OptionNicheToHeap", ip.saturating_sub(1)));
                }
                Instruction::HeapOptionToNiche => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode HeapOptionToNiche", ip.saturating_sub(1)));
                }
                Instruction::PairJumpIfTag => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode PairJumpIfTag", ip.saturating_sub(1)));
                }
                Instruction::PairToHeap => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode PairToHeap", ip.saturating_sub(1)));
                }
                Instruction::HeapToPair => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode HeapToPair", ip.saturating_sub(1)));
                }
                Instruction::ReturnPair => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode ReturnPair", ip.saturating_sub(1)));
                }
                Instruction::INC => {
                    let (slot, prefix, is_float) = opcode.inc_dec_parts();
                    promise!(sp + slot < stack_cap);
                    let idx = sp + slot;
                    let old = self.stack[idx];
                    let new_val = if is_float {
                        Value::from(old.as_float() + 1.0)
                    } else {
                        Value::from(old.as_int() + 1)
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
                        Value::from(old.as_int() - 1)
                    };
                    self.stack[idx] = new_val;
                    self.stack.push(if prefix { new_val } else { old });
                }
                Instruction::NOT => unary!(self.stack, !, as_int),
                Instruction::LogNot => {
                    let val = self.stack.pop();
                    self.stack.push(Value::from(!(val.as_int() != 0)));
                }
                Instruction::NEG => unary!(self.stack, -, as_int),
                // IEEE negate: flip sign bit (preserves NaN payload).
                Instruction::NEGF => {
                    let sp = self.stack.tell();
                    promise!(sp >= 1);
                    let idx = sp - 1;
                    let bits = self.stack[idx].raw() as u64;
                    self.stack[idx].replace((bits ^ (1u64 << 63)) as _);
                }
                Instruction::AND => binary!(self.stack, &&, as_bool),
                Instruction::OR => binary!(self.stack, ||, as_bool),
                Instruction::ADD => binary!(self.stack, +, as_int),
                Instruction::SUB => binary!(self.stack, -, as_int),
                Instruction::MUL => binary!(self.stack, *, as_int),
                Instruction::DIV => binary!(self.stack, /, as_int),
                Instruction::MOD => binary!(self.stack, %, as_int),
                Instruction::LE => binary!(self.stack, <, as_int),
                Instruction::LEQ => binary!(self.stack, <=, as_int),
                Instruction::GT => binary!(self.stack, >, as_int),
                Instruction::GEQ => binary!(self.stack, >=, as_int),
                Instruction::EQ => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1];
                    let lhs = self.stack[sp - 2];
                    let eq = crate::value_eq::values_eq(&self.heap, lhs, rhs);
                    self.stack[sp - 2].replace(eq as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::NEQ => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1];
                    let lhs = self.stack[sp - 2];
                    let eq = crate::value_eq::values_eq(&self.heap, lhs, rhs);
                    self.stack[sp - 2].replace((!eq) as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::ADDF => binary!(self.stack, +, as_float, to_bits),
                Instruction::SUBF => binary!(self.stack, -, as_float, to_bits),
                Instruction::MULF => binary!(self.stack, *, as_float, to_bits),
                Instruction::DIVF => binary!(self.stack, /, as_float, to_bits),
                Instruction::MODF => binary!(self.stack, %, as_float, to_bits),
                Instruction::SHL => binary!(self.stack, <<, as_int),
                Instruction::SHR => binary!(self.stack, >>, as_int),
                Instruction::XOR => binary!(self.stack, ^, as_int),
                Instruction::BITAND => binary!(self.stack, &, as_int),
                Instruction::BITOR => binary!(self.stack, |, as_int),
                Instruction::Pow => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1].as_int();
                    let lhs = self.stack[sp - 2].as_int();
                    let result = lhs.pow(rhs as u32);
                    self.stack[sp - 2].replace(result as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::PowF => {
                    let sp = self.stack.tell();
                    promise!(sp >= 2);
                    let rhs = self.stack[sp - 1].as_float();
                    let lhs = self.stack[sp - 2].as_float();
                    let result = lhs.powf(rhs);
                    self.stack[sp - 2].replace(result.to_bits() as _);
                    self.stack.seek(sp - 1);
                }
                Instruction::LEF => binary!(self.stack, <, as_float),
                Instruction::LEQF => binary!(self.stack, <=, as_float),
                Instruction::GTF => binary!(self.stack, >, as_float),
                Instruction::GEQF => binary!(self.stack, >=, as_float),
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
                                            params.pop().raw().addr()
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
                                            params.pop().raw().addr()
                                        );
                                    }
                                    Some('z') => {
                                        chars.next();
                                        message.push_str(if params.pop().raw() > 0 as _ {
                                            "true"
                                        } else {
                                            "false"
                                        });
                                    }
                                    Some('u') => {
                                        chars.next();
                                        message.push_str(&params.pop().raw().addr().to_string());
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
                    if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "{}", s);
                        let _ = out.flush();
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
                    let result = crate::fused::eval_bin(op, va, vb, &self.heap);
                    self.stack.push(result);
                }
                Instruction::NATIVE => {
                    #[cfg(debug_assertions)]
                    eprintln!("FFI: deprecated NATIVE opcode — recompile from source");
                }
                Instruction::FfiLoad => {
                    // Inlined to split-borrow `heap`/`libraries` from `frames`.
                    let path_val = self.stack.pop();
                    let path = {
                        let addr = path_val.raw() as u64;
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
                            self.push_result_ok(Value::from(addr as *mut u8));
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
                        let tags_addr = tags_val.raw() as u64;
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
                    let tuple_addr = tuple_val.raw() as u64;

                    let function_id_val = self.stack.pop();
                    let function_id = function_id_val.as_int() as usize;

                    let lib_val = self.stack.pop();
                    let lib_addr = lib_val.raw() as u64;

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
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(true);
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
                    let args_tuple_addr = args_tuple_val.raw() as u64;

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
                    let lib_addr = lib_val.raw() as u64;
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
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
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
                        crate::HostOp::Unwind => {
                            self.stack.seek(tell - consume);
                            *ip_out = ip;
                            *sp_out = sp;
                            self.unwind_resume();
                            return dispatch::RestFlow::Done(false);
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
                                    *ip_out = ip;
                                    *sp_out = sp;
                                    return dispatch::RestFlow::Continue;
                                }
                                TaskFlow::Panic(msg) => {
                                    *ip_out = ip;
                                    *sp_out = sp;
                                    return dispatch::RestFlow::Done(
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
                                *ip_out = ip;
                                *sp_out = sp;
                                return dispatch::RestFlow::Continue;
                            }
                            let native = self.natives.get_by_id(fn_id).expect("id checked above");
                            let args = &self.stack.top_window(consume)[1..];
                            let layout = crate::host_enum::HostEnumLayout::from_operand(
                                opcode.operand_u32(),
                            );
                            // `stream_park` parks the task (not the thread) under a scheduler.
                            let park_tasks = fn_id == common::STREAM_PARK_ID as usize
                                && self.tasks_can_switch();
                            if unlikely(park_tasks) {
                                crate::task::set_tasks_active(true);
                            }
                            let invoked = crate::host_enum::with_host_enum_layout(layout, || {
                                native.invoke(&mut self.heap, args)
                            });
                            if unlikely(park_tasks) {
                                crate::task::set_tasks_active(false);
                            }
                            match invoked {
                                Ok(Some(v)) => {
                                    self.stack.seek(tell - consume);
                                    self.stack.push(v);
                                }
                                Ok(None) => {
                                    self.stack.seek(tell - consume);
                                    if let Some(req) = crate::io::take_pending_io_park() {
                                        if self.tasks_can_switch() {
                                            // Suspend this task (with any generator it
                                            // is resuming); another task runs meanwhile.
                                            self.task_suspend_io(req, layout, &mut ip, &mut sp);
                                            *ip_out = ip;
                                            *sp_out = sp;
                                            return dispatch::RestFlow::Continue;
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
                                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(true);
                                        }
                                    } else {
                                        // Void natives must still leave a defined TOS.
                                        self.stack.push(Value::default());
                                    }
                                }
                                Err(e) => {
                                    let name = native.name();
                                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
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
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode HostInvokeNiche", ip.saturating_sub(1)));
                }
                Instruction::FloatChainStore => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("retired opcode FloatChainStore", ip.saturating_sub(1)));
                }
                // Fused `BinSlotSlot <arith>; CONST pool; CmpJmpf/CmpJmpt`, no stack traffic.
                Instruction::BinSlotSlotConstJmpf => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
                        "retired opcode BinSlotSlotConstJmpf",
                        ip.saturating_sub(1),
                    ));

                }
                Instruction::BinSlotSlotConstJmpt => {
                    let (bin_op, a, desc_idx) = opcode.bin_slot_slot_const_jmpf_parts();
                    promise!(desc_idx < constants.len());
                    let packed = unsafe { *constants.get_unchecked(desc_idx) };
                    let (b, cmp_op, float_idx, target) =
                        RawByte::unpack_bin_slot_slot_const_jmpf_desc(packed);
                    let b = b as usize;
                    promise!(float_idx < constants.len());
                    promise!(sp + a < stack_cap);
                    promise!(sp + b < stack_cap);
                    let va = self.stack[sp + a].as_float();
                    let vb = self.stack[sp + b].as_float();
                    let mag = crate::fused::eval_f64_bin(bin_op, va, vb);
                    let rhs =
                        Value::from(unsafe { *constants.get_unchecked(float_idx) }).as_float();
                    let taken = crate::fused::eval_f64_cmp(cmp_op, mag, rhs);
                    if taken == matches!(*bc, Instruction::BinSlotSlotConstJmpt) {
                        charge_step!(target < ip);
                        set_jump_target(&mut ip, target, code);
                    }
                }
                Instruction::HALT => {
                    if let Some(out) = self.output.as_mut() {
                        let _ = out.flush();
                    } else {
                        let _ = io::stdout().flush();
                    }
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(false);
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
                        *ip_out = ip;
                        *sp_out = sp;
                        return dispatch::RestFlow::Done(false);
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
                        let _ = write!(out, "panic: {}{}{}", s, loc_suffix, backtrace);
                        let _ = out.flush();
                    } else {
                        eprint!("panic: {}{}{}", s, loc_suffix, backtrace);
                        let _ = io::stderr().flush();
                    }
                    self.panicked = true;
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(false);
                }
                Instruction::STRING => {
                    let idx = opcode.operand_u32() as usize;
                    promise!(idx < self.program_strings.len());
                    self.push_program_string(idx, ip);
                }
                Instruction::NOOP => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Continue;
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
                    let addr = arr_val.raw() as u64;
                    if let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, addr) {
                        self.pin_current_array(slot, Object::Array(gc));
                    }
                }
                Instruction::Index | Instruction::IndexUnchecked => {
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::IndexUnchecked);
                    // Arrays dominate Index traffic (Vec); check Array before Tuple.
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        Some(crate::memory::Object::Tuple(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                    };
                    self.stack.push(result);
                }
                Instruction::IndexPin | Instruction::IndexPinUnchecked => {
                    let slot = opcode.operand_u32();
                    let index = self.stack.pop().as_int();
                    let unchecked = matches!(*bc, Instruction::IndexPinUnchecked);
                    let result = match self.pinned_object(slot) {
                        Some(Object::Array(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        Some(Object::Tuple(gc)) => {
                            Self::read_indexed(gc.as_ref().elements(), index, unchecked)
                        }
                        _ => None,
                    };
                    let Some(result) = result else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
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
                                Self::find_object_by_addr(&self.heap, value.raw() as u64)
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
                    let target_addr = target_val.raw() as u64;
                    let result = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Instance(gc)) => match gc.as_ref().get(key) {
                            Some(v) => v,
                            None => {
                                *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("no such field", ip.saturating_sub(1)));
                            }
                        },
                        _ => {
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("no such field", ip.saturating_sub(1)));
                        }
                    };
                    self.stack.push(result);
                }
                Instruction::SetField => {
                    if let Some(slot) = set_field_slot_index(opcode.operand_u32()) {
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let target_addr = target_val.raw() as u64;
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let idx = slot as usize;
                            promise!(gc.as_ref().slot_len().is_some_and(|n| idx < n));
                            gc.as_mut().set_slot(idx, value);
                        } else {
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("SetField on non-instance", ip.saturating_sub(1)));
                        }
                        self.stack.push(value);
                    } else {
                        let name_val = self.stack.pop();
                        let target_val = self.stack.pop();
                        let value = self.stack.pop();
                        let key = Self::intern_key(&mut self.heap, name_val);
                        let target_addr = target_val.raw() as u64;
                        if let Some(crate::memory::Object::Instance(mut gc)) =
                            Self::find_object_by_addr(&self.heap, target_addr)
                        {
                            let member = Self::value_as_member(&self.heap, value);
                            gc.as_mut().set(key, member);
                        } else {
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("SetField on non-instance", ip.saturating_sub(1)));
                        }
                        self.stack.push(value);
                    }
                }
                Instruction::StoreIndex | Instruction::StoreIndexUnchecked => {
                    let value = self.stack.pop();
                    let index_val = self.stack.pop();
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let index = index_val.as_int();
                    let unchecked = matches!(*bc, Instruction::StoreIndexUnchecked);
                    if let Some(crate::memory::Object::Array(mut gc)) =
                        Self::find_object_by_addr(&self.heap, target_addr)
                    {
                        if !gc.as_mut().store_indexed(index, value, unchecked) {
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                        }
                    } else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("StoreIndex on non-array", ip.saturating_sub(1)));
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
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("index out of bounds", ip.saturating_sub(1)));
                        }
                    } else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("StoreIndexPin on non-array", ip.saturating_sub(1)));
                    }
                    self.stack.push(value);
                }
                Instruction::VLoad => {
                    let (ty, vdest, arr, idx) = opcode.dense_abc_parts();
                    promise!(vdest < common::simd::NREGS);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + idx < stack_cap);
                    let index = self.stack[sp + idx].as_int();
                    let addr = self.stack[sp + arr].raw() as u64;
                    if !self.vload(vdest, addr, index, ty) {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("VLoad out of bounds", ip.saturating_sub(1)));
                    }
                }
                Instruction::VStore => {
                    let (ty, vsrc, arr, idx) = opcode.dense_abc_parts();
                    promise!(vsrc < common::simd::NREGS);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + idx < stack_cap);
                    let index = self.stack[sp + idx].as_int();
                    let addr = self.stack[sp + arr].raw() as u64;
                    if !self.vstore(vsrc, addr, index, ty) {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("VStore out of bounds", ip.saturating_sub(1)));
                    }
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
                    let out = crate::simd::eval_vbin(kind, lhs, rhs, scalar);
                    self.vregs[dest] = out;
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
                    self.stack[sp + dest] =
                        crate::simd::eval_vreduce(ty, acc, &self.vregs[vsrc], fold as u8);
                }
                Instruction::VFma => {
                    let (ty, dest, a, b) = opcode.dense_abc_parts();
                    promise!(dest < common::simd::NREGS);
                    promise!(a < common::simd::NREGS);
                    promise!(b < common::simd::NREGS);
                    let out = crate::simd::eval_vfma(
                        ty,
                        &self.vregs[a],
                        &self.vregs[b],
                        &self.vregs[dest],
                    );
                    self.vregs[dest] = out;
                }
                Instruction::DenseMake => {
                    let (kind, dest, arity, base) = opcode.dense_abc_parts();
                    promise!(sp + dest < stack_cap);
                    promise!(sp + base < stack_cap);
                    if arity > 0 {
                        promise!(sp + base + arity - 1 < stack_cap);
                    }
                    if arity <= 3 {
                        note_make_fast();
                    }
                    let lo = sp + base;
                    let addr = if kind >= common::dense::MAKE_ENUM {
                        // Inline payloads read the registers directly (no Vec).
                        let tag = u32::from(kind - common::dense::MAKE_ENUM);
                        let payload =
                            Self::dense_enum_payload(&self.stack[lo..lo + arity]);
                        let (object, _) = self.heap.alloc(ObjEnum::new(tag, payload), Object::Enum);
                        object.addr()
                    } else if kind == common::dense::MAKE_TUPLE && arity == 0 {
                        self.heap.immortal_empty_tuple().addr()
                    } else if kind == common::dense::MAKE_TUPLE {
                        let tuple = ObjTuple::from_slice(&self.stack[lo..lo + arity]);
                        let (object, _) = self.heap.alloc(tuple, Object::Tuple);
                        object.addr()
                    } else {
                        let values = Self::stack_copy_decl(&self.stack, lo, arity);
                        let arr = ObjArray::from_values(values, &self.heap);
                        let (object, _) = self.heap.alloc(arr, Object::Array);
                        object.addr()
                    };
                    self.stack[sp + dest] = Value::from(addr);
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::DenseMakeK => {
                    // Tuple / enum only; the verifier checked the pool index.
                    let Some((kind, dest, arity, base, kinds)) =
                        opcode.dense_make_k_parts(constants)
                    else {
                        *ip_out = ip;
                        *sp_out = sp;
                        return dispatch::RestFlow::Done(
                            self.runtime_panic("DenseMakeK pool", ip.saturating_sub(1)),
                        );
                    };
                    promise!(sp + dest < stack_cap);
                    promise!(sp + base + arity.max(1) - 1 < stack_cap);
                    let lo = sp + base;
                    let words = &self.stack[lo..lo + arity];
                    let addr = if kind >= common::dense::MAKE_ENUM {
                        let tag = u32::from(kind - common::dense::MAKE_ENUM);
                        let payload = Self::dense_enum_payload(words).with_kinds(kinds);
                        let (object, _) = self.heap.alloc(ObjEnum::new(tag, payload), Object::Enum);
                        object.addr()
                    } else if arity == 0 {
                        self.heap.immortal_empty_tuple().addr()
                    } else {
                        let tuple = ObjTuple::from_slice(words).with_kinds(kinds);
                        let (object, _) = self.heap.alloc(tuple, Object::Tuple);
                        object.addr()
                    };
                    self.stack[sp + dest] = Value::from(addr);
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::DensePush => {
                    let (arity, base) = opcode.dense_move_parts();
                    if arity > 0 {
                        promise!(sp + base + arity - 1 < stack_cap);
                    }
                    for i in 0..arity {
                        self.stack.push(self.stack[sp + base + i]);
                    }
                }
                Instruction::DenseArrayPush => {
                    let (_, dest, arr, val) = opcode.dense_abc_parts();
                    promise!(sp + dest < stack_cap);
                    promise!(sp + arr < stack_cap);
                    promise!(sp + val < stack_cap);
                    let target_val = self.stack[sp + arr];
                    let value = self.stack[sp + val];
                    if !self.array_push_value(target_val, value) {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("ArrayPush on non-array", ip.saturating_sub(1)));
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
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("ArrayPush on non-array", ip.saturating_sub(1)));
                    }
                    self.stack.push(target_val);
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::ArrayLen => {
                    let target_val = self.stack.pop();
                    let target_addr = target_val.raw() as u64;
                    let len = match Self::find_object_by_addr(&self.heap, target_addr) {
                        Some(crate::memory::Object::Array(gc)) => gc.as_ref().elements().len(),
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
                    let dict_addr = dict_val.raw() as u64;
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
                    let scrutinee_addr = self.stack.peek().raw() as u64;

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
                                *ip_out = ip;
                                *sp_out = sp;
                                return dispatch::RestFlow::Done(
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
                        }
                    }
                }
                Instruction::Unpack => {
                    // Pops enum scrutinee; pushes payload in declaration order
                    // (stack/locals overlap, see STORE).
                    let arity = opcode.operand_u32() as usize;

                    promise!(self.stack.tell() > 0);
                    let scrutinee_addr = self.stack.pop().raw() as u64;

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
                    let scrutinee_addr = self.stack.pop().raw() as u64;

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
                    let scrutinee_addr = self.stack[slot].raw() as u64;

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
                    let addr = handle.raw() as u64;
                    if let Some(Object::Coroutine(gc)) = Self::find_object_by_addr(&self.heap, addr)
                    {
                        if gc.as_ref().state == CoroState::Done {
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("resumed after completion", ip.saturating_sub(1)));
                        }
                        let target = match gc.as_ref().yield_from {
                            Some(sub) => {
                                Self::with_coroutine_mut(gc, |c| {
                                    c.pending_send = send_val;
                                });
                                sub
                            }
                            None => gc,
                        };
                        if !self.resume_coroutine(&mut ip, &mut sp, target, send_val, code, true) {
                            *ip_out = ip;
                            *sp_out = sp;
                            return dispatch::RestFlow::Done(
                                self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                            );
                        }
                    } else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
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
                    let addr = handle.raw() as u64;
                    if let Some(Object::Coroutine(sub)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        if !self.start_yield_from(&mut ip, &mut sp, sub, code) {
                            *ip_out = ip;
                            *sp_out = sp;
                            return dispatch::RestFlow::Done(
                                self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                            );
                        }
                    } else {
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
                            "yield from invalid coroutine handle",
                            ip.saturating_sub(1),
                        ));

                    }
                }
                Instruction::DoneCoro => {
                    promise!(self.stack.tell() > 0);
                    let handle = self.stack.pop();
                    let addr = handle.raw() as u64;
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

                    // First-class ObjFn: merge new args into holes / captures.
                    let fn_obj = {
                        let addr = raw.raw() as u64;
                        if raw.raw().is_null() {
                            None
                        } else {
                            self.heap.find_object_by_addr(addr).and_then(|o| match o {
                                Object::Fn(gc) => Some(gc),
                                _ => None,
                            })
                        }
                    };

                    if let Some(gc) = fn_obj {
                        for _ in 0..app_dict_arity {
                            let _ = self.stack.pop();
                        }
                        let mut new_args = Vec::with_capacity(value_arity);
                        for _ in 0..value_arity {
                            new_args.push(self.stack.pop());
                        }
                        new_args.reverse();

                        let base = gc.as_ref();
                        let arity = base.arity as usize;
                        let is_rest = base.is_rest;
                        let mut filled_mask = base.filled_mask;
                        let captures = base.captures.clone();
                        let entry = base.entry;

                        // Expand existing filled values into per-slot slots
                        // (decl order), then fill the next unfilled holes
                        // positionally from `new_args`.
                        let mut slot_vals: Vec<Option<Value>> = vec![None; arity];
                        {
                            let mut old_i = 0usize;
                            for (slot, slot_val) in slot_vals.iter_mut().enumerate() {
                                if filled_mask & (1u64 << slot) != 0
                                    && old_i < base.captured_args.len() {
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
                                && let Some(v) = *slot_val {
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
                                captures,
                            };
                            let (object, _) = self.heap.alloc(partial, Object::Fn);
                            self.stack.push(Value::from(object.addr()));
                            self.maybe_gc_after_alloc(ip);
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Continue;
                        }

                        // Fixed slots complete. Rest extras → MakeArray
                        // (including empty rest when `is_rest` and no extras).
                        let mut call_args = captured_args;
                        if is_rest {
                            let rest_val = if remaining_new.len() == 1 {
                                let v = remaining_new[0];
                                let addr = v.raw() as u64;
                                if !v.raw().is_null()
                                    && matches!(
                                        Self::find_object_by_addr(&self.heap, addr),
                                        Some(Object::Array(_))
                                    )
                                {
                                    v
                                } else {
                                    let arr = crate::memory::ObjArray::new(remaining_new.to_vec());
                                    let (object, _) = self.heap.alloc(arr, Object::Array);
                                    Value::from(object.addr())
                                }
                            } else {
                                let arr = crate::memory::ObjArray::new(remaining_new.to_vec());
                                let (object, _) = self.heap.alloc(arr, Object::Array);
                                Value::from(object.addr())
                            };
                            call_args.push(rest_val);
                        } else if !remaining_new.is_empty() {
                            // Too many args for a fixed fn, drop extras defensively.
                        }

                        if self.frames.len() >= crate::MAX_CALL_FRAMES
                            || !self.reserve_operand_words(captures.len() + call_args.len())
                        {
                            *ip_out = ip;
                            *sp_out = sp;
                            return dispatch::RestFlow::Done(
                                self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                            );
                        }
                        // Frame: [captures..., params...]
                        for c in &captures {
                            self.stack.push(*c);
                        }
                        for a in &call_args {
                            self.stack.push(*a);
                        }
                        let frame_arity = captures.len() + call_args.len();
                        let return_ip = ip;
                        let callee_sp = self.stack.tell() - frame_arity;
                        self.frames.get_mut().seek(return_ip);
                        self.frames
                            .setup_current_and_advance(|frame| frame.set(callee_sp));
                        self.keep_call_window(self.frames.len() - 1);
                        sp = callee_sp;
                        set_jump_target(&mut ip, entry as usize, code);
                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Continue;
                    }

                    let (target, captured) = {
                        let addr = raw.raw() as u64;
                        if raw.raw().is_null() {
                            (raw.as_int() as usize, Vec::new())
                        } else if let Some(Object::PolyFn(gc)) = self.heap.find_object_by_addr(addr)
                        {
                            let pfn = gc.as_ref();
                            (pfn.entry as usize, pfn.captured_dicts.clone())
                        } else {
                            (raw.as_int() as usize, Vec::new())
                        }
                    };

                    // Pop application dictionaries (TOS = last in declaration order).
                    let mut app_dicts = Vec::with_capacity(app_dict_arity);
                    for _ in 0..app_dict_arity {
                        app_dicts.push(self.stack.pop());
                    }
                    app_dicts.reverse();

                    let member_value = |m: &crate::memory::Member| -> Value {
                        match m {
                            crate::memory::Member::Value(v) => *v,
                            crate::memory::Member::Object(o) => Value::from(o.addr()),
                        }
                    };

                    let merged_dicts: Vec<Value> = if captured.is_empty() {
                        app_dicts
                    } else {
                        let mut app_i = 0usize;
                        let mut merged = Vec::with_capacity(captured.len());
                        for slot in &captured {
                            match slot {
                                Some(m) => {
                                    merged.push(member_value(m));
                                    if app_i < app_dicts.len() {
                                        app_i += 1;
                                    }
                                }
                                None => {
                                    if app_i < app_dicts.len() {
                                        merged.push(app_dicts[app_i]);
                                        app_i += 1;
                                    } else {
                                        merged.push(Value::default());
                                    }
                                }
                            }
                        }
                        merged
                    };

                    let dict_arity = merged_dicts.len();
                    if self.frames.len() >= crate::MAX_CALL_FRAMES || !self.reserve_operand_words(dict_arity) {
                        *ip_out = ip;
                        *sp_out = sp;
                        return dispatch::RestFlow::Done(
                            self.runtime_panic(STACK_OVERFLOW, ip.saturating_sub(1)),
                        );
                    }
                    for dict in merged_dicts {
                        self.stack.push(dict);
                    }

                    let arity = value_arity + dict_arity;

                    let return_ip = ip;
                    let callee_sp = self.stack.tell() - arity;
                    self.frames.get_mut().seek(return_ip);
                    self.frames
                        .setup_current_and_advance(|frame| frame.set(callee_sp));
                    self.keep_call_window(self.frames.len() - 1);
                    sp = callee_sp;
                    set_jump_target(&mut ip, target, code);
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
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::Fn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::LoadStatic => {
                    let slot = opcode.operand_u32() as usize;
                    promise!(slot < self.statics.len());
                    let val = self.statics[slot];
                    self.stack.push(val);
                }
                Instruction::StoreStatic => {
                    let slot = opcode.operand_u32() as usize;
                    promise!(slot < self.statics.len());
                    let val = self.stack.pop();
                    self.statics[slot] = val;
                }
                Instruction::BoxValue => {
                    let tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.raw() as u64;
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
                Instruction::TagEnumType => {
                    // Payload variants only: unit variants are shared immortals.
                    let v = *self.stack.top();
                    if let Some(Object::Enum(gc)) = Self::find_object_by_addr(&self.heap, v.raw() as u64)
                        && !gc.as_ref().payload.is_empty()
                    {
                        gc.payload_mut().type_id = opcode.operand_u32();
                    }
                }
                Instruction::TagArrayKind => {
                    let v = *self.stack.top();
                    if let Some(Object::Array(gc)) = Self::find_object_by_addr(&self.heap, v.raw() as u64) {
                        gc.payload_mut().set_elem_kind(opcode.operand_u32() as u8);
                    }
                }
                Instruction::UnboxValue => {
                    let expected_tag = (opcode.operand_u32() & 0xFFFF) as u16;
                    let v = self.stack.pop();
                    let addr = v.raw() as u64;
                    let result = if let Some(Object::Boxed(gc)) =
                        Self::find_object_by_addr(&self.heap, addr)
                    {
                        let b = gc.as_ref();
                        if b.tag == expected_tag {
                            match &b.payload {
                                Member::Value(inner) => *inner,
                                Member::Object(o) => Value::from(o.addr()),
                            }
                        } else {
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
                    let pfn = ObjPolyFn {
                        entry,
                        type_arity: 0,
                        captured_dicts: Vec::new(),
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::PolyFn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::MakePolyFnCapture => {
                    let count = (opcode.operand_u32() & 0xFF) as usize;
                    let entry = self.stack.pop().as_int() as u32;
                    let mut captured_dicts = vec![None; count];
                    for slot in (0..count).rev() {
                        let value = self.stack.pop();
                        let addr = value.raw() as u64;
                        captured_dicts[slot] = if addr == 0 {
                            // Unresolved evidence, filled at CallIndirect.
                            None
                        } else if let Some(obj) = Self::find_object_by_addr(&self.heap, addr) {
                            Some(Member::Object(obj))
                        } else {
                            Some(Member::Value(value))
                        };
                    }
                    let pfn = ObjPolyFn {
                        entry,
                        type_arity: 0,
                        captured_dicts,
                    };
                    let (object, _) = self.heap.alloc(pfn, Object::PolyFn);
                    self.stack.push(Value::from(object.addr()));
                    self.maybe_gc_after_alloc(ip);
                }
                Instruction::DynAdd
                | Instruction::DynSub
                | Instruction::DynMul
                | Instruction::DynDiv
                | Instruction::DynMod => {
                    /// Classify a value into (ValueTag, payload-Value).
                    /// Uses `Heap::find_object_by_addr` (mapped slot + header kind).
                    fn classify_dyn(v: Value, heap: &Heap) -> (ValueTag, Value) {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return (ValueTag::Int, v);
                        }
                        if let Some(obj) = heap.find_object_by_addr(addr) {
                            return match obj {
                                Object::Boxed(gc) => {
                                    let b = gc.as_ref();
                                    let tag = ValueTag::from_u16(b.tag).unwrap_or(ValueTag::Int);
                                    let inner = match &b.payload {
                                        Member::Value(iv) => *iv,
                                        Member::Object(o) => Value::from(o.addr()),
                                    };
                                    (tag, inner)
                                }
                                Object::String(_) => (ValueTag::String, v),
                                _ => (ValueTag::Int, v),
                            };
                        }
                        (ValueTag::Int, v)
                    }

                    let b_val = self.stack.pop();
                    let a_val = self.stack.pop();
                    let (a_tag, a_inner) = classify_dyn(a_val, &self.heap);
                    let (b_tag, b_inner) = classify_dyn(b_val, &self.heap);

                    let bc_instr = opcode.bytecode();
                    let result: Value = match (a_tag, b_tag) {
                        (ValueTag::Float, _) | (_, ValueTag::Float) => {
                            let af = a_inner.as_float();
                            let bf = b_inner.as_float();
                            let r = match bc_instr {
                                Instruction::DynAdd => af + bf,
                                Instruction::DynSub => af - bf,
                                Instruction::DynMul => af * bf,
                                Instruction::DynDiv => af / bf,
                                Instruction::DynMod => af % bf,
                                _ => unreachable!(),
                            };
                            Value::from(r)
                        }
                        (ValueTag::String, ValueTag::String)
                            if matches!(bc_instr, Instruction::DynAdd) =>
                        {
                            // Root before any GC (same as FORMAT/STRING).
                            self.push_concat(a_inner, b_inner, ip);
                            *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Continue;
                        }
                        _ => {
                            let ai = a_inner.as_int();
                            let bi = b_inner.as_int();
                            let r = match bc_instr {
                                Instruction::DynAdd => ai.wrapping_add(bi),
                                Instruction::DynSub => ai.wrapping_sub(bi),
                                Instruction::DynMul => ai.wrapping_mul(bi),
                                Instruction::DynDiv => {
                                    if bi == 0 {
                                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
                                            "division by zero",
                                            ip.saturating_sub(1),
                                        ));

                                    }
                                    ai / bi
                                }
                                Instruction::DynMod => {
                                    if bi == 0 {
                                        *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic(
                                            "division by zero",
                                            ip.saturating_sub(1),
                                        ));

                                    }
                                    ai % bi
                                }
                                _ => unreachable!(),
                            };
                            Value::from(r)
                        }
                    };
                    self.stack.push(result);
                }
                Instruction::DynCmp => {
                    fn classify_int_dyn(v: Value, heap: &Heap) -> i64 {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return v.as_int();
                        }
                        if let Some(Object::Boxed(gc)) = heap.find_object_by_addr(addr) {
                            return match &gc.as_ref().payload {
                                Member::Value(iv) => iv.as_int(),
                                Member::Object(_) => 0,
                            };
                        }
                        v.as_int()
                    }
                    let kind = opcode.operand_u32() & 0xFF;
                    let b_val = self.stack.pop();
                    let a_val = self.stack.pop();
                    let ai = classify_int_dyn(a_val, &self.heap);
                    let bi = classify_int_dyn(b_val, &self.heap);
                    let result = match kind {
                        0 => ai < bi,  // Le
                        1 => ai <= bi, // Leq
                        2 => ai > bi,  // Gt
                        3 => ai >= bi, // Geq
                        _ => false,
                    };
                    self.stack.push(Value::from(result));
                }
                Instruction::DynEq | Instruction::DynNe => {
                    fn unbox_dyn(v: Value, heap: &Heap) -> Value {
                        let addr = v.raw() as u64;
                        if v.raw().is_null() {
                            return v;
                        }
                        if let Some(Object::Boxed(gc)) = heap.find_object_by_addr(addr) {
                            return match &gc.as_ref().payload {
                                Member::Value(iv) => *iv,
                                Member::Object(o) => Value::from(o.addr()),
                            };
                        }
                        v
                    }
                    let b_val = unbox_dyn(self.stack.pop(), &self.heap);
                    let a_val = unbox_dyn(self.stack.pop(), &self.heap);
                    let eq = crate::value_eq::values_eq(&self.heap, a_val, b_val);
                    let result = if matches!(opcode.bytecode(), Instruction::DynEq) {
                        eq
                    } else {
                        !eq
                    };
                    self.stack.push(Value::from(result));
                }
                Instruction::DynPrint => {
                    let v = self.stack.pop();
                    let text = Self::stringify_value(&self.heap, v);
                    if let Some(out) = self.output.as_mut() {
                        let _ = write!(out, "{text}");
                    } else {
                        print!("{text}");
                    }
                }
                _ => {
                    *ip_out = ip;
                    *sp_out = sp;
                    return dispatch::RestFlow::Done(self.runtime_panic("unknown opcode", ip.saturating_sub(1)));
                }
        }
        *ip_out = ip;
        *sp_out = sp;
        dispatch::RestFlow::Continue
    }
}
