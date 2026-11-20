//! Append-only HostInvoke catalog shared by compiler and runtime.
//!
//! Ids are the table index. New natives go at the end. Do not reorder or
//! reuse slots. Frozen: 119 = `stream_attach`, 120 = `stream_park`.
//! Append-only after that: 121–123 = `clock_*`, 124 = `result_unit_probe`.

use crate::{Caps, EffectFlags};

/// One standard host native: stable name, declared arity, HostInvoke id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostNative {
    pub name: &'static str,
    /// Declared arity (signature length). `thread_spawn` and `packed_vec_arith`
    /// also accept a range at runtime; the id still keys off this row.
    pub arity: u8,
    pub id: u16,
    /// What a call may do (purity, auto-par, LICM). Every row states it, so a
    /// new native cannot be added without deciding its effects.
    pub effects: EffectFlags,
    /// What a program must be granted to call it (`--allow-read`, …).
    /// `open` is refined by its mode argument at the call site
    /// ([`crate::open_mode_caps`]). Every row states it, so a new native
    /// cannot ship ungated by accident.
    pub caps: Caps,
}

const fn fx(bits: u16) -> EffectFlags {
    EffectFlags::from_bits(bits)
}

const R: u16 = EffectFlags::RESIZE;
const PURE: EffectFlags = EffectFlags::empty();
const READ: EffectFlags = fx(EffectFlags::READ | R);
const WRITE: EffectFlags = fx(EffectFlags::WRITE);
const WRITE_R: EffectFlags = fx(EffectFlags::WRITE | R);
const READ_WRITE: EffectFlags = fx(EffectFlags::READ | EffectFlags::WRITE | R);
const NET: EffectFlags = fx(EffectFlags::NET | R);
const SUSPEND: EffectFlags = fx(EffectFlags::SUSPEND | R);
const ENV: EffectFlags = fx(EffectFlags::ENV | R);
const EXEC: EffectFlags = fx(EffectFlags::EXEC | R);
const HOST: EffectFlags = fx(EffectFlags::HOST);
const HOST_R: EffectFlags = fx(EffectFlags::HOST | R);
/// Reads nondeterministic host state into a fresh object (`entropy::bytes`).
const HOST_ALLOC: EffectFlags = fx(EffectFlags::HOST | EffectFlags::ALLOC);
const THREAD_R: EffectFlags = fx(EffectFlags::THREAD | R);
const GC_R: EffectFlags = fx(EffectFlags::GC | R);
const HEAP_R: EffectFlags = fx(EffectFlags::HEAP_MUT | R);
const PARK_R: EffectFlags = fx(EffectFlags::ATTACH_PARK | R);
/// Byte/string conversions and searches. Kept impure: `to_bytes` returns a
/// fresh mutable array each call, so MIR LICM must not hoist it out of a
/// loop (every iteration would then share one array).
const TEXT: EffectFlags = fx(EffectFlags::ALLOC);
const TEXT_R: EffectFlags = fx(EffectFlags::ALLOC | R);
const FFI_R: EffectFlags = fx(EffectFlags::FFI | R);

const NO_CAP: Caps = Caps::NONE;
const OPEN: Caps = Caps::READ.union(Caps::WRITE);
const READ_FS: Caps = Caps::READ;
const WRITE_FS: Caps = Caps::WRITE;
const COPY_FS: Caps = Caps::READ.union(Caps::WRITE);
const NET_CAP: Caps = Caps::NET;
const ENV_CAP: Caps = Caps::ENV;
const EXEC_CAP: Caps = Caps::EXEC;
const EXIT_CAP: Caps = Caps::EXIT;
const ATTACH_CAP: Caps = Caps::ATTACH;

/// The HostInvoke catalog, one row per native:
///
/// ```text
/// id "name" (arity) effects, caps [=> ID_CONST[, NATIVE_CONST]];
/// ```
///
/// Emits [`HOST_NATIVES`] in row order, plus `pub const ID_CONST: u16 = id`
/// and `pub const NATIVE_CONST: &str = "name"` for a row that names them
/// (`_` for no id const). The row's doc comments go on its consts. Ids are
/// literal and frozen: a row's id is its index, checked at compile time.
macro_rules! host_natives {
    ($( $(#[$doc:meta])* $id:literal $name:literal ($arity:literal) $fx:expr, $caps:expr $(=> $idc:tt $(, $nc:ident)?)? ; )*) => {
        /// Standard host natives in HostInvoke id order.
        pub const HOST_NATIVES: &[HostNative] = &[
            $( HostNative { name: $name, arity: $arity, id: $id, effects: $fx, caps: $caps }, )*
        ];
        $( host_natives!(@consts [$(#[$doc])*] $id, $name $(, $idc $(, $nc)?)?); )*
    };
    (@consts [$($doc:tt)*] $id:literal, $name:literal) => {};
    (@consts [$($doc:tt)*] $id:literal, $name:literal, _, $nc:ident) => {
        $($doc)* pub const $nc: &str = $name;
    };
    (@consts [$($doc:tt)*] $id:literal, $name:literal, $idc:ident) => {
        $($doc)* pub const $idc: u16 = $id;
    };
    (@consts [$($doc:tt)*] $id:literal, $name:literal, $idc:ident, $nc:ident) => {
        $($doc)* pub const $idc: u16 = $id;
        $($doc)* pub const $nc: &str = $name;
    };
}

host_natives! {
    0 "stdin" (0) READ, NO_CAP;
    1 "stdout" (0) WRITE, NO_CAP;
    2 "stderr" (0) WRITE, NO_CAP;
    /// `io::open` (frozen row 3): on `TooManyOpenFiles` the runtime collects and
    /// tries once more, as for [`TCP_ACCEPT_ID`].
    3 "open" (2) READ_WRITE, OPEN => IO_OPEN_ID;
    4 "close" (1) WRITE_R, NO_CAP;
    5 "read" (2) READ, NO_CAP;
    6 "write" (2) WRITE, NO_CAP;
    7 "await_readable" (1) SUSPEND, NO_CAP;
    8 "await_writable" (1) SUSPEND, NO_CAP;
    9 "drive" (0) SUSPEND, NO_CAP;
    10 "from_bytes" (1) TEXT, NO_CAP;
    11 "to_bytes" (1) TEXT, NO_CAP;
    12 "tcp_connect" (2) NET, NET_CAP;
    13 "tcp_connect_timeout" (3) NET, NET_CAP;
    14 "tcp_listen" (2) NET, NET_CAP;
    /// `tcp_accept` (frozen row 15).
    15 "tcp_accept" (1) NET, NO_CAP => TCP_ACCEPT_ID;
    16 "tcp_peer_addr" (1) NET, NO_CAP;
    17 "tcp_local_addr" (1) NET, NO_CAP;
    18 "tcp_set_nodelay" (2) NET, NO_CAP;
    19 "tcp_shutdown" (2) NET, NO_CAP;
    20 "udp_bind" (2) NET, NET_CAP;
    21 "udp_connect" (2) NET, NET_CAP;
    22 "udp_send_to" (4) NET, NET_CAP;
    23 "udp_recv_from" (2) NET, NO_CAP;
    24 "udp_local_port" (1) NET, NO_CAP;
    25 "fs_exists" (1) READ, READ_FS;
    26 "fs_is_file" (1) READ, READ_FS;
    27 "fs_is_dir" (1) READ, READ_FS;
    28 "fs_is_symlink" (1) READ, READ_FS;
    29 "fs_metadata" (1) READ, READ_FS;
    30 "fs_create_dir" (1) WRITE_R, WRITE_FS;
    31 "fs_create_dir_all" (1) WRITE_R, WRITE_FS;
    32 "fs_remove_file" (1) WRITE_R, WRITE_FS;
    33 "fs_remove_dir" (1) WRITE_R, WRITE_FS;
    34 "fs_remove_dir_all" (1) WRITE_R, WRITE_FS;
    35 "fs_rename" (2) WRITE_R, WRITE_FS;
    36 "fs_copy" (2) WRITE_R, COPY_FS;
    37 "fs_read_link" (1) READ, READ_FS;
    38 "fs_symlink" (2) WRITE_R, WRITE_FS;
    39 "fs_list_dir" (1) READ, READ_FS;
    40 "fs_realpath" (1) READ, READ_FS;
    41 "time_timestamp" (0) HOST_R, NO_CAP;
    42 "time_sleep_ms" (1) HOST_R, NO_CAP;
    43 "time_instant_now" (0) HOST_R, NO_CAP;
    44 "time_elapsed_nanos" (1) HOST_R, NO_CAP;
    45 "time_elapsed_millis" (1) HOST_R, NO_CAP;
    46 "time_period" (9) HOST_R, NO_CAP;
    47 "time_add" (2) HOST_R, NO_CAP;
    48 "time_sub" (2) HOST_R, NO_CAP;
    49 "time_period_add" (2) HOST_R, NO_CAP;
    50 "time_period_sub" (2) HOST_R, NO_CAP;
    51 "time_date" (0) HOST_R, NO_CAP;
    52 "time_date_from_period" (1) HOST_R, NO_CAP;
    53 "time_date_from_epoch_period" (1) HOST_R, NO_CAP;
    54 "time_epoch" (0) HOST_R, NO_CAP;
    55 "time_format" (2) HOST_R, NO_CAP;
    56 "time_parse" (2) HOST_R, NO_CAP;
    57 "env_args" (0) ENV, NO_CAP;
    58 "env_var" (1) ENV, ENV_CAP;
    59 "env_set_var" (2) ENV, ENV_CAP;
    60 "env_remove_var" (1) ENV, ENV_CAP;
    61 "env_cwd" (0) ENV, NO_CAP;
    62 "env_set_cwd" (1) ENV, ENV_CAP;
    63 "env_exit" (1) EXEC, EXIT_CAP;
    // Removed slot: `env::exec` (now the `process` module, ids 180–188). Kept so
    // later ids do not slide; nothing exports it and the runtime panics.
    64 "env_exec" (2) EXEC, Caps::NONE;
    65 "ord" (1) HOST_R, NO_CAP;
    66 "char" (1) HOST_R, NO_CAP;
    67 "hash_string" (1) HOST_R, NO_CAP;
    68 "thread_spawn" (1) THREAD_R, NO_CAP;
    69 "thread_join" (1) THREAD_R, NO_CAP;
    70 "thread_detach" (1) THREAD_R, NO_CAP;
    71 "thread_channel" (0) THREAD_R, NO_CAP;
    72 "thread_send" (2) THREAD_R, NO_CAP;
    73 "thread_recv" (1) THREAD_R, NO_CAP;
    74 "thread_try_send" (2) THREAD_R, NO_CAP;
    75 "thread_try_recv" (1) THREAD_R, NO_CAP;
    76 "thread_close" (1) THREAD_R, NO_CAP;
    77 "thread_mutex" (1) THREAD_R, NO_CAP;
    78 "thread_with_lock" (2) THREAD_R, NO_CAP;
    79 "thread_lock" (1) THREAD_R, NO_CAP;
    80 "thread_try_lock" (1) THREAD_R, NO_CAP;
    81 "thread_unlock" (1) THREAD_R, NO_CAP;
    82 "thread_rwlock" (1) THREAD_R, NO_CAP;
    83 "thread_with_read" (2) THREAD_R, NO_CAP;
    84 "thread_with_write" (2) THREAD_R, NO_CAP;
    85 "thread_try_read" (2) THREAD_R, NO_CAP;
    86 "thread_try_write" (2) THREAD_R, NO_CAP;
    /// First packed-LA HostInvoke (`packed_dot`).
    87 "packed_dot" (3) PURE, NO_CAP => PACKED_DOT_ID;
    88 "packed_matmul" (3) PURE, NO_CAP => PACKED_MATMUL_ID;
    89 "packed_matrix_zip" (3) PURE, NO_CAP => PACKED_MATRIX_ZIP_ID;
    90 "packed_matrix_neg" (2) PURE, NO_CAP => PACKED_MATRIX_NEG_ID;
    /// Last packed-LA HostInvoke (`packed_vec_arith`).
    91 "packed_vec_arith" (3) PURE, NO_CAP => PACKED_VEC_ARITH_ID;
    92 "wait_ready" (0) SUSPEND, NO_CAP;
    93 "write_from" (3) WRITE_R, NO_CAP;
    /// `gc::root` (its id in the table above).
    94 "gc_root" (1) GC_R, NO_CAP => GC_ROOT_ID;
    95 "gc_unroot" (1) GC_R, NO_CAP;
    96 "gc_get" (1) GC_R, NO_CAP;
    97 "gc_weak" (1) GC_R, NO_CAP;
    98 "gc_upgrade" (1) GC_R, NO_CAP;
    99 "gc_heap_bytes" (0) GC_R, NO_CAP;
    100 "gc_collect" (0) GC_R, NO_CAP => _, GC_COLLECT_NATIVE;
    101 "gc_register_finalizer" (2) GC_R, NO_CAP => _, GC_REGISTER_FINALIZER_NATIVE;
    /// Frozen HostInvoke id for `math_sin` (first frozen prelude math).
    102 "math_sin" (1) PURE, NO_CAP => MATH_SIN_ID;
    103 "math_cos" (1) PURE, NO_CAP;
    104 "math_tan" (1) PURE, NO_CAP;
    105 "math_sqrt" (1) PURE, NO_CAP;
    106 "math_floor" (1) PURE, NO_CAP;
    107 "math_ceil" (1) PURE, NO_CAP;
    108 "math_exp" (1) PURE, NO_CAP;
    109 "math_ln" (1) PURE, NO_CAP;
    /// Frozen HostInvoke id for `math_pow` (last frozen prelude math).
    110 "math_pow" (2) PURE, NO_CAP => MATH_POW_ID;
    111 "vec_with_capacity" (1) HEAP_R, NO_CAP;
    112 "vec_capacity" (1) HEAP_R, NO_CAP;
    113 "vec_reserve" (2) HEAP_R, NO_CAP;
    114 "vec_clear" (1) HEAP_R, NO_CAP;
    115 "vec_pop" (1) HEAP_R, NO_CAP;
    116 "vec_insert" (3) HEAP_R, NO_CAP;
    117 "vec_remove" (2) HEAP_R, NO_CAP;
    118 "vec_from_array" (1) HEAP_R, NO_CAP;
    /// Frozen HostInvoke id for `stream_attach`.
    119 "stream_attach" (6) PARK_R, ATTACH_CAP => STREAM_ATTACH_ID, STREAM_ATTACH_NATIVE;
    /// Frozen HostInvoke id for `stream_park`.
    120 "stream_park" (1) PARK_R, NO_CAP => STREAM_PARK_ID, STREAM_PARK_NATIVE;
    /// HostInvoke id for `clock_wall_nanos`.
    121 "clock_wall_nanos" (0) HOST, NO_CAP => CLOCK_WALL_NANOS_ID, CLOCK_WALL_NANOS_NATIVE;
    /// HostInvoke id for `clock_mono_nanos`.
    122 "clock_mono_nanos" (0) HOST, NO_CAP => CLOCK_MONO_NANOS_ID, CLOCK_MONO_NANOS_NATIVE;
    /// HostInvoke id for `clock_sleep_ms`.
    123 "clock_sleep_ms" (1) HOST, NO_CAP => CLOCK_SLEEP_MS_ID, CLOCK_SLEEP_MS_NATIVE;
    /// HostInvoke id for `result_unit_probe` (`Result<(), IoError>` pack helper).
    124 "result_unit_probe" (1) HOST_R, NO_CAP => RESULT_UNIT_PROBE_ID, RESULT_UNIT_PROBE_NATIVE;
    /// First HostInvoke id of the M1 `prelude::math` expansion (`math_atan`).
    125 "math_atan" (1) PURE, NO_CAP => MATH_ATAN_ID;
    126 "math_atan2" (2) PURE, NO_CAP;
    127 "math_asin" (1) PURE, NO_CAP;
    128 "math_acos" (1) PURE, NO_CAP;
    129 "math_log10" (1) PURE, NO_CAP;
    130 "math_log2" (1) PURE, NO_CAP;
    131 "math_cbrt" (1) PURE, NO_CAP;
    132 "math_rem" (2) PURE, NO_CAP;
    133 "math_sinh" (1) PURE, NO_CAP;
    134 "math_cosh" (1) PURE, NO_CAP;
    /// HostInvoke id for `math_tanh` (last M1 math native).
    135 "math_tanh" (1) PURE, NO_CAP => MATH_TANH_ID;
    /// Compiler-only HostInvoke id for MIR saxpy-reduce packs (COI-286).
    136 "simd_axpy_reduce" (5) PURE, NO_CAP => SIMD_AXPY_REDUCE_ID, SIMD_AXPY_REDUCE_NATIVE;
    137 "thread_spawn_shared" (1) THREAD_R, NO_CAP => THREAD_SPAWN_SHARED_ID, THREAD_SPAWN_SHARED_NATIVE;
    138 "stream_fd" (1) PARK_R, NO_CAP => STREAM_FD_ID, STREAM_FD_NATIVE;
    /// First byte-offset `string` native (`string_byte_at`); the block runs
    /// through [`STRING_MATCH_AT_ID`] (archive minor 31).
    139 "string_byte_at" (2) TEXT_R, NO_CAP => STRING_BYTE_AT_ID;
    140 "string_slice_bytes" (3) TEXT_R, NO_CAP;
    141 "string_find_from" (3) TEXT_R, NO_CAP;
    142 "string_rfind" (2) TEXT_R, NO_CAP;
    /// Last byte-offset `string` native (`string_match_at`).
    143 "string_match_at" (3) TEXT_R, NO_CAP => STRING_MATCH_AT_ID;
    /// First task-scheduler native (`task_scope_open`); the block runs through
    /// [`TASK_YIELD_ID`] (archive minor 32). The runtime handles these itself
    /// (`HostOp::Task`): they can switch tasks.
    144 "task_scope_open" (0) SUSPEND, NO_CAP => TASK_SCOPE_OPEN_ID;
    145 "task_scope_close" (1) SUSPEND, NO_CAP;
    146 "task_scope_error" (1) SUSPEND, NO_CAP;
    147 "task_spawn" (2) SUSPEND, NO_CAP;
    148 "task_join" (1) SUSPEND, NO_CAP;
    149 "task_error" (1) SUSPEND, NO_CAP;
    150 "task_sleep" (1) SUSPEND, NO_CAP;
    /// Last task-scheduler native (`task_yield`).
    151 "task_yield" (0) SUSPEND, NO_CAP => TASK_YIELD_ID;
    /// Ends a `defer` cleanup pad: the runtime unwinder takes the frame back
    /// (archive minor 33, `HostOp::Unwind`). Compiler-emitted only.
    152 "unwind_resume" (0) PURE, NO_CAP => UNWIND_RESUME_ID, UNWIND_RESUME_NATIVE;
    /// First cancellation native (`task_cancel`); the block runs through
    /// [`TASK_SHIELD_EXIT_ID`] (archive minor 33). `HostOp::Task`, like the
    /// scheduler block.
    153 "task_cancel" (1) SUSPEND, NO_CAP => TASK_CANCEL_ID;
    154 "task_shield_enter" (0) SUSPEND, NO_CAP;
    /// Last cancellation native (`task_shield_exit`).
    155 "task_shield_exit" (0) SUSPEND, NO_CAP => TASK_SHIELD_EXIT_ID;
    /// First wait-condition native (`task_cond_new`); the block runs through
    /// [`TASK_COND_NOTIFY_ID`] (archive minor 34). `HostOp::Task`: a wait
    /// suspends the task until a notify (`task::channel`).
    156 "task_cond_new" (0) SUSPEND, NO_CAP => TASK_COND_NEW_ID;
    157 "task_cond_wait" (1) SUSPEND, NO_CAP;
    /// Last wait-condition native (`task_cond_notify`).
    158 "task_cond_notify" (1) SUSPEND, NO_CAP => TASK_COND_NOTIFY_ID;
    /// A failed `requires` (archive minor 35). `HostOp::Task`: panics with the
    /// message plus the caller's location (the frame below), never returns.
    159 "contract_fail" (1) PURE, NO_CAP => CONTRACT_FAIL_ID, CONTRACT_FAIL_NATIVE;
    /// `ffi::read_ints(lib, ptr, count)` (archive minor 36): copy `count` C
    /// `int64_t`s from a pointer a native returned (an ovector, an out array).
    160 "ffi_read_ints" (3) FFI_R, NO_CAP => FFI_READ_INTS_ID, FFI_READ_INTS_NATIVE;
    /// `==` on an aggregate with `float` leaves, by its static shape
    /// (coil-lang#1253, archive minor 39). No longer emitted: such an `==`
    /// calls compiler-made glue (`codegen::eq_glue`). Kept for archives that
    /// still call it; a panic stub at the next major.
    161 "values_eq_shaped" (3) TEXT, NO_CAP => VALUES_EQ_SHAPED_ID, VALUES_EQ_SHAPED_NATIVE;
    /// `<` / `<=` / `>` / `>=` on `string`: `-1` / `0` / `1` by UTF-8 bytes
    /// (coil-stdlib#48, archive minor 41).
    162 "string_compare" (2) TEXT, NO_CAP => STRING_COMPARE_ID, STRING_COMPARE_NATIVE;
    /// `black_box(x)` (archive minor 43): returns `x`, which the optimizer
    /// cannot see through. Its `HOST` effect keeps it from being folded at
    /// compile time, hoisted or removed.
    163 "black_box" (1) HOST, NO_CAP => BLACK_BOX_ID, BLACK_BOX_NATIVE;
    /// `io::capture_begin()` / `io::capture_end()` (archive minor 44): keep
    /// this thread's stdout and stderr writes in memory instead of writing them,
    /// and hand them back as `(stdout, stderr)` strings.
    164 "io_capture_begin" (0) HOST, NO_CAP => IO_CAPTURE_BEGIN_ID, IO_CAPTURE_BEGIN_NATIVE;
    165 "io_capture_end" (0) HOST, NO_CAP => IO_CAPTURE_END_ID, IO_CAPTURE_END_NATIVE;
    /// `io::tty` (archive minor 47): terminal queries, raw mode, a timed read
    /// and resize notices on a stream. Ungated like `stdin` / `stdout`.
    166 "tty_is_tty" (1) HOST, NO_CAP => TTY_IS_TTY_ID, TTY_IS_TTY_NATIVE;
    167 "tty_size" (1) HOST, NO_CAP => _, TTY_SIZE_NATIVE;
    168 "tty_enter_raw" (3) HOST, NO_CAP => _, TTY_ENTER_RAW_NATIVE;
    169 "tty_leave_raw" (1) HOST, NO_CAP => _, TTY_LEAVE_RAW_NATIVE;
    170 "tty_is_raw" (1) HOST, NO_CAP => _, TTY_IS_RAW_NATIVE;
    171 "tty_read_timeout" (3) READ, NO_CAP => _, TTY_READ_TIMEOUT_NATIVE;
    172 "tty_resized" (0) HOST, NO_CAP => _, TTY_RESIZED_NATIVE;
    173 "tty_enable_vt" (1) HOST, NO_CAP => TTY_ENABLE_VT_ID, TTY_ENABLE_VT_NATIVE;
    /// `vec_extend_range(dst, src, from, to)` (archive minor 1, compiler
    /// rewrite only): append `src[from..to]` to `dst` and return `to`; the
    /// compiler has checked `0 <= from < to <= src.len()`. When `dst` is
    /// `src` it appends nothing and returns `from` (the loop runs scalar).
    174 "vec_extend_range" (4) HEAP_R, NO_CAP => VEC_EXTEND_RANGE_ID, VEC_EXTEND_RANGE_NATIVE;
    /// `vec_fill(dst, v, n)` (archive minor 1, compiler rewrite only): push
    /// `v` onto `dst` `n` times (`n >= 1`); returns `0`.
    175 "vec_fill" (3) HEAP_R, NO_CAP => VEC_FILL_ID, VEC_FILL_NATIVE;
    /// `from_bytes` of a buffer the compiler proved dead after the call (archive
    /// 6.4): the string takes the buffer's bytes. Never named in source; the
    /// compiler swaps it in for `from_bytes`.
    176 "from_bytes_take" (1) TEXT, NO_CAP => FROM_BYTES_TAKE_ID, FROM_BYTES_TAKE_NATIVE;
    /// HostInvoke ids around a `region { ... }` block (escape step 5, archive
    /// 6.2): enter starts a fresh nursery for the block, exit collects it.
    /// Never named in source; the compiler emits them for `region`.
    177 "gc_region_enter" (0) GC_R, NO_CAP => GC_REGION_ENTER_ID, GC_REGION_ENTER_NATIVE;
    178 "gc_region_exit" (0) GC_R, NO_CAP => GC_REGION_EXIT_ID, GC_REGION_EXIT_NATIVE;
    /// `symbol::next_id()` (archive 6.9): the next value of one process-wide
    /// atomic counter. Its `HOST` effect keeps it from being folded at compile
    /// time, hoisted or removed.
    179 "symbol_next_id" (0) HOST, NO_CAP => SYMBOL_NEXT_ID_ID, SYMBOL_NEXT_ID_NATIVE;
    /// The embedded `process` module's natives (archive 6.10, ids 180–188):
    /// spawn a program, take its pipes as streams, wait, kill, signal. Only
    /// `process_spawn` needs a grant (`--allow-exec`); the rest act on a child
    /// it started.
    180 "process_spawn" (6) EXEC, EXEC_CAP => PROCESS_SPAWN_ID, PROCESS_SPAWN_NATIVE;
    181 "process_take_pipe" (2) HOST_R, NO_CAP => _, PROCESS_TAKE_PIPE_NATIVE;
    182 "process_wait" (1) EXEC, NO_CAP => _, PROCESS_WAIT_NATIVE;
    183 "process_try_wait" (1) EXEC, NO_CAP => _, PROCESS_TRY_WAIT_NATIVE;
    184 "process_output" (1) EXEC, NO_CAP => _, PROCESS_OUTPUT_NATIVE;
    185 "process_kill" (1) EXEC, NO_CAP => _, PROCESS_KILL_NATIVE;
    186 "process_signal" (2) EXEC, NO_CAP => _, PROCESS_SIGNAL_NATIVE;
    187 "process_pid" (1) HOST, NO_CAP => _, PROCESS_PID_NATIVE;
    188 "process_release" (1) HOST, NO_CAP => PROCESS_RELEASE_ID, PROCESS_RELEASE_NATIVE;
    /// Closes a generator a `for` loop leaves early (archive 6.14,
    /// `HostOp::Task`): runs its armed `defer`s. Compiler-emitted only.
    189 "coro_close" (1) SUSPEND, NO_CAP => CORO_CLOSE_ID, CORO_CLOSE_NATIVE;
    /// `entropy::bytes(n)` (archive 6.15): `n` fresh bytes from the OS CSPRNG.
    /// No capability: like the clock, it reads host state and touches nothing.
    190 "entropy_bytes" (1) HOST_ALLOC, NO_CAP => ENTROPY_BYTES_ID, ENTROPY_BYTES_NATIVE;
    /// The key index natives (archive 6.19, `key_index::*`): an
    /// insertion-ordered hash index over parallel `Vec`s that any collection
    /// keyed by a `Hash + Eq` type builds on. See
    /// `docs/internals/key-index.md`.
    191 "key_index_table" (1) TEXT_R, NO_CAP => KEY_INDEX_TABLE_ID, KEY_INDEX_TABLE_NATIVE;
    192 "key_index_probe" (4) HOST_R, NO_CAP => _, KEY_INDEX_PROBE_NATIVE;
    193 "key_index_insert" (4) HEAP_R, NO_CAP => _, KEY_INDEX_INSERT_NATIVE;
    194 "key_index_remove" (4) HEAP_R, NO_CAP => _, KEY_INDEX_REMOVE_NATIVE;
    195 "key_index_compact" (3) HEAP_R, NO_CAP => _, KEY_INDEX_COMPACT_NATIVE;
    196 "key_index_retain" (2) HEAP_R, NO_CAP => _, KEY_INDEX_RETAIN_NATIVE;
    197 "key_index_clear" (3) HEAP_R, NO_CAP => KEY_INDEX_CLEAR_ID, KEY_INDEX_CLEAR_NATIVE;
    /// `key_index::find(table, hashes, keys, h, k, mode)`: the slot whose key
    /// equals `k` by `mode` (one of the `KEY_EQ_*` below), or `-1`.
    198 "key_index_find" (6) HOST_R, NO_CAP => _, KEY_INDEX_FIND_NATIVE;
    /// `key_index::eq_mode(k)`: how `find` may compare keys of `k`'s type. The
    /// compiler folds it to a constant in every body it plans; the native
    /// answers [`KEY_EQ_COIL`].
    199 "key_index_eq_mode" (1) PURE, NO_CAP => KEY_INDEX_EQ_MODE_ID, KEY_INDEX_EQ_MODE_NATIVE;
    /// `prelude::thread::lock_acquire`, the first lock primitive (archive 6.21).
    200 "lock_acquire" (1) THREAD_R, NO_CAP => LOCK_ACQUIRE_ID;
    201 "lock_release" (1) THREAD_R, NO_CAP;
    202 "lock_load" (1) THREAD_R, NO_CAP;
    203 "lock_store" (2) THREAD_R, NO_CAP;
    204 "rw_acquire_read" (1) THREAD_R, NO_CAP;
    205 "rw_acquire_write" (1) THREAD_R, NO_CAP;
    206 "rw_try_read" (1) THREAD_R, NO_CAP;
    207 "rw_try_write" (1) THREAD_R, NO_CAP;
    208 "rw_release_read" (1) THREAD_R, NO_CAP;
    209 "rw_release_write" (1) THREAD_R, NO_CAP;
    210 "rw_load" (1) THREAD_R, NO_CAP;
    /// `prelude::thread::rw_store`, the last lock primitive.
    211 "rw_store" (2) THREAD_R, NO_CAP => RW_STORE_ID;
    /// `vec_drain(v, from, to)` (archive 7 minor 3, compiler lowering only):
    /// drop elements `from..to` of array `v`, the ones after them moving down;
    /// the compiler has checked `0 <= from <= to <= v.len()`. Any array form,
    /// a lane array's included: lane code's `pop` and `remove` take the element
    /// out with it.
    212 "vec_drain" (3) HEAP_R, NO_CAP => VEC_DRAIN_ID, VEC_DRAIN_NATIVE;
    /// `vec_rotate_in(v, i)` (archive 7 minor 3, compiler lowering only): move
    /// the last element of array `v` to `i`, elements `i..` moving up one; the
    /// compiler has checked `0 <= i < v.len()`. Any array form: lane code's
    /// `insert` pushes and then rotates the element in.
    213 "vec_rotate_in" (2) HEAP_R, NO_CAP => VEC_ROTATE_IN_ID, VEC_ROTATE_IN_NATIVE;
    /// Restores the snapshot at a constant-pool index (archive 7 minor 4,
    /// `HostOp::Restore`): an AOT executable's native `__start` runs it where
    /// the startup restored a snapshot. Compiler-emitted only.
    214 "startup_restore" (1) SUSPEND, NO_CAP => STARTUP_RESTORE_ID, STARTUP_RESTORE_NATIVE;
}

/// `eq_mode`: the type's `==` is not one the runtime runs; the caller compares
/// candidates from `probe` itself.
pub const KEY_EQ_COIL: i64 = 0;
/// `eq_mode`: `==` compares the words (`int`).
pub const KEY_EQ_WORD: i64 = 1;
/// `eq_mode`: `==` compares string contents (`string`).
pub const KEY_EQ_STRING: i64 = 2;

/// Set on the `type_id` argument of `gc_register_finalizer` when the
/// type's `drop` keeps nothing of `self` (the compiler's escape facts):
/// the runtime may run it where an object of the type is freed early, then free
/// the object. An archive without it leaves every `drop` to the collector.
/// The argument is a 16-bit `CONST` immediate: a type id at or above the
/// flag is never flagged.
pub const EARLY_DROP_FLAG: u32 = 1 << 15;

/// Low 16 bits of a `HostInvoke` operand are the argument count.
pub const HOST_INVOKE_ARITY_MASK: u32 = 0xFFFF;
/// Bits `[17:16]` select the Option/Result host-edge layout (archive minor 2).
pub const HOST_ENUM_LAYOUT_SHIFT: u32 = 16;
pub const HOST_ENUM_LAYOUT_MASK: u32 = 0x3;
/// Boxed `ObjEnum` (default; old archives leave these bits clear).
pub const HOST_ENUM_LAYOUT_BOXED: u32 = 0;
/// Pointer-niche `Option` (`None` = `0`, `Some` = object address).
pub const HOST_ENUM_LAYOUT_OPTION_NICHE: u32 = 1;
/// Heap-heap `Result` (`Ok` = aligned pointer, `Err` = `pointer | 1`).
pub const HOST_ENUM_LAYOUT_RESULT_NICHE: u32 = 2;
/// Two words: the native returns the variant's payload word (`0` when it
/// has none) and leaves its tag in the native context's second return
/// word, with no enum object made (`Option<int>`, `Result<int, E>`).
pub const HOST_ENUM_LAYOUT_PAIR: u32 = 3;

/// Pack `HostInvoke` arity and host-edge Option/Result layout into one operand.
pub const fn pack_host_invoke_operand(arity: u32, layout: u32) -> u32 {
    (arity & HOST_INVOKE_ARITY_MASK) | ((layout & HOST_ENUM_LAYOUT_MASK) << HOST_ENUM_LAYOUT_SHIFT)
}

/// Argument count from a `HostInvoke` operand.
pub const fn host_invoke_arity(operand: u32) -> u32 {
    operand & HOST_INVOKE_ARITY_MASK
}

/// Host-edge Option/Result layout from a `HostInvoke` operand.
pub const fn host_invoke_enum_layout(operand: u32) -> u32 {
    (operand >> HOST_ENUM_LAYOUT_SHIFT) & HOST_ENUM_LAYOUT_MASK
}

pub const PACKED_DOT: &str = "packed_dot";
pub const PACKED_MATMUL: &str = "packed_matmul";
pub const PACKED_MATRIX_ZIP: &str = "packed_matrix_zip";
pub const PACKED_MATRIX_NEG: &str = "packed_matrix_neg";
pub const PACKED_VEC_ARITH: &str = "packed_vec_arith";

// Every row's id is its index (so its consts name their row), and the
// table only grows.
const _: () = {
    assert!(HOST_NATIVES.len() == 215);
    let mut i = 0;
    while i < HOST_NATIVES.len() {
        assert!(HOST_NATIVES[i].id as usize == i, "a host native's id is not its index");
        i += 1;
    }
};

/// HostInvoke id for a standard native name.
pub fn host_native_id(name: &str) -> Option<usize> {
    HOST_NATIVES
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.id as usize)
}

/// Natives the compiler may call with a direct host call:
/// plain functions of their arguments that always return a value and never
/// park, open descriptors or switch tasks. The runtime registers each of them
/// with a direct function (`NativeFn::direct`; a machine test checks it).
pub const DIRECT_HOST_NATIVES: &[&str] = &[
    "ord",
    "char",
    "hash_string",
    "black_box",
    "string_byte_at",
    "string_slice_bytes",
    "string_find_from",
    "string_rfind",
    "string_match_at",
    "vec_with_capacity",
    "vec_capacity",
    "vec_reserve",
    "vec_clear",
    "vec_pop",
    "vec_remove",
    "vec_from_array",
    "vec_extend_range",
    "vec_fill",
    VEC_DRAIN_NATIVE,
    VEC_ROTATE_IN_NATIVE,
    "symbol_next_id",
    "key_index_table",
    "key_index_probe",
    "key_index_insert",
    "key_index_remove",
    "key_index_compact",
    "key_index_retain",
    "key_index_clear",
    "key_index_find",
    "key_index_eq_mode",
];

/// Whether standard native `id` is in [`DIRECT_HOST_NATIVES`].
pub fn host_native_is_direct(id: usize) -> bool {
    HOST_NATIVES
        .get(id)
        .is_some_and(|n| n.id as usize == id && DIRECT_HOST_NATIVES.contains(&n.name))
}

/// `(name, id)` pairs for compiler `register_native_id`.
pub fn host_native_ids() -> impl Iterator<Item = (&'static str, usize)> {
    HOST_NATIVES.iter().map(|e| (e.name, e.id as usize))
}

/// True when `name` is a libc/CRT process-exec symbol.
pub fn is_ffi_exec_symbol(name: &str) -> bool {
    let n = name.trim().trim_matches('_').to_ascii_lowercase();
    matches!(
        n.as_str(),
        "system"
            | "wsystem"
            | "libc_system"
            | "exec"
            | "execl"
            | "execle"
            | "execlp"
            | "execv"
            | "execvp"
            | "execvpe"
            | "execve"
            | "fexecve"
            | "execveat"
            | "posix_spawn"
            | "posix_spawnp"
            | "popen"
            | "createprocessa"
            | "createprocessw"
            | "winexec"
    )
}

/// Filename stem for the `dload` gate (`/abs/libfoo.so` → `foo`).
pub fn dload_request_stem(name: &str) -> String {
    let file = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(name);
    library_stem(file)
}

/// Whether `name` (or its stem) refers to the C standard library.
pub fn is_libc_alias(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "c" | "libc" | "libc.so.6" | "libsystem" | "libsystem.b.dylib" | "ucrtbase" | "msvcrt"
    ) || {
        let stem = library_stem(&lower);
        matches!(
            stem.as_str(),
            "c" | "system" | "system.b" | "ucrtbase" | "msvcrt"
        )
    }
}

/// Strip a known shared-library suffix and optional `lib` prefix.
pub fn library_stem(name: &str) -> String {
    let mut stem = name.to_string();
    if let Some(idx) = stem.find(".so.") {
        stem.truncate(idx);
    } else if let Some(stripped) = stem.strip_suffix(".so") {
        stem = stripped.to_string();
    } else if let Some(stripped) = stem.strip_suffix(".dylib") {
        stem = stripped.to_string();
    } else if let Some(stripped) = stem.strip_suffix(".dll") {
        stem = stripped.to_string();
    }
    if let Some(stripped) = stem.strip_prefix("lib")
        && !stripped.is_empty() {
            stem = stripped.to_string();
        }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_tail_ids() {
        assert_eq!(host_native_id(STREAM_ATTACH_NATIVE), Some(119));
        assert_eq!(host_native_id(STREAM_PARK_NATIVE), Some(120));
        assert_eq!(host_native_id(CLOCK_WALL_NANOS_NATIVE), Some(121));
        assert_eq!(host_native_id(CLOCK_MONO_NANOS_NATIVE), Some(122));
        assert_eq!(host_native_id(CLOCK_SLEEP_MS_NATIVE), Some(123));
        assert_eq!(host_native_id(RESULT_UNIT_PROBE_NATIVE), Some(124));
        assert_eq!(host_native_id("math_atan"), Some(125));
        assert_eq!(host_native_id("math_tanh"), Some(135));
        assert_eq!(host_native_id(SIMD_AXPY_REDUCE_NATIVE), Some(136));
        assert_eq!(host_native_id(THREAD_SPAWN_SHARED_NATIVE), Some(137));
        assert_eq!(host_native_id("task_scope_open"), Some(TASK_SCOPE_OPEN_ID as usize));
        assert_eq!(host_native_id("task_yield"), Some(TASK_YIELD_ID as usize));
        assert_eq!(host_native_id(UNWIND_RESUME_NATIVE), Some(UNWIND_RESUME_ID as usize));
        assert_eq!(HOST_NATIVES[24].name, "udp_local_port");
        for (i, e) in HOST_NATIVES.iter().enumerate() {
            assert_eq!(e.id as usize, i, "{} id drifted", e.name);
        }
    }

    /// A name lookup finds its own row: no two natives share a name.
    #[test]
    fn host_native_names_are_unique() {
        for e in HOST_NATIVES {
            assert_eq!(host_native_id(e.name), Some(e.id as usize), "{} is not unique", e.name);
        }
    }

    #[test]
    fn dload_request_stem_strips_lib_and_suffix() {
        assert_eq!(dload_request_stem("libsum.so"), "sum");
        assert_eq!(dload_request_stem("/abs/libtime.so"), "time");
        assert_eq!(dload_request_stem("c"), "c");
    }

    #[test]
    fn ffi_exec_symbol_aliases() {
        assert!(is_ffi_exec_symbol("system"));
        assert!(is_ffi_exec_symbol("execve"));
        assert!(is_ffi_exec_symbol("_wsystem"));
        assert!(!is_ffi_exec_symbol("strlen"));
    }

    #[test]
    fn host_invoke_operand_packs_layout_in_high_bits() {
        let packed = pack_host_invoke_operand(3, HOST_ENUM_LAYOUT_OPTION_NICHE);
        assert_eq!(host_invoke_arity(packed), 3);
        assert_eq!(
            host_invoke_enum_layout(packed),
            HOST_ENUM_LAYOUT_OPTION_NICHE
        );
        assert_eq!(host_invoke_enum_layout(3), HOST_ENUM_LAYOUT_BOXED);
    }
}
