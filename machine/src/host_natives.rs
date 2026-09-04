//! Standard host-native registry for compile-time and packaged-runtime wiring.
//!
//! Registration order is ABI: `HostInvoke` fn_ids must match between the
//! compiler's `register_native_id` map and the runtime `Natives` table.

use std::sync::Arc;

use common::Value;

use crate::math_libm::MATH_LIBM_WIRING;
use crate::{
    packed_dot, packed_matmul, packed_matrix_neg, packed_matrix_zip, packed_vec_arith,
    FfiError, FfiSignature, FfiType, HostClosureFn, HostOp, NativeFn, CLOCK_WIRING, ENV_WIRING, FS_WIRING,
    PACKED_DOT,
    PACKED_MATMUL, PACKED_MATRIX_NEG, PACKED_MATRIX_ZIP, PACKED_VEC_ARITH,
};

use crate::GC_WIRING;
use crate::vec_ops::VEC_WIRING;

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
/// Virtual time slots are panic stubs so `stream_attach` / `stream_park`
/// stay HostInvoke 120 / 121. Append-only from this table.
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
    push_io_natives(&mut out, &mut register_id);
    push_wiring(&mut out, &mut register_id, FS_WIRING, "fs");
    push_removed_time_stubs(&mut out, &mut register_id);
    push_wiring(&mut out, &mut register_id, ENV_WIRING, "env");
    push_prelude_char_ord(&mut out, &mut register_id);
    push_thread_natives(&mut out, &mut register_id);
    push_packed_la(&mut out, &mut register_id);
    // Append-only: keep prior HostInvoke ids stable across ARCHIVE_MINOR bumps.
    push_io_wait_ready(&mut out, &mut register_id);
    push_io_write_from(&mut out, &mut register_id);
    push_wiring(&mut out, &mut register_id, GC_WIRING, "gc");
    push_gc_host_ops(&mut out, &mut register_id);
    push_math_libm(&mut out, &mut register_id);
    // Append-only after math_libm: Vec helpers.
    push_wiring(&mut out, &mut register_id, VEC_WIRING, "vec");
    push_unused_119(&mut out, &mut register_id);
    // Append-only after unused_119. `stream_attach` / `stream_park` are the
    // package-IO hooks (coil-tls uses these via `dload`, not VM TLS natives).
    push_stream_attach(&mut out, &mut register_id);
    push_stream_park(&mut out, &mut register_id);
    // Append-only after stream_park: process clocks (no Instant HashMap).
    push_wiring(&mut out, &mut register_id, CLOCK_WIRING, "clock");
    // Append-only after clocks: `Result<(), IoError>` pack probe (no IO).
    push_result_unit_probe(&mut out, &mut register_id);
    assert_eq!(
        out.len(),
        common::HOST_NATIVES.len(),
        "runtime host table length must match common::HOST_NATIVES"
    );
    out
}

pub use common::{
    CLOCK_MONO_NANOS_NATIVE, CLOCK_SLEEP_MS_NATIVE, CLOCK_WALL_NANOS_NATIVE,
    STREAM_ATTACH_NATIVE, STREAM_PARK_NATIVE, UNUSED_HOST_119_NATIVE,
};

fn push_unused_119(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    let sig = FfiSignature::from_parts(
        UNUSED_HOST_119_NATIVE.to_string(),
        vec![FfiType::Int],
        FfiType::Int,
    )
    .expect("unused_119 signature");
    let id = out.len();
    register_id(UNUSED_HOST_119_NATIVE, id);
    out.push(Arc::new(HostClosureFn::new(sig, |_heap, _args| {
        Ok(Some(Value::from(0i64)))
    })));
}

fn push_stream_attach(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
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
    register_id: &mut impl FnMut(&str, usize),
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

fn push_stream_park(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
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
        let r = crate::stream_attach::stream_park(heap, args[0]);
        Ok(Some(as_result_unit(heap, r)))
    })));
}

/// Register each native on `machine` (same order as [`build_standard_host_natives`]).
pub fn wire_standard_host_natives<const N: usize>(machine: &mut crate::Machine<N>) {
    for native in build_standard_host_natives(|_name, _id| {}) {
        machine.register_native(native);
    }
}

fn push_removed_time_stubs(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
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

fn push_gc_host_ops(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
) {
    let collect_sig = FfiSignature::from_parts(
        crate::GC_COLLECT_NATIVE.to_string(),
        vec![],
        FfiType::Int,
    )
    .expect("gc_collect signature");
    let collect_id = out.len();
    register_id(crate::GC_COLLECT_NATIVE, collect_id);
    out.push(Arc::new(
        HostClosureFn::new(collect_sig, |_heap, _args| {
            Err(FfiError::Unsupported(
                "gc_collect is HostOp::Collect (VM hook only)".into(),
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
                "gc_register_finalizer is HostOp::RegisterFinalizer (VM hook only)".into(),
            ))
        })
        .with_host_op(HostOp::RegisterFinalizer),
    ));
}

fn push_wiring(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
    table: &[(&str, usize, fn(&mut crate::Heap, &[Value]) -> Value)],
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
                    .map_err(|msg| FfiError::Unsupported(msg.to_string()))
            })));
            continue;
        }
        out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
            Ok(Some(host(heap, args)))
        })));
    }
}

fn push_math_libm(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    for &(name, arity, host) in MATH_LIBM_WIRING {
        let args = vec![FfiType::Float; arity];
        let sig = FfiSignature::from_parts(name.to_string(), args, FfiType::Float)
            .expect("math libm native signature");
        let id = out.len();
        register_id(name, id);
        out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
            Ok(Some(host(heap, args)))
        })));
    }
}

fn push_prelude_char_ord(
    out: &mut Vec<Arc<dyn NativeFn>>,
    register_id: &mut impl FnMut(&str, usize),
) {
    use crate::char_ord::{prelude_char, prelude_hash_string, prelude_ord};

    let ord_sig = FfiSignature::from_parts("ord".to_string(), vec![FfiType::Int], FfiType::Int)
        .expect("ord signature");
    let ord_id = out.len();
    register_id("ord", ord_id);
    out.push(Arc::new(HostClosureFn::new(ord_sig, |heap, args| {
        Ok(Some(prelude_ord(heap, args)))
    })));

    let char_sig = FfiSignature::from_parts("char".to_string(), vec![FfiType::Int], FfiType::Int)
        .expect("char signature");
    let char_id = out.len();
    register_id("char", char_id);
    out.push(Arc::new(HostClosureFn::new(char_sig, |heap, args| {
        Ok(Some(prelude_char(heap, args)))
    })));

    let hash_sig = FfiSignature::from_parts(
        "hash_string".to_string(),
        vec![FfiType::String],
        FfiType::Int,
    )
    .expect("hash_string signature");
    let hash_id = out.len();
    register_id("hash_string", hash_id);
    out.push(Arc::new(HostClosureFn::new(hash_sig, |heap, args| {
        Ok(Some(prelude_hash_string(heap, args)))
    })));
}

fn push_packed_la(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    let specs: &[(&str, usize, fn(&mut crate::Heap, &[Value]) -> Value)] = &[
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
            Ok(Some(kernel(heap, args)))
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
        |heap, args| Ok(Some(packed_vec_arith(heap, args))),
    )));
}

fn push_io_wait_ready(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    let sig = FfiSignature::from_parts("wait_ready".to_string(), vec![], FfiType::Int)
        .expect("wait_ready signature");
    let id = out.len();
    register_id("wait_ready", id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, _args| {
        Ok(Some(crate::io::io_wait_ready(heap)))
    })));
}

/// Write `buf[offset..]` without allocating a Coil suffix array.
fn push_io_write_from(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    use crate::io::{as_result_int, stream_write_from};
    let sig = FfiSignature::from_parts(
        "write_from".to_string(),
        vec![FfiType::Int, FfiType::Int, FfiType::Int],
        FfiType::Int,
    )
    .expect("write_from signature");
    let id = out.len();
    register_id("write_from", id);
    out.push(Arc::new(HostClosureFn::new(sig, |heap, args| {
        let r = stream_write_from(heap, args[0], args[1], args[2].as_int());
        Ok(Some(as_result_int(heap, r)))
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

fn push_io_natives(out: &mut Vec<Arc<dyn NativeFn>>, register_id: &mut impl FnMut(&str, usize)) {
    use crate::io::{
        as_result_int, as_result_option_int, as_result_unit, as_result_value, from_bytes, io_drive,
        stream_await_readable, stream_await_writable, stream_close, stream_open, stream_read,
        stream_stderr, stream_stdin, stream_stdout, stream_write, tcp_accept, tcp_connect,
        tcp_connect_timeout, tcp_listen, tcp_local_addr, tcp_peer_addr, tcp_set_nodelay,
        tcp_shutdown, to_bytes, udp_bind, udp_connect, udp_local_port, udp_recv_from, udp_send_to,
        value_as_open_mode, value_as_string,
    };

    for &kind in IoKind::all() {
        let name = kind.native_name().to_string();
        let arity = kind.arity();
        let args = vec![FfiType::Int; arity];
        let sig = FfiSignature::from_parts(name.clone(), args, FfiType::Int)
            .expect("io native arity/signature");
        let id = out.len();
        register_id(&name, id);
        out.push(Arc::new(HostClosureFn::new(sig, move |heap, args| {
            let v = match kind {
                // Stdio handles are `() -> Stream` (not Result).
                IoKind::Stdin => stream_stdin(heap).unwrap_or_default(),
                IoKind::Stdout => stream_stdout(heap).unwrap_or_default(),
                IoKind::Stderr => stream_stderr(heap).unwrap_or_default(),
                IoKind::Open => {
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
                }
                IoKind::Close => {
                    let r = stream_close(heap, args[0]);
                    as_result_unit(heap, r)
                }
                IoKind::Read => {
                    let r = stream_read(heap, args[0], args[1]);
                    as_result_option_int(heap, r)
                }
                IoKind::Write => {
                    let r = stream_write(heap, args[0], args[1]);
                    as_result_int(heap, r)
                }
                IoKind::AwaitReadable => match stream_await_readable(heap, args[0]) {
                    Ok(v) => return Ok(v),
                    Err(tag) => as_result_unit(heap, Err(tag)),
                },
                IoKind::AwaitWritable => match stream_await_writable(heap, args[0]) {
                    Ok(v) => return Ok(v),
                    Err(tag) => as_result_unit(heap, Err(tag)),
                },
                IoKind::Drive => return Ok(Some(io_drive(heap))),
                IoKind::FromBytes => {
                    let r = from_bytes(heap, args[0]);
                    as_result_value(heap, r)
                }
                IoKind::ToBytes => to_bytes(heap, args[0]),
                IoKind::TcpConnect => {
                    let host = match value_as_string(heap, args[0]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = tcp_connect(heap, &host, args[1].as_int());
                    as_result_value(heap, r)
                }
                IoKind::TcpConnectTimeout => {
                    let host = match value_as_string(heap, args[0]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = tcp_connect_timeout(heap, &host, args[1].as_int(), args[2].as_int());
                    as_result_value(heap, r)
                }
                IoKind::TcpListen => {
                    let host = match value_as_string(heap, args[0]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = tcp_listen(heap, &host, args[1].as_int());
                    as_result_value(heap, r)
                }
                IoKind::TcpAccept => {
                    let r = tcp_accept(heap, args[0]);
                    as_result_value(heap, r)
                }
                IoKind::TcpPeerAddr => {
                    let r = tcp_peer_addr(heap, args[0]);
                    as_result_value(heap, r)
                }
                IoKind::TcpLocalAddr => {
                    let r = tcp_local_addr(heap, args[0]);
                    as_result_value(heap, r)
                }
                IoKind::TcpSetNodelay => {
                    let r = tcp_set_nodelay(heap, args[0], args[1].as_bool());
                    as_result_unit(heap, r)
                }
                IoKind::TcpShutdown => {
                    let r = tcp_shutdown(heap, args[0], args[1].as_int());
                    as_result_unit(heap, r)
                }
                IoKind::UdpBind => {
                    let host = match value_as_string(heap, args[0]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = udp_bind(heap, &host, args[1].as_int());
                    as_result_value(heap, r)
                }
                IoKind::UdpConnect => {
                    let host = match value_as_string(heap, args[0]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = udp_connect(heap, &host, args[1].as_int());
                    as_result_value(heap, r)
                }
                IoKind::UdpSendTo => {
                    let host = match value_as_string(heap, args[2]) {
                        Ok(s) => s,
                        Err(tag) => {
                            return Ok(Some(as_result_value(heap, Err(tag))));
                        }
                    };
                    let r = udp_send_to(heap, args[0], args[1], &host, args[3].as_int());
                    as_result_int(heap, r)
                }
                IoKind::UdpRecvFrom => {
                    let r = udp_recv_from(heap, args[0], args[1]);
                    as_result_value(heap, r)
                }
                IoKind::UdpLocalPort => {
                    let r = udp_local_port(heap, args[0]).map(Value::from);
                    as_result_value(heap, r)
                }
            };
            Ok(Some(v))
        })));
    }
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
    register_id: &mut impl FnMut(&str, usize),
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
    fn unused_119_is_appended_after_vec_helpers() {
        let mut names = Vec::new();
        build_standard_host_natives(|name, _id| names.push(name.to_string()));
        let hole = names
            .iter()
            .position(|n| n == UNUSED_HOST_119_NATIVE)
            .expect("unused_119");
        assert_eq!(
            names.get(hole + 1).map(String::as_str),
            Some(STREAM_ATTACH_NATIVE)
        );
        assert_eq!(
            names.get(hole + 2).map(String::as_str),
            Some(STREAM_PARK_NATIVE)
        );
        assert_eq!(
            names.get(hole + 3).map(String::as_str),
            Some(CLOCK_WALL_NANOS_NATIVE)
        );
        assert_eq!(
            names.get(hole + 4).map(String::as_str),
            Some(CLOCK_MONO_NANOS_NATIVE)
        );
        assert_eq!(
            names.get(hole + 5).map(String::as_str),
            Some(CLOCK_SLEEP_MS_NATIVE)
        );
        assert_eq!(
            names.last().map(String::as_str),
            Some(common::RESULT_UNIT_PROBE_NATIVE)
        );
    }

    /// COI-232: 120/121 are live attach/park, not leftover TLS/crypto stubs.
    #[test]
    fn stream_attach_and_park_own_hostinvoke_120_and_121() {
        let mut map = std::collections::HashMap::new();
        build_standard_host_natives(|name, id| {
            map.insert(name.to_string(), id);
        });
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(120));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(121));
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
        let hole = names
            .iter()
            .position(|n| n == UNUSED_HOST_119_NATIVE)
            .expect("unused_119");
        assert_eq!(names[hole + 1], STREAM_ATTACH_NATIVE);
        assert_eq!(names[hole + 2], STREAM_PARK_NATIVE);
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(hole + 1));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(hole + 2));
        // Attach/park ids stay put after virtual time became panic stubs.
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(120));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(121));
        assert_eq!(hole, 119);
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
        assert_eq!(map.get(STREAM_ATTACH_NATIVE).copied(), Some(120));
        assert_eq!(map.get(STREAM_PARK_NATIVE).copied(), Some(121));
        assert_eq!(map.get(UNUSED_HOST_119_NATIVE).copied(), Some(119));
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

    /// Instant handles were a VM HashMap leak; drop lives in coil-time, not HostInvoke.
    #[test]
    fn instant_drop_is_not_a_vm_host() {
        let mut names = Vec::new();
        build_standard_host_natives(|name, _id| names.push(name.to_string()));
        assert!(
            names
                .iter()
                .all(|n| n != "time_instant_drop" && n != "instant_drop"),
            "Instant drop must not be a VM host: {names:?}"
        );
        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src = crate_dir.join("src");
        let mut leftover = Vec::new();
        for rel in ["lib.rs", "host_natives.rs", "vm.rs", "memory/heap.rs"] {
            let text = std::fs::read_to_string(src.join(rel)).unwrap_or_default();
            let prod = text.split("#[cfg(test)]").next().unwrap_or(&text);
            if prod.contains("NEXT_INSTANT_ID") || prod.contains("static INSTANTS") {
                leftover.push(rel);
            }
        }
        assert!(
            leftover.is_empty(),
            "VM Instant registry must stay gone: {leftover:?}"
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
        assert_eq!(common::host_native_id(UNUSED_HOST_119_NATIVE), Some(119));
        assert_eq!(common::host_native_id(STREAM_ATTACH_NATIVE), Some(120));
        assert_eq!(common::host_native_id(STREAM_PARK_NATIVE), Some(121));
        assert_eq!(common::host_native_id(CLOCK_WALL_NANOS_NATIVE), Some(122));
        assert_eq!(common::host_native_id(CLOCK_MONO_NANOS_NATIVE), Some(123));
        assert_eq!(common::host_native_id(CLOCK_SLEEP_MS_NATIVE), Some(124));
        assert_eq!(
            common::host_native_id(common::RESULT_UNIT_PROBE_NATIVE),
            Some(125)
        );
    }
}
