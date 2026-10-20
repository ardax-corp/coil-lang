//! Compile-time execution of user macros on the coil VM.
//!
//! [`VmMacroHost`] implements [`compiler::macros::MacroHost`]: each expansion
//! entry runs on a [`Machine`] with only pure host natives wired (see
//! [`compiler::macros::native_allowed_at_compile_time`]), a step budget and
//! a heap cap ([`ComptimeLimits`], `--comptime-steps` / `--comptime-mem`),
//! so a macro cannot touch the outside world, hang the build or exhaust its
//! memory.
//!
//! Each thread keeps one VM per expansion program, with the natives and the
//! program loaded once (#1008). After a call that returns, the VM is reset
//! like a reused reactor worker ([`Machine::reset_isolate_heap`]: statics,
//! frames and heap back to their initial state), so every call starts as
//! on a fresh VM. A call that panics, runs out of budget or aborts drops the
//! VM; the next call builds a new one.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

use common::{Byte, Value};
use compiler::ComptimeLimits;
use compiler::macros::{CompiledExpansion, MacroHost, native_allowed_at_compile_time};
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
        program: &CompiledExpansion,
        calls: &[(u32, String)],
        limits: &ComptimeLimits,
    ) -> Vec<Result<String, String>> {
        calls
            .iter()
            .map(|(entry, input)| run_entry(program, *entry, input, limits))
            .collect()
    }
}

/// A thread's VM for one expansion program. Holding the bytecode `Arc`
/// keeps the program alive, so its address never names another one.
struct WarmVm {
    program: Arc<Vec<Byte>>,
    vm: Box<Machine<256>>,
}

thread_local! {
    static WARM: RefCell<Vec<WarmVm>> = const { RefCell::new(Vec::new()) };
}

/// This thread's VM for `program`, or a new one with it loaded.
fn take_vm(program: &CompiledExpansion) -> Box<Machine<256>> {
    let warm = WARM.with(|warm| {
        let mut warm = warm.borrow_mut();
        let at = warm.iter().position(|w| Arc::ptr_eq(&w.program, &program.bytecode))?;
        Some(warm.swap_remove(at).vm)
    });
    warm.unwrap_or_else(|| new_vm(program))
}

fn keep_vm(program: &CompiledExpansion, vm: Box<Machine<256>>) {
    WARM.with(|warm| {
        warm.borrow_mut().push(WarmVm { program: program.bytecode.clone(), vm });
    });
}

fn new_vm(program: &CompiledExpansion) -> Box<Machine<256>> {
    let mut vm = Box::new(Machine::<256>::with_operand_capacity(program.operand_stack_slots as usize));
    for native in sandbox_natives() {
        vm.register_native(native.clone());
    }
    vm.set_program_debug(program.program_debug.clone());
    vm.init_static_slots(program.static_slot_count);
    vm.load_shared_program(
        program.bytecode.clone(),
        program.constants.clone(),
        program.strings.clone(),
    );
    vm
}

/// Standard host table with every impure native replaced by a stub that
/// fails the macro. Ids stay in ABI order. Built once per process.
fn sandbox_natives() -> &'static [Arc<dyn NativeFn>] {
    static NATIVES: OnceLock<Vec<Arc<dyn NativeFn>>> = OnceLock::new();
    NATIVES.get_or_init(build_sandbox_natives)
}

fn build_sandbox_natives() -> Vec<Arc<dyn NativeFn>> {
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

fn run_entry(program: &CompiledExpansion, entry: u32, input: &str, limits: &ComptimeLimits) -> Result<String, String> {
    let mut vm = take_vm(program);
    let captured = Captured::default();
    // `with_output` installs a thread-local redirect pointing into the VM;
    // put the caller's back before the VM is dropped.
    let outer = machine::io::set_output_redirect(None);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = vm.with_output(captured.clone());
        let result = call_entry(&mut vm, entry, input, limits);
        let _ = vm.restore_output();
        result
    }));
    machine::io::set_output_redirect(outer);
    // Only a call that returned leaves the VM between frames; any other
    // outcome drops it.
    if matches!(outcome, Ok(Ok(_))) {
        vm.reset_isolate_heap();
        keep_vm(program, vm);
    }
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

fn call_entry(vm: &mut Machine<256>, entry: u32, input: &str, limits: &ComptimeLimits) -> Result<String, String> {
    vm.set_step_budget(Some(limits.steps));
    vm.set_heap_cap(Some(usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX)));
    let input = vm.heap_mut().intern(input.to_string());
    let arg = Value::from(input.as_ptr() as *mut u8 as u64);
    let ret: Value = vm.call_function(entry, &[arg]);
    if vm.step_budget_exhausted() {
        return Err(format!(
            "it ran too long: {} steps (loop iterations + calls) is the limit; \
             is there an infinite loop? Raise it with `--comptime-steps N`",
            limits.steps
        ));
    }
    if vm.heap_cap_exceeded() {
        return Err(format!(
            "it used too much memory: {} MiB of live objects is the limit; \
             raise it with `--comptime-mem MIB`",
            limits.heap_bytes >> 20
        ));
    }
    if vm.panicked() {
        return Err(String::new());
    }
    match vm.heap().find_object_by_addr(ret.raw() as u64) {
        Some(Object::String(s)) => Ok(s.as_ref().data.to_string()),
        _ => Err("it did not return a string".to_string()),
    }
}
