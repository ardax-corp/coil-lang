//! Platform-owned IO handles: Unix fds vs Windows `HANDLE` / `SOCKET`.
//!
//! [`NativeHandle`] is stored on [`crate::memory::ObjStream`]. [`WaitHandle`] is
//! the Copy identity the reactor waits on (`poll` / `WSAPoll` /
//! `WaitForSingleObject`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};

/// Owned host stream: file/stdio, TCP, TCP listener, or UDP.
pub enum NativeHandle {
    File(File),
    Tcp(TcpStream),
    Listener(TcpListener),
    Udp(UdpSocket),
}

impl NativeHandle {
    /// Duplicate process stdin so closing the stream does not close fd 0 / STD_INPUT.
    pub fn dup_stdin() -> io::Result<Self> {
        Ok(Self::File(dup_stdio_file(StdioKind::Stdin)?))
    }

    /// Duplicate process stdout.
    pub fn dup_stdout() -> io::Result<Self> {
        Ok(Self::File(dup_stdio_file(StdioKind::Stdout)?))
    }

    /// Duplicate process stderr.
    pub fn dup_stderr() -> io::Result<Self> {
        Ok(Self::File(dup_stdio_file(StdioKind::Stderr)?))
    }

    /// Open a filesystem path (`r` / `w` / `a` / `rw`).
    pub fn open_file(path: &str, mode: &str) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        match mode {
            "r" => {
                opts.read(true);
            }
            "w" => {
                opts.write(true).create(true).truncate(true);
            }
            "a" => {
                opts.write(true).create(true).append(true);
            }
            "rw" => {
                opts.read(true).write(true).create(true);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid stream open mode",
                ));
            }
        }
        Ok(Self::File(opts.open(path)?))
    }

    /// Best-effort non-blocking. Sockets always; Unix files/pipes; Windows files skip.
    pub fn set_nonblocking(&self, nb: bool) -> io::Result<()> {
        match self {
            Self::File(f) => {
                #[cfg(unix)]
                {
                    set_file_nonblocking(f, nb)
                }
                #[cfg(windows)]
                {
                    let _ = (f, nb);
                    Ok(())
                }
            }
            Self::Tcp(s) => s.set_nonblocking(nb),
            Self::Listener(s) => s.set_nonblocking(nb),
            Self::Udp(s) => s.set_nonblocking(nb),
        }
    }

    /// Copyable wait identity for the IO reactor.
    pub fn wait_handle(&self) -> WaitHandle {
        match self {
            Self::File(f) => WaitHandle::from_file(f),
            Self::Tcp(s) => WaitHandle::from_socket(s),
            Self::Listener(s) => WaitHandle::from_socket(s),
            Self::Udp(s) => WaitHandle::from_socket(s),
        }
    }

    pub fn as_tcp_mut(&mut self) -> Option<&mut TcpStream> {
        match self {
            Self::Tcp(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_listener_mut(&mut self) -> Option<&mut TcpListener> {
        match self {
            Self::Listener(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_udp_mut(&mut self) -> Option<&mut UdpSocket> {
        match self {
            Self::Udp(s) => Some(s),
            _ => None,
        }
    }

    #[cfg(unix)]
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        match self {
            Self::File(f) => f.as_raw_fd(),
            Self::Tcp(s) => s.as_raw_fd(),
            Self::Listener(s) => s.as_raw_fd(),
            Self::Udp(s) => s.as_raw_fd(),
        }
    }

    /// Socket/fd identity for attach / leftover enable (Unix fd or Windows SOCKET).
    pub fn fd_i64(&self) -> i64 {
        self.tls_abi_fd()
    }

    /// Socket/fd identity passed to leftover `coil_tls_*` (Unix fd or Windows SOCKET).
    pub fn tls_abi_fd(&self) -> i64 {
        #[cfg(unix)]
        {
            self.as_raw_fd() as i64
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::{AsRawHandle, AsRawSocket};
            match self {
                Self::File(f) => f.as_raw_handle() as usize as i64,
                Self::Tcp(s) => s.as_raw_socket() as i64,
                Self::Listener(s) => s.as_raw_socket() as i64,
                Self::Udp(s) => s.as_raw_socket() as i64,
            }
        }
    }
}

impl Read for NativeHandle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.read(buf),
            Self::Tcp(s) => s.read(buf),
            Self::Udp(s) => s.recv_from(buf).map(|(n, _)| n),
            Self::Listener(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot read from a TCP listener",
            )),
        }
    }
}

impl Write for NativeHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.write(buf),
            Self::Tcp(s) => s.write(buf),
            Self::Udp(s) => s.send(buf),
            Self::Listener(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot write to a TCP listener",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(f) => f.flush(),
            Self::Tcp(s) => s.flush(),
            Self::Udp(_) => Ok(()),
            Self::Listener(_) => Ok(()),
        }
    }
}

/// Copyable reactor wait key (Unix fd or Windows socket / file handle).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct WaitHandle {
    inner: WaitInner,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum WaitInner {
    #[cfg(unix)]
    Fd(std::os::fd::RawFd),
    #[cfg(windows)]
    Socket(std::os::windows::io::RawSocket),
    #[cfg(windows)]
    File(usize),
}

impl WaitHandle {
    fn from_file(file: &File) -> Self {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Self {
                inner: WaitInner::Fd(file.as_raw_fd()),
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            Self {
                inner: WaitInner::File(file.as_raw_handle() as usize),
            }
        }
    }

    fn from_socket(sock: &impl AsWaitSocket) -> Self {
        sock.wait_socket()
    }

    /// Wait identity for a TCP stream (tests and TLS handshake).
    pub fn from_tcp(stream: &TcpStream) -> Self {
        Self::from_socket(stream)
    }

    #[cfg(unix)]
    pub(crate) fn as_raw_fd(self) -> std::os::fd::RawFd {
        match self.inner {
            WaitInner::Fd(fd) => fd,
        }
    }

    #[cfg(windows)]
    pub(crate) fn as_raw_socket(self) -> Option<std::os::windows::io::RawSocket> {
        match self.inner {
            WaitInner::Socket(s) => Some(s),
            WaitInner::File(_) => None,
        }
    }

    #[cfg(windows)]
    pub(crate) fn as_raw_handle(self) -> Option<std::os::windows::io::RawHandle> {
        match self.inner {
            WaitInner::File(h) => Some(h as std::os::windows::io::RawHandle),
            WaitInner::Socket(_) => None,
        }
    }
}

trait AsWaitSocket {
    fn wait_socket(&self) -> WaitHandle;
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd> AsWaitSocket for T {
    fn wait_socket(&self) -> WaitHandle {
        WaitHandle {
            inner: WaitInner::Fd(self.as_raw_fd()),
        }
    }
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsRawSocket> AsWaitSocket for T {
    fn wait_socket(&self) -> WaitHandle {
        WaitHandle {
            inner: WaitInner::Socket(self.as_raw_socket()),
        }
    }
}

enum StdioKind {
    Stdin,
    Stdout,
    Stderr,
}

#[cfg(unix)]
fn set_file_nonblocking(file: &File, nb: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if nb {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn dup_stdio_file(kind: StdioKind) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let owned = match kind {
            StdioKind::Stdin => std::io::stdin().as_fd().try_clone_to_owned()?,
            StdioKind::Stdout => std::io::stdout().as_fd().try_clone_to_owned()?,
            StdioKind::Stderr => std::io::stderr().as_fd().try_clone_to_owned()?,
        };
        Ok(File::from(owned))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let owned = match kind {
            StdioKind::Stdin => std::io::stdin().as_handle().try_clone_to_owned()?,
            StdioKind::Stdout => std::io::stdout().as_handle().try_clone_to_owned()?,
            StdioKind::Stderr => std::io::stderr().as_handle().try_clone_to_owned()?,
        };
        Ok(File::from(owned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream, UdpSocket};

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn open_file_rejects_invalid_mode() {
        let path = temp_path("coil_native_handle_bad_mode.bin");
        match NativeHandle::open_file(path.to_str().unwrap(), "x") {
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("invalid mode must fail"),
        }
    }

    #[test]
    fn open_file_append_preserves_prior_bytes() {
        let path = temp_path("coil_native_handle_append.bin");
        let _ = std::fs::remove_file(&path);
        {
            let mut w = NativeHandle::open_file(path.to_str().unwrap(), "w").expect("open w");
            w.write_all(b"ab").expect("write");
        }
        {
            let mut a = NativeHandle::open_file(path.to_str().unwrap(), "a").expect("open a");
            a.write_all(b"cd").expect("append");
        }
        let mut r = NativeHandle::open_file(path.to_str().unwrap(), "r").expect("open r");
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).expect("read");
        assert_eq!(buf, b"abcd");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_file_rw_round_trip() {
        let path = temp_path("coil_native_handle_rw.bin");
        let _ = std::fs::remove_file(&path);
        let mut h = NativeHandle::open_file(path.to_str().unwrap(), "rw").expect("open rw");
        h.write_all(b"xy").expect("write");
        h.flush().expect("flush");
        // Re-open for a clean read cursor (platform seek behavior varies).
        drop(h);
        let mut r = NativeHandle::open_file(path.to_str().unwrap(), "r").expect("reopen");
        let mut buf = [0u8; 2];
        r.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"xy");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn listener_read_write_are_invalid_input() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let mut h = NativeHandle::Listener(listener);
        assert_eq!(
            h.read(&mut [0u8; 1]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            h.write(b"x").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        h.flush().expect("listener flush is a no-op");
    }

    #[test]
    fn accessors_match_variants() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let udp = UdpSocket::bind("127.0.0.1:0").expect("udp");

        let mut tcp = NativeHandle::Tcp(server);
        assert!(tcp.as_tcp_mut().is_some());
        assert!(tcp.as_listener_mut().is_none());
        assert!(tcp.as_udp_mut().is_none());

        let mut listen = NativeHandle::Listener(TcpListener::bind("127.0.0.1:0").expect("bind2"));
        assert!(listen.as_listener_mut().is_some());
        assert!(listen.as_tcp_mut().is_none());

        let mut u = NativeHandle::Udp(udp);
        assert!(u.as_udp_mut().is_some());
        assert!(u.as_tcp_mut().is_none());

        drop(client);
        drop(tcp);
    }

    #[test]
    fn wait_handle_stable_for_same_tcp() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let from_tcp = WaitHandle::from_tcp(&server);
        let h = NativeHandle::Tcp(server);
        let a = h.wait_handle();
        let b = h.wait_handle();
        assert_eq!(a, b);
        assert_eq!(a, from_tcp);
        drop(client);
    }

    #[test]
    fn set_nonblocking_on_tcp_ok() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        let h = NativeHandle::Tcp(server);
        h.set_nonblocking(true).expect("nb");
        h.set_nonblocking(false).expect("blocking");
        drop(client);
    }

    #[test]
    fn file_wait_handle_is_copy_eq() {
        let path = temp_path("coil_native_handle_wait.bin");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"z").expect("write");
        let h = NativeHandle::open_file(path.to_str().unwrap(), "r").expect("open");
        let a = h.wait_handle();
        let b = h.wait_handle();
        assert_eq!(a, b);
        let _ = std::fs::remove_file(&path);
    }
}
