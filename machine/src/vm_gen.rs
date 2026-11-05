// Generators whose body is native code, on the VM: `gen.rs` runs their
// fibers; the VM roots and reports for them.
//
// Included into `vm.rs` (needs `Machine`'s private fields).

use crate::gen_fiber::GenStep;

impl<const S: usize> crate::gen_fiber::GenHost for Machine<S> {
    fn rt(&mut self) -> &mut crate::runtime::Runtime {
        &mut self.rt
    }

    #[cfg(not(target_family = "wasm"))]
    fn gen_stacks(&mut self) -> &mut Vec<corosensei::stack::DefaultStack> {
        &mut self.gen_stacks
    }

    fn gen_running(&mut self) -> &mut Vec<Value> {
        &mut self.gen_running
    }

    fn native_env(&mut self) -> crate::native::NativeEnv {
        Machine::native_env(self)
    }

    /// VM code under the body runs nested: tasks do not switch there.
    fn nest(&mut self, entered: bool) {
        if entered {
            self.nested_depth += 1;
        } else {
            self.nested_depth -= 1;
        }
    }

    fn free_early(&mut self, v: Value, deep: bool) {
        let _ = Machine::free_early(self, v, deep);
    }
}

impl<const S: usize> Machine<S> {
    /// Resume generator `gc` with `send` when its body runs natively:
    /// `None` when the VM runs it (no native body, started on the VM, or
    /// the body bailed before its first yield).
    fn native_gen_step(&mut self, gc: RefCoroutine, send: Value) -> Option<GenStep> {
        #[cfg(any(test, feature = "debugger"))]
        if self.debug.is_some() && gc.as_ref().native.is_none() {
            return None;
        }
        crate::gen_fiber::step(self, gc, send)
    }

    /// Close suspended generator `gc`, whose body runs natively: resume
    /// its fiber to unwind from the yield it waits at, then run the armed
    /// `defer`s it collected. A panic in one is reported, as at the close
    /// before `ip`: `Some(stop)` then. The generator is done either way.
    #[cfg(not(target_family = "wasm"))]
    fn native_gen_close(&mut self, gc: RefCoroutine, ip: usize) -> Option<bool> {
        let pads = crate::gen_fiber::close(self, gc)?;
        let was_panicked = self.panicked;
        self.panicked = false;
        let env = self.native_env();
        let mut stop = None;
        crate::native::run_pads(pads, &env, &mut |p| {
            if stop.is_none() {
                stop = Some(self.native_panic(&p, ip.saturating_sub(1)));
            }
        });
        if stop.is_none() {
            self.panicked = was_panicked;
        }
        stop
    }

    /// `ResumeCoro` of `target` (the innermost delegate of the generator
    /// resumed) when its body runs natively. `Some(None)`: the word to push
    /// is on the stack, or the delegator went on (`finish_yield_from`).
    /// `Some(Some(p))`: it panicked. `None`: the VM resumes it.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    fn native_resume_coro(
        &mut self,
        target: RefCoroutine,
        send: Value,
        ip: &mut usize,
        sp: &mut usize,
    ) -> Option<Option<crate::native::NativePanic>> {
        // The resumer's return address: a panic or VM call inside names it.
        if !self.frames.is_empty() {
            self.frames.get_mut().seek(*ip);
        }
        let step = self.native_gen_step(target, send)?;
        Some(match step {
            GenStep::Yielded(v) => {
                self.stack.push(v);
                None
            }
            GenStep::Returned(v) => {
                let base = self.stack.tell();
                self.stack.push(v);
                // A VM generator's `yield from` of it goes on.
                if let Some(parent) = self.find_delegator(target) {
                    self.finish_yield_from(parent, target, base, ip, sp);
                }
                None
            }
            GenStep::Panicked(p) => Some(p),
            GenStep::Waiting => unreachable!("only a driver parks a task"),
        })
    }

    /// A native generator body's panic at the resume before `ip`, as a
    /// native call's at its `CALL`.
    fn native_gen_panic(&mut self, p: crate::native::NativePanic, ip: usize) -> bool {
        if p.code == crate::native::TRAP_VM_PANIC {
            self.rearm_unwind_after_early_drop(ip.saturating_sub(1));
            self.run_native_pads(p.pads, ip.saturating_sub(1));
            return false;
        }
        let out = self.native_panic(&p, ip.saturating_sub(1));
        self.run_native_pads(p.pads, ip.saturating_sub(1));
        out
    }
}
