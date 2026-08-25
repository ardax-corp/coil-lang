//! Compiler-provided virtual modules (`prelude`, `ffi`, …).
//!
//! These are not `.hy` files on disk. `use` resolves against this
//! registry before falling back to [`crate::manifest::Manifest`] path
//! discovery, and every file gets an implicit prelude import.

use std::collections::HashMap;

/// Canonical module path for Option / Result.
pub const PRELUDE_MODULE: &str = "prelude";

/// Canonical module path for operator / comparison traits.
pub const PRELUDE_OPS_MODULE: &str = "prelude::ops";

/// Canonical module path for FFI callables (`dload` / `declare` / `invoke`).
pub const FFI_MODULE: &str = "ffi";

/// Canonical module path for FFI type-tag constructors (`Int`, `Ptr`, …).
pub const FFI_TYPES_MODULE: &str = "ffi::types";

/// Canonical module path for test helpers (`assert`).
pub const PRELUDE_TEST_MODULE: &str = "prelude::test";

/// Canonical module path for linear-algebra helpers (`dot` / `matmul` / `cross`).
pub const PRELUDE_MATH_MODULE: &str = "prelude::math";

/// Canonical module path for IO streams (`open`, `read`, `Stream`, …).
pub const IO_MODULE: &str = "io";

/// Canonical module path for string helpers (`format`, `from_bytes`, `to_bytes`).
pub const STRING_MODULE: &str = "string";

/// TCP helpers under `io::net::tcp` (`connect`, `listen`, …).
pub const IO_NET_TCP_MODULE: &str = "io::net::tcp";

/// UDP helpers under `io::net::udp` (`bind`, `send_to`, …).
pub const IO_NET_UDP_MODULE: &str = "io::net::udp";

/// Leftover TLS HostInvoke for coil-tls (`alpn_protocol`). Not `tls` / `io::net::tls`.
pub const IO_TLS_LEFTOVER_MODULE: &str = "io::__tls";

/// Leftover client TLS (`enable` / `disable`) under `io::__tls::client`.
pub const IO_TLS_LEFTOVER_CLIENT_MODULE: &str = "io::__tls::client";

/// Leftover server TLS (`enable` / `disable`) under `io::__tls::server`.
pub const IO_TLS_LEFTOVER_SERVER_MODULE: &str = "io::__tls::server";

/// Canonical module path for OS threads, channels, and locks.
pub const THREAD_MODULE: &str = "thread";

/// Path-oriented filesystem helpers (`exists`, `realpath`, …).
pub const IO_FS_MODULE: &str = "io::fs";

/// Wall clock, periods, and formatting (`timestamp`, `sleep_ms`, …).
#[cfg(feature = "time")]
pub const TIME_MODULE: &str = "time";

/// Process environment (`args`, `var`, `exec`, …).
pub const ENV_MODULE: &str = "env";

/// Cryptographic primitives (`sha256`, `random_bytes`, …).
#[cfg(feature = "crypto")]
pub const CRYPTO_MODULE: &str = "crypto";

/// PCRE2 regex moved to [coil-regex](https://github.com/ardax-corp/coil-regex).

/// Explicit GC pins and weak handles (`root`, `weak`, `Root`, `Weak`, …).
pub const GC_MODULE: &str = "gc";

/// Which userland FFI builtin a virtual export names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FfiBuiltin {
    Dload,
    Declare,
    Invoke,
}

impl FfiBuiltin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dload => "dload",
            Self::Declare => "declare",
            Self::Invoke => "invoke",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "dload" => Some(Self::Dload),
            "declare" => Some(Self::Declare),
            "invoke" => Some(Self::Invoke),
            _ => None,
        }
    }
}

/// Prelude/test callables exported from virtual modules (parallel to [`FfiBuiltin`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreludeFn {
    Assert,
    Dot,
    MatMul,
    Cross,
    /// Construct a nominal `Matrix` from nested static rows.
    Matrix,
    /// Construct a single UTF-8 code unit as `Result<string, string>`.
    Char,
    /// First code unit of a `string` as `Result<byte, string>`.
    Ord,
    /// Drive a coroutine to completion: `block_on(coro) -> Y`.
    BlockOn,
    Sin,
    Cos,
    Tan,
    Sqrt,
    Floor,
    Ceil,
    Exp,
    Ln,
    Pow,
}

impl PreludeFn {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assert => "assert",
            Self::Dot => "dot",
            Self::MatMul => "matmul",
            Self::Cross => "cross",
            Self::Matrix => "matrix",
            Self::Char => "char",
            Self::Ord => "ord",
            Self::BlockOn => "block_on",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
            Self::Sqrt => "sqrt",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Exp => "exp",
            Self::Ln => "ln",
            Self::Pow => "pow",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "assert" => Some(Self::Assert),
            "dot" => Some(Self::Dot),
            "matmul" => Some(Self::MatMul),
            "cross" => Some(Self::Cross),
            "matrix" => Some(Self::Matrix),
            "char" => Some(Self::Char),
            "ord" => Some(Self::Ord),
            "block_on" => Some(Self::BlockOn),
            "sin" => Some(Self::Sin),
            "cos" => Some(Self::Cos),
            "tan" => Some(Self::Tan),
            "sqrt" => Some(Self::Sqrt),
            "floor" => Some(Self::Floor),
            "ceil" => Some(Self::Ceil),
            "exp" => Some(Self::Exp),
            "ln" => Some(Self::Ln),
            "pow" => Some(Self::Pow),
            _ => None,
        }
    }

    pub fn math_native_name(self) -> Option<&'static str> {
        match self {
            Self::Sin => Some("math_sin"),
            Self::Cos => Some("math_cos"),
            Self::Tan => Some("math_tan"),
            Self::Sqrt => Some("math_sqrt"),
            Self::Floor => Some("math_floor"),
            Self::Ceil => Some("math_ceil"),
            Self::Exp => Some("math_exp"),
            Self::Ln => Some("math_ln"),
            Self::Pow => Some("math_pow"),
            _ => None,
        }
    }
}

/// IO host natives exported from virtual `io` / `io::net::*` modules.
///
/// Surface names (after `use`) are short (`bind`, `connect`). Host registry
/// keys stay uniquely prefixed (`udp_bind`, `tcp_connect`) so TCP and UDP
/// never collide in [`Compiler::native`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoBuiltin {
    Stdin,
    Stdout,
    Stderr,
    Open,
    Close,
    Read,
    Write,
    /// Write `buf[offset..]` without allocating a suffix array (`write_from`).
    WriteFrom,
    AwaitReadable,
    AwaitWritable,
    Drive,
    /// Block until any registered async waiter is ready (`() -> int`).
    WaitReady,
    /// Decode `[byte]` as UTF-8 → `Result<string, IoError>`.
    FromBytes,
    /// Encode `string` → `[byte]` (UTF-8).
    ToBytes,
    TcpConnect,
    TcpConnectTimeout,
    TcpListen,
    TcpAccept,
    TcpPeerAddr,
    TcpLocalAddr,
    TcpSetNodelay,
    TcpShutdown,
    /// Bind a UDP datagram socket (`host`, `port`; `port` may be `0`).
    UdpBind,
    /// Create a connected UDP socket toward (`host`, `port`).
    UdpConnect,
    /// Send a datagram to an explicit peer.
    UdpSendTo,
    /// Non-blocking recv; returns `(nbytes, peer_host, peer_port)`.
    UdpRecvFrom,
    /// Local bound port of a UDP socket (useful after `bind(..., 0)`).
    UdpLocalPort,
    /// Leftover client TLS upgrade (`io::__tls::client::enable`).
    TlsClientEnable,
    /// Leftover client TLS teardown (`io::__tls::client::disable`).
    TlsClientDisable,
    /// Leftover server TLS upgrade (`io::__tls::server::enable`).
    TlsServerEnable,
    /// Leftover server TLS teardown (`io::__tls::server::disable`).
    TlsServerDisable,
    /// Negotiated ALPN protocol (`io::__tls::alpn_protocol`).
    TlsAlpnProtocol,
}

impl IoBuiltin {
    /// Name bound by `use` / shown in diagnostics (`bind`, `connect`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdin => "stdin",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Open => "open",
            Self::Close => "close",
            Self::Read => "read",
            Self::Write => "write",
            Self::WriteFrom => "write_from",
            Self::AwaitReadable => "await_readable",
            Self::AwaitWritable => "await_writable",
            Self::Drive => "drive",
            Self::WaitReady => "wait_ready",
            Self::FromBytes => "from_bytes",
            Self::ToBytes => "to_bytes",
            Self::TcpConnect => "connect",
            Self::TcpConnectTimeout => "connect_timeout",
            Self::TcpListen => "listen",
            Self::TcpAccept => "accept",
            Self::TcpPeerAddr => "peer_addr",
            Self::TcpLocalAddr => "local_addr",
            Self::TcpSetNodelay => "set_nodelay",
            Self::TcpShutdown => "shutdown",
            Self::UdpBind => "bind",
            Self::UdpConnect => "connect",
            Self::UdpSendTo => "send_to",
            Self::UdpRecvFrom => "recv_from",
            Self::UdpLocalPort => "local_port",
            Self::TlsClientEnable | Self::TlsServerEnable => "enable",
            Self::TlsClientDisable | Self::TlsServerDisable => "disable",
            Self::TlsAlpnProtocol => "alpn_protocol",
        }
    }

    /// Stable host-native registry key (unique across TCP/UDP).
    pub fn native_name(self) -> &'static str {
        match self {
            Self::Stdin
            | Self::Stdout
            | Self::Stderr
            | Self::Open
            | Self::Close
            | Self::Read
            | Self::Write
            | Self::WriteFrom
            | Self::AwaitReadable
            | Self::AwaitWritable
            | Self::Drive
            | Self::WaitReady
            | Self::FromBytes
            | Self::ToBytes => self.as_str(),
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
            Self::TlsClientEnable => "tls_client_enable",
            Self::TlsClientDisable => "tls_client_disable",
            Self::TlsServerEnable => "tls_server_enable",
            Self::TlsServerDisable => "tls_server_disable",
            Self::TlsAlpnProtocol => "tls_alpn_protocol",
        }
    }

    /// Core stream / file / text helpers on the top-level `io` module.
    pub fn core() -> &'static [IoBuiltin] {
        &[
            Self::Stdin,
            Self::Stdout,
            Self::Stderr,
            Self::Open,
            Self::Close,
            Self::Read,
            Self::Write,
            Self::WriteFrom,
            Self::AwaitReadable,
            Self::AwaitWritable,
            Self::Drive,
            Self::WaitReady,
            Self::FromBytes,
            Self::ToBytes,
        ]
    }

    /// Exports of `io::net::tcp`.
    pub fn tcp() -> &'static [IoBuiltin] {
        &[
            Self::TcpConnect,
            Self::TcpConnectTimeout,
            Self::TcpListen,
            Self::TcpAccept,
            Self::TcpPeerAddr,
            Self::TcpLocalAddr,
            Self::TcpSetNodelay,
            Self::TcpShutdown,
        ]
    }

    /// Exports of `io::net::udp`.
    pub fn udp() -> &'static [IoBuiltin] {
        &[
            Self::UdpBind,
            Self::UdpConnect,
            Self::UdpSendTo,
            Self::UdpRecvFrom,
            Self::UdpLocalPort,
        ]
    }

    /// Exports of leftover `io::__tls::client`.
    pub fn tls_client() -> &'static [IoBuiltin] {
        &[Self::TlsClientEnable, Self::TlsClientDisable]
    }

    /// Exports of leftover `io::__tls::server`.
    pub fn tls_server() -> &'static [IoBuiltin] {
        &[Self::TlsServerEnable, Self::TlsServerDisable]
    }

    /// Every leftover TLS HostInvoke (not exported from `io` or `io::net::tls`).
    pub fn tls() -> &'static [IoBuiltin] {
        &[
            Self::TlsClientEnable,
            Self::TlsClientDisable,
            Self::TlsServerEnable,
            Self::TlsServerDisable,
            Self::TlsAlpnProtocol,
        ]
    }

    /// Every IO host native (for pipeline registration).
    pub fn all() -> &'static [IoBuiltin] {
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
            // Appended after UDP so historical `IoBuiltin::all` positions stay
            // stable for any tooling that indexes this list; HostInvoke ids for
            // `wait_ready` / `write_from` / leftover TLS natives come from
            // `build_standard_host_natives` (append-only).
            Self::WaitReady,
            Self::WriteFrom,
            // Leftover internals under `io::__tls` (not `io` / `io::net::tls`).
            Self::TlsClientEnable,
            Self::TlsClientDisable,
            Self::TlsServerEnable,
            Self::TlsServerDisable,
            Self::TlsAlpnProtocol,
        ]
    }
}

/// String helpers exported from virtual `string`.
///
/// `format` is a compiler intrinsic that lowers to [`common::Instruction::FORMAT`];
/// byte conversion helpers reuse the same host-native registry entries as `io`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StringBuiltin {
    Format,
    FromBytes,
    ToBytes,
}

impl StringBuiltin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::FromBytes => "from_bytes",
            Self::ToBytes => "to_bytes",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "format" => Some(Self::Format),
            "from_bytes" => Some(Self::FromBytes),
            "to_bytes" => Some(Self::ToBytes),
            _ => None,
        }
    }

    pub fn native_name(self) -> Option<&'static str> {
        match self {
            Self::Format => None,
            Self::FromBytes => Some(IoBuiltin::FromBytes.native_name()),
            Self::ToBytes => Some(IoBuiltin::ToBytes.native_name()),
        }
    }

    pub fn all() -> &'static [StringBuiltin] {
        &[Self::Format, Self::FromBytes, Self::ToBytes]
    }
}

/// Thread host natives exported from virtual `thread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadBuiltin {
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

impl ThreadBuiltin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Join => "join",
            Self::Detach => "detach",
            Self::Channel => "channel",
            Self::Send => "send",
            Self::Recv => "recv",
            Self::TrySend => "try_send",
            Self::TryRecv => "try_recv",
            Self::Close => "close",
            Self::Mutex => "mutex",
            Self::WithLock => "with_lock",
            Self::Lock => "lock",
            Self::TryLock => "try_lock",
            Self::Unlock => "unlock",
            Self::Rwlock => "rwlock",
            Self::WithRead => "with_read",
            Self::WithWrite => "with_write",
            Self::TryRead => "try_read",
            Self::TryWrite => "try_write",
        }
    }

    pub fn native_name(self) -> &'static str {
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

    pub fn all() -> &'static [ThreadBuiltin] {
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
}

/// GC host natives exported from virtual `gc` (`Root` / `Weak`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GcBuiltin {
    Root,
    Unroot,
    Get,
    Weak,
    Upgrade,
    HeapBytes,
    Collect,
}

impl GcBuiltin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Unroot => "unroot",
            Self::Get => "get",
            Self::Weak => "weak",
            Self::Upgrade => "upgrade",
            Self::HeapBytes => "heap_bytes",
            Self::Collect => "collect",
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Self::Root => "gc_root",
            Self::Unroot => "gc_unroot",
            Self::Get => "gc_get",
            Self::Weak => "gc_weak",
            Self::Upgrade => "gc_upgrade",
            Self::HeapBytes => "gc_heap_bytes",
            Self::Collect => "gc_collect",
        }
    }

    pub fn all() -> &'static [GcBuiltin] {
        &[
            Self::Root,
            Self::Unroot,
            Self::Get,
            Self::Weak,
            Self::Upgrade,
            Self::HeapBytes,
            Self::Collect,
        ]
    }
}

/// One item exported by a virtual module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinExport {
    /// Built-in sum type (`Option`, `Result`). Internal registry key is `name`.
    Enum { name: &'static str },
    /// Built-in typeclass (`Eq`, `Num`, …). Internal key is `name`.
    TypeClass { name: &'static str },
    /// FFI tag constructor (`Int`, `Ptr`, …) → same tags as historical `FFIType::X`.
    FfiTag { variant: &'static str },
    /// Userland FFI callable.
    FfiFn { kind: FfiBuiltin },
    /// Prelude/test callable (`assert`, …).
    Fn { kind: PreludeFn },
    /// Opaque built-in type name (`Stream`).
    OpaqueType { name: &'static str },
    /// IO host native (`open`, `read`, …).
    IoFn { kind: IoBuiltin },
    /// String helper (`format`, `from_bytes`, `to_bytes`).
    StringFn { kind: StringBuiltin },
    /// Thread host native (`spawn`, `send`, …).
    ThreadFn { kind: ThreadBuiltin },
    /// GC host native (`root`, `weak`, …).
    GcFn { kind: GcBuiltin },
    /// Generic pipeline host native (`registry` key for [`HostInvoke`]).
    HostFn {
        surface: &'static str,
        registry: &'static str,
    },
}

impl BuiltinExport {
    pub fn short_name(&self) -> &str {
        match self {
            Self::Enum { name } => name,
            Self::TypeClass { name } => name,
            Self::FfiTag { variant } => variant,
            Self::FfiFn { kind } => kind.as_str(),
            Self::Fn { kind } => kind.as_str(),
            Self::OpaqueType { name } => name,
            Self::IoFn { kind } => kind.as_str(),
            Self::StringFn { kind } => kind.as_str(),
            Self::ThreadFn { kind } => kind.as_str(),
            Self::GcFn { kind } => kind.as_str(),
            Self::HostFn { surface, .. } => surface,
        }
    }

    pub fn host_registry(&self) -> Option<&'static str> {
        match self {
            Self::HostFn { registry, .. } => Some(registry),
            _ => None,
        }
    }
}

/// Path → exports for compiler virtual modules.
#[derive(Debug, Clone)]
pub struct VirtualModules {
    modules: HashMap<&'static str, Vec<BuiltinExport>>,
}

impl Default for VirtualModules {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualModules {
    pub fn new() -> Self {
        let mut modules: HashMap<&'static str, Vec<BuiltinExport>> = HashMap::new();

        modules.insert(
            PRELUDE_MODULE,
            vec![
                BuiltinExport::Enum {
                    name: common::BUILTIN_OPTION_ENUM,
                },
                BuiltinExport::Enum {
                    name: common::BUILTIN_RESULT_ENUM,
                },
                BuiltinExport::OpaqueType {
                    name: common::BUILTIN_VEC_TYPE,
                },
                BuiltinExport::TypeClass { name: "Iterator" },
                BuiltinExport::TypeClass {
                    name: "IntoIterator",
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Ord,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Char,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::BlockOn,
                },
            ],
        );

        modules.insert(
            PRELUDE_OPS_MODULE,
            vec![
                BuiltinExport::TypeClass { name: "Add" },
                BuiltinExport::TypeClass { name: "Sub" },
                BuiltinExport::TypeClass { name: "Mul" },
                BuiltinExport::TypeClass { name: "Div" },
                BuiltinExport::TypeClass { name: "Num" },
                BuiltinExport::TypeClass { name: "Eq" },
                BuiltinExport::TypeClass { name: "Ord" },
                BuiltinExport::TypeClass { name: "Lt" },
                BuiltinExport::TypeClass { name: "Le" },
                BuiltinExport::TypeClass { name: "Gt" },
                BuiltinExport::TypeClass { name: "Ge" },
                BuiltinExport::TypeClass { name: "Show" },
                BuiltinExport::TypeClass { name: "Length" },
                BuiltinExport::TypeClass { name: "Into" },
            ],
        );

        modules.insert(
            FFI_MODULE,
            vec![
                BuiltinExport::Enum {
                    name: common::BUILTIN_FFI_ERROR_ENUM,
                },
                BuiltinExport::Enum {
                    name: common::BUILTIN_FFI_ERROR_KIND_ENUM,
                },
                BuiltinExport::FfiFn {
                    kind: FfiBuiltin::Dload,
                },
                BuiltinExport::FfiFn {
                    kind: FfiBuiltin::Declare,
                },
                BuiltinExport::FfiFn {
                    kind: FfiBuiltin::Invoke,
                },
            ],
        );

        let ffi_tags: Vec<BuiltinExport> = common::BUILTIN_FFI_TYPE_VARIANTS
            .iter()
            .map(|variant| BuiltinExport::FfiTag { variant })
            .collect();
        modules.insert(FFI_TYPES_MODULE, ffi_tags);

        modules.insert(
            PRELUDE_TEST_MODULE,
            vec![BuiltinExport::Fn {
                kind: PreludeFn::Assert,
            }],
        );

        modules.insert(
            PRELUDE_MATH_MODULE,
            vec![
                BuiltinExport::Fn {
                    kind: PreludeFn::Dot,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::MatMul,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Cross,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Matrix,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Sin,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Cos,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Tan,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Sqrt,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Floor,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Ceil,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Exp,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Ln,
                },
                BuiltinExport::Fn {
                    kind: PreludeFn::Pow,
                },
                BuiltinExport::OpaqueType {
                    name: common::BUILTIN_MATRIX_TYPE,
                },
            ],
        );

        let mut io_exports = vec![
            BuiltinExport::OpaqueType { name: "Stream" },
            BuiltinExport::Enum {
                name: common::BUILTIN_IO_ERROR_ENUM,
            },
            BuiltinExport::TypeClass { name: "Read" },
            BuiltinExport::TypeClass { name: "Write" },
        ];
        for kind in IoBuiltin::core() {
            io_exports.push(BuiltinExport::IoFn { kind: *kind });
        }
        modules.insert(IO_MODULE, io_exports);

        let string_exports: Vec<BuiltinExport> = StringBuiltin::all()
            .iter()
            .map(|kind| BuiltinExport::StringFn { kind: *kind })
            .collect();
        modules.insert(STRING_MODULE, string_exports);

        let tcp_exports: Vec<BuiltinExport> = IoBuiltin::tcp()
            .iter()
            .map(|kind| BuiltinExport::IoFn { kind: *kind })
            .collect();
        modules.insert(IO_NET_TCP_MODULE, tcp_exports);

        let udp_exports: Vec<BuiltinExport> = IoBuiltin::udp()
            .iter()
            .map(|kind| BuiltinExport::IoFn { kind: *kind })
            .collect();
        modules.insert(IO_NET_UDP_MODULE, udp_exports);

        modules.insert(
            IO_TLS_LEFTOVER_MODULE,
            vec![BuiltinExport::IoFn {
                kind: IoBuiltin::TlsAlpnProtocol,
            }],
        );
        let tls_client_exports: Vec<BuiltinExport> = IoBuiltin::tls_client()
            .iter()
            .map(|kind| BuiltinExport::IoFn { kind: *kind })
            .collect();
        modules.insert(IO_TLS_LEFTOVER_CLIENT_MODULE, tls_client_exports);
        let tls_server_exports: Vec<BuiltinExport> = IoBuiltin::tls_server()
            .iter()
            .map(|kind| BuiltinExport::IoFn { kind: *kind })
            .collect();
        modules.insert(IO_TLS_LEFTOVER_SERVER_MODULE, tls_server_exports);

        let mut thread_exports = vec![
            BuiltinExport::OpaqueType { name: "Thread" },
            BuiltinExport::OpaqueType { name: "Sender" },
            BuiltinExport::OpaqueType { name: "Receiver" },
            BuiltinExport::OpaqueType { name: "Mutex" },
            BuiltinExport::OpaqueType { name: "RwLock" },
            BuiltinExport::Enum {
                name: common::BUILTIN_THREAD_ERROR_ENUM,
            },
        ];
        for kind in ThreadBuiltin::all() {
            thread_exports.push(BuiltinExport::ThreadFn { kind: *kind });
        }
        modules.insert(THREAD_MODULE, thread_exports);

        let mut gc_exports = vec![
            BuiltinExport::OpaqueType {
                name: common::BUILTIN_ROOT_TYPE,
            },
            BuiltinExport::OpaqueType {
                name: common::BUILTIN_WEAK_TYPE,
            },
        ];
        for kind in GcBuiltin::all() {
            gc_exports.push(BuiltinExport::GcFn { kind: *kind });
        }
        modules.insert(GC_MODULE, gc_exports);

        fn host_exports(pairs: &[(&'static str, &'static str)]) -> Vec<BuiltinExport> {
            pairs
                .iter()
                .map(|(surface, registry)| BuiltinExport::HostFn { surface, registry })
                .collect()
        }

        modules.insert(
            IO_FS_MODULE,
            host_exports(&[
                ("exists", "fs_exists"),
                ("is_file", "fs_is_file"),
                ("is_dir", "fs_is_dir"),
                ("is_symlink", "fs_is_symlink"),
                ("metadata", "fs_metadata"),
                ("create_dir", "fs_create_dir"),
                ("create_dir_all", "fs_create_dir_all"),
                ("remove_file", "fs_remove_file"),
                ("remove_dir", "fs_remove_dir"),
                ("remove_dir_all", "fs_remove_dir_all"),
                ("rename", "fs_rename"),
                ("copy", "fs_copy"),
                ("read_link", "fs_read_link"),
                ("symlink", "fs_symlink"),
                ("list_dir", "fs_list_dir"),
                ("realpath", "fs_realpath"),
            ]),
        );

        let mut env_exports = vec![BuiltinExport::Enum {
            name: common::BUILTIN_ENV_ERROR_ENUM,
        }];
        env_exports.extend(host_exports(&[
            ("args", "env_args"),
            ("var", "env_var"),
            ("set_var", "env_set_var"),
            ("remove_var", "env_remove_var"),
            ("cwd", "env_cwd"),
            ("set_cwd", "env_set_cwd"),
            ("exit", "env_exit"),
            ("exec", "env_exec"),
        ]));
        modules.insert(ENV_MODULE, env_exports);

        #[cfg(feature = "time")]
        {
            let mut time_exports = vec![BuiltinExport::Enum {
                name: common::BUILTIN_TIME_ERROR_ENUM,
            }];
            time_exports.extend(host_exports(&[
                ("timestamp", "time_timestamp"),
                ("sleep_ms", "time_sleep_ms"),
                ("instant_now", "time_instant_now"),
                ("elapsed_nanos", "time_elapsed_nanos"),
                ("elapsed_millis", "time_elapsed_millis"),
                ("period", "time_period"),
                ("add", "time_add"),
                ("sub", "time_sub"),
                ("period_add", "time_period_add"),
                ("period_sub", "time_period_sub"),
                ("date", "time_date"),
                ("date_from_period", "time_date_from_period"),
                ("date_from_epoch_period", "time_date_from_epoch_period"),
                ("epoch", "time_epoch"),
                ("format", "time_format"),
                ("parse", "time_parse"),
            ]));
            modules.insert(TIME_MODULE, time_exports);
        }

        #[cfg(feature = "crypto")]
        {
            let mut crypto_exports = vec![BuiltinExport::Enum {
                name: common::BUILTIN_CRYPTO_ERROR_ENUM,
            }];
            crypto_exports.extend(host_exports(&[
                ("sha256", "crypto_sha256"),
                ("sha512", "crypto_sha512"),
                ("blake3", "crypto_blake3"),
                ("init", "crypto_hasher_init"),
                ("update", "crypto_hasher_update"),
                ("finalize", "crypto_hasher_finalize"),
                ("hmac_sha256", "crypto_hmac_sha256"),
                ("hmac_sha512", "crypto_hmac_sha512"),
                ("hmac_verify_sha256", "crypto_hmac_verify_sha256"),
                ("random_bytes", "crypto_random_bytes"),
                ("random_u64", "crypto_random_u64"),
                (
                    "chacha20_poly1305_encrypt",
                    "crypto_chacha20_poly1305_encrypt",
                ),
                (
                    "chacha20_poly1305_decrypt",
                    "crypto_chacha20_poly1305_decrypt",
                ),
                ("aes_256_gcm_encrypt", "crypto_aes_256_gcm_encrypt"),
                ("aes_256_gcm_decrypt", "crypto_aes_256_gcm_decrypt"),
                ("ed25519_generate", "crypto_ed25519_generate"),
                ("ed25519_sign", "crypto_ed25519_sign"),
                ("ed25519_verify", "crypto_ed25519_verify"),
                ("x25519_generate", "crypto_x25519_generate"),
                ("x25519_shared_secret", "crypto_x25519_shared_secret"),
                ("argon2id_hash", "crypto_argon2id_hash"),
                ("argon2id_verify", "crypto_argon2id_verify"),
                ("ct_eq", "crypto_ct_eq"),
            ]));
            modules.insert(CRYPTO_MODULE, crypto_exports);
        }

        Self { modules }
    }

    /// True when `module_path` is a known virtual module (`"prelude"`, `"ffi::types"`, …).
    pub fn is_virtual_module(&self, module_path: &str) -> bool {
        self.modules.contains_key(module_path)
    }

    /// Join `use` path segments (+ optional final item that is not `*`) into a module path.
    pub fn module_path_of(path: &[String], name: &str) -> String {
        if name == "*" {
            path.join("::")
        } else if path.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", path.join("::"), name)
        }
    }

    /// Resolve a concrete `use path::name` (not glob) against virtual modules.
    ///
    /// `path` is the directory segments; `name` is the last segment (item).
    /// For `use prelude::ops::Eq`, path=`["prelude","ops"]`, name=`"Eq"`.
    pub fn resolve_item(&self, path: &[String], name: &str) -> Option<BuiltinExport> {
        if name == "*" {
            return None;
        }
        let module = path.join("::");
        self.modules
            .get(module.as_str())?
            .iter()
            .find(|e| e.short_name() == name)
            .cloned()
    }

    /// Resolve `use module::*` — returns every export of that module.
    pub fn resolve_glob(&self, path: &[String]) -> Option<&[BuiltinExport]> {
        let module = path.join("::");
        self.modules.get(module.as_str()).map(|v| v.as_slice())
    }

    /// True when this `use` targets a virtual module (concrete or glob).
    ///
    /// Used by the pipeline to skip disk discovery.
    pub fn resolves_use(&self, path: &[String], name: &str) -> bool {
        if name == "*" {
            self.resolve_glob(path).is_some()
        } else {
            self.resolve_item(path, name).is_some()
        }
    }

    /// Exports injected into every file (implicit
    /// `use prelude::*; use prelude::ops::*; use prelude::test::*; use prelude::math::*;`).
    ///
    /// `pow` stays on virtual `prelude::math` for an explicit import, but is
    /// **not** auto-injected so userland `num::pow` overloads can own the bare name.
    pub fn prelude_exports(&self) -> Vec<BuiltinExport> {
        let mut out = Vec::new();
        if let Some(e) = self.modules.get(PRELUDE_MODULE) {
            out.extend(e.iter().cloned());
        }
        if let Some(e) = self.modules.get(PRELUDE_OPS_MODULE) {
            out.extend(e.iter().cloned());
        }
        if let Some(e) = self.modules.get(PRELUDE_TEST_MODULE) {
            out.extend(e.iter().cloned());
        }
        if let Some(e) = self.modules.get(PRELUDE_MATH_MODULE) {
            out.extend(e.iter().cloned().filter(|export| {
                !matches!(
                    export,
                    BuiltinExport::Fn {
                        kind: PreludeFn::Pow
                    }
                )
            }));
        }
        out
    }

    /// Look up a typeclass by qualified path (`prelude::ops::Eq` → `"Eq"`).
    pub fn resolve_typeclass_path(&self, segments: &[&str]) -> Option<&'static str> {
        if segments.len() < 2 {
            return None;
        }
        let (module_segs, name) = segments.split_at(segments.len() - 1);
        let module = module_segs.join("::");
        match self
            .modules
            .get(module.as_str())?
            .iter()
            .find(|e| matches!(e, BuiltinExport::TypeClass { name: n } if n == &name[0]))?
        {
            BuiltinExport::TypeClass { name } => Some(*name),
            _ => None,
        }
    }

    /// Look up an enum by qualified path (`prelude::Option` → `"Option"`).
    pub fn resolve_enum_path(&self, segments: &[&str]) -> Option<&'static str> {
        if segments.len() < 2 {
            return None;
        }
        let (module_segs, name) = segments.split_at(segments.len() - 1);
        let module = module_segs.join("::");
        match self
            .modules
            .get(module.as_str())?
            .iter()
            .find(|e| matches!(e, BuiltinExport::Enum { name: n } if n == &name[0]))?
        {
            BuiltinExport::Enum { name } => Some(*name),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prelude_exports_option_result_and_ops() {
        let vm = VirtualModules::new();
        let exports = vm.prelude_exports();
        assert!(
            exports
                .iter()
                .any(|e| matches!(e, BuiltinExport::Enum { name: "Option" }))
        );
        assert!(
            exports
                .iter()
                .any(|e| matches!(e, BuiltinExport::TypeClass { name: "Eq" }))
        );
        assert!(
            exports
                .iter()
                .any(|e| matches!(e, BuiltinExport::TypeClass { name: "Into" }))
        );
        assert!(exports.iter().any(|e| matches!(
            e,
            BuiltinExport::Fn {
                kind: PreludeFn::Assert
            }
        )));
        assert!(exports.iter().any(|e| matches!(
            e,
            BuiltinExport::Fn {
                kind: PreludeFn::Dot
            }
        )));
        for kind in [
            PreludeFn::Sin,
            PreludeFn::Cos,
            PreludeFn::Tan,
            PreludeFn::Sqrt,
            PreludeFn::Floor,
            PreludeFn::Ceil,
            PreludeFn::Exp,
            PreludeFn::Ln,
        ] {
            assert!(
                exports
                    .iter()
                    .any(|e| matches!(e, BuiltinExport::Fn { kind: found } if *found == kind)),
                "missing prelude::math export {}",
                kind.as_str()
            );
            assert!(kind.math_native_name().is_some());
        }
        assert!(
            !exports.iter().any(|e| matches!(
                e,
                BuiltinExport::Fn {
                    kind: PreludeFn::Pow
                }
            )),
            "pow must not be auto-injected (lives in userland num)"
        );
        assert!(
            vm.resolve_item(&["prelude".to_string(), "math".to_string()], "pow",)
                .is_some(),
            "pow remains importable from prelude::math"
        );
        assert!(
            !exports
                .iter()
                .any(|e| matches!(e, BuiltinExport::FfiFn { .. }))
        );
    }

    #[test]
    fn resolve_concrete_prelude_test_assert() {
        let vm = VirtualModules::new();
        let e = vm
            .resolve_item(&["prelude".into(), "test".into()], "assert")
            .expect("prelude::test::assert");
        assert_eq!(
            e,
            BuiltinExport::Fn {
                kind: PreludeFn::Assert
            }
        );
        assert!(vm.resolves_use(&["prelude".into(), "test".into()], "*"));
    }

    #[test]
    fn ffi_types_glob_lists_tags() {
        let vm = VirtualModules::new();
        let tags = vm
            .resolve_glob(&["ffi".into(), "types".into()])
            .expect("ffi::types");
        assert!(
            tags.iter()
                .any(|e| matches!(e, BuiltinExport::FfiTag { variant: "Int" }))
        );
        assert!(
            tags.iter()
                .any(|e| matches!(e, BuiltinExport::FfiTag { variant: "Ptr" }))
        );
    }

    #[test]
    fn resolve_concrete_ffi_dload() {
        let vm = VirtualModules::new();
        let e = vm
            .resolve_item(&["ffi".into()], "dload")
            .expect("ffi::dload");
        assert_eq!(
            e,
            BuiltinExport::FfiFn {
                kind: FfiBuiltin::Dload
            }
        );
    }

    #[test]
    fn io_net_udp_exports_short_names_not_prefixed() {
        let vm = VirtualModules::new();
        let exports = vm
            .resolve_glob(&["io".into(), "net".into(), "udp".into()])
            .expect("io::net::udp");
        assert!(exports.iter().any(|e| e.short_name() == "bind"));
        assert!(exports.iter().any(|e| e.short_name() == "send_to"));
        assert!(exports.iter().any(|e| e.short_name() == "recv_from"));
        assert!(!exports.iter().any(|e| e.short_name() == "recv_from_wait"));
        assert!(!exports.iter().any(|e| e.short_name() == "udp_bind"));

        let bind = vm
            .resolve_item(&["io".into(), "net".into(), "udp".into()], "bind")
            .expect("io::net::udp::bind");
        assert_eq!(
            bind,
            BuiltinExport::IoFn {
                kind: IoBuiltin::UdpBind
            }
        );
        assert_eq!(IoBuiltin::UdpBind.native_name(), "udp_bind");
        assert_eq!(IoBuiltin::TcpConnect.as_str(), "connect");
        assert_eq!(IoBuiltin::TcpConnect.native_name(), "tcp_connect");
        assert_eq!(
            IoBuiltin::TcpConnectTimeout.native_name(),
            "tcp_connect_timeout"
        );
        assert_eq!(IoBuiltin::TcpPeerAddr.as_str(), "peer_addr");
        assert_eq!(IoBuiltin::TcpSetNodelay.native_name(), "tcp_set_nodelay");
    }

    #[test]
    fn io_net_tls_is_not_a_virtual_module() {
        let vm = VirtualModules::new();
        assert!(
            vm.resolve_glob(&["io".into(), "net".into(), "tls".into()])
                .is_none()
        );
        assert!(
            vm.resolve_glob(&["io".into(), "net".into(), "tls".into(), "client".into()])
                .is_none()
        );
        assert!(
            vm.resolve_glob(&["io".into(), "net".into(), "tls".into(), "server".into()])
                .is_none()
        );
        assert!(!vm.resolves_use(&["io".into(), "net".into(), "tls".into()], "alpn_protocol"));
        assert!(!vm.resolves_use(
            &["io".into(), "net".into(), "tls".into(), "client".into()],
            "enable"
        ));
        assert!(!vm.resolves_use(&["tls".into()], "client"));
        assert!(vm.resolve_item(&["tls".into()], "client").is_none());
    }

    #[test]
    fn io_tls_leftover_client_and_server_namespaces() {
        let vm = VirtualModules::new();
        let leftover = vm
            .resolve_glob(&["io".into(), "__tls".into()])
            .expect("io::__tls");
        assert!(leftover.iter().any(|e| e.short_name() == "alpn_protocol"));
        assert!(!leftover.iter().any(|e| e.short_name() == "enable"));
        assert_eq!(
            IoBuiltin::TlsAlpnProtocol.native_name(),
            "tls_alpn_protocol"
        );

        let alpn = vm
            .resolve_item(&["io".into(), "__tls".into()], "alpn_protocol")
            .expect("io::__tls::alpn_protocol");
        assert_eq!(
            alpn,
            BuiltinExport::IoFn {
                kind: IoBuiltin::TlsAlpnProtocol
            }
        );

        let client = vm
            .resolve_glob(&["io".into(), "__tls".into(), "client".into()])
            .expect("io::__tls::client");
        assert!(client.iter().any(|e| e.short_name() == "enable"));
        assert!(client.iter().any(|e| e.short_name() == "disable"));

        let server = vm
            .resolve_glob(&["io".into(), "__tls".into(), "server".into()])
            .expect("io::__tls::server");
        assert!(server.iter().any(|e| e.short_name() == "enable"));
        assert!(server.iter().any(|e| e.short_name() == "disable"));

        assert_eq!(
            IoBuiltin::TlsClientEnable.native_name(),
            "tls_client_enable"
        );
        assert_eq!(
            IoBuiltin::TlsServerEnable.native_name(),
            "tls_server_enable"
        );
        assert_eq!(IoBuiltin::TlsClientEnable.as_str(), "enable");
        assert_eq!(IoBuiltin::TlsServerEnable.as_str(), "enable");

        let client_enable = vm
            .resolve_item(&["io".into(), "__tls".into(), "client".into()], "enable")
            .expect("io::__tls::client::enable");
        assert_eq!(
            client_enable,
            BuiltinExport::IoFn {
                kind: IoBuiltin::TlsClientEnable
            }
        );
        let server_enable = vm
            .resolve_item(&["io".into(), "__tls".into(), "server".into()], "enable")
            .expect("io::__tls::server::enable");
        assert_eq!(
            server_enable,
            BuiltinExport::IoFn {
                kind: IoBuiltin::TlsServerEnable
            }
        );
        assert!(!vm.resolves_use(&["io".into(), "__tls".into()], "enable"));
        assert!(!vm.resolves_use(&["io".into(), "net".into(), "tls".into()], "enable"));
        assert_eq!(IoBuiltin::tls().len(), 5);
    }

    #[test]
    fn io_glob_excludes_net_helpers() {
        let vm = VirtualModules::new();
        let exports = vm.resolve_glob(&["io".into()]).expect("io");
        assert!(exports.iter().any(|e| e.short_name() == "open"));
        assert!(exports.iter().any(|e| e.short_name() == "from_bytes"));
        assert!(exports.iter().any(|e| e.short_name() == "await_readable"));
        assert!(exports.iter().any(|e| e.short_name() == "wait_ready"));
        assert!(exports.iter().any(|e| e.short_name() == "write_from"));
        assert!(!exports.iter().any(|e| e.short_name() == "write_all"));
        assert!(!exports.iter().any(|e| e.short_name() == "set_read_timeout"));
        assert!(
            !exports
                .iter()
                .any(|e| e.short_name() == "set_write_timeout")
        );
        assert!(!exports.iter().any(|e| e.short_name() == "bind"));
        assert!(!exports.iter().any(|e| e.short_name() == "listen"));
        assert!(!exports.iter().any(|e| e.short_name() == "enable"));
        assert!(!exports.iter().any(|e| e.short_name() == "alpn_protocol"));
        let tcp_path = ["io".into(), "net".into(), "tcp".into()];
        assert!(vm.resolves_use(&tcp_path, "*"));
        assert!(vm.resolves_use(&tcp_path, "connect_timeout"));
        assert!(!vm.resolves_use(&tcp_path, "accept_wait"));
        assert!(!vm.resolves_use(&tcp_path, "accept_wait_timeout"));
        assert!(vm.resolves_use(&tcp_path, "peer_addr"));
        assert!(vm.resolves_use(&tcp_path, "local_addr"));
        assert!(vm.resolves_use(&tcp_path, "set_nodelay"));
        assert!(vm.resolves_use(&tcp_path, "shutdown"));
    }

    #[test]
    fn string_exports_format_and_byte_helpers() {
        let vm = VirtualModules::new();
        let exports = vm.resolve_glob(&["string".into()]).expect("string");
        assert!(exports.iter().any(|e| e.short_name() == "format"));
        assert!(exports.iter().any(|e| e.short_name() == "from_bytes"));
        assert!(exports.iter().any(|e| e.short_name() == "to_bytes"));

        let format = vm
            .resolve_item(&["string".into()], "format")
            .expect("string::format");
        assert_eq!(
            format,
            BuiltinExport::StringFn {
                kind: StringBuiltin::Format
            }
        );
        assert_eq!(StringBuiltin::FromBytes.native_name(), Some("from_bytes"));
        assert_eq!(StringBuiltin::ToBytes.native_name(), Some("to_bytes"));
    }

    #[test]
    fn resolves_use_detects_virtual_paths() {
        let vm = VirtualModules::new();
        assert!(vm.resolves_use(&["prelude".into(), "ops".into()], "Eq"));
        assert!(vm.resolves_use(&["ffi".into(), "types".into()], "*"));
        assert!(!vm.resolves_use(&["foo".into()], "sadge"));
    }

    #[test]
    fn resolves_time_fs_env_crypto_exports() {
        let vm = VirtualModules::new();
        #[cfg(feature = "time")]
        assert!(vm.resolves_use(&["time".into()], "*"));
        #[cfg(not(feature = "time"))]
        assert!(!vm.resolves_use(&["time".into()], "*"));
        assert!(vm.resolves_use(&["io".into(), "fs".into()], "*"));
        assert!(vm.resolves_use(&["env".into()], "*"));
        #[cfg(feature = "crypto")]
        assert!(vm.resolves_use(&["crypto".into()], "*"));
        #[cfg(not(feature = "crypto"))]
        assert!(!vm.resolves_use(&["crypto".into()], "*"));

        #[cfg(feature = "time")]
        assert!(matches!(
            vm.resolve_item(&["time".into()], "epoch"),
            Some(BuiltinExport::HostFn {
                surface: "epoch",
                registry: "time_epoch"
            })
        ));
        #[cfg(not(feature = "time"))]
        assert!(vm.resolve_item(&["time".into()], "epoch").is_none());
        assert!(matches!(
            vm.resolve_item(&["io".into(), "fs".into()], "exists"),
            Some(BuiltinExport::HostFn {
                surface: "exists",
                registry: "fs_exists"
            })
        ));
        assert!(matches!(
            vm.resolve_item(&["env".into()], "var"),
            Some(BuiltinExport::HostFn {
                surface: "var",
                registry: "env_var"
            })
        ));
        #[cfg(feature = "crypto")]
        assert!(matches!(
            vm.resolve_item(&["crypto".into()], "sha256"),
            Some(BuiltinExport::HostFn {
                surface: "sha256",
                registry: "crypto_sha256"
            })
        ));
        #[cfg(not(feature = "crypto"))]
        assert!(vm.resolve_item(&["crypto".into()], "sha256").is_none());
        assert!(!vm.resolves_use(&["regex".into()], "*"));
        assert!(vm.resolve_item(&["regex".into()], "compile").is_none());
        assert!(!vm.resolves_use(
            &["io".into(), "net".into(), "tls".into(), "client".into()],
            "enable"
        ));
        assert!(
            vm.resolve_item(
                &["io".into(), "net".into(), "tls".into(), "client".into()],
                "enable"
            )
            .is_none()
        );
        assert!(!vm.resolves_use(&["tls".into()], "*"));
        assert!(vm.resolves_use(&["gc".into()], "*"));
        assert!(matches!(
            vm.resolve_item(&["gc".into()], "root"),
            Some(BuiltinExport::GcFn {
                kind: GcBuiltin::Root
            })
        ));
        assert!(
            vm.resolve_glob(&["gc".into()])
                .expect("gc")
                .iter()
                .any(|e| e.short_name() == "Root")
        );
        assert!(
            vm.resolve_glob(&["gc".into()])
                .expect("gc")
                .iter()
                .any(|e| e.short_name() == "Weak")
        );
    }
}
