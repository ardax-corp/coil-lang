//! Append-only HostInvoke catalog shared by compiler and VM.
//!
//! Ids are the table index. New natives go at the end. Do not reorder or
//! reuse slots. Frozen: 119 unused, 120 = `stream_attach`, 121 = `stream_park`.
//! Append-only after that: 122–124 = `clock_*`, 125 = `result_unit_probe`.

/// One standard host native: stable name, declared arity, HostInvoke id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostNative {
    pub name: &'static str,
    /// Declared arity (signature length). `thread_spawn` and `packed_vec_arith`
    /// also accept a range at runtime; the id still keys off this row.
    pub arity: u8,
    pub id: u16,
}

/// Standard host natives in HostInvoke id order.
pub const HOST_NATIVES: &[HostNative] = &[
    HostNative {
        name: "stdin",
        arity: 0,
        id: 0,
    },
    HostNative {
        name: "stdout",
        arity: 0,
        id: 1,
    },
    HostNative {
        name: "stderr",
        arity: 0,
        id: 2,
    },
    HostNative {
        name: "open",
        arity: 2,
        id: 3,
    },
    HostNative {
        name: "close",
        arity: 1,
        id: 4,
    },
    HostNative {
        name: "read",
        arity: 2,
        id: 5,
    },
    HostNative {
        name: "write",
        arity: 2,
        id: 6,
    },
    HostNative {
        name: "await_readable",
        arity: 1,
        id: 7,
    },
    HostNative {
        name: "await_writable",
        arity: 1,
        id: 8,
    },
    HostNative {
        name: "drive",
        arity: 0,
        id: 9,
    },
    HostNative {
        name: "from_bytes",
        arity: 1,
        id: 10,
    },
    HostNative {
        name: "to_bytes",
        arity: 1,
        id: 11,
    },
    HostNative {
        name: "tcp_connect",
        arity: 2,
        id: 12,
    },
    HostNative {
        name: "tcp_connect_timeout",
        arity: 3,
        id: 13,
    },
    HostNative {
        name: "tcp_listen",
        arity: 2,
        id: 14,
    },
    HostNative {
        name: "tcp_accept",
        arity: 1,
        id: 15,
    },
    HostNative {
        name: "tcp_peer_addr",
        arity: 1,
        id: 16,
    },
    HostNative {
        name: "tcp_local_addr",
        arity: 1,
        id: 17,
    },
    HostNative {
        name: "tcp_set_nodelay",
        arity: 2,
        id: 18,
    },
    HostNative {
        name: "tcp_shutdown",
        arity: 2,
        id: 19,
    },
    HostNative {
        name: "udp_bind",
        arity: 2,
        id: 20,
    },
    HostNative {
        name: "udp_connect",
        arity: 2,
        id: 21,
    },
    HostNative {
        name: "udp_send_to",
        arity: 4,
        id: 22,
    },
    HostNative {
        name: "udp_recv_from",
        arity: 2,
        id: 23,
    },
    HostNative {
        name: "udp_local_port",
        arity: 1,
        id: 24,
    },
    HostNative {
        name: "fs_exists",
        arity: 1,
        id: 25,
    },
    HostNative {
        name: "fs_is_file",
        arity: 1,
        id: 26,
    },
    HostNative {
        name: "fs_is_dir",
        arity: 1,
        id: 27,
    },
    HostNative {
        name: "fs_is_symlink",
        arity: 1,
        id: 28,
    },
    HostNative {
        name: "fs_metadata",
        arity: 1,
        id: 29,
    },
    HostNative {
        name: "fs_create_dir",
        arity: 1,
        id: 30,
    },
    HostNative {
        name: "fs_create_dir_all",
        arity: 1,
        id: 31,
    },
    HostNative {
        name: "fs_remove_file",
        arity: 1,
        id: 32,
    },
    HostNative {
        name: "fs_remove_dir",
        arity: 1,
        id: 33,
    },
    HostNative {
        name: "fs_remove_dir_all",
        arity: 1,
        id: 34,
    },
    HostNative {
        name: "fs_rename",
        arity: 2,
        id: 35,
    },
    HostNative {
        name: "fs_copy",
        arity: 2,
        id: 36,
    },
    HostNative {
        name: "fs_read_link",
        arity: 1,
        id: 37,
    },
    HostNative {
        name: "fs_symlink",
        arity: 2,
        id: 38,
    },
    HostNative {
        name: "fs_list_dir",
        arity: 1,
        id: 39,
    },
    HostNative {
        name: "fs_realpath",
        arity: 1,
        id: 40,
    },
    HostNative {
        name: "time_timestamp",
        arity: 0,
        id: 41,
    },
    HostNative {
        name: "time_sleep_ms",
        arity: 1,
        id: 42,
    },
    HostNative {
        name: "time_instant_now",
        arity: 0,
        id: 43,
    },
    HostNative {
        name: "time_elapsed_nanos",
        arity: 1,
        id: 44,
    },
    HostNative {
        name: "time_elapsed_millis",
        arity: 1,
        id: 45,
    },
    HostNative {
        name: "time_period",
        arity: 9,
        id: 46,
    },
    HostNative {
        name: "time_add",
        arity: 2,
        id: 47,
    },
    HostNative {
        name: "time_sub",
        arity: 2,
        id: 48,
    },
    HostNative {
        name: "time_period_add",
        arity: 2,
        id: 49,
    },
    HostNative {
        name: "time_period_sub",
        arity: 2,
        id: 50,
    },
    HostNative {
        name: "time_date",
        arity: 0,
        id: 51,
    },
    HostNative {
        name: "time_date_from_period",
        arity: 1,
        id: 52,
    },
    HostNative {
        name: "time_date_from_epoch_period",
        arity: 1,
        id: 53,
    },
    HostNative {
        name: "time_epoch",
        arity: 0,
        id: 54,
    },
    HostNative {
        name: "time_format",
        arity: 2,
        id: 55,
    },
    HostNative {
        name: "time_parse",
        arity: 2,
        id: 56,
    },
    HostNative {
        name: "env_args",
        arity: 0,
        id: 57,
    },
    HostNative {
        name: "env_var",
        arity: 1,
        id: 58,
    },
    HostNative {
        name: "env_set_var",
        arity: 2,
        id: 59,
    },
    HostNative {
        name: "env_remove_var",
        arity: 1,
        id: 60,
    },
    HostNative {
        name: "env_cwd",
        arity: 0,
        id: 61,
    },
    HostNative {
        name: "env_set_cwd",
        arity: 1,
        id: 62,
    },
    HostNative {
        name: "env_exit",
        arity: 1,
        id: 63,
    },
    HostNative {
        name: "env_exec",
        arity: 2,
        id: 64,
    },
    HostNative {
        name: "ord",
        arity: 1,
        id: 65,
    },
    HostNative {
        name: "char",
        arity: 1,
        id: 66,
    },
    HostNative {
        name: "hash_string",
        arity: 1,
        id: 67,
    },
    HostNative {
        name: "thread_spawn",
        arity: 1,
        id: 68,
    },
    HostNative {
        name: "thread_join",
        arity: 1,
        id: 69,
    },
    HostNative {
        name: "thread_detach",
        arity: 1,
        id: 70,
    },
    HostNative {
        name: "thread_channel",
        arity: 0,
        id: 71,
    },
    HostNative {
        name: "thread_send",
        arity: 2,
        id: 72,
    },
    HostNative {
        name: "thread_recv",
        arity: 1,
        id: 73,
    },
    HostNative {
        name: "thread_try_send",
        arity: 2,
        id: 74,
    },
    HostNative {
        name: "thread_try_recv",
        arity: 1,
        id: 75,
    },
    HostNative {
        name: "thread_close",
        arity: 1,
        id: 76,
    },
    HostNative {
        name: "thread_mutex",
        arity: 1,
        id: 77,
    },
    HostNative {
        name: "thread_with_lock",
        arity: 2,
        id: 78,
    },
    HostNative {
        name: "thread_lock",
        arity: 1,
        id: 79,
    },
    HostNative {
        name: "thread_try_lock",
        arity: 1,
        id: 80,
    },
    HostNative {
        name: "thread_unlock",
        arity: 1,
        id: 81,
    },
    HostNative {
        name: "thread_rwlock",
        arity: 1,
        id: 82,
    },
    HostNative {
        name: "thread_with_read",
        arity: 2,
        id: 83,
    },
    HostNative {
        name: "thread_with_write",
        arity: 2,
        id: 84,
    },
    HostNative {
        name: "thread_try_read",
        arity: 2,
        id: 85,
    },
    HostNative {
        name: "thread_try_write",
        arity: 2,
        id: 86,
    },
    HostNative {
        name: "packed_dot",
        arity: 3,
        id: 87,
    },
    HostNative {
        name: "packed_matmul",
        arity: 3,
        id: 88,
    },
    HostNative {
        name: "packed_matrix_zip",
        arity: 3,
        id: 89,
    },
    HostNative {
        name: "packed_matrix_neg",
        arity: 2,
        id: 90,
    },
    HostNative {
        name: "packed_vec_arith",
        arity: 3,
        id: 91,
    },
    HostNative {
        name: "wait_ready",
        arity: 0,
        id: 92,
    },
    HostNative {
        name: "write_from",
        arity: 3,
        id: 93,
    },
    HostNative {
        name: "gc_root",
        arity: 1,
        id: 94,
    },
    HostNative {
        name: "gc_unroot",
        arity: 1,
        id: 95,
    },
    HostNative {
        name: "gc_get",
        arity: 1,
        id: 96,
    },
    HostNative {
        name: "gc_weak",
        arity: 1,
        id: 97,
    },
    HostNative {
        name: "gc_upgrade",
        arity: 1,
        id: 98,
    },
    HostNative {
        name: "gc_heap_bytes",
        arity: 0,
        id: 99,
    },
    HostNative {
        name: "gc_collect",
        arity: 0,
        id: 100,
    },
    HostNative {
        name: "gc_register_finalizer",
        arity: 2,
        id: 101,
    },
    HostNative {
        name: "math_sin",
        arity: 1,
        id: 102,
    },
    HostNative {
        name: "math_cos",
        arity: 1,
        id: 103,
    },
    HostNative {
        name: "math_tan",
        arity: 1,
        id: 104,
    },
    HostNative {
        name: "math_sqrt",
        arity: 1,
        id: 105,
    },
    HostNative {
        name: "math_floor",
        arity: 1,
        id: 106,
    },
    HostNative {
        name: "math_ceil",
        arity: 1,
        id: 107,
    },
    HostNative {
        name: "math_exp",
        arity: 1,
        id: 108,
    },
    HostNative {
        name: "math_ln",
        arity: 1,
        id: 109,
    },
    HostNative {
        name: "math_pow",
        arity: 2,
        id: 110,
    },
    HostNative {
        name: "vec_with_capacity",
        arity: 1,
        id: 111,
    },
    HostNative {
        name: "vec_capacity",
        arity: 1,
        id: 112,
    },
    HostNative {
        name: "vec_reserve",
        arity: 2,
        id: 113,
    },
    HostNative {
        name: "vec_clear",
        arity: 1,
        id: 114,
    },
    HostNative {
        name: "vec_pop",
        arity: 1,
        id: 115,
    },
    HostNative {
        name: "vec_insert",
        arity: 3,
        id: 116,
    },
    HostNative {
        name: "vec_remove",
        arity: 2,
        id: 117,
    },
    HostNative {
        name: "vec_from_array",
        arity: 1,
        id: 118,
    },
    HostNative {
        name: "unused_119",
        arity: 1,
        id: 119,
    },
    HostNative {
        name: "stream_attach",
        arity: 6,
        id: 120,
    },
    HostNative {
        name: "stream_park",
        arity: 1,
        id: 121,
    },
    HostNative {
        name: "clock_wall_nanos",
        arity: 0,
        id: 122,
    },
    HostNative {
        name: "clock_mono_nanos",
        arity: 0,
        id: 123,
    },
    HostNative {
        name: "clock_sleep_ms",
        arity: 1,
        id: 124,
    },
    HostNative {
        name: "result_unit_probe",
        arity: 1,
        id: 125,
    },
];

/// Unused HostInvoke id (do not reuse; later ids stay put).
pub const UNUSED_HOST_119_ID: u16 = 119;
/// Frozen HostInvoke id for `stream_attach`.
pub const STREAM_ATTACH_ID: u16 = 120;
/// Frozen HostInvoke id for `stream_park`.
pub const STREAM_PARK_ID: u16 = 121;
/// HostInvoke id for `clock_wall_nanos`.
pub const CLOCK_WALL_NANOS_ID: u16 = 122;
/// HostInvoke id for `clock_mono_nanos`.
pub const CLOCK_MONO_NANOS_ID: u16 = 123;
/// HostInvoke id for `clock_sleep_ms`.
pub const CLOCK_SLEEP_MS_ID: u16 = 124;
/// HostInvoke id for `result_unit_probe` (`Result<(), IoError>` pack helper).
pub const RESULT_UNIT_PROBE_ID: u16 = 125;

pub const UNUSED_HOST_119_NATIVE: &str = "unused_119";
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
    assert!(HOST_NATIVES.len() == 126);
    assert!(HOST_NATIVES[119].id == UNUSED_HOST_119_ID);
    assert!(HOST_NATIVES[120].id == STREAM_ATTACH_ID);
    assert!(HOST_NATIVES[121].id == STREAM_PARK_ID);
    assert!(HOST_NATIVES[122].id == CLOCK_WALL_NANOS_ID);
    assert!(HOST_NATIVES[123].id == CLOCK_MONO_NANOS_ID);
    assert!(HOST_NATIVES[124].id == CLOCK_SLEEP_MS_ID);
    assert!(HOST_NATIVES[125].id == RESULT_UNIT_PROBE_ID);
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
    if let Some(stripped) = stem.strip_prefix("lib") {
        if !stripped.is_empty() {
            stem = stripped.to_string();
        }
    }
    stem
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_tail_ids() {
        assert_eq!(host_native_id(UNUSED_HOST_119_NATIVE), Some(119));
        assert_eq!(host_native_id(STREAM_ATTACH_NATIVE), Some(120));
        assert_eq!(host_native_id(STREAM_PARK_NATIVE), Some(121));
        assert_eq!(host_native_id(CLOCK_WALL_NANOS_NATIVE), Some(122));
        assert_eq!(host_native_id(CLOCK_MONO_NANOS_NATIVE), Some(123));
        assert_eq!(host_native_id(CLOCK_SLEEP_MS_NATIVE), Some(124));
        assert_eq!(host_native_id(RESULT_UNIT_PROBE_NATIVE), Some(125));
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
        assert_eq!(host_invoke_enum_layout(packed), HOST_ENUM_LAYOUT_OPTION_NICHE);
        assert_eq!(host_invoke_enum_layout(3), HOST_ENUM_LAYOUT_BOXED);
    }
}
