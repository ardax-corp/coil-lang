//! Stack VM, managed heap, and FFI runtime for coil bytecode.

pub mod char_ord;
pub mod clock;
#[cfg(any(test, feature = "debugger"))]
pub mod debug;
pub mod env;
mod fused;
mod ffi;
pub mod fs;
pub mod gc_handles;
pub mod host_enum;
pub mod host_natives;
pub mod io;
mod io_handle;
pub mod io_reactor;
pub mod math_libm;
mod memory;
mod opcode;
pub mod packed_la;
pub mod reactor;
pub mod runtime_wire;
pub mod stream_attach;
pub mod thread;
pub mod value_eq;
pub mod vec_ops;
mod vm;

#[cfg(any(test, feature = "debugger"))]
pub use debug::{DebugController, StepMode, StopReason};
pub use clock::CLOCK_WIRING;
pub use env::ENV_WIRING;
pub use ffi::*;
pub use fs::FS_WIRING;
pub use gc_handles::{GC_COLLECT_NATIVE, GC_REGISTER_FINALIZER_NATIVE, GC_WIRING};
pub use host_natives::{
    CLOCK_MONO_NANOS_NATIVE, CLOCK_SLEEP_MS_NATIVE, CLOCK_WALL_NANOS_NATIVE,
    STREAM_ATTACH_NATIVE, STREAM_PARK_NATIVE, build_standard_host_natives,
    wire_standard_host_natives,
};
pub use memory::*;
pub use opcode::*;
pub use packed_la::{
    PACKED_DOT, PACKED_MATMUL, PACKED_MATRIX_NEG, PACKED_MATRIX_ZIP, PACKED_VEC_ARITH, packed_dot,
    packed_matmul, packed_matrix_neg, packed_matrix_zip, packed_vec_arith,
};
pub use runtime_wire::{VmHostSpec, wire_thread_program, wire_vm_host};
pub use stream_attach::{AttachedIo, StreamVTable, stream_attach, stream_park};
pub use thread::{
    LiveThreadRegistry, ThreadErrorTag, ThreadProgram, join_undetached_threads,
    new_live_thread_registry,
};
pub use vm::*;

/// Default operand-stack capacity when analysis does not request more.
pub const DEFAULT_OPERAND_STACK_SLOTS: usize = 256;

/// Hard ceiling for analysis-driven stack sizing (guards absurd `#[max_depth]`).
pub const MAX_OPERAND_STACK_SLOTS: usize = 1_048_576;
