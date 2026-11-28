//! A program's runtime with no interpreter: compile-time code runs on one.

use std::io::Write as IoWrite;
use std::sync::Arc;

use common::{ProgramDebug, Value};

use crate::driver::{Driver, DriverProgram};
use crate::memory::Heap;
use crate::native::{NativeCode, NativeTable};
use crate::runtime::{OutputSink, Runtime};

/// What an [`Isolate`] runs.
pub struct IsolateProgram {
    pub constants: Arc<Vec<u64>>,
    pub strings: Arc<Vec<String>>,
    pub debug: ProgramDebug,
    pub native: Arc<NativeCode>,
    pub static_slots: u32,
}

/// A loaded program's heap, statics and natives, called body by body on a
/// driver. It keeps its heap between calls until [`Self::reset`].
pub struct Isolate {
    rt: Runtime,
    constants: Arc<Vec<u64>>,
    debug: Arc<ProgramDebug>,
    native: Arc<NativeCode>,
    panicked: bool,
}

impl Isolate {
    pub fn new(program: IsolateProgram) -> Self {
        let mut rt = Runtime::new();
        rt.statics = vec![Value::default(); program.static_slots as usize];
        rt.program_string_cache = vec![Value::default(); program.strings.len()];
        rt.program_strings = program.strings;
        rt.native = Some(Box::new(NativeTable::new(Arc::clone(&program.native))));
        Self { rt, constants: program.constants, debug: Arc::new(program.debug), native: program.native, panicked: false }
    }

    pub fn register_native(&mut self, native: Arc<dyn crate::ffi::NativeFn>) {
        self.rt.natives.register(native);
    }

    pub fn heap(&self) -> &Heap {
        self.rt.heap.get()
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        self.rt.heap.get_mut()
    }

    pub fn static_value(&self, slot: u32) -> Option<Value> {
        self.rt.statics.get(slot as usize).copied()
    }

    /// `false` when the program has no such slot.
    pub fn set_static(&mut self, slot: u32, value: Value) -> bool {
        self.rt.statics.get_mut(slot as usize).map(|s| *s = value).is_some()
    }

    /// Send `print` to `writer`; the sink it replaces.
    pub fn with_output<W: IoWrite + Send + 'static>(&mut self, writer: W) -> Option<OutputSink> {
        self.rt.with_output(writer)
    }

    /// Back to stdout; the sink it replaces.
    pub fn restore_output(&mut self) -> Option<OutputSink> {
        self.rt.restore_output()
    }

    /// Loop back-edges and calls a run may take before it panics; `None` is
    /// unlimited.
    pub fn set_step_budget(&mut self, budget: Option<u64>) {
        self.rt.set_step_budget(budget);
    }

    /// Steps charged since the budget was last set.
    pub fn steps_charged(&self) -> u64 {
        self.rt.steps_charged()
    }

    pub fn step_budget_exhausted(&self) -> bool {
        self.rt.step_budget_hit
    }

    /// Bytes the heap may grow to before the run panics; `None` is no cap.
    pub fn set_heap_cap(&mut self, cap: Option<usize>) {
        self.rt.set_heap_cap(cap);
    }

    pub fn heap_cap_exceeded(&self) -> bool {
        self.rt.heap_cap_hit
    }

    /// Whether a call panicked since the last [`Self::reset`].
    pub fn panicked(&self) -> bool {
        self.panicked
    }

    /// Back to the program as loaded: statics, heap and panics.
    pub fn reset(&mut self) {
        self.panicked = false;
        self.rt.reset_isolate();
    }

    /// Call the body at `offset`: a panic sets [`Self::panicked`] and
    /// returns the default word. `None`: the code does not cover it, and
    /// nothing ran.
    pub fn call(&mut self, offset: u32, args: &[Value]) -> Option<Value> {
        let start = offset as usize;
        if self.native.lookup(start).is_none() || !self.native.is_complete() {
            return None;
        }
        let out = self.on_driver(|d| d.call(start, args));
        Some(out.map_or(Value::default(), |(lo, _)| Value::from(lo)))
    }

    fn on_driver<R>(&mut self, f: impl FnOnce(&mut Driver<'_>) -> R) -> R {
        let program = DriverProgram {
            startup: Arc::default(),
            main: None,
            constants: Arc::clone(&self.constants),
            native: Arc::clone(&self.native),
            debug: Arc::clone(&self.debug),
            thread_program: None,
            shared_print: None,
        };
        let mut driver = Driver::new(&mut self.rt, program);
        let guard = crate::thread::HostStateGuard::enter_driver(&mut driver);
        let out = f(&mut driver);
        drop(guard);
        self.panicked |= driver.panicked();
        out
    }
}

impl Drop for Isolate {
    fn drop(&mut self) {
        // A warm isolate goes with its thread's locals, after a reset left
        // nothing owed: a driver needs those locals.
        if !self.rt.finalizer_by_type.is_empty() || self.rt.closeables {
            self.on_driver(|d| d.run_remaining_finalizers());
        }
    }
}
