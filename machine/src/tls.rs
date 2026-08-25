//! Host-backed TLS streams via rustls (`io::net::tls::{client,server}`).
//!
//! Client: [`tls_client_enable`] / [`tls_client_disable`].
//! Server: [`tls_server_enable`] / [`tls_server_disable`].
//! After handshake, [`tls_alpn_protocol`] reports the negotiated ALPN (or `""`).
//! Both upgrade a TCP [`crate::memory::ObjStream`] in place; after handshake,
//! normal Stream read/write use the shared TLS session.
//!
//! HostInvoke ids reserved; do not reorder; userland extract will stub these
//! (coil-tls). See [`crate::reserved_hostinvoke`].

use std::io::{self, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{
    ClientConfig, ClientConnection, Connection, DigitallySignedStruct, Error as TlsError,
    RootCertStore, ServerConfig, ServerConnection, SignatureScheme,
};

use common::Value;

use crate::io::{IoErrorTag, value_as_string, with_stream_mut};
use crate::io_handle::{NativeHandle, WaitHandle};
use crate::memory::{Heap, Member, Object, ObjString, StreamKind};

/// rustls session state owned by a TLS [`crate::memory::ObjStream`] (client or server).
pub struct TlsSession {
    conn: Connection,
    /// Plaintext drained from rustls but not yet returned to coil.
    plaintext: Vec<u8>,
    plaintext_pos: usize,
}

impl TlsSession {
    fn from_client(conn: ClientConnection) -> Self {
        Self {
            conn: Connection::Client(conn),
            plaintext: Vec::new(),
            plaintext_pos: 0,
        }
    }

    fn from_server(conn: ServerConnection) -> Self {
        Self {
            conn: Connection::Server(conn),
            plaintext: Vec::new(),
            plaintext_pos: 0,
        }
    }

    /// True when app data is buffered and a read need not wait on the socket.
    pub fn has_buffered_plaintext(&self) -> bool {
        self.plaintext_pos < self.plaintext.len()
    }

    /// Whether rustls still has ciphertext to flush to the socket.
    pub fn wants_write(&self) -> bool {
        self.conn.wants_write()
    }

    /// Negotiated ALPN after handshake, or empty if none was selected.
    fn alpn_protocol(&self) -> String {
        self.conn
            .alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .unwrap_or_default()
    }

    fn drain_plaintext_into(&mut self, out: &mut [u8]) -> usize {
        let avail = self.plaintext.len() - self.plaintext_pos;
        if avail == 0 || out.is_empty() {
            return 0;
        }
        let n = avail.min(out.len());
        out[..n].copy_from_slice(&self.plaintext[self.plaintext_pos..self.plaintext_pos + n]);
        self.plaintext_pos += n;
        if self.plaintext_pos >= self.plaintext.len() {
            self.plaintext.clear();
            self.plaintext_pos = 0;
        }
        n
    }

    fn pull_plaintext_from_conn(&mut self) -> Result<(), IoErrorTag> {
        loop {
            let mut tmp = [0u8; 16 * 1024];
            match self.conn.reader().read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => self.plaintext.extend_from_slice(&tmp[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // Abrupt TCP close without close_notify — truncation risk for
                // EOF-framed protocols. L0 `read` surfaces Truncated; bulk
                // `read_to_end` treats it as EOF with accumulated bytes.
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Err(IoErrorTag::Truncated);
                }
                Err(_) => return Err(IoErrorTag::Other),
            }
        }
        Ok(())
    }
}

fn map_tls_err(e: TlsError) -> IoErrorTag {
    match e {
        TlsError::NoCertificatesPresented
        | TlsError::UnsupportedNameType
        | TlsError::InvalidCertificate(_) => IoErrorTag::Certificate,
        _ => IoErrorTag::Handshake,
    }
}

fn map_io(e: io::Error) -> IoErrorTag {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::Interrupted => IoErrorTag::WouldBlock,
        other => IoErrorTag::from_kind(other),
    }
}

fn verified_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(webpki_root_store())
                .with_no_client_auth(),
        )
    })
    .clone()
}

fn webpki_root_store() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

/// Add PEM certificates from `pem` into `roots` (append; does not clear).
fn add_pem_certs(roots: &mut RootCertStore, pem: &str) -> Result<(), IoErrorTag> {
    let mut reader = std::io::Cursor::new(pem.as_bytes());
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IoErrorTag::InvalidInput)?;
    if certs.is_empty() {
        return Err(IoErrorTag::InvalidInput);
    }
    for cert in certs {
        roots.add(cert).map_err(|_| IoErrorTag::InvalidInput)?;
    }
    Ok(())
}

/// Verified client config: always starts from webpki roots, then appends
/// optional extra PEM from `ca_pem` and/or a file at `ca_path`.
fn verified_config_with_extras(
    ca_pem: Option<&str>,
    ca_path: Option<&str>,
) -> Result<Arc<ClientConfig>, IoErrorTag> {
    if ca_pem.is_none() && ca_path.is_none() {
        return Ok(verified_config());
    }
    let mut roots = webpki_root_store();
    if let Some(pem) = ca_pem {
        add_pem_certs(&mut roots, pem)?;
    }
    if let Some(path) = ca_path {
        let pem = std::fs::read_to_string(path).map_err(|e| IoErrorTag::from_kind(e.kind()))?;
        add_pem_certs(&mut roots, &pem)?;
    }
    Ok(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

/// Client config for `verify: false` — skips trust/name checks only.
/// Prefer `verify: true` in production; see tutorial 10 (IO streams).
fn insecure_config() -> Arc<ClientConfig> {
    static CFG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoCertVerify))
                .with_no_client_auth(),
        )
    })
    .clone()
}

/// Dangerous verifier for `enable(..., { verify: false })`: skips **trust** /
/// name checks only. TLS 1.2/1.3 record signatures are still verified.
#[derive(Debug)]
struct NoCertVerify;

impl ServerCertVerifier for NoCertVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn parse_server_name(host: &str) -> Result<ServerName<'static>, IoErrorTag> {
    ServerName::try_from(host.to_string()).map_err(|_| IoErrorTag::InvalidInput)
}

fn handshake_with_deadline(
    stream: &mut TcpStream,
    conn: &mut Connection,
    deadline: Option<std::time::Instant>,
) -> Result<(), IoErrorTag> {
    let wait = WaitHandle::from_tcp(stream);
    while conn.is_handshaking() {
        while conn.wants_write() {
            match conn.write_tls(stream) {
                // rustls may return Ok(0) when the socket is not ready.
                Ok(0) => wait_handle_deadline(wait, false, deadline)?,
                Ok(_) => {}
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    wait_handle_deadline(wait, false, deadline)?;
                }
                Err(e) => return Err(map_io(e)),
            }
        }
        if conn.is_handshaking() {
            match conn.read_tls(stream) {
                Ok(0) => {
                    // Peer closed mid-handshake.
                    return Err(IoErrorTag::Handshake);
                }
                Ok(_) => {
                    let _ = conn.process_new_packets().map_err(map_tls_err)?;
                }
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    wait_handle_deadline(wait, true, deadline)?;
                }
                Err(e) => return Err(map_io(e)),
            }
        }
    }
    while conn.wants_write() {
        match conn.write_tls(stream) {
            Ok(0) => wait_handle_deadline(wait, false, deadline)?,
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
                wait_handle_deadline(wait, false, deadline)?;
            }
            Err(e) => return Err(map_io(e)),
        }
    }
    Ok(())
}

fn wait_handle_deadline(
    handle: WaitHandle,
    for_read: bool,
    deadline: Option<std::time::Instant>,
) -> Result<(), IoErrorTag> {
    let timeout = match deadline {
        None => None,
        Some(end) => {
            let now = std::time::Instant::now();
            if now >= end {
                return Err(IoErrorTag::TimedOut);
            }
            Some(end - now)
        }
    };
    let interest = if for_read {
        crate::io_reactor::Interest::Readable
    } else {
        crate::io_reactor::Interest::Writable
    };
    // No help-steal: nesting the peer spawn under this wait deadlocks (COI-116).
    crate::io::reactor_wait_fd_no_help(handle, interest, timeout)
}

/// Run handshake while keeping the socket non-blocking; honor optional deadline.
///
/// Always leaves the fd non-blocking for [`ObjStream`].
fn with_handshake(
    tcp: &mut TcpStream,
    deadline: Option<std::time::Instant>,
    build: impl FnOnce(&mut TcpStream) -> Result<Connection, IoErrorTag>,
) -> Result<Connection, IoErrorTag> {
    // Ensure non-blocking for poll-based handshake (also restores after any
    // prior blocking attempt).
    tcp.set_nonblocking(true)
        .map_err(|e| IoErrorTag::from_kind(e.kind()))?;
    let hs = (|| -> Result<Connection, IoErrorTag> {
        let mut conn = build(tcp)?;
        // `build` may already complete handshake; if not, finish with deadline.
        if conn.is_handshaking() || conn.wants_write() {
            handshake_with_deadline(tcp, &mut conn, deadline)?;
        }
        Ok(conn)
    })();
    // Belt-and-suspenders: leave non-blocking even if something flipped it.
    let nb_err = match tcp.set_nonblocking(true) {
        Ok(()) => None,
        Err(e1) => match tcp.set_nonblocking(true) {
            Ok(()) => None,
            Err(_) => Some(IoErrorTag::from_kind(e1.kind())),
        },
    };
    match (hs, nb_err) {
        (Ok(conn), None) => Ok(conn),
        (Ok(conn), Some(e)) => {
            drop(conn);
            Err(e)
        }
        // Prefer the handshake/TLS error when both fail — callers care about
        // Handshake/Certificate/TimedOut more than a rare set_nonblocking miss.
        (Err(e), _) => Err(e),
    }
}

struct ClientEnableOpts {
    verify: bool,
    ca_pem: Option<String>,
    ca_path: Option<String>,
    timeout_ms: i64,
    alpn: String,
}

/// Map `alpn` opt (`""` = none, `"h2"`, `"http/1.1"`, or comma-separated) to rustls ALPN bytes.
fn alpn_protocols_from_opt(alpn: &str) -> Vec<Vec<u8>> {
    if alpn.is_empty() {
        return Vec::new();
    }
    alpn.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.as_bytes().to_vec())
        .collect()
}

fn client_config_with_alpn(mut config: ClientConfig, alpn: &str) -> Arc<ClientConfig> {
    let protos = alpn_protocols_from_opt(alpn);
    if protos.is_empty() {
        return Arc::new(config);
    }
    config.alpn_protocols = protos;
    Arc::new(config)
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

/// Parse client `opts`: require `verify`, `ca_pem`, `ca_path`, `timeout_ms`.
///
/// `ca_pem` / `ca_path` are `Option<string>`: `None` leaves webpki defaults;
/// `Some` appends extra trust anchors. `timeout_ms <= 0` → no deadline.
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

fn deadline_from_ms(ms: i64) -> Option<std::time::Instant> {
    crate::io::duration_from_timeout_ms(ms).map(|d| std::time::Instant::now() + d)
}

/// Upgrade a TCP `Stream` in place with a TLS client handshake.
///
/// `opts` requires `verify: bool`, `ca_pem: Option<string>`,
/// `ca_path: Option<string>`, and `timeout_ms: int` (`<= 0` → no handshake
/// deadline). Extra CA PEM / path **append** to webpki roots when `verify`.
pub fn tls_client_enable(
    heap: &mut Heap,
    stream: Value,
    host: &str,
    opts: Value,
) -> Result<Value, IoErrorTag> {
    let opts = parse_tls_options(heap, opts)?;
    let server_name = parse_server_name(host)?;
    let base = if !opts.verify {
        insecure_config()
    } else {
        verified_config_with_extras(opts.ca_pem.as_deref(), opts.ca_path.as_deref())?
    };
    let config = if opts.alpn.is_empty() {
        base
    } else {
        client_config_with_alpn((*base).clone(), &opts.alpn)
    };
    let deadline = deadline_from_ms(opts.timeout_ms);

    let hs = with_stream_mut(heap, stream, |s| -> Result<Connection, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::Tcp || s.tls.is_some() {
            return Err(IoErrorTag::InvalidInput);
        }
        let tcp = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_tcp_mut)
            .ok_or(IoErrorTag::InvalidInput)?;
        with_handshake(tcp, deadline, |_tcp| {
            let client = ClientConnection::new(config, server_name).map_err(map_tls_err)?;
            Ok(Connection::Client(client))
        })
    })??;

    let session = match hs {
        Connection::Client(c) => TlsSession::from_client(c),
        Connection::Server(s) => TlsSession::from_server(s),
    };
    with_stream_mut(heap, stream, |s| {
        s.kind = StreamKind::Tls;
        s.tls = Some(Box::new(session));
    })?;
    Ok(stream)
}

fn member_as_value(member: &Member) -> Result<Value, IoErrorTag> {
    match member {
        Member::Value(v) => Ok(*v),
        Member::Object(o) => Ok(Value::from(o.addr())),
    }
}

struct ServerEnableOpts {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    timeout_ms: i64,
    client_ca_pem: String,
    alpn: String,
}

fn server_config_with_alpn(mut config: ServerConfig, alpn: &str) -> Arc<ServerConfig> {
    let protos = alpn_protocols_from_opt(alpn);
    if protos.is_empty() {
        return Arc::new(config);
    }
    config.alpn_protocols = protos;
    Arc::new(config)
}

/// Parse server `enable` opts: require `cert_pem`, `key_pem`, `timeout_ms`,
/// and `client_ca_pem` (empty → no client auth / mTLS).
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
    let cert_pem = cert_pem.ok_or(IoErrorTag::InvalidInput)?;
    let key_pem = key_pem.ok_or(IoErrorTag::InvalidInput)?;
    let (certs, key) = parse_pem_cert_key(&cert_pem, &key_pem)?;
    Ok(ServerEnableOpts {
        certs,
        key,
        timeout_ms: timeout_ms.ok_or(IoErrorTag::InvalidInput)?,
        client_ca_pem: client_ca_pem.ok_or(IoErrorTag::InvalidInput)?,
        alpn: alpn.unwrap_or_default(),
    })
}

fn parse_pem_cert_key(
    cert_pem: &str,
    key_pem: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), IoErrorTag> {
    let mut cert_reader = std::io::Cursor::new(cert_pem.as_bytes());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| IoErrorTag::InvalidInput)?;
    if certs.is_empty() {
        return Err(IoErrorTag::InvalidInput);
    }
    let mut key_reader = std::io::Cursor::new(key_pem.as_bytes());
    let key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|_| IoErrorTag::InvalidInput)?
        .ok_or(IoErrorTag::InvalidInput)?;
    Ok((certs, key))
}

fn server_config_from_opts(opts: ServerEnableOpts) -> Result<Arc<ServerConfig>, IoErrorTag> {
    let builder = ServerConfig::builder();
    let config = if opts.client_ca_pem.is_empty() {
        builder
            .with_no_client_auth()
            .with_single_cert(opts.certs, opts.key)
            .map_err(|_| IoErrorTag::InvalidInput)?
    } else {
        let mut roots = RootCertStore::empty();
        let mut reader = std::io::Cursor::new(opts.client_ca_pem.as_bytes());
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| IoErrorTag::InvalidInput)?;
        if certs.is_empty() {
            return Err(IoErrorTag::InvalidInput);
        }
        for cert in certs {
            roots.add(cert).map_err(|_| IoErrorTag::InvalidInput)?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
            .build()
            .map_err(|_| IoErrorTag::InvalidInput)?;
        builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(opts.certs, opts.key)
            .map_err(|_| IoErrorTag::InvalidInput)?
    };
    Ok(Arc::new(config))
}

/// Upgrade a TCP `Stream` in place with a TLS **server** handshake.
///
/// `opts` requires `cert_pem`, `key_pem`, `timeout_ms` (`<= 0` → no deadline),
/// and `client_ca_pem` (empty → no mTLS).
pub fn tls_server_enable(heap: &mut Heap, stream: Value, opts: Value) -> Result<Value, IoErrorTag> {
    // Validate the stream before PEM work so kind/closed errors do not depend
    // on whether cert/key parse.
    with_stream_mut(heap, stream, |s| -> Result<(), IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if s.kind != StreamKind::Tcp || s.tls.is_some() {
            return Err(IoErrorTag::InvalidInput);
        }
        Ok(())
    })??;

    let opts = parse_server_enable_options(heap, opts)?;
    let deadline = deadline_from_ms(opts.timeout_ms);
    let alpn = opts.alpn.clone();
    let base = server_config_from_opts(opts)?;
    let config = if alpn.is_empty() {
        base
    } else {
        server_config_with_alpn((*base).clone(), &alpn)
    };

    let hs = with_stream_mut(heap, stream, |s| -> Result<Connection, IoErrorTag> {
        if s.closed || s.handle.is_none() {
            return Err(IoErrorTag::AlreadyClosed);
        }
        let tcp = s
            .handle
            .as_mut()
            .and_then(NativeHandle::as_tcp_mut)
            .ok_or(IoErrorTag::InvalidInput)?;
        with_handshake(tcp, deadline, |_tcp| {
            let server = ServerConnection::new(config).map_err(map_tls_err)?;
            Ok(Connection::Server(server))
        })
    })??;

    let session = match hs {
        Connection::Client(c) => TlsSession::from_client(c),
        Connection::Server(s) => TlsSession::from_server(s),
    };
    with_stream_mut(heap, stream, |s| {
        s.kind = StreamKind::Tls;
        s.tls = Some(Box::new(session));
    })?;
    Ok(stream)
}

/// Tear down TLS on `stream` and resume plaintext TCP on the same fd.
///
/// Sends `close_notify` (best effort), drops the session, sets
/// [`StreamKind::Tcp`]. Unread TLS plaintext is discarded. Returns the same handle.
///
/// Client-facing name; identical to [`tls_server_disable`].
pub fn tls_client_disable(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    tls_teardown(heap, stream)
}

/// Server-facing teardown; identical to [`tls_client_disable`].
pub fn tls_server_disable(heap: &mut Heap, stream: Value) -> Result<Value, IoErrorTag> {
    tls_teardown(heap, stream)
}

/// Negotiated ALPN protocol on a TLS stream, or `""` if none.
///
/// Returns [`IoErrorTag::InvalidInput`] if `stream` is not TLS.
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
        if let (Some(handle), Some(tls)) = (s.handle.as_mut(), s.tls.as_mut()) {
            let _ = send_close_notify(handle, tls);
        }
        s.tls.take();
        s.kind = StreamKind::Tcp;
        Ok(())
    })??;
    Ok(stream)
}

fn flush_tls<S: Read + Write>(sock: &mut S, tls: &mut TlsSession) -> Result<(), IoErrorTag> {
    while tls.conn.wants_write() {
        let n = tls.conn.write_tls(sock).map_err(map_io)?;
        if n == 0 {
            return Err(IoErrorTag::WouldBlock);
        }
    }
    Ok(())
}

fn read_tls_records<S: Read + Write>(
    sock: &mut S,
    tls: &mut TlsSession,
) -> Result<usize, IoErrorTag> {
    match tls.conn.read_tls(sock) {
        Ok(0) => Ok(0),
        Ok(n) => {
            let _ = tls.conn.process_new_packets().map_err(map_tls_err)?;
            tls.pull_plaintext_from_conn()?;
            Ok(n)
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted => {
            Err(IoErrorTag::WouldBlock)
        }
        Err(e) => Err(map_io(e)),
    }
}

/// Non-blocking TLS application read into `buf`.
///
/// `Ok(None)` = clean EOF; `Err(WouldBlock)` when more socket data is needed.
pub fn tls_read<S: Read + Write>(
    sock: &mut S,
    tls: &mut TlsSession,
    buf: &mut [u8],
) -> Result<Option<usize>, IoErrorTag> {
    if buf.is_empty() {
        return Ok(Some(0));
    }
    // Drain pending ciphertext first so a prior write that returned Ok(n) with
    // a WouldBlock flush cannot leave the peer waiting while we poll for read.
    flush_tls(sock, tls)?;
    // Prefer already-buffered plaintext.
    let n = tls.drain_plaintext_into(buf);
    if n > 0 {
        return Ok(Some(n));
    }
    // Pull any plaintext already sitting in rustls.
    tls.pull_plaintext_from_conn()?;
    let n = tls.drain_plaintext_into(buf);
    if n > 0 {
        return Ok(Some(n));
    }
    // Need more TLS records from the socket.
    match read_tls_records(sock, tls) {
        Ok(0) => {
            // Peer closed; drain any final plaintext.
            tls.pull_plaintext_from_conn()?;
            let n = tls.drain_plaintext_into(buf);
            if n > 0 { Ok(Some(n)) } else { Ok(None) }
        }
        Ok(_) => {
            let n = tls.drain_plaintext_into(buf);
            if n > 0 {
                Ok(Some(n))
            } else {
                // Record processed but no app data yet (e.g. key update).
                Err(IoErrorTag::WouldBlock)
            }
        }
        Err(e) => Err(e),
    }
}

/// Non-blocking TLS application write of `bytes`.
pub fn tls_write<S: Read + Write>(
    sock: &mut S,
    tls: &mut TlsSession,
    bytes: &[u8],
) -> Result<usize, IoErrorTag> {
    // Always try to flush pending ciphertext first.
    flush_tls(sock, tls)?;
    if bytes.is_empty() {
        return Ok(0);
    }
    let n = match tls.conn.writer().write(bytes) {
        Ok(n) => n,
        Err(e) if e.kind() == ErrorKind::WouldBlock => {
            flush_tls(sock, tls)?;
            return Err(IoErrorTag::WouldBlock);
        }
        Err(_) => return Err(IoErrorTag::Other),
    };
    // Best-effort flush; WouldBlock after accepting app bytes is OK — next
    // write/read will resume flushing via wants_write.
    match flush_tls(sock, tls) {
        Ok(()) | Err(IoErrorTag::WouldBlock) => Ok(n),
        Err(e) => Err(e),
    }
}

/// Send TLS `close_notify` (best-effort) before the handle is closed.
pub fn send_close_notify<S: Read + Write>(
    sock: &mut S,
    tls: &mut TlsSession,
) -> Result<(), IoErrorTag> {
    tls.conn.send_close_notify();
    match flush_tls(sock, tls) {
        Ok(()) | Err(IoErrorTag::WouldBlock) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{
        alloc_option_none, alloc_option_some, stream_close, stream_open, stream_read_to_end,
        stream_write_all, tcp_connect,
    };
    use crate::memory::{Heap, ObjArray, ObjInstance, ObjString, Object};
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::{ServerConfig, ServerConnection};
    use std::io::{ErrorKind, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    fn make_byte_array(heap: &mut Heap, bytes: &[u8]) -> Value {
        let elements: Vec<Value> = bytes.iter().map(|&b| Value::from(b as i64)).collect();
        let (obj, _) = heap.alloc(ObjArray { elements }, Object::Array);
        Value::from(obj.addr())
    }

    fn make_opts(heap: &mut Heap, verify: bool) -> Value {
        make_opts_full(heap, verify, None, None, 0)
    }

    fn option_string_member(heap: &mut Heap, s: Option<&str>) -> Member {
        let v = match s {
            None => alloc_option_none(heap),
            Some(text) => {
                let (s_obj, _) = heap.alloc(ObjString::from(text), Object::String);
                alloc_option_some(heap, Value::from(s_obj.addr()))
            }
        };
        Member::Object(
            heap.find_object_by_addr(v.raw() as u64)
                .expect("option enum on heap"),
        )
    }

    fn make_opts_full(
        heap: &mut Heap,
        verify: bool,
        ca_pem: Option<&str>,
        ca_path: Option<&str>,
        timeout_ms: i64,
    ) -> Value {
        make_opts_alpn(heap, verify, ca_pem, ca_path, timeout_ms, "")
    }

    fn make_opts_alpn(
        heap: &mut Heap,
        verify: bool,
        ca_pem: Option<&str>,
        ca_path: Option<&str>,
        timeout_ms: i64,
        alpn: &str,
    ) -> Value {
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let k_verify = heap.intern("verify".into());
        let k_ca = heap.intern("ca_pem".into());
        let k_path = heap.intern("ca_path".into());
        let k_to = heap.intern("timeout_ms".into());
        let k_alpn = heap.intern("alpn".into());
        let (alpn_obj, _) = heap.alloc(ObjString::from(alpn), Object::String);
        let ca_pem_m = option_string_member(heap, ca_pem);
        let ca_path_m = option_string_member(heap, ca_path);
        gc.as_mut()
            .set(k_verify, Member::Value(Value::from(verify)));
        gc.as_mut().set(k_ca, ca_pem_m);
        gc.as_mut().set(k_path, ca_path_m);
        gc.as_mut()
            .set(k_to, Member::Value(Value::from(timeout_ms)));
        gc.as_mut().set(k_alpn, Member::Object(alpn_obj));
        Value::from(obj.addr())
    }

    fn make_empty_opts(heap: &mut Heap) -> Value {
        let (obj, _) = heap.alloc(ObjInstance::default(), Object::Instance);
        Value::from(obj.addr())
    }

    fn make_server_enable_opts(heap: &mut Heap, cert_pem: &str, key_pem: &str) -> Value {
        make_server_enable_opts_full(heap, cert_pem, key_pem, 0, "")
    }

    fn make_server_enable_opts_full(
        heap: &mut Heap,
        cert_pem: &str,
        key_pem: &str,
        timeout_ms: i64,
        client_ca_pem: &str,
    ) -> Value {
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (cert_obj, _) = heap.alloc(ObjString::from(cert_pem), Object::String);
        let (key_obj, _) = heap.alloc(ObjString::from(key_pem), Object::String);
        let (cca_obj, _) = heap.alloc(ObjString::from(client_ca_pem), Object::String);
        let k_cert = heap.intern("cert_pem".into());
        let k_key = heap.intern("key_pem".into());
        let k_to = heap.intern("timeout_ms".into());
        let k_cca = heap.intern("client_ca_pem".into());
        let k_alpn = heap.intern("alpn".into());
        let (alpn_obj, _) = heap.alloc(ObjString::from(""), Object::String);
        gc.as_mut().set(k_cert, Member::Object(cert_obj));
        gc.as_mut().set(k_key, Member::Object(key_obj));
        gc.as_mut()
            .set(k_to, Member::Value(Value::from(timeout_ms)));
        gc.as_mut().set(k_cca, Member::Object(cca_obj));
        gc.as_mut().set(k_alpn, Member::Object(alpn_obj));
        Value::from(obj.addr())
    }

    fn test_server_pem() -> (String, String) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
        (cert.cert.pem(), cert.key_pair.serialize_pem())
    }

    fn array_bytes(heap: &Heap, v: Value) -> Vec<u8> {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::Array(gc)) => gc
                .as_ref()
                .elements
                .iter()
                .map(|e| e.as_int() as u8)
                .collect(),
            _ => panic!("expected array"),
        }
    }

    fn tcp_then_enable(
        heap: &mut Heap,
        host: &str,
        port: i64,
        verify: bool,
    ) -> Result<Value, IoErrorTag> {
        let s = tcp_connect(heap, host, port)?;
        let opts = make_opts(heap, verify);
        tls_client_enable(heap, s, host, opts)
    }

    fn test_server_config() -> (Arc<ServerConfig>, String) {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
        let cert_der = CertificateDer::from(cert.cert);
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key)
            .expect("server config");
        (Arc::new(config), "localhost".into())
    }

    fn io_transient(err: &std::io::Error) -> bool {
        matches!(
            err.kind(),
            ErrorKind::WouldBlock | ErrorKind::Interrupted | ErrorKind::TimedOut
        )
    }

    fn drain_app_data(conn: &mut ServerConnection, acc: &mut Vec<u8>) -> bool {
        let mut got = false;
        let mut tmp = [0u8; 4096];
        loop {
            match conn.reader().read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&tmp[..n]);
                    got = true;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
        got
    }

    /// Echo server: read app data, echo it back, then close_notify.
    ///
    /// Uses non-blocking sockets and retries transient IO. The previous
    /// SO_RCVTIMEO + `Err(_) => return` handshake treated macOS `TimedOut`
    /// mid-handshake as fatal, so the server quit before echoing and the
    /// client saw `read_to_end == []` (CI flake).
    fn spawn_tls_echo_server() -> (u16, thread::JoinHandle<()>) {
        let (cfg, _name) = test_server_config();
        spawn_tls_echo_server_cfg(cfg)
    }

    fn spawn_tls_echo_server_with_alpn(alpn: &[&[u8]]) -> (u16, thread::JoinHandle<()>) {
        let (cfg, _) = test_server_config();
        let mut cfg = (*cfg).clone();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        spawn_tls_echo_server_cfg(Arc::new(cfg))
    }

    fn spawn_tls_echo_server_cfg(cfg: Arc<ServerConfig>) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            ready_tx.send(()).ok();
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            let _ = sock.set_nonblocking(true);
            let Ok(mut conn) = ServerConnection::new(cfg) else {
                return;
            };
            let deadline = std::time::Instant::now() + Duration::from_secs(5);

            while conn.is_handshaking() && std::time::Instant::now() < deadline {
                while conn.wants_write() {
                    match conn.write_tls(&mut sock) {
                        // rustls may return Ok(0) when the socket is not ready.
                        Ok(0) => {
                            thread::sleep(Duration::from_millis(1));
                            break;
                        }
                        Ok(_) => {}
                        Err(e) if io_transient(&e) => {
                            thread::sleep(Duration::from_millis(1));
                            break;
                        }
                        Err(_) => return,
                    }
                }
                if !conn.is_handshaking() {
                    break;
                }
                match conn.read_tls(&mut sock) {
                    Ok(0) => return,
                    Ok(_) => {
                        if conn.process_new_packets().is_err() {
                            return;
                        }
                    }
                    Err(e) if io_transient(&e) => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(_) => return,
                }
            }
            if conn.is_handshaking() {
                return;
            }
            while conn.wants_write() && std::time::Instant::now() < deadline {
                match conn.write_tls(&mut sock) {
                    Ok(0) => thread::sleep(Duration::from_millis(1)),
                    Ok(_) => {}
                    Err(e) if io_transient(&e) => thread::sleep(Duration::from_millis(1)),
                    Err(_) => return,
                }
            }

            let mut acc = Vec::new();
            // Plaintext may already be buffered from the finishing handshake records.
            let mut last_data = if drain_app_data(&mut conn, &mut acc) {
                Some(std::time::Instant::now())
            } else {
                None
            };
            // Accumulate until peer goes idle after first byte (or EOF / deadline).
            while std::time::Instant::now() < deadline {
                if let Some(t) = last_data
                    && t.elapsed() > Duration::from_millis(100)
                {
                    break;
                }
                match conn.read_tls(&mut sock) {
                    Ok(0) => break,
                    Ok(_) => {
                        if conn.process_new_packets().is_err() {
                            return;
                        }
                        if drain_app_data(&mut conn, &mut acc) {
                            last_data = Some(std::time::Instant::now());
                        }
                    }
                    Err(e) if io_transient(&e) => {
                        if drain_app_data(&mut conn, &mut acc) {
                            last_data = Some(std::time::Instant::now());
                        } else {
                            thread::sleep(Duration::from_millis(2));
                        }
                    }
                    Err(_) => return,
                }
            }
            if acc.is_empty() {
                return;
            }
            if conn.writer().write_all(&acc).is_err() {
                return;
            }
            conn.send_close_notify();
            while conn.wants_write() && std::time::Instant::now() < deadline {
                match conn.write_tls(&mut sock) {
                    Ok(0) => thread::sleep(Duration::from_millis(1)),
                    Ok(_) => {}
                    Err(e) if io_transient(&e) => thread::sleep(Duration::from_millis(1)),
                    Err(_) => break,
                }
            }
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server ready");
        (port, handle)
    }

    #[test]
    fn enable_verify_false_round_trips_bytes() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let msg = make_byte_array(&mut heap, b"hello-tls");
        stream_write_all(&mut heap, s, msg).expect("write_all");
        let echoed = stream_read_to_end(&mut heap, s).expect("read_to_end");
        assert_eq!(array_bytes(&heap, echoed), b"hello-tls");
        stream_close(&mut heap, s).expect("close");
        handle.join().expect("server thread");
    }

    fn heap_string(heap: &Heap, v: Value) -> String {
        match heap.find_object_by_addr(v.raw() as u64) {
            Some(Object::String(gc)) => gc.as_ref().data.clone(),
            _ => panic!("expected string"),
        }
    }

    #[test]
    fn alpn_protocol_empty_when_neither_side_offers() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    #[test]
    fn alpn_protocol_negotiates_h2() {
        let (port, handle) = spawn_tls_echo_server_with_alpn(&[b"h2"]);
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_alpn(&mut heap, false, None, None, 0, "h2");
        let s = tls_client_enable(&mut heap, s, "127.0.0.1", opts).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "h2");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    #[test]
    fn alpn_protocol_client_prefers_server_overlap() {
        let (port, handle) = spawn_tls_echo_server_with_alpn(&[b"http/1.1"]);
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_alpn(&mut heap, false, None, None, 0, "h2,http/1.1");
        let s = tls_client_enable(&mut heap, s, "127.0.0.1", opts).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "http/1.1");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    #[test]
    fn alpn_protocol_on_non_tls_is_invalid_input() {
        let mut heap = Heap::default();
        let path = "coil_alpn_not_tls.bin";
        let s = stream_open(&mut heap, path, "w").expect("open");
        let err = tls_alpn_protocol(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn alpn_protocols_from_opt_parses_comma_list() {
        assert!(alpn_protocols_from_opt("").is_empty());
        assert_eq!(alpn_protocols_from_opt("h2"), vec![b"h2".to_vec()]);
        assert_eq!(
            alpn_protocols_from_opt("h2,http/1.1"),
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert_eq!(
            alpn_protocols_from_opt("h2, http/1.1"),
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert_eq!(
            alpn_protocols_from_opt("h2,,http/1.1,"),
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn alpn_protocol_on_plain_tcp_is_invalid_input() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let err = tls_alpn_protocol(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn alpn_protocol_after_disable_is_invalid_input() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let s = tls_client_disable(&mut heap, s).expect("disable");
        let err = tls_alpn_protocol(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    /// Client advertises ALPN but the peer offers none → `""` (HTTP/1.1 fallback path).
    #[test]
    fn alpn_protocol_empty_when_only_client_offers() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_alpn(&mut heap, false, None, None, 0, "h2,http/1.1");
        let s = tls_client_enable(&mut heap, s, "127.0.0.1", opts).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    /// Both sides offer `h2` and `http/1.1`; client preference selects `h2` (COI-69).
    #[test]
    fn alpn_protocol_client_preferred_when_both_overlap() {
        let (port, handle) = spawn_tls_echo_server_with_alpn(&[b"h2", b"http/1.1"]);
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_alpn(&mut heap, false, None, None, 0, "h2,http/1.1");
        let s = tls_client_enable(&mut heap, s, "127.0.0.1", opts).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "h2");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    /// Whitespace after commas in the opts string must still negotiate.
    #[test]
    fn alpn_protocol_whitespace_list_negotiates() {
        let (port, handle) = spawn_tls_echo_server_with_alpn(&[b"http/1.1"]);
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_alpn(&mut heap, false, None, None, 0, "h2, http/1.1");
        let s = tls_client_enable(&mut heap, s, "127.0.0.1", opts).expect("enable");
        let proto = tls_alpn_protocol(&mut heap, s).expect("alpn");
        assert_eq!(heap_string(&heap, proto), "http/1.1");
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    /// Large payload so rustls may buffer ciphertext across write/flush; ensures
    /// read_to_end still drains pending writes instead of hanging on poll(read).
    #[test]
    fn enable_verify_false_large_write_then_read_to_end() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i % 251) as u8).collect();
        let msg = make_byte_array(&mut heap, &payload);
        stream_write_all(&mut heap, s, msg).expect("write_all");
        let echoed = stream_read_to_end(&mut heap, s).expect("read_to_end");
        assert_eq!(array_bytes(&heap, echoed), payload);
        stream_close(&mut heap, s).expect("close");
        handle.join().expect("server thread");
    }

    fn stream_is_nonblocking(heap: &mut Heap, stream: Value) -> bool {
        #[cfg(unix)]
        {
            let fd =
                with_stream_mut(heap, stream, |s| s.handle.as_ref().unwrap().as_raw_fd()).unwrap();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            flags >= 0 && (flags as libc::c_int & libc::O_NONBLOCK) != 0
        }
        #[cfg(windows)]
        {
            let _ = (heap, stream);
            true
        }
    }

    #[test]
    fn enable_verify_true_rejects_self_signed() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        // Dial `localhost` to match the test cert SAN; failure is trust, not name.
        let s = tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        let opts = make_opts(&mut heap, true);
        let err = tls_client_enable(&mut heap, s, "localhost", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::Certificate);
        assert!(
            stream_is_nonblocking(&mut heap, s),
            "failed handshake must restore non-blocking"
        );
        stream_close(&mut heap, s).ok();
        handle.join().expect("server thread");
    }

    /// Peer accepts then closes without speaking TLS → handshake fails; the
    /// original TCP stream must stay Tcp + O_NONBLOCK for later IO.
    #[test]
    fn enable_handshake_fail_restores_nonblocking() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                drop(sock);
            }
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        assert!(stream_is_nonblocking(&mut heap, s));
        let opts = make_opts(&mut heap, false);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert!(
            matches!(
                err,
                IoErrorTag::Handshake | IoErrorTag::Other | IoErrorTag::Truncated
            ),
            "unexpected handshake err: {err:?}"
        );
        assert_eq!(
            with_stream_mut(&mut heap, s, |st| st.kind).unwrap(),
            StreamKind::Tcp
        );
        assert!(
            with_stream_mut(&mut heap, s, |st| st.tls.is_none()).unwrap(),
            "failed enable must not attach a session"
        );
        assert!(
            stream_is_nonblocking(&mut heap, s),
            "failed handshake must restore non-blocking"
        );
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_handshake_fail_restores_nonblocking() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            if let Ok((sock, _)) = listener.accept() {
                drop(sock); // no TLS client → server handshake fails
            }
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert!(
            matches!(
                err,
                IoErrorTag::Handshake | IoErrorTag::Other | IoErrorTag::Truncated
            ),
            "unexpected handshake err: {err:?}"
        );
        assert_eq!(
            with_stream_mut(&mut heap, s, |st| st.kind).unwrap(),
            StreamKind::Tcp
        );
        assert!(
            with_stream_mut(&mut heap, s, |st| st.tls.is_none()).unwrap(),
            "failed server enable must not attach a session"
        );
        assert!(
            stream_is_nonblocking(&mut heap, s),
            "failed handshake must restore non-blocking"
        );
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn enable_rejects_empty_server_name() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts(&mut heap, false);
        let err = tls_client_enable(&mut heap, s, "", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        let _ = accept.join();
    }

    #[test]
    fn enable_connection_refused() {
        let mut heap = Heap::default();
        let err = tcp_then_enable(&mut heap, "127.0.0.1", 1, false).unwrap_err();
        assert_eq!(err, IoErrorTag::Other);
    }

    #[test]
    fn enable_requires_verify_key() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_empty_opts(&mut heap);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_rejects_unknown_option_key() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let k0 = heap.intern("verify".into());
        let k1 = heap.intern("bogus".into());
        gc.as_mut().set(k0, Member::Value(Value::from(false)));
        gc.as_mut().set(
            k1,
            Member::Value(Value::from(heap.intern("x".into()).as_ptr() as u64)),
        );
        let opts = Value::from(obj.addr());
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_rejects_file_stream() {
        let mut heap = Heap::default();
        let s = stream_open(&mut heap, "coil_tls_file_kind.bin", "w").expect("open");
        let opts = make_opts(&mut heap, false);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
    }

    #[test]
    fn enable_rejects_non_tcp_and_double_enable() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let opts = make_opts(&mut heap, false);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn disable_on_tcp_is_invalid() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let err = tls_client_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn enable_disable_returns_tcp_kind() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let s = tls_client_disable(&mut heap, s).expect("disable");
        let kind = with_stream_mut(&mut heap, s, |st| st.kind).expect("kind");
        assert_eq!(kind, StreamKind::Tcp);
        assert!(
            with_stream_mut(&mut heap, s, |st| st.tls.is_none()).unwrap(),
            "session cleared"
        );
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn empty_write_then_double_close() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let empty = make_byte_array(&mut heap, b"");
        stream_write_all(&mut heap, s, empty).expect("empty write_all");
        stream_close(&mut heap, s).expect("close");
        let err = stream_close(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = handle.join();
    }

    /// Server `enable` + client `enable(verify: false)` echo round-trip.
    #[test]
    fn server_enable_then_client_enable_round_trip() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut heap = Heap::default();
            // Signal ready, then accept and run server-side encrypt on the socket.
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let mut buf = make_byte_array(&mut heap, &[0u8; 64]);
            // Read until we get data (sync adapter style).
            let n = loop {
                match crate::io::stream_read(&mut heap, s, buf) {
                    Ok(Some(n)) if n > 0 => break n,
                    Ok(Some(0)) | Ok(None) => panic!("eof before data"),
                    Ok(_) | Err(IoErrorTag::WouldBlock) => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("read {e:?}"),
                }
            };
            let read_bytes = array_bytes(&heap, buf);
            let payload = &read_bytes[..n];
            let echo = make_byte_array(&mut heap, payload);
            stream_write_all(&mut heap, s, echo).expect("echo");
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "localhost", port as i64, false).expect("client enable");
        let msg = make_byte_array(&mut heap, b"ping-encrypt");
        stream_write_all(&mut heap, s, msg).expect("write");
        let echoed = stream_read_to_end(&mut heap, s).expect("read_to_end");
        assert_eq!(array_bytes(&heap, echoed), b"ping-encrypt");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
    }

    /// COI-116: same round-trip with HostState bound so waits go through the
    /// reactor path (must use no-help TLS parks, not nested help-steal).
    #[test]
    fn server_client_enable_with_host_state_bound() {
        use crate::reactor::Reactor;
        use crate::thread::HostStateGuard;
        use std::sync::Arc;

        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let reactor = Reactor::new(1);
        let server_reactor = Arc::clone(&reactor);
        let server = thread::spawn(move || {
            let mut vm = crate::Machine::<64>::default();
            vm.set_reactor(Arc::clone(&server_reactor));
            let _guard = HostStateGuard::enter(&mut vm);
            let mut heap = Heap::default();
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts_full(&mut heap, &cert_pem, &key_pem, 5000, "");
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut vm = crate::Machine::<64>::default();
        vm.set_reactor(Arc::clone(&reactor));
        let _guard = HostStateGuard::enter(&mut vm);
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        let opts = make_opts_full(&mut heap, false, None, None, 5000);
        let s = tls_client_enable(&mut heap, s, "localhost", opts).expect("client enable");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
        reactor.shutdown();
    }

    #[test]
    fn server_enable_requires_cert_and_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_empty_opts(&mut heap);
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_unknown_option_key() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        // Unknown key on a fresh opts instance.
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (cert_obj, _) = heap.alloc(ObjString::from(cert_pem.as_str()), Object::String);
        let (key_obj, _) = heap.alloc(ObjString::from(key_pem.as_str()), Object::String);
        let k0 = heap.intern("cert_pem".into());
        let k1 = heap.intern("key_pem".into());
        let k2 = heap.intern("bogus".into());
        gc.as_mut().set(k0, Member::Object(cert_obj));
        gc.as_mut().set(k1, Member::Object(key_obj));
        gc.as_mut().set(
            k2,
            Member::Value(Value::from(heap.intern("x".into()).as_ptr() as u64)),
        );
        let err = tls_server_enable(&mut heap, s, Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_file_stream() {
        let (cert_pem, key_pem) = test_server_pem();
        let mut heap = Heap::default();
        let s = stream_open(&mut heap, "coil_tls_server_enable_file.bin", "w").expect("open");
        let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
    }

    #[test]
    fn server_enable_disable_returns_tcp_kind() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let mut heap = Heap::default();
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let s = tls_server_disable(&mut heap, s).expect("server disable");
            let kind = with_stream_mut(&mut heap, s, |st| st.kind).expect("kind");
            assert_eq!(kind, StreamKind::Tcp);
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "localhost", port as i64, false).expect("client");
        // Client handshake completes; then peer decrypts (may race). Close client.
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
    }

    #[test]
    fn server_enable_rejects_double_enable() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let mut heap = Heap::default();
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let opts2 = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
            let err = tls_server_enable(&mut heap, s, opts2).unwrap_err();
            assert_eq!(err, IoErrorTag::InvalidInput);
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "localhost", port as i64, false).expect("client");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
    }

    #[test]
    fn decrypt_on_tcp_is_invalid() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let err = tls_server_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_empty_pem_strings() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_server_enable_opts(&mut heap, "", "");
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_malformed_pem() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_server_enable_opts(
            &mut heap,
            "-----BEGIN CERTIFICATE-----\nnot-valid-base64\n-----END CERTIFICATE-----\n",
            "-----BEGIN PRIVATE KEY-----\nalso-not-valid\n-----END PRIVATE KEY-----\n",
        );
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_missing_cert_pem_only() {
        let (_, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (key_obj, _) = heap.alloc(ObjString::from(key_pem.as_str()), Object::String);
        let k_key = heap.intern("key_pem".into());
        gc.as_mut().set(k_key, Member::Object(key_obj));
        let err = tls_server_enable(&mut heap, s, Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_missing_key_pem_only() {
        let (cert_pem, _) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (cert_obj, _) = heap.alloc(ObjString::from(cert_pem.as_str()), Object::String);
        let k_cert = heap.intern("cert_pem".into());
        gc.as_mut().set(k_cert, Member::Object(cert_obj));
        let err = tls_server_enable(&mut heap, s, Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_mismatched_cert_and_key() {
        let (cert_pem, _) = test_server_pem();
        let (_, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_rejects_non_string_cert_pem() {
        let (_, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (key_obj, _) = heap.alloc(ObjString::from(key_pem.as_str()), Object::String);
        let k_cert = heap.intern("cert_pem".into());
        let k_key = heap.intern("key_pem".into());
        gc.as_mut().set(k_cert, Member::Value(Value::from(1i64)));
        gc.as_mut().set(k_key, Member::Object(key_obj));
        let err = tls_server_enable(&mut heap, s, Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_on_closed_stream_is_already_closed() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        stream_close(&mut heap, s).expect("close");
        let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = accept.join();
    }

    /// `decrypt` shares teardown with `disable` (client TLS → TCP).
    #[test]
    fn decrypt_after_client_enable_returns_tcp() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let s = tls_server_disable(&mut heap, s).expect("server disable alias");
        let kind = with_stream_mut(&mut heap, s, |st| st.kind).expect("kind");
        assert_eq!(kind, StreamKind::Tcp);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn decrypt_on_closed_tls_is_already_closed() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        stream_close(&mut heap, s).expect("close");
        let err = tls_server_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = handle.join();
    }

    #[test]
    fn decrypt_twice_is_invalid() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let s = tls_server_disable(&mut heap, s).expect("server disable once");
        let err = tls_server_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_on_closed_is_already_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        stream_close(&mut heap, s).expect("close");
        let opts = make_opts(&mut heap, false);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = accept.join();
    }

    #[test]
    fn disable_on_closed_is_already_closed() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        stream_close(&mut heap, s).expect("close");
        let err = tls_client_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::AlreadyClosed);
        let _ = handle.join();
    }

    #[test]
    fn disable_twice_is_invalid() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "127.0.0.1", port as i64, false).expect("enable");
        let s = tls_client_disable(&mut heap, s).expect("disable");
        let err = tls_client_disable(&mut heap, s).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_rejects_non_bool_verify() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let key = heap.intern("verify".into());
        // Tagged ints that are not 0/1 must be rejected (bools are 0/1).
        gc.as_mut().set(key, Member::Value(Value::from(2i64)));
        let opts = Value::from(obj.addr());
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn enable_rejects_non_instance_opts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", Value::from(0i64)).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    /// Client `disable` tears down a server-enabled TLS stream (shared teardown).
    #[test]
    fn client_disable_tears_down_server_enabled_stream() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let mut heap = Heap::default();
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &cert_pem, &key_pem);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let s = tls_client_disable(&mut heap, s).expect("client disable");
            let kind = with_stream_mut(&mut heap, s, |st| st.kind).expect("kind");
            assert_eq!(kind, StreamKind::Tcp);
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_then_enable(&mut heap, "localhost", port as i64, false).expect("client");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
    }

    #[test]
    fn enable_with_custom_ca_pem_round_trips() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server_cert = cert_pem.clone();
        let server_key = key_pem.clone();
        let server = thread::spawn(move || {
            let mut heap = Heap::default();
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &server_cert, &server_key);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let buf = make_byte_array(&mut heap, &[0u8; 64]);
            let n = loop {
                match crate::io::stream_read(&mut heap, s, buf) {
                    Ok(Some(n)) if n > 0 => break n,
                    Ok(Some(0)) | Ok(None) => panic!("eof before data"),
                    Ok(_) | Err(IoErrorTag::WouldBlock) => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("read {e:?}"),
                }
            };
            let read_bytes = array_bytes(&heap, buf);
            let echo = make_byte_array(&mut heap, &read_bytes[..n]);
            stream_write_all(&mut heap, s, echo).expect("echo");
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        // Trust the self-signed leaf via custom CA PEM (leaf-as-CA is fine for tests).
        let opts = make_opts_full(&mut heap, true, Some(&cert_pem), None, 0);
        let s = tls_client_enable(&mut heap, s, "localhost", opts).expect("enable+ca");
        let msg = make_byte_array(&mut heap, b"ca-ok");
        stream_write_all(&mut heap, s, msg).expect("write");
        let echoed = stream_read_to_end(&mut heap, s).expect("read_to_end");
        assert_eq!(array_bytes(&heap, echoed), b"ca-ok");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
    }

    #[test]
    fn enable_with_custom_ca_path_round_trips() {
        let (cert_pem, key_pem) = test_server_pem();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("coil_tls_ca_{}.pem", std::process::id()));
        std::fs::write(&path, &cert_pem).expect("write ca");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let server_cert = cert_pem.clone();
        let server_key = key_pem.clone();
        let server = thread::spawn(move || {
            let mut heap = Heap::default();
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts = make_server_enable_opts(&mut heap, &server_cert, &server_key);
            let s = tls_server_enable(&mut heap, s, opts).expect("server enable");
            let buf = make_byte_array(&mut heap, &[0u8; 64]);
            let n = loop {
                match crate::io::stream_read(&mut heap, s, buf) {
                    Ok(Some(n)) if n > 0 => break n,
                    Ok(Some(0)) | Ok(None) => panic!("eof before data"),
                    Ok(_) | Err(IoErrorTag::WouldBlock) => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("read {e:?}"),
                }
            };
            let read_bytes = array_bytes(&heap, buf);
            let echo = make_byte_array(&mut heap, &read_bytes[..n]);
            stream_write_all(&mut heap, s, echo).expect("echo");
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        let path_s = path.to_string_lossy();
        let opts = make_opts_full(&mut heap, true, None, Some(path_s.as_ref()), 0);
        let s = tls_client_enable(&mut heap, s, "localhost", opts).expect("enable+ca_path");
        let msg = make_byte_array(&mut heap, b"path-ok");
        stream_write_all(&mut heap, s, msg).expect("write");
        let echoed = stream_read_to_end(&mut heap, s).expect("read_to_end");
        assert_eq!(array_bytes(&heap, echoed), b"path-ok");
        stream_close(&mut heap, s).ok();
        server.join().expect("server");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enable_rejects_missing_ca_path() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_full(
            &mut heap,
            true,
            None,
            Some("coil_tls_missing_ca_does_not_exist.pem"),
            0,
        );
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert!(
            matches!(err, IoErrorTag::NotFound | IoErrorTag::Other),
            "unexpected {err:?}"
        );
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_rejects_garbage_ca_pem() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_full(&mut heap, true, Some("not-a-pem"), None, 0);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn enable_requires_ca_opts_and_timeout_ms_keys() {
        let (port, handle) = spawn_tls_echo_server();
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let k_verify = heap.intern("verify".into());
        gc.as_mut().set(k_verify, Member::Value(Value::from(false)));
        let err =
            tls_client_enable(&mut heap, s, "127.0.0.1", Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = handle.join();
    }

    #[test]
    fn client_enable_handshake_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hold = thread::spawn(move || {
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            // Keep the TCP session open without speaking TLS.
            thread::sleep(Duration::from_millis(500));
            drop(sock);
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_full(&mut heap, false, None, None, 40);
        let err = tls_client_enable(&mut heap, s, "127.0.0.1", opts).unwrap_err();
        assert_eq!(err, IoErrorTag::TimedOut);
        stream_close(&mut heap, s).ok();
        let _ = hold.join();
    }

    #[test]
    fn server_enable_rejects_garbage_client_ca_pem() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_server_enable_opts_full(&mut heap, &cert_pem, &key_pem, 0, "not-a-ca");
        let err = tls_server_enable(&mut heap, s, opts).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_enable_requires_client_ca_pem_key() {
        let (cert_pem, key_pem) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = thread::spawn(move || {
            let _ = listener.accept();
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let (obj, mut gc) = heap.alloc(ObjInstance::default(), Object::Instance);
        let (cert_obj, _) = heap.alloc(ObjString::from(cert_pem.as_str()), Object::String);
        let (key_obj, _) = heap.alloc(ObjString::from(key_pem.as_str()), Object::String);
        let k_cert = heap.intern("cert_pem".into());
        let k_key = heap.intern("key_pem".into());
        let k_to = heap.intern("timeout_ms".into());
        gc.as_mut().set(k_cert, Member::Object(cert_obj));
        gc.as_mut().set(k_key, Member::Object(key_obj));
        gc.as_mut().set(k_to, Member::Value(Value::from(0i64)));
        let err = tls_server_enable(&mut heap, s, Value::from(obj.addr())).unwrap_err();
        assert_eq!(err, IoErrorTag::InvalidInput);
        stream_close(&mut heap, s).ok();
        let _ = accept.join();
    }

    #[test]
    fn server_mtls_rejects_client_without_certificate() {
        let (cert_pem, key_pem) = test_server_pem();
        let (client_ca_pem, _) = test_server_pem();
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            ready_tx.send(()).ok();
            let Ok((sock, _)) = listener.accept() else {
                return;
            };
            let mut heap = Heap::default();
            let s = crate::io::alloc_stream(&mut heap, NativeHandle::Tcp(sock), StreamKind::Tcp)
                .expect("stream");
            let opts =
                make_server_enable_opts_full(&mut heap, &cert_pem, &key_pem, 2000, &client_ca_pem);
            let result = tls_server_enable(&mut heap, s, opts);
            err_tx.send(result.map(|_| ())).ok();
            stream_close(&mut heap, s).ok();
        });
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ready");
        let mut heap = Heap::default();
        // Client has no client certificate; mTLS server must fail the handshake.
        let s = tcp_connect(&mut heap, "localhost", port as i64).expect("tcp");
        let opts = make_opts_full(&mut heap, false, None, None, 2000);
        let client_err = tls_client_enable(&mut heap, s, "localhost", opts).err();
        stream_close(&mut heap, s).ok();
        let server_err = err_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("server result");
        server.join().expect("server");
        assert!(
            client_err.is_some() || server_err.is_err(),
            "mTLS without client cert must fail on at least one side"
        );
        if let Some(e) = client_err {
            assert!(
                matches!(
                    e,
                    IoErrorTag::Handshake
                        | IoErrorTag::Certificate
                        | IoErrorTag::Other
                        | IoErrorTag::Truncated
                        | IoErrorTag::TimedOut
                ),
                "unexpected client err {e:?}"
            );
        }
        if let Err(e) = server_err {
            assert!(
                matches!(
                    e,
                    IoErrorTag::Handshake
                        | IoErrorTag::Certificate
                        | IoErrorTag::Other
                        | IoErrorTag::Truncated
                        | IoErrorTag::TimedOut
                ),
                "unexpected server err {e:?}"
            );
        }
    }

    /// COI-116: TLS handshake parks must not `help_once` CPU jobs (nested peer deadlock).
    #[test]
    fn handshake_wait_does_not_help_steal_cpu_jobs() {
        use crate::reactor::{Job, Reactor};
        use crate::thread::{HostStateGuard, JoinState, ThreadProgram};
        use crate::ffi::Natives;
        use common::{Byte, Instruction, ProgramDebug};
        use std::sync::Arc;

        fn const_job(reactor: &Arc<Reactor>, imm: i32) -> Arc<JoinState> {
            let state = Arc::new(JoinState::new());
            let code = vec![
                Byte::new(Instruction::CONST).with_value_u32(imm as u32),
                Byte::new(Instruction::RETURN),
            ];
            let program = Arc::new(ThreadProgram {
                code: Arc::new(code),
                constants: Arc::new(Vec::new()),
                strings: Arc::new(Vec::new()),
                static_slot_count: 0,
                debug: ProgramDebug::default(),
                operand_stack_slots: crate::DEFAULT_OPERAND_STACK_SLOTS as u32,
            });
            reactor.submit(Job {
                entry: 0,
                args: Vec::new(),
                state: Arc::clone(&state),
                program,
                natives: Natives::new(),
                shared_print: None,
                live_threads: crate::thread::new_live_thread_registry(),
                reactor: Arc::clone(reactor),
                io_reactor: crate::io_reactor::IoReactor::new(),
            });
            state
        }

        let reactor = Reactor::new(1);
        let warmup = const_job(&reactor, 0);
        let _ = reactor.wait_join(&warmup);
        reactor.shutdown();

        let pending = const_job(&reactor, 99);
        assert!(
            pending.try_take_result().is_none(),
            "job must sit in injector after shutdown"
        );

        let mut vm = crate::Machine::<64>::default();
        vm.set_reactor(Arc::clone(&reactor));
        let _guard = HostStateGuard::enter(&mut vm);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = thread::spawn(move || {
            let _ = listener.accept();
            thread::sleep(Duration::from_millis(150));
        });
        let mut heap = Heap::default();
        let s = tcp_connect(&mut heap, "127.0.0.1", port as i64).expect("tcp");
        let opts = make_opts_full(&mut heap, false, None, None, 100);
        let _ = tls_client_enable(&mut heap, s, "127.0.0.1", opts);
        stream_close(&mut heap, s).ok();
        let _ = peer.join();

        assert!(
            pending.try_take_result().is_none(),
            "TLS handshake wait must not help-steal CPU jobs (COI-116)"
        );
        reactor.help_once();
        assert_eq!(
            pending.try_take_result().expect("helped job"),
            Ok(crate::thread::PortableValue::Immediate(99))
        );
    }
}
