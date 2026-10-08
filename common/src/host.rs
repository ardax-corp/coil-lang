//! Append-only HostInvoke catalog shared by compiler and VM.
//!
//! Ids are the table index. New natives go at the end. Do not reorder or
//! reuse slots. Frozen: 119 = `stream_attach`, 120 = `stream_park`.
//! Append-only after that: 121–123 = `clock_*`, 124 = `result_unit_probe`.

use crate::EffectFlags;

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
const THREAD_R: EffectFlags = fx(EffectFlags::THREAD | R);
const GC_R: EffectFlags = fx(EffectFlags::GC | R);
const HEAP_R: EffectFlags = fx(EffectFlags::HEAP_MUT | R);
const PARK_R: EffectFlags = fx(EffectFlags::ATTACH_PARK | R);
/// Byte/string conversions and searches. Pure in fact; kept impure (as
/// before the split) until a bench shows marking them pure is safe for
/// auto-par and LICM.
const TEXT: EffectFlags = fx(EffectFlags::READ);
const TEXT_R: EffectFlags = fx(EffectFlags::READ | R);

/// Standard host natives in HostInvoke id order.
pub const HOST_NATIVES: &[HostNative] = &[
    HostNative {
        name: "stdin",
        arity: 0,
        id: 0,
        effects: READ,
    },
    HostNative {
        name: "stdout",
        arity: 0,
        id: 1,
        effects: WRITE,
    },
    HostNative {
        name: "stderr",
        arity: 0,
        id: 2,
        effects: WRITE,
    },
    HostNative {
        name: "open",
        arity: 2,
        id: 3,
        effects: READ_WRITE,
    },
    HostNative {
        name: "close",
        arity: 1,
        id: 4,
        effects: WRITE_R,
    },
    HostNative {
        name: "read",
        arity: 2,
        id: 5,
        effects: READ,
    },
    HostNative {
        name: "write",
        arity: 2,
        id: 6,
        effects: WRITE,
    },
    HostNative {
        name: "await_readable",
        arity: 1,
        id: 7,
        effects: SUSPEND,
    },
    HostNative {
        name: "await_writable",
        arity: 1,
        id: 8,
        effects: SUSPEND,
    },
    HostNative {
        name: "drive",
        arity: 0,
        id: 9,
        effects: SUSPEND,
    },
    HostNative {
        name: "from_bytes",
        arity: 1,
        id: 10,
        effects: TEXT,
    },
    HostNative {
        name: "to_bytes",
        arity: 1,
        id: 11,
        effects: TEXT,
    },
    HostNative {
        name: "tcp_connect",
        arity: 2,
        id: 12,
        effects: NET,
    },
    HostNative {
        name: "tcp_connect_timeout",
        arity: 3,
        id: 13,
        effects: NET,
    },
    HostNative {
        name: "tcp_listen",
        arity: 2,
        id: 14,
        effects: NET,
    },
    HostNative {
        name: "tcp_accept",
        arity: 1,
        id: 15,
        effects: NET,
    },
    HostNative {
        name: "tcp_peer_addr",
        arity: 1,
        id: 16,
        effects: NET,
    },
    HostNative {
        name: "tcp_local_addr",
        arity: 1,
        id: 17,
        effects: NET,
    },
    HostNative {
        name: "tcp_set_nodelay",
        arity: 2,
        id: 18,
        effects: NET,
    },
    HostNative {
        name: "tcp_shutdown",
        arity: 2,
        id: 19,
        effects: NET,
    },
    HostNative {
        name: "udp_bind",
        arity: 2,
        id: 20,
        effects: NET,
    },
    HostNative {
        name: "udp_connect",
        arity: 2,
        id: 21,
        effects: NET,
    },
    HostNative {
        name: "udp_send_to",
        arity: 4,
        id: 22,
        effects: NET,
    },
    HostNative {
        name: "udp_recv_from",
        arity: 2,
        id: 23,
        effects: NET,
    },
    HostNative {
        name: "udp_local_port",
        arity: 1,
        id: 24,
        effects: NET,
    },
    HostNative {
        name: "fs_exists",
        arity: 1,
        id: 25,
        effects: READ,
    },
    HostNative {
        name: "fs_is_file",
        arity: 1,
        id: 26,
        effects: READ,
    },
    HostNative {
        name: "fs_is_dir",
        arity: 1,
        id: 27,
        effects: READ,
    },
    HostNative {
        name: "fs_is_symlink",
        arity: 1,
        id: 28,
        effects: READ,
    },
    HostNative {
        name: "fs_metadata",
        arity: 1,
        id: 29,
        effects: READ,
    },
    HostNative {
        name: "fs_create_dir",
        arity: 1,
        id: 30,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_create_dir_all",
        arity: 1,
        id: 31,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_remove_file",
        arity: 1,
        id: 32,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_remove_dir",
        arity: 1,
        id: 33,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_remove_dir_all",
        arity: 1,
        id: 34,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_rename",
        arity: 2,
        id: 35,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_copy",
        arity: 2,
        id: 36,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_read_link",
        arity: 1,
        id: 37,
        effects: READ,
    },
    HostNative {
        name: "fs_symlink",
        arity: 2,
        id: 38,
        effects: WRITE_R,
    },
    HostNative {
        name: "fs_list_dir",
        arity: 1,
        id: 39,
        effects: READ,
    },
    HostNative {
        name: "fs_realpath",
        arity: 1,
        id: 40,
        effects: READ,
    },
    HostNative {
        name: "time_timestamp",
        arity: 0,
        id: 41,
        effects: HOST_R,
    },
    HostNative {
        name: "time_sleep_ms",
        arity: 1,
        id: 42,
        effects: HOST_R,
    },
    HostNative {
        name: "time_instant_now",
        arity: 0,
        id: 43,
        effects: HOST_R,
    },
    HostNative {
        name: "time_elapsed_nanos",
        arity: 1,
        id: 44,
        effects: HOST_R,
    },
    HostNative {
        name: "time_elapsed_millis",
        arity: 1,
        id: 45,
        effects: HOST_R,
    },
    HostNative {
        name: "time_period",
        arity: 9,
        id: 46,
        effects: HOST_R,
    },
    HostNative {
        name: "time_add",
        arity: 2,
        id: 47,
        effects: HOST_R,
    },
    HostNative {
        name: "time_sub",
        arity: 2,
        id: 48,
        effects: HOST_R,
    },
    HostNative {
        name: "time_period_add",
        arity: 2,
        id: 49,
        effects: HOST_R,
    },
    HostNative {
        name: "time_period_sub",
        arity: 2,
        id: 50,
        effects: HOST_R,
    },
    HostNative {
        name: "time_date",
        arity: 0,
        id: 51,
        effects: HOST_R,
    },
    HostNative {
        name: "time_date_from_period",
        arity: 1,
        id: 52,
        effects: HOST_R,
    },
    HostNative {
        name: "time_date_from_epoch_period",
        arity: 1,
        id: 53,
        effects: HOST_R,
    },
    HostNative {
        name: "time_epoch",
        arity: 0,
        id: 54,
        effects: HOST_R,
    },
    HostNative {
        name: "time_format",
        arity: 2,
        id: 55,
        effects: HOST_R,
    },
    HostNative {
        name: "time_parse",
        arity: 2,
        id: 56,
        effects: HOST_R,
    },
    HostNative {
        name: "env_args",
        arity: 0,
        id: 57,
        effects: ENV,
    },
    HostNative {
        name: "env_var",
        arity: 1,
        id: 58,
        effects: ENV,
    },
    HostNative {
        name: "env_set_var",
        arity: 2,
        id: 59,
        effects: ENV,
    },
    HostNative {
        name: "env_remove_var",
        arity: 1,
        id: 60,
        effects: ENV,
    },
    HostNative {
        name: "env_cwd",
        arity: 0,
        id: 61,
        effects: ENV,
    },
    HostNative {
        name: "env_set_cwd",
        arity: 1,
        id: 62,
        effects: ENV,
    },
    HostNative {
        name: "env_exit",
        arity: 1,
        id: 63,
        effects: EXEC,
    },
    HostNative {
        name: "env_exec",
        arity: 2,
        id: 64,
        effects: EXEC,
    },
    HostNative {
        name: "ord",
        arity: 1,
        id: 65,
        effects: HOST_R,
    },
    HostNative {
        name: "char",
        arity: 1,
        id: 66,
        effects: HOST_R,
    },
    HostNative {
        name: "hash_string",
        arity: 1,
        id: 67,
        effects: HOST_R,
    },
    HostNative {
        name: "thread_spawn",
        arity: 1,
        id: 68,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_join",
        arity: 1,
        id: 69,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_detach",
        arity: 1,
        id: 70,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_channel",
        arity: 0,
        id: 71,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_send",
        arity: 2,
        id: 72,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_recv",
        arity: 1,
        id: 73,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_try_send",
        arity: 2,
        id: 74,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_try_recv",
        arity: 1,
        id: 75,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_close",
        arity: 1,
        id: 76,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_mutex",
        arity: 1,
        id: 77,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_with_lock",
        arity: 2,
        id: 78,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_lock",
        arity: 1,
        id: 79,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_try_lock",
        arity: 1,
        id: 80,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_unlock",
        arity: 1,
        id: 81,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_rwlock",
        arity: 1,
        id: 82,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_with_read",
        arity: 2,
        id: 83,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_with_write",
        arity: 2,
        id: 84,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_try_read",
        arity: 2,
        id: 85,
        effects: THREAD_R,
    },
    HostNative {
        name: "thread_try_write",
        arity: 2,
        id: 86,
        effects: THREAD_R,
    },
    HostNative {
        name: "packed_dot",
        arity: 3,
        id: 87,
        effects: PURE,
    },
    HostNative {
        name: "packed_matmul",
        arity: 3,
        id: 88,
        effects: PURE,
    },
    HostNative {
        name: "packed_matrix_zip",
        arity: 3,
        id: 89,
        effects: PURE,
    },
    HostNative {
        name: "packed_matrix_neg",
        arity: 2,
        id: 90,
        effects: PURE,
    },
    HostNative {
        name: "packed_vec_arith",
        arity: 3,
        id: 91,
        effects: PURE,
    },
    HostNative {
        name: "wait_ready",
        arity: 0,
        id: 92,
        effects: SUSPEND,
    },
    HostNative {
        name: "write_from",
        arity: 3,
        id: 93,
        effects: WRITE_R,
    },
    HostNative {
        name: "gc_root",
        arity: 1,
        id: 94,
        effects: GC_R,
    },
    HostNative {
        name: "gc_unroot",
        arity: 1,
        id: 95,
        effects: GC_R,
    },
    HostNative {
        name: "gc_get",
        arity: 1,
        id: 96,
        effects: GC_R,
    },
    HostNative {
        name: "gc_weak",
        arity: 1,
        id: 97,
        effects: GC_R,
    },
    HostNative {
        name: "gc_upgrade",
        arity: 1,
        id: 98,
        effects: GC_R,
    },
    HostNative {
        name: "gc_heap_bytes",
        arity: 0,
        id: 99,
        effects: GC_R,
    },
    HostNative {
        name: "gc_collect",
        arity: 0,
        id: 100,
        effects: GC_R,
    },
    HostNative {
        name: "gc_register_finalizer",
        arity: 2,
        id: 101,
        effects: GC_R,
    },
    HostNative {
        name: "math_sin",
        arity: 1,
        id: 102,
        effects: PURE,
    },
    HostNative {
        name: "math_cos",
        arity: 1,
        id: 103,
        effects: PURE,
    },
    HostNative {
        name: "math_tan",
        arity: 1,
        id: 104,
        effects: PURE,
    },
    HostNative {
        name: "math_sqrt",
        arity: 1,
        id: 105,
        effects: PURE,
    },
    HostNative {
        name: "math_floor",
        arity: 1,
        id: 106,
        effects: PURE,
    },
    HostNative {
        name: "math_ceil",
        arity: 1,
        id: 107,
        effects: PURE,
    },
    HostNative {
        name: "math_exp",
        arity: 1,
        id: 108,
        effects: PURE,
    },
    HostNative {
        name: "math_ln",
        arity: 1,
        id: 109,
        effects: PURE,
    },
    HostNative {
        name: "math_pow",
        arity: 2,
        id: 110,
        effects: PURE,
    },
    HostNative {
        name: "vec_with_capacity",
        arity: 1,
        id: 111,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_capacity",
        arity: 1,
        id: 112,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_reserve",
        arity: 2,
        id: 113,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_clear",
        arity: 1,
        id: 114,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_pop",
        arity: 1,
        id: 115,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_insert",
        arity: 3,
        id: 116,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_remove",
        arity: 2,
        id: 117,
        effects: HEAP_R,
    },
    HostNative {
        name: "vec_from_array",
        arity: 1,
        id: 118,
        effects: HEAP_R,
    },
    HostNative {
        name: "stream_attach",
        arity: 6,
        id: 119,
        effects: PARK_R,
    },
    HostNative {
        name: "stream_park",
        arity: 1,
        id: 120,
        effects: PARK_R,
    },
    HostNative {
        name: "clock_wall_nanos",
        arity: 0,
        id: 121,
        effects: HOST,
    },
    HostNative {
        name: "clock_mono_nanos",
        arity: 0,
        id: 122,
        effects: HOST,
    },
    HostNative {
        name: "clock_sleep_ms",
        arity: 1,
        id: 123,
        effects: HOST,
    },
    HostNative {
        name: "result_unit_probe",
        arity: 1,
        id: 124,
        effects: HOST_R,
    },
    HostNative {
        name: "math_atan",
        arity: 1,
        id: 125,
        effects: PURE,
    },
    HostNative {
        name: "math_atan2",
        arity: 2,
        id: 126,
        effects: PURE,
    },
    HostNative {
        name: "math_asin",
        arity: 1,
        id: 127,
        effects: PURE,
    },
    HostNative {
        name: "math_acos",
        arity: 1,
        id: 128,
        effects: PURE,
    },
    HostNative {
        name: "math_log10",
        arity: 1,
        id: 129,
        effects: PURE,
    },
    HostNative {
        name: "math_log2",
        arity: 1,
        id: 130,
        effects: PURE,
    },
    HostNative {
        name: "math_cbrt",
        arity: 1,
        id: 131,
        effects: PURE,
    },
    HostNative {
        name: "math_rem",
        arity: 2,
        id: 132,
        effects: PURE,
    },
    HostNative {
        name: "math_sinh",
        arity: 1,
        id: 133,
        effects: PURE,
    },
    HostNative {
        name: "math_cosh",
        arity: 1,
        id: 134,
        effects: PURE,
    },
    HostNative {
        name: "math_tanh",
        arity: 1,
        id: 135,
        effects: PURE,
    },
    HostNative {
        name: "simd_axpy_reduce",
        arity: 5,
        id: 136,
        effects: PURE,
    },
    HostNative {
        name: "thread_spawn_shared",
        arity: 1,
        id: 137,
        effects: THREAD_R,
    },
    HostNative {
        name: "stream_fd",
        arity: 1,
        id: 138,
        effects: PARK_R,
    },
    HostNative {
        name: "string_byte_at",
        arity: 2,
        id: 139,
        effects: TEXT_R,
    },
    HostNative {
        name: "string_slice_bytes",
        arity: 3,
        id: 140,
        effects: TEXT_R,
    },
    HostNative {
        name: "string_find_from",
        arity: 3,
        id: 141,
        effects: TEXT_R,
    },
    HostNative {
        name: "string_rfind",
        arity: 2,
        id: 142,
        effects: TEXT_R,
    },
    HostNative {
        name: "string_match_at",
        arity: 3,
        id: 143,
        effects: TEXT_R,
    },
];

/// First packed-LA HostInvoke (`packed_dot`).
pub const PACKED_DOT_ID: u16 = 87;
pub const PACKED_MATMUL_ID: u16 = 88;
pub const PACKED_MATRIX_ZIP_ID: u16 = 89;
pub const PACKED_MATRIX_NEG_ID: u16 = 90;
/// Last packed-LA HostInvoke (`packed_vec_arith`).
pub const PACKED_VEC_ARITH_ID: u16 = 91;
/// Frozen HostInvoke id for `math_sin` (first frozen prelude math).
pub const MATH_SIN_ID: u16 = 102;
/// Frozen HostInvoke id for `math_pow` (last frozen prelude math).
pub const MATH_POW_ID: u16 = 110;
/// Frozen HostInvoke id for `stream_attach`.
pub const STREAM_ATTACH_ID: u16 = 119;
/// Frozen HostInvoke id for `stream_park`.
pub const STREAM_PARK_ID: u16 = 120;
/// HostInvoke id for `clock_wall_nanos`.
pub const CLOCK_WALL_NANOS_ID: u16 = 121;
/// HostInvoke id for `clock_mono_nanos`.
pub const CLOCK_MONO_NANOS_ID: u16 = 122;
/// HostInvoke id for `clock_sleep_ms`.
pub const CLOCK_SLEEP_MS_ID: u16 = 123;
/// HostInvoke id for `result_unit_probe` (`Result<(), IoError>` pack helper).
pub const RESULT_UNIT_PROBE_ID: u16 = 124;
/// First HostInvoke id of the M1 `prelude::math` expansion (`math_atan`).
pub const MATH_ATAN_ID: u16 = 125;
/// HostInvoke id for `math_tanh` (last M1 math native).
pub const MATH_TANH_ID: u16 = 135;
/// Compiler-only HostInvoke id for MIR saxpy-reduce packs (COI-286).
pub const SIMD_AXPY_REDUCE_ID: u16 = 136;
pub const SIMD_AXPY_REDUCE_NATIVE: &str = "simd_axpy_reduce";
pub const THREAD_SPAWN_SHARED_ID: u16 = 137;
pub const THREAD_SPAWN_SHARED_NATIVE: &str = "thread_spawn_shared";
pub const STREAM_FD_ID: u16 = 138;
pub const STREAM_FD_NATIVE: &str = "stream_fd";
/// First byte-offset `string` native (`string_byte_at`); the block runs
/// through [`STRING_MATCH_AT_ID`] (archive minor 31).
pub const STRING_BYTE_AT_ID: u16 = 139;
/// Last byte-offset `string` native (`string_match_at`).
pub const STRING_MATCH_AT_ID: u16 = 143;

pub const STREAM_ATTACH_NATIVE: &str = "stream_attach";
pub const STREAM_PARK_NATIVE: &str = "stream_park";
pub const CLOCK_WALL_NANOS_NATIVE: &str = "clock_wall_nanos";
pub const CLOCK_MONO_NANOS_NATIVE: &str = "clock_mono_nanos";
pub const CLOCK_SLEEP_MS_NATIVE: &str = "clock_sleep_ms";
pub const RESULT_UNIT_PROBE_NATIVE: &str = "result_unit_probe";

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
/// Reserved operand code. Decoders treat this as boxed (not a niche).
pub const HOST_ENUM_LAYOUT_RESERVED: u32 = 3;

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

pub const GC_COLLECT_NATIVE: &str = "gc_collect";
pub const GC_REGISTER_FINALIZER_NATIVE: &str = "gc_register_finalizer";

const _: () = {
    assert!(HOST_NATIVES.len() == 144);
    assert!(HOST_NATIVES[119].id == STREAM_ATTACH_ID);
    assert!(HOST_NATIVES[120].id == STREAM_PARK_ID);
    assert!(HOST_NATIVES[121].id == CLOCK_WALL_NANOS_ID);
    assert!(HOST_NATIVES[122].id == CLOCK_MONO_NANOS_ID);
    assert!(HOST_NATIVES[123].id == CLOCK_SLEEP_MS_ID);
    assert!(HOST_NATIVES[124].id == RESULT_UNIT_PROBE_ID);
    assert!(HOST_NATIVES[125].id == MATH_ATAN_ID);
    assert!(HOST_NATIVES[135].id == MATH_TANH_ID);
    assert!(HOST_NATIVES[136].id == SIMD_AXPY_REDUCE_ID);
    assert!(HOST_NATIVES[137].id == THREAD_SPAWN_SHARED_ID);
    assert!(HOST_NATIVES[138].id == STREAM_FD_ID);
    assert!(HOST_NATIVES[139].id == STRING_BYTE_AT_ID);
    assert!(HOST_NATIVES[143].id == STRING_MATCH_AT_ID);
};

/// HostInvoke id for a standard native name.
pub fn host_native_id(name: &str) -> Option<usize> {
    HOST_NATIVES
        .iter()
        .find(|e| e.name == name)
        .map(|e| e.id as usize)
}

/// `(name, id)` pairs for compiler `register_native_id`.
pub fn host_native_ids() -> impl Iterator<Item = (&'static str, usize)> {
    HOST_NATIVES.iter().map(|e| (e.name, e.id as usize))
}

/// True when `name` is a libc/CRT process-exec symbol (not `env::exec`).
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
        assert_eq!(HOST_NATIVES[24].name, "udp_local_port");
        for (i, e) in HOST_NATIVES.iter().enumerate() {
            assert_eq!(e.id as usize, i, "{} id drifted", e.name);
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
