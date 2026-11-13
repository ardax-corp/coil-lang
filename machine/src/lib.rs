//! Managed heap, and FFI runtime for coil programs.

mod apply;
mod collect;
mod driver;
mod gen_fiber;
mod isolate;
mod lanes;
mod locate;
pub mod char_ord;
pub mod clock;
pub mod entropy;
pub mod key_index;
pub mod symbol;
pub mod debug;
pub mod env;
mod simd;
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
pub mod native;
pub mod packed_la;
pub mod reactor;
pub mod runtime_wire;
mod runtime;
mod stacks;
mod start;
mod worker;
mod runtime_ffi;
mod format;
pub mod shared_heap;
pub mod process;
pub mod str_bytes;
pub mod stream_attach;
mod task;
pub mod thread;
pub mod tty;
pub mod value_eq;
pub mod vec_ops;

/// Host native `(heap, args) -> value`. Arity lives in the wiring column, so
/// zero-arg clocks and three-arg vec helpers share this function type.
pub type HostValueFn = fn(&mut memory::Heap, &[common::Value]) -> common::Value;

pub use debug::{DebugController, DebugObject, DebugTask, DebugTaskFrames, StepMode, StopReason};
pub use clock::CLOCK_WIRING;
pub use collect::swap_static_word_kinds;
pub use env::ENV_WIRING;
pub use ffi::*;
pub use fs::FS_WIRING;
pub use gc_handles::{GC_COLLECT_NATIVE, GC_REGISTER_FINALIZER_NATIVE, GC_WIRING};
pub use host_natives::{
    CLOCK_MONO_NANOS_NATIVE, CLOCK_SLEEP_MS_NATIVE, CLOCK_WALL_NANOS_NATIVE,
    STREAM_ATTACH_NATIVE, STREAM_PARK_NATIVE, build_standard_host_natives, AotHostGroup, core_host_groups, HostPush, host_group_of, host_group_spans,
};
pub use memory::*;
pub use packed_la::{
    PACKED_DOT, PACKED_MATMUL, PACKED_MATRIX_NEG, PACKED_MATRIX_ZIP, PACKED_VEC_ARITH, packed_dot,
    packed_matmul, packed_matrix_neg, packed_matrix_zip, packed_vec_arith,
};
pub use runtime_wire::class_kind_table;
pub use isolate::{Isolate, IsolateProgram};
pub use start::{NativeStart, run_natively, spawn_context, standard_host_natives};
pub use stream_attach::{AttachedIo, StreamVTable, stream_attach, stream_park};
pub use thread::{
    LiveThreadRegistry, ThreadErrorTag, ThreadProgram, join_undetached_threads,
    new_live_thread_registry,
};


