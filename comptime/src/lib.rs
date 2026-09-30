//! Compile-time execution of user macros on the coil VM.
//!
//! [`VmMacroHost`] implements [`compiler::macros::MacroHost`]: each expansion
//! entry runs on a fresh [`Machine`] with only pure host natives wired (see
//! [`compiler::macros::native_allowed_at_compile_time`]) and a step budget,
//! so a macro cannot touch the outside world or hang the build.

use std::sync::{Arc, Mutex};

use common::{Byte, Value};
use compiler::Pipeline;
use compiler::macros::{MACRO_STEP_BUDGET, MacroHost, native_allowed_at_compile_time};
use machine::{FfiError, HostClosureFn, Machine, NativeFn, Object};

/// Runs macros in a sandboxed VM.
#[derive(Default)]
pub struct VmMacroHost;

impl VmMacroHost {
    pub fn shared() -> Arc<dyn MacroHost> {
        Arc::new(Self)
    }
}

/// Make every pipeline created after this call run user macros on the VM.
pub fn install() {
    compiler::macros::install_default_host(VmMacroHost::shared());
}

impl MacroHost for VmMacroHost {
    fn run(
        &self,
        program: &Pipeline,
        bytecode: &[Byte],
        constants: &[u64],
        entries: &[u32],
    ) -> Vec<Result<String, String>> {
        entries
            .iter()
            .map(|&entry| run_entry(program, bytecode, constants, entry))
            .collect()
    }
}

/// Standard host table with every impure native replaced by a stub that
/// fails the macro. Ids stay in ABI order.
fn sandbox_natives() -> Vec<Arc<dyn NativeFn>> {
    let mut names = Vec::new();
    let natives = machine::build_standard_host_natives(|name, _| names.push(name.to_string()));
    natives
        .into_iter()
        .zip(names)
        .map(|(native, name)| {
            if native_allowed_at_compile_time(&name) {
                native
            } else {
                let sig = native.signature().clone();
                Arc::new(HostClosureFn::new(sig, move |_heap, _args| {
                    Err(FfiError::Unsupported(format!(
                        "`{name}` is not available to compile-time macros"
                    )))
                })) as Arc<dyn NativeFn>
            }
        })
        .collect()
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_entry(program: &Pipeline, bytecode: &[Byte], constants: &[u64], entry: u32) -> Result<String, String> {
    let captured = Captured::default();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut vm = Machine::<256>::with_operand_capacity(program.operand_stack_slots() as usize);
        let _ = vm.with_output(captured.clone());
        for native in sandbox_natives() {
            vm.register_native(native);
        }
        vm.set_program_debug(program.program_debug());
        vm.init_static_slots(program.static_slot_count());
        vm.load_program(bytecode, constants, program.strings());
        vm.set_step_budget(Some(MACRO_STEP_BUDGET));
        let ret: Value = vm.call_function(entry, &[]);
        if vm.step_budget_exhausted() {
            return Err("it ran too long (step budget exhausted; is there an infinite loop?)".to_string());
        }
        if vm.panicked() {
            return Err(String::new());
        }
        match vm.heap().find_object_by_addr(ret.raw() as u64) {
            Some(Object::String(s)) => Ok(s.as_ref().data.clone()),
            _ => Err("it did not return a string".to_string()),
        }
    }));
    let printed = String::from_utf8_lossy(&captured.0.lock().expect("capture lock")).trim().to_string();
    match outcome {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(reason)) if reason.is_empty() => Err(if printed.is_empty() {
            "it panicked".to_string()
        } else {
            printed
        }),
        Ok(Err(reason)) => Err(reason),
        Err(_) => Err(if printed.is_empty() {
            "the VM aborted".to_string()
        } else {
            printed
        }),
    }
}
