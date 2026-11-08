//! Standard host-native registry for compile-time and packaged-runtime wiring.
//!
//! Registration order is ABI: `HostInvoke` fn_ids must match between the
//! compiler's `register_native_id` map and the runtime `Natives` table.

use std::sync::Arc;

use common::Value;

use crate::math_libm::{MATH_LIBM_M1_REMOVED, MATH_LIBM_REMOVED};
use crate::{
    packed_dot, packed_matmul, packed_matrix_neg, packed_matrix_zip, packed_vec_arith, FfiError,
    FfiSignature, FfiType, HostClosureFn, HostOp, NativeFn, CLOCK_WIRING, ENV_WIRING, FS_WIRING,
    PACKED_DOT, PACKED_MATMUL, PACKED_MATRIX_NEG, PACKED_MATRIX_ZIP, PACKED_VEC_ARITH,
};

use crate::vec_ops::VEC_WIRING;
use crate::GC_WIRING;

/// Removed virtual-time HostInvoke names/arities (COI-257). Same 16 slots as
/// the old `TIME_WIRING` table, after FS and before ENV, so later ids stay put.
const TIME_REMOVED: &[(&str, usize)] = &[
    ("time_timestamp", 0),
    ("time_sleep_ms", 1),
    ("time_instant_now", 0),
    ("time_elapsed_nanos", 1),
    ("time_elapsed_millis", 1),
    ("time_period", 9),
    ("time_add", 2),
    ("time_sub", 2),
    ("time_period_add", 2),
    ("time_period_sub", 2),
    ("time_date", 0),
    ("time_date_from_period", 1),
    ("time_date_from_epoch_period", 1),
    ("time_epoch", 0),
    ("time_format", 2),
    ("time_parse", 2),
];

/// Build the standard host-native table in stable order.
///
/// `register_id` is invoked as each native is appended (`name`, `id`) so the
/// compiler can record `HostInvoke` lookups. Runtime-only callers may pass a
/// no-op closure.
///
/// Leftover TLS (`tls_client_enable` … `tls_alpn_protocol`) and virtual crypto
/// slots were dropped; holes collapsed. Those ids are not reserved stubs.
/// Virtual time slots are panic stubs so later catalog ids do not slide
/// through the time block. `stream_attach` / `stream_park` are 119 / 120.
pub fn build_standard_host_natives(
    mut register_id: impl FnMut(&str, usize),
) -> Vec<Arc<dyn NativeFn>> {
    let mut out: Vec<Arc<dyn NativeFn>> = Vec::new();
    let mut register_id = |name: &str, id: usize| {
        assert_eq!(
            common::host_native_id(name),
            Some(id),
            "HostInvoke `{name}` id {id} must match common::HOST_NATIVES"
        );
        register_id(name, id);
    };
    for push in STANDARD_HOST_GROUPS {
        push(&mut out, &mut register_id);
    }
    assert_eq!(
        out.len(),
        common::HOST_NATIVES.len(),
        "runtime host table length must match common::HOST_NATIVES"
    );
    guard_streams(&mut out);
    out
}

/// Wrap each native that takes a `Stream` first in [`UnsentStreamGuard`].
fn guard_streams(out: &mut [Arc<dyn NativeFn>]) {
    for native in out {
        if !native.is_absent() && stream_first(native.name()) {
            *native = Arc::new(UnsentStreamGuard(Arc::clone(native)));
        }
    }
}

/// What pushes one group of host natives: consecutive ids, the first at
/// `out.len()`.
pub type HostPush = fn(&mut Vec<Arc<dyn NativeFn>>, &mut dyn FnMut(&str, usize));

/// The standard groups, in id order, and one exported symbol per group,
/// `coil_host_group_K`, holding its [`HostPush`]: an AOT executable's object
/// lists the groups its program calls by those symbols
/// ([`AotHostGroup`]), so the link keeps only their natives
/// (`docs/internals/native.md`, "Per-program host natives").
macro_rules! host_groups {
    ($($k:literal => $push:expr;)*) => {
        /// Every host-native group, in id order.
        pub const STANDARD_HOST_GROUPS: &[HostPush] = &[$($push),*];
        // Group `K` is the `K`th entry.
        const _: () = {
            let ks: &[usize] = &[$($k),*];
            let mut i = 0;
            while i < ks.len() {
                assert!(ks[i] == i, "host_groups! numbers its groups 0, 1, 2, ...");
                i += 1;
            }
        };
        $(
            const _: () = {
                #[unsafe(export_name = concat!("coil_host_group_", $k))]
                static GROUP: HostPush = $push;
            };
        )*
    };
}

host_groups! {
    0 => push_io_kind::<0>;
    1 => push_io_kind::<1>;
    2 => push_io_kind::<2>;
    3 => push_io_kind::<3>;
    4 => push_io_kind::<4>;
    5 => push_io_kind::<5>;
    6 => push_io_kind::<6>;
    7 => push_io_kind::<7>;
    8 => push_io_kind::<8>;
    9 => push_io_kind::<9>;
    10 => push_io_kind::<10>;
    11 => push_io_kind::<11>;
    12 => push_io_kind::<12>;
    13 => push_io_kind::<13>;
    14 => push_io_kind::<14>;
    15 => push_io_kind::<15>;
    16 => push_io_kind::<16>;
    17 => push_io_kind::<17>;
    18 => push_io_kind::<18>;
    19 => push_io_kind::<19>;
    20 => push_io_kind::<20>;
    21 => push_io_kind::<21>;
    22 => push_io_kind::<22>;
    23 => push_io_kind::<23>;
    24 => push_io_kind::<24>;
    25 => |o, r| push_wiring(o, r, FS_WIRING, "fs");
    26 => |o, r| push_removed_time_stubs(o, r);
    27 => |o, r| push_wiring(o, r, ENV_WIRING, "env");
    28 => |o, r| push_prelude_char_ord(o, r);
    29 => |o, r| push_thread_natives(o, r);
    30 => |o, r| push_packed_la(o, r);
    // Append-only: keep prior HostInvoke ids stable across ARCHIVE_MINOR bumps.
    31 => |o, r| push_io_wait_ready(o, r);
    32 => |o, r| push_io_write_from(o, r);
    33 => |o, r| push_wiring(o, r, GC_WIRING, "gc");
    34 => |o, r| push_gc_host_ops(o, r);
    35 => |o, r| push_math_libm(o, r);
    // Append-only after math_libm: Vec helpers.
    36 => |o, r| push_wiring(o, r, VEC_WIRING, "vec");
    // `stream_attach` / `stream_park` are the package-IO hooks (coil-tls uses
    // these via `dload`, not runtime TLS natives).
    37 => |o, r| push_stream_attach(o, r);
    38 => |o, r| push_stream_park(o, r);
    // Append-only after stream_park: process clocks (no Instant HashMap).
    39 => |o, r| push_wiring(o, r, CLOCK_WIRING, "clock");
    // Append-only after clocks: `Result<(), IoError>` pack probe (no IO).
    40 => |o, r| push_result_unit_probe(o, r);
    // Append-only after result_unit_probe: M1 prelude::math expansion.
    41 => |o, r| push_removed_math_stubs(o, r, MATH_LIBM_M1_REMOVED);
    // Append-only after math: MIR saxpy-reduce pack (compiler rewrite only).
    42 => |o, r| push_simd_axpy_reduce(o, r);
    43 => |o, r| push_thread_spawn_shared(o, r);
    44 => |o, r| push_stream_fd(o, r);
    // Append-only after stream_fd: byte-offset `string` natives (minor 31).
    45 => |o, r| push_string_bytes(o, r);
    // Append-only after string bytes: task scheduler natives (minor 32).
    46 => |o, r| push_task_natives(o, r, common::TASK_SCOPE_OPEN_ID..=common::TASK_YIELD_ID);
    47 => |o, r| push_unwind_resume(o, r);
    48 => |o, r| push_task_natives(o, r, common::TASK_CANCEL_ID..=common::TASK_SHIELD_EXIT_ID);
    49 => |o, r| push_task_natives(o, r, common::TASK_COND_NEW_ID..=common::TASK_COND_NOTIFY_ID);
    // `contract_fail` reads the caller's frame, so the runtime runs it too.
    50 => |o, r| push_task_natives(o, r, common::CONTRACT_FAIL_ID..=common::CONTRACT_FAIL_ID);
    // Append-only after contract_fail: `ffi::read_ints` (minor 36).
    51 => |o, r| push_ffi_read_ints(o, r);
    // Append-only after ffi_read_ints: shaped `==` (minor 39).
    52 => |o, r| push_values_eq_shaped(o, r);
    // Append-only after values_eq_shaped: string ordering (minor 41).
    53 => |o, r| push_string_compare(o, r);
    // Append-only after string_compare: `black_box` (minor 43).
    54 => |o, r| push_black_box(o, r);
    // Append-only after black_box: `io::capture_begin` / `capture_end` (minor 44).
    55 => |o, r| push_wiring(o, r, crate::io::IO_CAPTURE_WIRING, "io capture");
    // Append-only after io capture: `io::tty` (minor 47).
    56 => |o, r| push_wiring(o, r, crate::tty::TTY_WIRING, "io tty");
    // Append-only after io tty: bulk Vec copy / fill (major 6 minor 1).
    57 => |o, r| push_wiring(o, r, crate::vec_ops::VEC_BULK_WIRING, "vec bulk");
    // Append-only after vec bulk: `from_bytes` taking a dead buffer (major 6 minor 4).
    58 => |o, r| push_from_bytes_take(o, r);
    // Append-only after from_bytes_take: `region` enter / exit (major 6 minor 5).
    59 => |o, r| push_region(o, r);
    // Append-only after region: `symbol::next_id` (major 6 minor 9).
    60 => |o, r| push_wiring(o, r, crate::symbol::SYMBOL_WIRING, "symbol");
    // Append-only after symbol: the `process` module (major 6 minor 10).
    61 => |o, r| push_wiring(o, r, crate::process::PROCESS_WIRING, "process");
    // Append-only after process: `coro_close` (major 6 minor 14), run by the runtime.
    62 => |o, r| push_task_natives(o, r, common::CORO_CLOSE_ID..=common::CORO_CLOSE_ID);
    // Append-only after coro_close: `entropy::bytes` (major 6 minor 15).
    63 => |o, r| push_entropy(o, r);
    // Append-only after entropy: `key_index::*` (major 6 minor 18).
    64 => |o, r| push_wiring(o, r, crate::key_index::KEY_INDEX_WIRING, "key_index");
    // Append-only after key_index: the lock primitives (major 6 minor 21).
    65 => |o, r| push_lock_natives(o, r);
}

/// Each standard group's first id and how many natives it pushes, by
/// group number.
pub fn host_group_spans() -> &'static [(u32, u32)] {
    static SPANS: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    SPANS.get_or_init(|| {
        let mut out: Vec<Arc<dyn NativeFn>> = Vec::new();
        STANDARD_HOST_GROUPS
            .iter()
            .map(|push| {
                let first = out.len();
                push(&mut out, &mut |_, _| {});
                (first as u32, (out.len() - first) as u32)
            })
            .collect()
    })
}

/// The group (by number) native `id` belongs to.
pub fn host_group_of(id: usize) -> Option<usize> {
    host_group_spans().iter().position(|&(first, count)| (first as usize..(first + count) as usize).contains(&id))
}

/// Groups every program's runtime may call whether or not its code names
/// them: the task scheduler, `unwind_resume`, `contract_fail`, `coro_close`,
/// the collector's and `region`'s hooks.
pub fn core_host_groups() -> Vec<usize> {
    let ids = [
        common::TASK_SCOPE_OPEN_ID as usize,
        common::TASK_CANCEL_ID as usize,
        common::TASK_COND_NEW_ID as usize,
        common::UNWIND_RESUME_ID as usize,
        common::CONTRACT_FAIL_ID as usize,
        common::CORO_CLOSE_ID as usize,
        common::GC_REGION_ENTER_ID as usize,
    ];
    let names = [crate::GC_COLLECT_NATIVE, crate::GC_REGISTER_FINALIZER_NATIVE, GC_WIRING[0].0];
    let mut groups: Vec<usize> = ids
        .into_iter()
        .chain(names.into_iter().filter_map(common::host_native_id))
        .filter_map(host_group_of)
        .collect();
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// One group an AOT executable links: the address of its
/// `coil_host_group_K` symbol, its first id and how many natives it pushes.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AotHostGroup {
    /// Points at the group's [`HostPush`].
    pub push: *const std::ffi::c_void,
    pub first: u32,
    pub count: u32,
}

/// A host table of `groups` only: each group's natives at their ids, and an
/// absent native ([`NativeFn::is_absent`]) at every other id.
pub fn host_natives_of_groups(groups: &[(HostPush, u32, u32)]) -> Vec<Arc<dyn NativeFn>> {
    let absent: Arc<dyn NativeFn> = Arc::new(AbsentNative);
    let mut groups = groups.to_vec();
    groups.sort_by_key(|g| g.1);
    let mut out: Vec<Arc<dyn NativeFn>> = Vec::with_capacity(common::HOST_NATIVES.len());
    for (push, first, count) in groups {
        if (first as usize) < out.len() {
            continue;
        }
        out.resize(first as usize, Arc::clone(&absent));
        push(&mut out, &mut |_, _| {});
        assert_eq!(out.len(), (first + count) as usize, "host group at {first} pushed another count");
    }
    out.resize(common::HOST_NATIVES.len(), absent);
    guard_streams(&mut out);
    out
}

/// A host native this executable did not link: its program never calls it.
struct AbsentNative;

impl NativeFn for AbsentNative {
    fn name(&self) -> &str {
        "(absent host native)"
    }

    fn signature(&self) -> &FfiSignature {
        static SIG: std::sync::OnceLock<FfiSignature> = std::sync::OnceLock::new();
        SIG.get_or_init(|| FfiSignature::from_parts("(absent host native)".to_string(), Vec::new(), FfiType::Int).expect("absent signature"))
    }

    fn invoke(&self, _: &mut crate::Heap, _: &[Value]) -> Result<Option<Value>, FfiError> {
        Err(FfiError::Unsupported("this executable does not link the host native".into()))
    }

    fn is_absent(&self) -> bool {
        true
    }
}

/// Whether native `name` takes a `Stream` first, which it checks is not
/// unsent ([`UnsentStreamGuard`]).
pub fn stream_first(name: &str) -> bool {
    STREAM_FIRST_NATIVES.contains(&name)
}


/// Natives whose first argument is a `Stream`.
const STREAM_FIRST_NATIVES: &[&str] = &[
    "close",
    "read",
    "write",
    "write_from",
    "await_readable",
    "await_writable",
    "tcp_accept",
    "tcp_peer_addr",
    "tcp_local_addr",
    "tcp_set_nodelay",
    "tcp_shutdown",
    "udp_send_to",
    "udp_recv_from",
    "udp_local_port",
    STREAM_ATTACH_NATIVE,
    STREAM_PARK_NATIVE,
    STREAM_FD_NATIVE,
];

/// A stream native that panics when its stream is one a `thread::spawn`
/// worker read from a static of its spawner but could not take, a file or
/// socket (#1684): an `Err` there is easy to drop, and the write with it.
struct UnsentStreamGuard(Arc<dyn NativeFn>);

impl NativeFn for UnsentStreamGuard {
    fn name(&self) -> &str {
        self.0.name()
    }

    fn signature(&self) -> &FfiSignature {
        self.0.signature()
    }

    fn host_op(&self) -> HostOp {
        self.0.host_op()
    }

    fn guarded_direct(&self) -> Option<crate::HostValueFn> {
        self.0.direct()
    }

    fn invoke(&self, heap: &mut crate::Heap, args: &[Value]) -> Result<Option<Value>, FfiError> {
        if let Some(&stream) = args.first()
            && crate::io::is_unsent_stream(heap, stream)
        {
            return Err(FfiError::Panic(format!(
                "{}: {}",
                self.0.name(),
                crate::io::UNSENT_STREAM_PANIC
            )));
        }
        self.0.invoke(heap, args)
    }
}

pub use common::{
    CLOCK_MONO_NANOS_NATIVE, CLOCK_SLEEP_MS_NATIVE, CLOCK_WALL_NANOS_NATIVE, STREAM_ATTACH_NATIVE,
    STREAM_FD_NATIVE, STREAM_PARK_NATIVE,
};

fn push_stream_attach(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    use crate::io::as_result_value;
    let sig = FfiSignature::from_parts(
        STREAM_ATTACH_NATIVE.to_string(),
        vec![FfiType::Int; 6],
        FfiType::Int,
    )
    .expect("stream_attach signature");
    let id = out.len();
    register_id(STREAM_ATTACH_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        use crate::stream_attach::{typed_fn_from_hashed_dload, StreamVTable};
        let r = (|| {
            let vtable = StreamVTable {
                read: typed_fn_from_hashed_dload(args[2].as_int())?,
                write: typed_fn_from_hashed_dload(args[3].as_int())?,
                shutdown: typed_fn_from_hashed_dload(args[4].as_int())?,
                free: typed_fn_from_hashed_dload(args[5].as_int())?,
            };
            crate::stream_attach::stream_attach(heap, args[0], args[1].as_int(), vtable)
        })();
        Ok(Some(as_result_value(heap, r)))
    })));
}

fn push_result_unit_probe(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    use crate::io::{as_result_unit, IoErrorTag};
    let sig = FfiSignature::from_parts(
        common::RESULT_UNIT_PROBE_NATIVE.to_string(),
        vec![FfiType::Int],
        FfiType::Int,
    )
    .expect("result_unit_probe signature");
    let id = out.len();
    register_id(common::RESULT_UNIT_PROBE_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        let n = args.first().map(|v| v.as_int()).unwrap_or(0);
        let r = if n < 0 {
            Err(IoErrorTag::InvalidInput)
        } else {
            Ok(())
        };
        Ok(Some(as_result_unit(heap, r)))
    })));
}

fn push_simd_axpy_reduce(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    let sig = FfiSignature::from_parts(
        common::SIMD_AXPY_REDUCE_NATIVE.to_string(),
        vec![
            FfiType::Int,
            FfiType::Float,
            FfiType::Float,
            FfiType::Float,
            FfiType::Float,
        ],
        FfiType::Float,
    )
    .expect("simd_axpy_reduce signature");
    let id = out.len();
    register_id(common::SIMD_AXPY_REDUCE_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |_heap, args| {
        if args.len() < 5 {
            return Ok(Some(Value::from(0.0)));
        }
        let n = args[0].as_int();
        if n <= 0 {
            return Ok(Some(Value::from(0.0)));
        }
        Ok(Some(Value::from(coil_simd::axpy_reduce_f64(
            n as usize,
            args[1].as_float(),
            args[2].as_float(),
            args[3].as_float(),
            args[4].as_float(),
        ))))
    })));
}

/// `thread_spawn_shared` running `spawn`.
fn spawn_shared_native(spawn: impl Fn(&mut crate::Heap, &[Value]) -> Value + Send + Sync + 'static) -> Arc<dyn NativeFn> {
    let sig = FfiSignature::from_parts(
        common::THREAD_SPAWN_SHARED_NATIVE.to_string(),
        vec![FfiType::Int],
        FfiType::Int,
    )
    .expect("thread_spawn_shared signature");
    Arc::new(HostClosureFn::new_with_arity_range(sig, 1, 1 + common::MAX_THREAD_SPAWN_ARGS, move |heap: &mut crate::Heap, args: &[Value]| {
        Ok(Some(spawn(heap, args)))
    }))
}

/// `thread_spawn_shared` where there are no threads to use: every spawn
/// fails, and auto-par's fork runs its range on this thread, as it does at
/// any failed spawn.
pub fn refused_spawn_shared() -> Arc<dyn NativeFn> {
    use crate::thread::{ErrorSet, ThreadErrorTag, as_result_value};
    spawn_shared_native(|heap, _| as_result_value(heap, ErrorSet::Thread, Err(ThreadErrorTag::Other)))
}

fn push_thread_spawn_shared(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    let id = out.len();
    register_id(common::THREAD_SPAWN_SHARED_NATIVE, id);
    out.push(spawn_shared_native(crate::thread::thread_spawn_shared));
}

fn push_stream_park(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    use crate::io::as_result_unit;
    let sig = FfiSignature::from_parts(
        STREAM_PARK_NATIVE.to_string(),
        vec![FfiType::Int],
        FfiType::Int,
    )
    .expect("stream_park signature");
    let id = out.len();
    register_id(STREAM_PARK_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        if crate::task::tasks_active() {
            // Under a task scheduler: suspend this task, not the thread.
            return match crate::stream_attach::stream_park_request(heap, args[0]) {
                Ok(req) => {
                    crate::io::request_io_park(req);
                    Ok(None)
                }
                Err(e) => Ok(Some(as_result_unit(heap, Err(e)))),
            };
        }
        let r = crate::stream_attach::stream_park(heap, args[0]);
        Ok(Some(as_result_unit(heap, r)))
    })));
}

fn push_stream_fd(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    use crate::io::{as_result_value, IoErrorTag};
    let sig = FfiSignature::from_parts(
        STREAM_FD_NATIVE.to_string(),
        vec![FfiType::Int],
        FfiType::Int,
    )
    .expect("stream_fd signature");
    let id = out.len();
    register_id(STREAM_FD_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        let r = match crate::io::stream_fd_i64(heap, args[0]) {
            Some(fd) => Ok(Value::from(fd)),
            None => Err(IoErrorTag::InvalidInput),
        };
        Ok(Some(as_result_value(heap, r)))
    })));
}

fn push_string_bytes(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    use crate::str_bytes::{
        string_byte_at, string_find_from, string_match_at, string_rfind, string_slice_bytes,
    };
    let specs: &[(&str, usize, crate::HostValueFn)] = &[
        ("string_byte_at", 2, string_byte_at),
        ("string_slice_bytes", 3, string_slice_bytes),
        ("string_find_from", 3, string_find_from),
        ("string_rfind", 2, string_rfind),
        ("string_match_at", 3, string_match_at),
    ];
    for &(name, arity, host) in specs {
        let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int; arity], FfiType::Int)
            .expect("string byte native signature");
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new_direct(sig, host)));
    }
}

/// `black_box(x) -> x`: the identity, as a call the optimizer cannot see
/// through.
/// `from_bytes_take(buf) -> Result<string, StringError>`: `from_bytes`
/// that moves the bytes out of a buffer nothing reads again.
fn push_from_bytes_take(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::FROM_BYTES_TAKE_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int], FfiType::Int).expect("from_bytes_take signature");
    let id = out.len();
    register_id(name, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        let r = crate::io::from_bytes_take(heap, args[0]);
        Ok(Some(crate::str_bytes::as_string_result_value(heap, r)))
    })));
}

/// `gc_region_enter` / `gc_region_exit`: runtime hooks ([`HostOp::RegionEnter`]).
fn push_region(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    for (name, op) in [
        (common::GC_REGION_ENTER_NATIVE, HostOp::RegionEnter),
        (common::GC_REGION_EXIT_NATIVE, HostOp::RegionExit),
    ] {
        let sig = FfiSignature::from_parts(name.to_string(), vec![], FfiType::Int).expect("region signature");
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(
            HostClosureFn::new(sig, move |_heap, _args| Err(FfiError::Unsupported(format!("{name} is a runtime hook only"))))
                .with_host_op(op),
        ));
    }
}

fn push_black_box(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::BLACK_BOX_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int], FfiType::Int).expect("black_box signature");
    let id = out.len();
    register_id(name, id);
    out.push(Arc::new(HostClosureFn::new_direct(sig, |_heap, args| args[0])));
}

/// `values_eq_shaped(a, b, shape) -> bool`: `==` on an aggregate whose
/// `float` leaves compare by IEEE ([`crate::value_eq::values_eq_shaped`]).
fn push_values_eq_shaped(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::VALUES_EQ_SHAPED_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int; 3], FfiType::Int)
        .expect("values_eq_shaped signature");
    let id = out.len();
    register_id(name, id);
    out.push(Arc::new(HostClosureFn::new_direct(sig, |heap, args| {
        let eq = match heap.find_object_by_addr(args[2].bits()) {
            Some(crate::memory::Object::String(gc)) => {
                crate::value_eq::values_eq_shaped(heap, args[0], args[1], gc.as_ref().data.as_bytes())
            }
            _ => crate::value_eq::values_eq(heap, args[0], args[1]),
        };
        Value::from(i64::from(eq))
    })));
}

/// `string_compare(a, b) -> int`: `-1`, `0` or `1` as `a` orders before,
/// equal to or after `b` by UTF-8 bytes (a prefix first), for `<` / `<=` /
/// `>` / `>=` on `string` (coil-stdlib#48).
fn push_string_compare(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::STRING_COMPARE_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int; 2], FfiType::Int)
        .expect("string_compare signature");
    let id = out.len();
    register_id(name, id);
    // Direct (native code calls it in place): the checker passes only
    // strings; anything else orders before every string.
    out.push(Arc::new(HostClosureFn::new_direct(sig, |heap, args| {
        let text = |v: Value| match heap.find_object_by_addr(crate::io::peel_one_boxed(heap, v).bits()) {
            Some(crate::memory::Object::String(gc)) => Some(gc),
            _ => None,
        };
        let order = match (text(args[0]), text(args[1])) {
            (Some(a), Some(b)) => a.as_ref().data.as_bytes().cmp(b.as_ref().data.as_bytes()),
            (a, b) => a.is_some().cmp(&b.is_some()),
        };
        Value::from(order as i64)
    })));
}

/// `ffi::read_ints(lib, ptr, count) -> Result<Vec<int>, ffi::Error>`.
fn push_ffi_read_ints(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::FFI_READ_INTS_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int; 3], FfiType::Int)
        .expect("ffi_read_ints signature");
    let id = out.len();
    register_id(name, id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        Ok(Some(crate::ffi::read_ints(heap, args)))
    })));
}

/// `task_*` natives and `contract_fail`: runtime hooks ([`HostOp::Task`]), like `gc_collect`.
fn push_task_natives(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
    ids: std::ops::RangeInclusive<u16>,
) {
    for id in ids {
        let row = &common::HOST_NATIVES[id as usize];
        let name = row.name;
        let sig = FfiSignature::from_parts(
            name.to_string(),
            vec![FfiType::Int; row.arity as usize],
            FfiType::Int,
        )
        .expect("task native signature");
        assert_eq!(out.len(), id as usize);
        register_id(name, out.len());
        out.push(Arc::new(
            HostClosureFn::new(sig, move |_heap, _args| {
                Err(FfiError::Unsupported(format!(
                    "{name} is HostOp::Task (runtime hook only)"
                )))
            })
            .with_host_op(HostOp::Task),
        ));
    }
}

/// `unwind_resume` ends a `defer` cleanup pad; the runtime runs it (`HostOp::Unwind`).
fn push_unwind_resume(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::UNWIND_RESUME_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![], FfiType::Int).expect("unwind_resume signature");
    assert_eq!(out.len(), common::UNWIND_RESUME_ID as usize);
    register_id(name, out.len());
    out.push(Arc::new(
        HostClosureFn::new(sig, move |_heap, _args| {
            Err(FfiError::Unsupported(format!("{name} is HostOp::Unwind (runtime hook only)")))
        })
        .with_host_op(HostOp::Unwind),
    ));
}

fn push_removed_time_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    for &(name, arity) in TIME_REMOVED {
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Int)
            .unwrap_or_else(|_| panic!("removed time stub signature `{name}`"));
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new(sig, move |_heap, _args| {
            panic!("removed HostInvoke `{name}` (virtual time is gone)");
        })));
    }
}

fn push_gc_host_ops(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let collect_sig =
        FfiSignature::from_parts(crate::GC_COLLECT_NATIVE.to_string(), vec![], FfiType::Int)
            .expect("gc_collect signature");
    let collect_id = out.len();
    register_id(crate::GC_COLLECT_NATIVE, collect_id);
    out.push(Arc::new(
        HostClosureFn::new(collect_sig, |_heap, _args| {
            Err(FfiError::Unsupported(
                "gc_collect is HostOp::Collect (runtime hook only)".into(),
            ))
        })
        .with_host_op(HostOp::Collect),
    ));

    let register_sig = FfiSignature::from_parts(
        crate::GC_REGISTER_FINALIZER_NATIVE.to_string(),
        vec![FfiType::Int, FfiType::Int],
        FfiType::Int,
    )
    .expect("gc_register_finalizer signature");
    let register_id_n = out.len();
    register_id(crate::GC_REGISTER_FINALIZER_NATIVE, register_id_n);
    out.push(Arc::new(
        HostClosureFn::new(register_sig, |_heap, _args| {
            Err(FfiError::Unsupported(
                "gc_register_finalizer is HostOp::RegisterFinalizer (runtime hook only)".into(),
            ))
        })
        .with_host_op(HostOp::RegisterFinalizer),
    ));
}

fn push_wiring(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
    table: &[(&str, usize, crate::HostValueFn)],
    label: &str,
) {
    for &(name, arity, host) in table {
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Int)
            .unwrap_or_else(|_| panic!("{label} native signature"));
        let id = out.len();
        register_id(name, id);
        if name == "vec_insert" {
            out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
                crate::vec_ops::host_vec_insert(heap, args)
                    .map(Some)
                    .map_err(FfiError::Panic)
            })));
            continue;
        }
        if name == "env_exit" {
            out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
                crate::env::try_host_exit(heap, args)
                    .map(Some)
                    .map_err(FfiError::Panic)
            })));
            continue;
        }
        if common::host_native_id(name) == Some(common::CLOCK_SLEEP_MS_ID as usize) {
            // Inside a scope the runtime suspends the task instead of calling it.
            out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| Ok(Some(host(heap, args))))));
            continue;
        }
        out.push(Arc::new(HostClosureFn::new_direct(sig, host)));
    }
}

/// The lock primitives the Coil `with_lock` family builds on. An acquire
/// may park the task (no result yet; the interpreter or the native driver runs it
/// again after the wake), so it is an ordinary closure; the rest are
/// direct.
fn push_lock_natives(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    for &(name, arity, host) in crate::thread::LOCK_WIRING {
        if !matches!(name, "lock_acquire" | "rw_acquire_read" | "rw_acquire_write") {
            push_wiring(out, register_id, &[(name, arity, host)], "lock");
            continue;
        }
        let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int; arity], FfiType::Int)
            .unwrap_or_else(|_| panic!("lock native signature"));
        register_id(name, out.len());
        out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
            let v = host(heap, args);
            Ok((!crate::task::thread_parked()).then_some(v))
        })));
    }
}

/// `entropy_bytes`: a negative count or a failed OS source is a Coil panic.
fn push_entropy(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let name = common::ENTROPY_BYTES_NATIVE;
    let sig = FfiSignature::from_parts(name.to_string(), vec![FfiType::Int], FfiType::Int)
        .unwrap_or_else(|_| panic!("entropy native signature"));
    register_id(name, out.len());
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        crate::entropy::host_bytes(heap, args)
            .map(Some)
            .map_err(FfiError::Panic)
    })));
}

fn push_math_libm(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    push_removed_math_stubs(out, register_id, MATH_LIBM_REMOVED);
}

/// Retired `prelude::math` slots: float-typed panic stubs so their ids and
/// every later id stay put.
fn push_removed_math_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
    removed: &'static [(&'static str, usize)],
) {
    for &(name, arity) in removed {
        let args = vec![FfiType::Float; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Float)
            .unwrap_or_else(|_| panic!("removed math stub signature `{name}`"));
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new(sig, move |_heap, _args| {
            panic!(
                "removed HostInvoke `{name}`: prelude::math moved to coil-stdlib's `math` module; recompile with `use math::{{…}};`"
            );
        })));
    }
}

fn push_prelude_char_ord(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    use crate::char_ord::{prelude_char, prelude_hash_string, prelude_ord};

    let ord_sig = FfiSignature::from_parts("ord".to_string(), vec![FfiType::Int], FfiType::Int)
        .expect("ord signature");
    let ord_id = out.len();
    register_id("ord", ord_id);
    out.push(Arc::new(HostClosureFn::new_direct(ord_sig, prelude_ord)));

    let char_sig = FfiSignature::from_parts("char".to_string(), vec![FfiType::Int], FfiType::Int)
        .expect("char signature");
    let char_id = out.len();
    register_id("char", char_id);
    out.push(Arc::new(HostClosureFn::new_direct(char_sig, prelude_char)));

    let hash_sig = FfiSignature::from_parts(
        "hash_string".to_string(),
        vec![FfiType::String],
        FfiType::Int,
    )
    .expect("hash_string signature");
    let hash_id = out.len();
    register_id("hash_string", hash_id);
    out.push(Arc::new(HostClosureFn::new_direct(hash_sig, prelude_hash_string)));
}

fn push_packed_la(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    type Kernel = fn(&mut crate::Heap, &[Value]) -> Result<Value, common::int_arith::IntTrap>;
    let specs: &[(&str, usize, Kernel)] = &[
        (PACKED_DOT, 3, packed_dot),
        (PACKED_MATMUL, 3, packed_matmul),
        (PACKED_MATRIX_ZIP, 3, packed_matrix_zip),
        (PACKED_MATRIX_NEG, 2, packed_matrix_neg),
    ];
    for &(name, arity, kernel) in specs {
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Int)
            .expect("packed LA native signature");
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
            kernel(heap, args).map(Some).map_err(FfiError::IntTrap)
        })));
    }

    // Zip/broadcast use 3 args; unary neg uses 2 (vec + meta).
    let vec_sig = FfiSignature::from_parts(
        PACKED_VEC_ARITH.to_string(),
        vec![FfiType::Int; 3],
        FfiType::Int,
    )
    .expect("packed_vec_arith signature");
    let vec_id = out.len();
    register_id(PACKED_VEC_ARITH, vec_id);
    out.push(Arc::new(HostClosureFn::new_with_arity_range(
        vec_sig,
        2,
        3,
        |heap, args| packed_vec_arith(heap, args).map(Some).map_err(FfiError::IntTrap),
    )));
}

fn push_io_wait_ready(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    let sig = FfiSignature::from_parts("wait_ready".to_string(), vec![], FfiType::Int)
        .expect("wait_ready signature");
    let id = out.len();
    register_id("wait_ready", id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, _args| {
        Ok(Some(crate::io::io_wait_ready(heap)))
    })));
}

/// Write `buf[offset..]` without allocating a Coil suffix array.
fn push_io_write_from(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    use crate::io::{as_result_int, stream_write_from};
    let sig = FfiSignature::from_parts(
        "write_from".to_string(),
        vec![FfiType::Int, FfiType::Int, FfiType::Int],
        FfiType::Int,
    )
    .expect("write_from signature");
    let id = out.len();
    register_id("write_from", id);
    // Never parks (a full pipe answers `WouldBlock`): a direct native.
    out.push(Arc::new(HostClosureFn::new_direct(sig, |heap, args| {
        let r = stream_write_from(heap, args[0], args[1], args[2].as_int());
        as_result_int(heap, r)
    })));
}

#[derive(Clone, Copy)]
enum IoKind {
    Stdin,
    Stdout,
    Stderr,
    Open,
    Close,
    Read,
    Write,
    AwaitReadable,
    AwaitWritable,
    Drive,
    FromBytes,
    ToBytes,
    TcpConnect,
    TcpConnectTimeout,
    TcpListen,
    TcpAccept,
    TcpPeerAddr,
    TcpLocalAddr,
    TcpSetNodelay,
    TcpShutdown,
    UdpBind,
    UdpConnect,
    UdpSendTo,
    UdpRecvFrom,
    UdpLocalPort,
}

impl IoKind {
    fn all() -> &'static [IoKind] {
        &[
            Self::Stdin,
            Self::Stdout,
            Self::Stderr,
            Self::Open,
            Self::Close,
            Self::Read,
            Self::Write,
            Self::AwaitReadable,
            Self::AwaitWritable,
            Self::Drive,
            Self::FromBytes,
            Self::ToBytes,
            Self::TcpConnect,
            Self::TcpConnectTimeout,
            Self::TcpListen,
            Self::TcpAccept,
            Self::TcpPeerAddr,
            Self::TcpLocalAddr,
            Self::TcpSetNodelay,
            Self::TcpShutdown,
            Self::UdpBind,
            Self::UdpConnect,
            Self::UdpSendTo,
            Self::UdpRecvFrom,
            Self::UdpLocalPort,
        ]
    }

    fn native_name(self) -> &'static str {
        match self {
            Self::Stdin => "stdin",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Open => "open",
            Self::Close => "close",
            Self::Read => "read",
            Self::Write => "write",
            Self::AwaitReadable => "await_readable",
            Self::AwaitWritable => "await_writable",
            Self::Drive => "drive",
            Self::FromBytes => "from_bytes",
            Self::ToBytes => "to_bytes",
            Self::TcpConnect => "tcp_connect",
            Self::TcpConnectTimeout => "tcp_connect_timeout",
            Self::TcpListen => "tcp_listen",
            Self::TcpAccept => "tcp_accept",
            Self::TcpPeerAddr => "tcp_peer_addr",
            Self::TcpLocalAddr => "tcp_local_addr",
            Self::TcpSetNodelay => "tcp_set_nodelay",
            Self::TcpShutdown => "tcp_shutdown",
            Self::UdpBind => "udp_bind",
            Self::UdpConnect => "udp_connect",
            Self::UdpSendTo => "udp_send_to",
            Self::UdpRecvFrom => "udp_recv_from",
            Self::UdpLocalPort => "udp_local_port",
        }
    }

    fn arity(self) -> usize {
        match self {
            Self::Stdin | Self::Stdout | Self::Stderr | Self::Drive => 0,
            Self::Close
            | Self::AwaitReadable
            | Self::AwaitWritable
            | Self::FromBytes
            | Self::ToBytes
            | Self::TcpAccept
            | Self::TcpPeerAddr
            | Self::TcpLocalAddr
            | Self::UdpLocalPort => 1,
            Self::Open
            | Self::Read
            | Self::Write
            | Self::TcpConnect
            | Self::TcpListen
            | Self::TcpSetNodelay
            | Self::TcpShutdown
            | Self::UdpBind
            | Self::UdpConnect
            | Self::UdpRecvFrom => 2,
            Self::TcpConnectTimeout => 3,
            Self::UdpSendTo => 4,
        }
    }
}

/// The `io` native of `IoKind::all()[K]`: one group per kind, so an AOT
/// executable links only the kinds its program calls.
fn push_io_kind<const K: usize>(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize)) {
    push_io_native(out, register_id, IoKind::all()[K]);
}

fn io_closure<F>(sig: FfiSignature, f: F) -> Arc<dyn NativeFn>
where
    F: Fn(&mut crate::Heap, &[Value]) -> Result<Option<Value>, FfiError> + Send + Sync + 'static,
{
    Arc::new(HostClosureFn::new(sig, f))
}

/// The native of `kind`. Inlined into each [`push_io_kind`], where `kind` is
/// a constant, so each group keeps its own closure only.
#[inline(always)]
#[allow(unused_variables)]
fn push_io_native(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut dyn FnMut(&str, usize), kind: IoKind) {
    use crate::io::{
        as_result_int, as_result_option_int, as_result_unit, as_result_value, from_bytes, io_drive,
        stream_await_readable, stream_await_writable, stream_close, stream_open, stream_read,
        stream_stderr, stream_stdin, stream_stdout, stream_write, tcp_accept, tcp_connect,
        tcp_connect_timeout, tcp_listen, tcp_local_addr, tcp_peer_addr, tcp_set_nodelay,
        tcp_shutdown, to_bytes, udp_bind, udp_connect, udp_local_port, udp_recv_from, udp_send_to,
        value_as_open_mode, value_as_string,
    };

    let name = kind.native_name().to_string();
    let arity = kind.arity();
    let args = vec![FfiType::Int; arity];
    let sig = FfiSignature::from_parts(name.clone(), args, FfiType::Int)
        .expect("io native arity/signature");
    let id = out.len();
    register_id(&name, id);
    // The stdio handles are plain lookups: direct natives.
    let direct: Option<crate::HostValueFn> = match kind {
        IoKind::Stdin => Some(|heap, _| stream_stdin(heap).unwrap_or_default()),
        IoKind::Stdout => Some(|heap, _| stream_stdout(heap).unwrap_or_default()),
        IoKind::Stderr => Some(|heap, _| stream_stderr(heap).unwrap_or_default()),
        // Pure conversions: they only allocate.
        IoKind::ToBytes => Some(|heap, args| to_bytes(heap, args[0])),
        IoKind::FromBytes => Some(|heap, args| {
            let r = from_bytes(heap, args[0]);
            crate::str_bytes::as_string_result_value(heap, r)
        }),
        _ => None,
    };
    if let Some(f) = direct {
        out.push(Arc::new(HostClosureFn::new_direct(sig, f)));
        return;
    }
    let native = match kind {
        // Stdio handles are `() -> Stream` (not Result).
        IoKind::Stdin => io_closure(sig, move |heap, args| {
            let v = stream_stdin(heap).unwrap_or_default();
            Ok(Some(v))
        }),
        IoKind::Stdout => io_closure(sig, move |heap, args| {
            let v = stream_stdout(heap).unwrap_or_default();
            Ok(Some(v))
        }),
        IoKind::Stderr => io_closure(sig, move |heap, args| {
            let v = stream_stderr(heap).unwrap_or_default();
            Ok(Some(v))
        }),
        IoKind::Open => io_closure(sig, move |heap, args| {
            let v = {
                let path = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let mode = match value_as_open_mode(heap, args[1]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let r = stream_open(heap, &path, &mode);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::Close => io_closure(sig, move |heap, args| {
            let v = {
                let r = stream_close(heap, args[0]);
                as_result_unit(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::Read => io_closure(sig, move |heap, args| {
            let v = {
                let r = stream_read(heap, args[0], args[1]);
                as_result_option_int(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::Write => io_closure(sig, move |heap, args| {
            let v = {
                let r = stream_write(heap, args[0], args[1]);
                as_result_int(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::AwaitReadable => io_closure(sig, move |heap, args| {
            let v = match stream_await_readable(heap, args[0]) {
                Ok(v) => return Ok(v),
                Err(tag) => as_result_unit(heap, Err(tag)),
            };
            Ok(Some(v))
        }),
        IoKind::AwaitWritable => io_closure(sig, move |heap, args| {
            let v = match stream_await_writable(heap, args[0]) {
                Ok(v) => return Ok(v),
                Err(tag) => as_result_unit(heap, Err(tag)),
            };
            Ok(Some(v))
        }),
        IoKind::Drive => io_closure(sig, move |heap, args| Ok(Some(io_drive(heap)))),
        IoKind::FromBytes => io_closure(sig, move |heap, args| {
            let v = {
                let r = from_bytes(heap, args[0]);
                crate::str_bytes::as_string_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::ToBytes => io_closure(sig, move |heap, args| {
            let v = to_bytes(heap, args[0]);
            Ok(Some(v))
        }),
        IoKind::TcpConnect => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let (port, ms) = (args[1].as_int(), 0);
                let r = match crate::task::connect_in_task(&host, port, ms) {
                    Some(Some(r)) => r.and_then(|s| crate::io::alloc_tcp_stream(heap, s)),
                    Some(None) => return Ok(None),
                    None => tcp_connect(heap, &host, port),
                };
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpConnectTimeout => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let (port, ms) = (args[1].as_int(), args[2].as_int());
                let r = match crate::task::connect_in_task(&host, port, ms) {
                    Some(Some(r)) => r.and_then(|s| crate::io::alloc_tcp_stream(heap, s)),
                    Some(None) => return Ok(None),
                    None => tcp_connect_timeout(heap, &host, port, ms),
                };
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpListen => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let r = tcp_listen(heap, &host, args[1].as_int());
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpAccept => io_closure(sig, move |heap, args| {
            let v = {
                let r = tcp_accept(heap, args[0]);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpPeerAddr => io_closure(sig, move |heap, args| {
            let v = {
                let r = tcp_peer_addr(heap, args[0]);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpLocalAddr => io_closure(sig, move |heap, args| {
            let v = {
                let r = tcp_local_addr(heap, args[0]);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpSetNodelay => io_closure(sig, move |heap, args| {
            let v = {
                let r = tcp_set_nodelay(heap, args[0], args[1].as_bool());
                as_result_unit(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::TcpShutdown => io_closure(sig, move |heap, args| {
            let v = {
                let r = tcp_shutdown(heap, args[0], args[1].as_int());
                as_result_unit(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::UdpBind => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let r = udp_bind(heap, &host, args[1].as_int());
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::UdpConnect => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[0]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let r = udp_connect(heap, &host, args[1].as_int());
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::UdpSendTo => io_closure(sig, move |heap, args| {
            let v = {
                let host = match value_as_string(heap, args[2]) {
                    Ok(s) => s,
                    Err(tag) => {
                        return Ok(Some(as_result_value(heap, Err(tag))));
                    }
                };
                let r = udp_send_to(heap, args[0], args[1], &host, args[3].as_int());
                as_result_int(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::UdpRecvFrom => io_closure(sig, move |heap, args| {
            let v = {
                let r = udp_recv_from(heap, args[0], args[1]);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
        IoKind::UdpLocalPort => io_closure(sig, move |heap, args| {
            let v = {
                let r = udp_local_port(heap, args[0]).map(Value::from);
                as_result_value(heap, r)
            };
            Ok(Some(v))
        }),
    };
    out.push(native);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThreadKind {
    Spawn,
    Join,
    Detach,
    Channel,
    Send,
    Recv,
    TrySend,
    TryRecv,
    Close,
    Mutex,
    WithLock,
    Lock,
    TryLock,
    Unlock,
    Rwlock,
    WithRead,
    WithWrite,
    TryRead,
    TryWrite,
}

impl ThreadKind {
    fn all() -> &'static [ThreadKind] {
        &[
            Self::Spawn,
            Self::Join,
            Self::Detach,
            Self::Channel,
            Self::Send,
            Self::Recv,
            Self::TrySend,
            Self::TryRecv,
            Self::Close,
            Self::Mutex,
            Self::WithLock,
            Self::Lock,
            Self::TryLock,
            Self::Unlock,
            Self::Rwlock,
            Self::WithRead,
            Self::WithWrite,
            Self::TryRead,
            Self::TryWrite,
        ]
    }

    fn native_name(self) -> &'static str {
        match self {
            Self::Spawn => "thread_spawn",
            Self::Join => "thread_join",
            Self::Detach => "thread_detach",
            Self::Channel => "thread_channel",
            Self::Send => "thread_send",
            Self::Recv => "thread_recv",
            Self::TrySend => "thread_try_send",
            Self::TryRecv => "thread_try_recv",
            Self::Close => "thread_close",
            Self::Mutex => "thread_mutex",
            Self::WithLock => "thread_with_lock",
            Self::Lock => "thread_lock",
            Self::TryLock => "thread_try_lock",
            Self::Unlock => "thread_unlock",
            Self::Rwlock => "thread_rwlock",
            Self::WithRead => "thread_with_read",
            Self::WithWrite => "thread_with_write",
            Self::TryRead => "thread_try_read",
            Self::TryWrite => "thread_try_write",
        }
    }

    fn arity(self) -> usize {
        match self {
            Self::Channel => 0,
            Self::Spawn
            | Self::Recv
            | Self::TryRecv
            | Self::Close
            | Self::Mutex
            | Self::Rwlock
            | Self::Lock
            | Self::TryLock
            | Self::Unlock
            | Self::Join
            | Self::Detach => 1,
            Self::Send
            | Self::TrySend
            | Self::WithLock
            | Self::WithRead
            | Self::WithWrite
            | Self::TryRead
            | Self::TryWrite => 2,
        }
    }
}

fn push_thread_natives(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut dyn FnMut(&str, usize),
) {
    use crate::thread;

    for &kind in ThreadKind::all() {
        let name = kind.native_name().to_string();
        let arity = kind.arity();
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.clone(), args, FfiType::Int)
            .expect("thread native arity/signature");
        let id = out.len();
        register_id(&name, id);
        let closure = move |heap: &mut crate::Heap, args: &[Value]| {
            let v = match kind {
                ThreadKind::Spawn => thread::thread_spawn(heap, args),
                ThreadKind::Join => thread::thread_join(heap, args),
                ThreadKind::Detach => thread::thread_detach(heap, args),
                ThreadKind::Channel => thread::thread_channel(heap, args),
                ThreadKind::Send => thread::thread_send(heap, args),
                ThreadKind::Recv => thread::thread_recv(heap, args),
                ThreadKind::TrySend => thread::thread_try_send(heap, args),
                ThreadKind::TryRecv => thread::thread_try_recv(heap, args),
                ThreadKind::Close => thread::thread_close(heap, args),
                ThreadKind::Mutex => thread::thread_mutex(heap, args),
                ThreadKind::WithLock => thread::thread_with_lock(heap, args),
                ThreadKind::Lock => thread::thread_lock(heap, args),
                ThreadKind::TryLock => thread::thread_try_lock(heap, args),
                ThreadKind::Unlock => thread::thread_unlock(heap, args),
                ThreadKind::Rwlock => thread::thread_rwlock(heap, args),
                ThreadKind::WithRead => thread::thread_with_read(heap, args),
                ThreadKind::WithWrite => thread::thread_with_write(heap, args),
                ThreadKind::TryRead => thread::thread_try_read(heap, args),
                ThreadKind::TryWrite => thread::thread_try_write(heap, args),
            };
            if let Some(msg) = thread::take_pending_panic() {
                return Err(FfiError::Panic(msg));
            }
            if crate::task::thread_parked() {
                // Parked the task (`recv`, `join`, a lock): no result yet.
                return Ok(None);
            }
            Ok(Some(v))
        };
        let native: Arc<dyn NativeFn> = if kind == ThreadKind::Spawn {
            Arc::new(HostClosureFn::new_with_arity_range(
                sig,
                1,
                1 + common::MAX_THREAD_SPAWN_ARGS,
                closure,
            ))
        } else {
            Arc::new(HostClosureFn::new(sig, closure))
        };
        out.push(native);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_native_table_has_no_regex_entries() {
        let mut registrations = Vec::new();
        build_standard_host_natives(|name, id| {
            registrations.push((name.to_string(), id));
        });
        let regex: Vec<_> = registrations
            .iter()
            .filter(|(name, _)| name.starts_with("regex_"))
            .collect();
        assert!(
            regex.is_empty(),
            "regex HostInvoke slots must not be registered: {:?}",
            regex
        );
    }

    #[test]
    fn math_libm_natives_are_float_typed_and_appended_after_gc() {
        let mut registrations = Vec::new();
        let natives = build_standard_host_natives(|name, id| {
            registrations.push((name.to_string(), id));
        });

        let math_start = registrations
            .iter()
            .position(|(name, _)| name == "math_sin")
            .expect("math_sin registration");
        let gc_collect = registrations
            .iter()
            .position(|(name, _)| name == crate::GC_COLLECT_NATIVE)
            .expect("gc_collect registration");
        let gc_register = registrations
            .iter()
            .position(|(name, _)| name == crate::GC_REGISTER_FINALIZER_NATIVE)
            .expect("gc_register_finalizer registration");
        assert_eq!(gc_register, gc_collect + 1);
        assert_eq!(math_start, gc_register + 1);

        let expected = [
            "math_sin",
            "math_cos",
            "math_tan",
            "math_sqrt",
            "math_floor",
            "math_ceil",
            "math_exp",
            "math_ln",
            "math_pow",
        ];
        let math_end = math_start + expected.len();
        assert_eq!(
            registrations[math_start..math_end]
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        // Vec helpers are append-only after math_libm (stable HostInvoke ids).
        assert_eq!(registrations[math_end].0, "vec_with_capacity");

        for (offset, native) in natives[math_start..math_end].iter().enumerate() {
            let signature = native.signature();
            assert_eq!(signature.ret, FfiType::Float);
            let expected_arity = if offset + 1 == expected.len() { 2 } else { 1 };
            assert_eq!(signature.args, vec![FfiType::Float; expected_arity]);
            assert_eq!(registrations[math_start + offset].1, math_start + offset);
        }
    }

    #[test]
    fn write_from_is_registered_immediately_after_wait_ready() {
        let mut registrations = Vec::new();
        let natives = build_standard_host_natives(|name, id| {
            registrations.push((name.to_string(), id));
        });
        let wait = registrations
            .iter()
            .position(|(name, _)| name == "wait_ready")
            .expect("wait_ready");
        let write_from = registrations
            .iter()
            .position(|(name, _)| name == "write_from")
            .expect("write_from");
        assert_eq!(write_from, wait + 1);
        assert_eq!(registrations[write_from].1, write_from);
        let sig = natives[write_from].signature();
        assert_eq!(sig.args, vec![FfiType::Int, FfiType::Int, FfiType::Int]);
        assert_eq!(sig.ret, FfiType::Int);
        assert_eq!(registrations[write_from + 1].0, "gc_root");
        assert!(
            registrations
                .iter()
                .all(|(name, _)| name != "tls_alpn_protocol"
                    && name != "tls_client_enable"
                    && !name.starts_with("crypto_")),
            "leftover TLS and virtual crypto HostInvoke slots must be gone"
        );
    }

    /// Auto-par specializations spawn N-ary recursive calls; the host native
    /// The compiler emits `DenseHostCall` for every native
    /// `common::DIRECT_HOST_NATIVES` names, so each must have a direct
    /// function (else every call takes the cold generic fallback).
    #[test]
    fn every_compiler_direct_native_has_a_direct_function() {
        let natives = build_standard_host_natives(|_, _| {});
        for name in common::DIRECT_HOST_NATIVES {
            let id = common::host_native_id(name).unwrap_or_else(|| panic!("`{name}` is not a standard native"));
            assert!(common::host_native_is_direct(id), "`{name}`");
            assert!(natives[id].direct().is_some(), "`{name}` (id {id}) has no direct function");
        }
        assert!(natives[common::CLOCK_SLEEP_MS_ID as usize].direct().is_none(), "sleep suspends a task");
    }

    /// must accept 1..=1+MAX args and still reject garbage beyond that.
    #[test]
    fn thread_spawn_arity_range_covers_max_thread_spawn_args() {
        let natives = build_standard_host_natives(|_, _| {});
        let spawn = natives
            .iter()
            .find(|n| n.name() == "thread_spawn")
            .expect("thread_spawn");
        let mut heap = crate::Heap::default();
        let max = 1 + common::MAX_THREAD_SPAWN_ARGS;

        let too_few = spawn.invoke(&mut heap, &[]);
        assert!(
            matches!(too_few, Err(crate::ffi::FfiError::ArityMismatch { .. })),
            "zero args must fail arity: {too_few:?}"
        );

        let too_many: Vec<Value> = (0..=max).map(|i| Value::from(i as i64)).collect();
        let over = spawn.invoke(&mut heap, &too_many);
        assert!(
            matches!(over, Err(crate::ffi::FfiError::ArityMismatch { .. })),
            "above MAX_THREAD_SPAWN_ARGS must fail arity: {over:?}"
        );

        // Arity at the upper bound is accepted; the spawn itself fails because
        // the first arg is not a callable — that still proves the range gate.
        let at_max: Vec<Value> = (0..max).map(|i| Value::from(i as i64)).collect();
        let at = spawn.invoke(&mut heap, &at_max);
        assert!(
            !matches!(at, Err(crate::ffi::FfiError::ArityMismatch { .. })),
            "1+MAX args must pass the arity gate: {at:?}"
        );
    }

    #[test]
    fn stream_attach_follows_vec_helpers() {
        let mut names = Vec::new();
        build_standard_host_natives(|name, _id| names.push(name.to_string()));
        let attach = names
            .iter()
            .position(|n| n == STREAM_ATTACH_NATIVE)
            .expect("stream_attach");
        assert_eq!(
            names.get(attach.wrapping_sub(1)).map(String::as_str),
            Some("vec_from_array")
        );
        assert_eq!(
            names.get(attach + 1).map(String::as_str),
            Some(STREAM_PARK_NATIVE)
        );
        assert_eq!(
            names.get(attach + 2).map(String::as_str),
            Some(CLOCK_WALL_NANOS_NATIVE)
        );
        assert_eq!(
            names.get(attach + 3).map(String::as_str),
            Some(CLOCK_MONO_NANOS_NATIVE)
        );
        assert_eq!(
            names.get(attach + 4).map(String::as_str),
            Some(CLOCK_SLEEP_MS_NATIVE)
        );
        assert_eq!(
            names.get(attach + 5).map(String::as_str),
            Some(common::RESULT_UNIT_PROBE_NATIVE)
        );
        assert_eq!(
            names.get(160).map(String::as_str),
            Some(common::FFI_READ_INTS_NATIVE)
        );
        assert_eq!(
            names.get(161).map(String::as_str),
            Some(common::VALUES_EQ_SHAPED_NATIVE)
        );
        assert_eq!(
            names.get(162).map(String::as_str),
            Some(common::STRING_COMPARE_NATIVE)
        );
        assert_eq!(
            names.get(163).map(String::as_str),
            Some(common::BLACK_BOX_NATIVE)
        );
        assert_eq!(
            names.get(164).map(String::as_str),
            Some(common::IO_CAPTURE_BEGIN_NATIVE)
        );
        assert_eq!(
            names.get(165).map(String::as_str),
            Some(common::IO_CAPTURE_END_NATIVE)
        );
        assert_eq!(
            names.get(166).map(String::as_str),
            Some(common::TTY_IS_TTY_NATIVE)
        );
        assert_eq!(
            names.get(173).map(String::as_str),
            Some(common::TTY_ENABLE_VT_NATIVE)
        );
        assert_eq!(
            names.get(175).map(String::as_str),
            Some(common::VEC_FILL_NATIVE)
        );
        assert_eq!(
            names.get(176).map(String::as_str),
            Some(common::FROM_BYTES_TAKE_NATIVE)
        );
        assert_eq!(
            names.get(178).map(String::as_str),
            Some(common::GC_REGION_EXIT_NATIVE)
        );
        assert_eq!(
            names.get(179).map(String::as_str),
            Some(common::SYMBOL_NEXT_ID_NATIVE)
        );
        assert_eq!(
            names.get(180).map(String::as_str),
            Some(common::PROCESS_SPAWN_NATIVE)
        );
        assert_eq!(
            names.get(188).map(String::as_str),
            Some(common::PROCESS_RELEASE_NATIVE)
        );
        assert_eq!(
            names.get(189).map(String::as_str),
            Some(common::CORO_CLOSE_NATIVE)
        );
        assert_eq!(
            names.get(190).map(String::as_str),
            Some(common::ENTROPY_BYTES_NATIVE)
        );
        assert_eq!(
            names.get(191).map(String::as_str),
            Some(common::KEY_INDEX_TABLE_NATIVE)
        );
        assert_eq!(
            names.get(197).map(String::as_str),
            Some(common::KEY_INDEX_CLEAR_NATIVE)
        );
        assert_eq!(
            names.get(199).map(String::as_str),
            Some(common::KEY_INDEX_EQ_MODE_NATIVE)
        );
        assert_eq!(names.get(200).map(String::as_str), Some("lock_acquire"));
        assert_eq!(names.last().map(String::as_str), Some("rw_store"));
        assert_eq!(attach, 119);
    }

    /// COI-232: attach/park are live package-IO natives, not leftover TLS/crypto stubs.
    #[test]
    fn stream_attach_and_park_own_hostinvoke_119_and_120() {
        let mut map = std::collections::HashMap::new();
        build_standard_host_natives(|name, id| {
            map.insert(name.to_string(), id);
        });
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(119));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(120));
        assert!(!map.contains_key("tls_client_enable"));
        assert!(!map.contains_key("crypto_sha256"));
    }

    #[test]
    fn leftover_tls_and_crypto_hostinvoke_slots_are_gone() {
        let mut map = std::collections::HashMap::new();
        let mut names = Vec::new();
        build_standard_host_natives(|name, id| {
            map.insert(name.to_string(), id);
            names.push(name.to_string());
        });
        for gone in [
            "tls_client_enable",
            "tls_client_disable",
            "tls_server_enable",
            "tls_server_disable",
            "tls_alpn_protocol",
            "crypto_sha256",
        ] {
            assert!(
                !map.contains_key(gone),
                "leftover HostInvoke `{gone}` must be dropped"
            );
        }
        let attach = names
            .iter()
            .position(|n| n == STREAM_ATTACH_NATIVE)
            .expect("stream_attach");
        assert_eq!(names[attach + 1], STREAM_PARK_NATIVE);
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(attach));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(attach + 1));
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(119));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(120));
        assert_eq!(attach, 119);
        // IO block ends at udp_local_port; leftover TLS 25–28 used to follow it.
        let udp = names
            .iter()
            .position(|n| n == "udp_local_port")
            .expect("udp_local_port");
        assert_eq!(names[udp + 1], "fs_exists");
        assert_eq!(udp, 24);
    }

    #[test]
    fn removed_time_hostinvoke_stubs_keep_names_and_attach_ids() {
        let mut map = std::collections::HashMap::new();
        let mut names = Vec::new();
        let natives = build_standard_host_natives(|name, id| {
            map.insert(name.to_string(), id);
            names.push(name.to_string());
        });
        let expected: Vec<&str> = TIME_REMOVED.iter().map(|&(n, _)| n).collect();
        assert_eq!(expected.len(), 16);
        let fs_end = names
            .iter()
            .position(|n| n == "fs_realpath")
            .expect("fs_realpath");
        let env_start = names
            .iter()
            .position(|n| n == "env_args")
            .expect("env_args");
        assert_eq!(env_start, fs_end + 1 + TIME_REMOVED.len());
        assert_eq!(
            names[fs_end + 1..env_start]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected
        );
        for &(name, arity) in TIME_REMOVED {
            let id = *map.get(name).expect(name);
            assert_eq!(natives[id].signature().args.len(), arity, "{name}");
            assert_eq!(natives[id].signature().ret, FfiType::Int, "{name}");
        }
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(119));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(120));
    }

    /// COI-260: virtual time sources stay gone (no `time.rs`, chrono, TIME_WIRING table).
    #[test]
    fn virtual_time_machine_sources_are_absent() {
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            !crate_dir.join("src/time.rs").exists(),
            "machine/src/time.rs must stay deleted"
        );
        let lib = include_str!("lib.rs");
        assert!(
            !lib.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with("mod time")
                    || t.starts_with("pub mod time")
                    || t.contains("feature = \"time\"")
            }),
            "machine/src/lib.rs must not declare a time module"
        );
        let natives = include_str!("host_natives.rs");
        assert!(
            !natives.lines().any(|l| {
                let t = l.trim_start();
                t.starts_with("const TIME_WIRING") || t.starts_with("pub const TIME_WIRING")
            }),
            "TIME_WIRING table must not return"
        );
        let cargo = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(
            !cargo.contains("chrono"),
            "machine must not depend on chrono"
        );
        assert!(
            !cargo.contains("time = "),
            "machine must not declare a time cargo feature"
        );
    }

    /// COI-257/260: leftover `time_*` slots panic; they must not sleep or clock.
    #[test]
    fn removed_time_stubs_panic_and_do_not_run_real_time() {
        let natives = build_standard_host_natives(|_, _| {});
        let mut heap = crate::Heap::default();
        for &(name, arity) in TIME_REMOVED {
            let native = natives.iter().find(|n| n.name() == name).expect(name);
            let args: Vec<Value> = (0..arity).map(|i| Value::from(i as i64)).collect();
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                native.invoke(&mut heap, &args)
            }));
            let payload = panicked.expect_err(&format!("{name} must panic, not run"));
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_default();
            assert!(
                msg.contains("virtual time is gone") && msg.contains(name),
                "{name} stub must name the removed host, got {msg:?}"
            );
        }
    }

    /// Instant handles were a runtime HashMap leak; drop lives in coil-time, not HostInvoke.
    #[test]
    fn instant_drop_is_not_a_vm_host() {
        let mut names = Vec::new();
        build_standard_host_natives(|name, _id| names.push(name.to_string()));
        assert!(
            names
                .iter()
                .all(|n| n != "time_instant_drop" && n != "instant_drop"),
            "Instant drop must not be a host: {names:?}"
        );
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = crate_dir.join("src");
        let mut leftover = Vec::new();
        for rel in ["lib.rs", "host_natives.rs", "runtime.rs", "memory/heap.rs"] {
            let text = std::fs::read_to_string(src.join(rel)).unwrap_or_default();
            let prod = text.split("#[cfg(test)]").next().unwrap_or(&text);
            if prod.contains("NEXT_INSTANT_ID") || prod.contains("static INSTANTS") {
                leftover.push(rel);
            }
        }
        assert!(
            leftover.is_empty(),
            "Instant registry must stay gone: {leftover:?}"
        );
    }

    #[test]
    fn compiler_catalog_matches_runtime_table() {
        let mut runtime = Vec::new();
        let natives = build_standard_host_natives(|name, id| {
            runtime.push((name.to_string(), id));
        });
        let catalog: Vec<(String, usize)> = common::host_native_ids()
            .map(|(n, id)| (n.to_string(), id))
            .collect();
        assert_eq!(
            runtime, catalog,
            "compiler ids must equal runtime table ids"
        );
        assert_eq!(natives.len(), common::HOST_NATIVES.len());
        for (native, entry) in natives.iter().zip(common::HOST_NATIVES.iter()) {
            assert_eq!(native.name(), entry.name);
            assert_eq!(native.signature().arity(), entry.arity as usize);
        }
        assert_eq!(common::host_native_id(STREAM_ATTACH_NATIVE), Some(119));
        assert_eq!(common::host_native_id(STREAM_PARK_NATIVE), Some(120));
        assert_eq!(common::host_native_id(CLOCK_WALL_NANOS_NATIVE), Some(121));
        assert_eq!(common::host_native_id(CLOCK_MONO_NANOS_NATIVE), Some(122));
        assert_eq!(common::host_native_id(CLOCK_SLEEP_MS_NATIVE), Some(123));
        assert_eq!(
            common::host_native_id(common::RESULT_UNIT_PROBE_NATIVE),
            Some(124)
        );
        assert_eq!(common::host_native_id("math_atan"), Some(125));
        assert_eq!(common::host_native_id("math_tanh"), Some(135));
        assert_eq!(
            common::host_native_id(common::SIMD_AXPY_REDUCE_NATIVE),
            Some(136)
        );
        assert_eq!(
            common::host_native_id(common::THREAD_SPAWN_SHARED_NATIVE),
            Some(137)
        );
        assert_eq!(common::host_native_id(common::STREAM_FD_NATIVE), Some(138));
    }

    #[test]
    fn math_libm_m1_natives_append_after_result_unit_probe() {
        let mut registrations = Vec::new();
        let natives = build_standard_host_natives(|name, id| {
            registrations.push((name.to_string(), id));
        });

        let probe = registrations
            .iter()
            .position(|(name, _)| name == common::RESULT_UNIT_PROBE_NATIVE)
            .expect("result_unit_probe");
        assert_eq!(probe, 124);

        let expected = [
            "math_atan",
            "math_atan2",
            "math_asin",
            "math_acos",
            "math_log10",
            "math_log2",
            "math_cbrt",
            "math_rem",
            "math_sinh",
            "math_cosh",
            "math_tanh",
        ];
        let start = probe + 1;
        let end = start + expected.len();
        assert_eq!(
            registrations[start..end]
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        for (offset, native) in natives[start..end].iter().enumerate() {
            let signature = native.signature();
            assert_eq!(signature.ret, FfiType::Float);
            let expected_arity =
                if expected[offset] == "math_atan2" || expected[offset] == "math_rem" {
                    2
                } else {
                    1
                };
            assert_eq!(signature.args, vec![FfiType::Float; expected_arity]);
            assert_eq!(registrations[start + offset].1, start + offset);
        }
        assert_eq!(
            registrations.get(end).map(|(n, _)| n.as_str()),
            Some(common::SIMD_AXPY_REDUCE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 1).map(|(n, _)| n.as_str()),
            Some(common::THREAD_SPAWN_SHARED_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 2).map(|(n, _)| n.as_str()),
            Some(common::STREAM_FD_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 3).map(|(n, _)| n.as_str()),
            Some("string_byte_at")
        );
        assert_eq!(
            registrations.get(end + 8).map(|(n, _)| n.as_str()),
            Some("task_scope_open")
        );
        assert_eq!(
            registrations.get(end + 16).map(|(n, _)| n.as_str()),
            Some(common::UNWIND_RESUME_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 19).map(|(n, _)| n.as_str()),
            Some("task_shield_exit")
        );
        assert_eq!(
            registrations.get(end + 22).map(|(n, _)| n.as_str()),
            Some("task_cond_notify")
        );
        assert_eq!(
            registrations.get(end + 23).map(|(n, _)| n.as_str()),
            Some(common::CONTRACT_FAIL_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 24).map(|(n, _)| n.as_str()),
            Some(common::FFI_READ_INTS_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 25).map(|(n, _)| n.as_str()),
            Some(common::VALUES_EQ_SHAPED_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 26).map(|(n, _)| n.as_str()),
            Some(common::STRING_COMPARE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 27).map(|(n, _)| n.as_str()),
            Some(common::BLACK_BOX_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 29).map(|(n, _)| n.as_str()),
            Some(common::IO_CAPTURE_END_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 30).map(|(n, _)| n.as_str()),
            Some(common::TTY_IS_TTY_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 37).map(|(n, _)| n.as_str()),
            Some(common::TTY_ENABLE_VT_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 38).map(|(n, _)| n.as_str()),
            Some(common::VEC_EXTEND_RANGE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 39).map(|(n, _)| n.as_str()),
            Some(common::VEC_FILL_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 40).map(|(n, _)| n.as_str()),
            Some(common::FROM_BYTES_TAKE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 41).map(|(n, _)| n.as_str()),
            Some(common::GC_REGION_ENTER_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 42).map(|(n, _)| n.as_str()),
            Some(common::GC_REGION_EXIT_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 43).map(|(n, _)| n.as_str()),
            Some(common::SYMBOL_NEXT_ID_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 44).map(|(n, _)| n.as_str()),
            Some(common::PROCESS_SPAWN_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 52).map(|(n, _)| n.as_str()),
            Some(common::PROCESS_RELEASE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 53).map(|(n, _)| n.as_str()),
            Some(common::CORO_CLOSE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 54).map(|(n, _)| n.as_str()),
            Some(common::ENTROPY_BYTES_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 61).map(|(n, _)| n.as_str()),
            Some(common::KEY_INDEX_CLEAR_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 63).map(|(n, _)| n.as_str()),
            Some(common::KEY_INDEX_EQ_MODE_NATIVE)
        );
        assert_eq!(
            registrations.get(end + 75).map(|(n, _)| n.as_str()),
            Some("rw_store")
        );
        assert_eq!(registrations.len(), end + 76);
    }

    #[test]
    fn host_groups_cover_every_id_once() {
        let mut next = 0;
        for &(first, count) in host_group_spans() {
            assert_eq!(first, next, "groups follow each other");
            assert!(count > 0);
            next = first + count;
        }
        assert_eq!(next as usize, common::HOST_NATIVES.len());
        for id in 0..common::HOST_NATIVES.len() {
            let g = host_group_of(id).expect("every id has a group");
            let (first, count) = host_group_spans()[g];
            assert!((first..first + count).contains(&(id as u32)));
        }
    }

    #[test]
    fn each_io_kind_is_its_own_group() {
        for kind in IoKind::all() {
            let id = common::host_native_id(kind.native_name()).expect("io native id");
            let g = host_group_of(id).expect("io group");
            assert_eq!(host_group_spans()[g], (id as u32, 1), "{}", kind.native_name());
        }
    }

    #[test]
    fn a_table_of_some_groups_holds_their_natives_and_absent_slots_elsewhere() {
        let standard = build_standard_host_natives(|_, _| {});
        let write = common::host_native_id("write").expect("write");
        let keep = [host_group_of(write).expect("write group"), host_group_of(common::TASK_SCOPE_OPEN_ID as usize).expect("task group")];
        let groups: Vec<(HostPush, u32, u32)> = keep.iter().map(|&g| (STANDARD_HOST_GROUPS[g], host_group_spans()[g].0, host_group_spans()[g].1)).collect();
        let table = host_natives_of_groups(&groups);
        assert_eq!(table.len(), standard.len());
        for (id, native) in table.iter().enumerate() {
            let kept = host_group_of(id).is_some_and(|g| keep.contains(&g));
            assert_eq!(!native.is_absent(), kept, "id {id}");
            if kept {
                assert_eq!(native.name(), standard[id].name());
            }
        }
        // The stream guard wraps a kept stream native as in the full table.
        assert!(crate::ffi::HostCall::of(table[write].as_ref()).guarded);
        let mut natives = crate::ffi::Natives::new();
        for n in table {
            natives.register(n);
        }
        assert!(natives.get("write").is_some());
        assert!(natives.get("tcp_connect").is_none(), "an absent native has no name");
        assert!(natives.host_call(common::host_native_id("tcp_connect").expect("tcp")).is_some_and(|c| c.func.is_none() && !c.on_machine));
    }
}
