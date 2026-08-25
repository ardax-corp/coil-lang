//! Leftover HostInvoke for in-place TCP→TLS upgrade after rustls left coil.
//!
//! Bodies resolve dloaded `coil_tls_*` ([`crate::tls_native::preferred`] /
//! FFI search paths), call enable once, [`attach_enable_outcome`], then park
//! WouldBlock on [`reactor_wait_fd_no_help`] and pump via empty read/write
//! until handshake completes (COI-116). Never call enable again, never free a
//! WouldBlock session. `coil_tls_disable` is close_notify; `coil_tls_free`
//! is Drop.
//!
//! Ids reserved; do not reorder; do not bump `ARCHIVE_VERSION`.

use std::time::Instant;

use common::Value;

use crate::io::{
    IoErrorTag, duration_from_timeout_ms, reactor_wait_fd_no_help, stream_wait_handle,
    value_as_string, with_stream_mut,
};
use crate::io_reactor::Interest;
use crate::memory::{Heap, Member, ObjString, Object, StreamKind};
use crate::tls_native::{NativeEnable, TlsNativeAbi, attach_enable_outcome, resolve_preferred};

struct ClientEnableOpts {
    verify: bool,
    ca_pem: Option<String>,
    ca_path: Option<String>,
    timeout_ms: i64,
    alpn: String,
}

struct ServerEnableOpts {
    cert_pem: String,
    key_pem: String,
    timeout_ms: i64,
    client_ca_pem: String,
    alpn: String,
}

fn member_as_value(member: &Member) -> Result<Value, IoErrorTag> {
    match member {
        Member::Value(v) => Ok(*v),
        Member::Object(o) => Ok(Value::from(o.addr())),
    }
}

/// Decode `Option<string>` from a heap enum (`None` = 0, `Some` = 1).
fn value_as_option_string(heap: &Heap, v: Value) -> Result<Option<String>, IoErrorTag> {
    match heap.find_object_by_addr(v.raw() as u64) {
        Some(Object::Enum(gc)) => {
            let e = gc.as_ref();
            match e.tag {
                0 => Ok(None),
                1 => {
                    let payload = e.payload.first().ok_or(IoErrorTag::InvalidInput)?;
                    let inner = member_as_value(payload)?;
                    Ok(Some(value_as_string(heap, inner)?))
                }
                _ => Err(IoErrorTag::InvalidInput),
            }
        }
        _ => Err(IoErrorTag::InvalidInput),
    }
}

fn parse_tls_options(heap: &Heap, opts: Value) -> Result<ClientEnableOpts, IoErrorTag> {
    let addr = opts.raw() as u64;
    let Some(Object::Instance(gc)) = heap.find_object_by_addr(addr) else {
        return Err(IoErrorTag::InvalidInput);
    };
    let mut verify: Option<bool> = None;
    let mut ca_pem: Option<Option<String>> = None;
    let mut ca_path: Option<Option<String>> = None;
    let mut timeout_ms: Option<i64> = None;
    let mut alpn: Option<String> = None;
    for (key, member) in gc.as_ref().iter_fields() {
        let name = key.as_ref().data.as_str();
        match name {
            "verify" => {
                let Member::Value(v) = member else {
                    return Err(IoErrorTag::InvalidInput);
                };
                let raw = v.raw() as u64;
                if raw != 0 && raw != 1 {
                    return Err(IoErrorTag::InvalidInput);
                }
                verify = Some(v.as_bool());
            }
            "ca_pem" => {
                ca_pem = Some(value_as_option_string(heap, member_as_value(&member)?)?);
            }
            "ca_path" => {
                ca_path = Some(value_as_option_string(heap, member_as_value(&member)?)?);
            }
            "timeout_ms" => {
                let Member::Value(v) = member else {
                    return Err(IoErrorTag::InvalidInput);
                };
                timeout_ms = Some(v.as_int());
            }
            "alpn" => {
                alpn = Some(value_as_string(heap, member_as_value(&member)?)?);
            }
            _ => return Err(IoErrorTag::InvalidInput),
        }
    }
    Ok(ClientEnableOpts {
        verify: verify.ok_or(IoErrorTag::InvalidInput)?,
        ca_pem: ca_pem.ok_or(IoErrorTag::InvalidInput)?,
        ca_path: ca_path.ok_or(IoErrorTag::InvalidInput)?,
        timeout_ms: timeout_ms.ok_or(IoErrorTag::InvalidInput)?,
        alpn: alpn.unwrap_or_default(),
    })
}

fn parse_server_enable_options(heap: &Heap, opts: Value) -> Result<ServerEnableOpts, IoErrorTag> {
    let addr = opts.raw() as u64;
    let Some(Object::Instance(gc)) = heap.find_object_by_addr(addr) else {
        return Err(IoErrorTag::InvalidInput);
    };
    let mut cert_pem: Option<String> = None;
    let mut key_pem: Option<String> = None;
    let mut timeout_ms: Option<i64> = None;
    let mut client_ca_pem: Option<String> = None;
    let mut alpn: Option<String> = None;
    for (key, member) in gc.as_ref().iter_fields() {
        let name = key.as_ref().data.as_str();
        match name {
            "cert_pem" => {
                cert_pem = Some(value_as_string(heap, member_as_value(&member)?)?);
            }
            "key_pem" => {
                key_pem = Some(value_as_string(heap, member_as_value(&member)?)?);
            }
            "timeout_ms" => {
                let Member::Value(v) = member else {
                    return Err(IoErrorTag::InvalidInput);
                };
                timeout_ms = Some(v.as_int());
            }
            "client_ca_pem" => {
                client_ca_pem = Some(value_as_string(heap, member_as_value(&member)?)?);
            }
            "alpn" => {
                alpn = Some(value_as_string(heap, member_as_value(&member)?)?);
            }
            _ => return Err(IoErrorTag::InvalidInput),
        }
    }
    Ok(ServerEnableOpts {
        cert_pem: cert_pem.ok_or(IoErrorTag::InvalidInput)?,
        key_pem: key_pem.ok_or(IoErrorTag::InvalidInput)?,
        timeout_ms: timeout_ms.ok_or(IoErrorTag::InvalidInput)?,
        client_ca_pem: client_ca_pem.ok_or(IoErrorTag::InvalidInput)?,
        alpn: alpn.unwrap_or_default(),
    })
}

fn require_tcp_stream(heap: &mut Heap, stream: Value) -> Result<i64, IoErrorTag> {
    with_stream_mut(heap, stream, |s| {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::Tcp || s.tls.is_some() {
            return Err(IoErrorTag::InvalidInput);
        }
        Ok(s.handle.as_ref().unwrap().tls_abi_fd())
    })?
}

fn require_abi() -> Result<std::sync::Arc<TlsNativeAbi>, IoErrorTag> {
    resolve_preferred().ok_or(IoErrorTag::Other)
}

fn mark_enable_would_block(outcome: NativeEnable, wants_write: bool) -> NativeEnable {
    match outcome {
        NativeEnable::WouldBlock(session) => {
            session.set_wants_write(wants_write);
            NativeEnable::WouldBlock(session)
        }
        other => other,
    }
}

/// Park once after WouldBlock enable. Session stays attached; next IO is read/write.
fn park_enable_would_block(
    heap: &mut Heap,
    stream: Value,
    timeout_ms: i64,
) -> Result<(), IoErrorTag> {
    let wait = stream_wait_handle(heap, stream)?;
    let wants_write = with_stream_mut(heap, stream, |s| {
        s.tls.as_ref().is_some_and(|t| t.wants_write())
    })?;
    let interest = if wants_write {
        Interest::Writable
    } else {
        Interest::Readable
    };
    reactor_wait_fd_no_help(wait, interest, duration_from_timeout_ms(timeout_ms))
}

fn remaining_timeout_ms(deadline: Option<Instant>) -> Result<i64, IoErrorTag> {
    match deadline {
        None => Ok(0),
        Some(end) => {
            let now = Instant::now();
            if now >= end {
                return Err(IoErrorTag::TimedOut);
            }
            let ms = end.saturating_duration_since(now).as_millis() as i64;
            Ok(ms.max(1))
        }
    }
}

fn pump_enable_handshake(heap: &mut Heap, stream: Value) -> Result<(), IoErrorTag> {
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let fd = s.handle.as_ref().unwrap().tls_abi_fd();
        let tls = s.tls.as_ref().ok_or(IoErrorTag::Other)?;
        tls.handshake_step(fd)
    })?
}

/// Finish handshake on the attached session: park without help-steal (COI-116),
/// then pump via empty read/write. Do not call enable again.
fn complete_enable_handshake(
    heap: &mut Heap,
    stream: Value,
    timeout_ms: i64,
) -> Result<Value, IoErrorTag> {
    let deadline = duration_from_timeout_ms(timeout_ms).map(|d| Instant::now() + d);
    loop {
        let remain = remaining_timeout_ms(deadline)?;
        match park_enable_would_block(heap, stream, remain) {
            Ok(()) | Err(IoErrorTag::WouldBlock) => {}
            Err(e) => return Err(e),
        }
        match pump_enable_handshake(heap, stream) {
            Ok(()) => return Ok(stream),
            Err(IoErrorTag::WouldBlock) => continue,
            Err(e) => return Err(e),
        }
    }
}

fn finish_enable(
    heap: &mut Heap,
    stream: Value,
    outcome: NativeEnable,
    timeout_ms: i64,
) -> Result<Value, IoErrorTag> {
    match attach_enable_outcome(heap, stream, outcome) {
        Ok(()) => Ok(stream),
        Err(IoErrorTag::WouldBlock) => complete_enable_handshake(heap, stream, timeout_ms),
        Err(e) => Err(e),
    }
}

/// Upgrade a TCP `Stream` in place via dloaded `coil_tls_client_enable`.
pub fn tls_client_enable(
    heap: &mut Heap,
    stream: Value,
    host: &str,
    opts: Value,
) -> Result<Value, IoErrorTag> {
    let opts = parse_tls_options(heap, opts)?;
    let fd = require_tcp_stream(heap, stream)?;
    let abi = require_abi()?;
    let outcome = mark_enable_would_block(
        abi.client_enable(
            fd,
            host,
            opts.verify,
            opts.ca_pem.as_deref(),
            opts.ca_path.as_deref(),
            opts.timeout_ms,
            &opts.alpn,
        ),
        true,
    );
    finish_enable(heap, stream, outcome, opts.timeout_ms)
}

/// Upgrade a TCP `Stream` in place via dloaded `coil_tls_server_enable`.
pub fn tls_server_enable(heap: &mut Heap, stream: Value, opts: Value) -> Result<Value, IoErrorTag> {
    let opts = parse_server_enable_options(heap, opts)?;
    let fd = require_tcp_stream(heap, stream)?;
    let abi = require_abi()?;
    let outcome = mark_enable_would_block(
        abi.server_enable(
            fd,
            &opts.cert_pem,
            &opts.key_pem,
            opts.timeout_ms,
            &opts.client_ca_pem,
            &opts.alpn,
        ),
        false,
    );
    finish_enable(heap, stream, outcome, opts.timeout_ms)
}

/// Tear down TLS: `coil_tls_disable` (close_notify), then free on drop.
pub fn tls_client_disable(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    tls_teardown(heap, stream)
}

/// Server-facing teardown; identical to [`tls_client_disable`].
pub fn tls_server_disable(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    tls_teardown(heap, stream)
}

/// Negotiated ALPN on a TLS stream, or `""` if none.
pub fn tls_alpn_protocol(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    let proto = with_stream_mut(heap, stream, |s| {
        if s.kind != StreamKind::Tls {
            return Err(IoErrorTag::InvalidInput);
        }
        Ok(s.tls
            .as_ref()
            .map(|t| t.alpn_protocol())
            .unwrap_or_default())
    })?;
    let proto = proto?;
    let (obj, _) = heap.alloc(ObjString::from(proto.as_str()), Object::String);
    Ok(Value::from(obj.addr()))
}

fn tls_teardown(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::Tls {
            return Err(IoErrorTag::InvalidInput);
        }
        if let Some(slot) = s.tls.take() {
            crate::tls_native::drop_slot(s.handle.as_mut(), slot);
        }
        s.kind = StreamKind::Tcp;
        Ok(())
    })??;
    Ok(stream)
}
