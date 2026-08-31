//! IO readiness reactor — sibling of the CPU work-stealing [`crate::reactor::Reactor`].
//!
//! `await_*` and package attach handshake waits block here via a single-handle wait
//! (`poll` on Unix, `WSAPoll` / `WaitForSingleObject` on Windows). Userland
//! sync adapters reach the same path through `await_readable` / `await_writable`.
//! Async waiters register interest and are woken when [`IoReactor::poll_once`]
//! observes readiness.

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::io::IoErrorTag;
use crate::io_handle::WaitHandle;

/// Readiness interest for a native IO handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interest {
    Readable,
    Writable,
}

impl Interest {
    fn poll_events(self) -> i16 {
        #[cfg(unix)]
        {
            match self {
                Self::Readable => libc::POLLIN,
                Self::Writable => libc::POLLOUT,
            }
        }
        #[cfg(windows)]
        {
            match self {
                Self::Readable => win::POLLIN,
                Self::Writable => win::POLLOUT,
            }
        }
    }
}

/// Token identifying an async waiter registered with the reactor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaitToken(pub u64);

/// One async readiness subscription.
struct AsyncWait {
    handle: WaitHandle,
    interest: Interest,
    done: bool,
}

struct Inner {
    next_token: AtomicU64,
    waits: Mutex<HashMap<WaitToken, AsyncWait>>,
    ready: Mutex<Vec<WaitToken>>,
    cvar: Condvar,
}

/// Per-root-VM IO readiness reactor.
pub struct IoReactor {
    inner: Inner,
}

impl IoReactor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Inner {
                next_token: AtomicU64::new(1),
                waits: Mutex::new(HashMap::new()),
                ready: Mutex::new(Vec::new()),
                cvar: Condvar::new(),
            },
        })
    }

    /// Block until `handle` is ready for `interest`, or `timeout` elapses.
    ///
    /// Used by sync adapters and package attach handshake (COI-116). Prefer
    /// [`Self::wait_fd_helping`] when a CPU reactor is available so fork-join
    /// work can progress during the wait.
    pub fn wait_fd(
        &self,
        handle: WaitHandle,
        interest: Interest,
        timeout: Option<Duration>,
    ) -> Result<(), IoErrorTag> {
        poll_one(handle, interest, timeout)
    }

    /// Like [`Self::wait_fd`], but invokes `help` between short poll slices so
    /// the caller can steal CPU jobs / drive other work (true async overlap).
    pub fn wait_fd_helping(
        &self,
        handle: WaitHandle,
        interest: Interest,
        timeout: Option<Duration>,
        mut help: impl FnMut(),
    ) -> Result<(), IoErrorTag> {
        let deadline = timeout.map(|d| Instant::now() + d);
        // Short slices keep help responsive without spinning.
        const SLICE: Duration = Duration::from_millis(1);
        loop {
            let slice = match deadline {
                None => Some(SLICE),
                Some(end) => {
                    let now = Instant::now();
                    if now >= end {
                        return Err(IoErrorTag::TimedOut);
                    }
                    Some((end - now).min(SLICE))
                }
            };
            match poll_one(handle, interest, slice) {
                Ok(()) => return Ok(()),
                Err(IoErrorTag::TimedOut) => {
                    help();
                    if deadline.is_none() {
                        // Infinite wait: TimedOut on slice means not ready yet.
                        continue;
                    }
                    let now = Instant::now();
                    if let Some(end) = deadline {
                        if now >= end {
                            return Err(IoErrorTag::TimedOut);
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Register an async waiter; returns a token woken by [`Self::poll_once`].
    pub fn register_wait(&self, handle: WaitHandle, interest: Interest) -> WaitToken {
        let token = WaitToken(self.inner.next_token.fetch_add(1, Ordering::Relaxed));
        self.inner
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                token,
                AsyncWait {
                    handle,
                    interest,
                    done: false,
                },
            );
        token
    }

    /// Cancel a waiter (e.g. stream closed); safe if already ready.
    pub fn cancel_wait(&self, token: WaitToken) {
        self.inner
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&token);
    }

    /// Drop every async waiter on this handle (stream close).
    pub fn cancel_waits_for(&self, handle: WaitHandle) {
        self.inner
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, w| w.handle != handle);
        self.inner.cvar.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn has_wait(&self, token: WaitToken) -> bool {
        self.inner
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&token)
    }

    /// Block until `token` is marked ready (or cancelled → TimedOut-like Other).
    pub fn wait_token(
        &self,
        token: WaitToken,
        timeout: Option<Duration>,
    ) -> Result<(), IoErrorTag> {
        let deadline = timeout.map(|d| Instant::now() + d);
        let mut ready = self.inner.ready.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if ready.iter().any(|t| *t == token) {
                ready.retain(|t| *t != token);
                self.inner
                    .waits
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&token);
                return Ok(());
            }
            // Still registered?
            if !self
                .inner
                .waits
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&token)
            {
                return Err(IoErrorTag::Other);
            }
            let wait_dur = match deadline {
                None => Duration::from_millis(50),
                Some(end) => {
                    let now = Instant::now();
                    if now >= end {
                        self.cancel_wait(token);
                        return Err(IoErrorTag::TimedOut);
                    }
                    (end - now).min(Duration::from_millis(50))
                }
            };
            // Drive readiness while waiting.
            drop(ready);
            let _ = self.poll_once(Some(Duration::ZERO));
            ready = self.inner.ready.lock().unwrap_or_else(|e| e.into_inner());
            let (guard, timed_out) = self
                .inner
                .cvar
                .wait_timeout(ready, wait_dur)
                .unwrap_or_else(|e| e.into_inner());
            ready = guard;
            if timed_out.timed_out() {
                let _ = self.poll_once(Some(Duration::ZERO));
            }
        }
    }

    /// Poll registered waiters once; marks ready tokens and notifies.
    ///
    /// Returns the number of newly ready waiters.
    pub fn poll_once(&self, timeout: Option<Duration>) -> usize {
        let snapshot: Vec<(WaitToken, WaitHandle, Interest)> = {
            let waits = self.inner.waits.lock().unwrap_or_else(|e| e.into_inner());
            waits
                .iter()
                .filter(|(_, w)| !w.done)
                .map(|(t, w)| (*t, w.handle, w.interest))
                .collect()
        };
        if snapshot.is_empty() {
            if let Some(d) = timeout {
                if !d.is_zero() {
                    std::thread::sleep(d.min(Duration::from_millis(1)));
                }
            }
            return 0;
        }
        let timeout_ms = match timeout {
            None => 0, // non-blocking by default for poll_once
            Some(d) if d.is_zero() => 0,
            Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
        };
        let ready_idx = poll_many(&snapshot, timeout_ms);
        if ready_idx.is_empty() {
            return 0;
        }
        let mut n = 0usize;
        let mut waits = self.inner.waits.lock().unwrap_or_else(|e| e.into_inner());
        let mut ready = self.inner.ready.lock().unwrap_or_else(|e| e.into_inner());
        for i in ready_idx {
            let token = snapshot[i].0;
            if let Some(w) = waits.get_mut(&token) {
                if !w.done {
                    w.done = true;
                    ready.push(token);
                    n += 1;
                }
            }
        }
        if n > 0 {
            self.inner.cvar.notify_all();
        }
        n
    }

    /// True when at least one async waiter is still registered.
    pub fn has_waiters(&self) -> bool {
        !self
            .inner
            .waits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Block until at least one registered waiter is ready (or `timeout`).
    ///
    /// Returns newly-ready count. Returns `0` immediately when nothing is
    /// registered — callers interleaving CPU yields can loop without parking.
    pub fn wait_any(&self, timeout: Option<Duration>) -> usize {
        if !self.has_waiters() {
            return 0;
        }
        let deadline = timeout.map(|d| Instant::now() + d);
        const SLICE: Duration = Duration::from_millis(50);
        loop {
            {
                let ready = self.inner.ready.lock().unwrap_or_else(|e| e.into_inner());
                if !ready.is_empty() {
                    return ready.len();
                }
            }
            let slice = match deadline {
                None => Some(SLICE),
                Some(end) => {
                    let now = Instant::now();
                    if now >= end {
                        return 0;
                    }
                    Some((end - now).min(SLICE))
                }
            };
            let n = self.poll_once(slice);
            if n > 0 {
                return n;
            }
            if !self.has_waiters() {
                return 0;
            }
            if let Some(end) = deadline
                && Instant::now() >= end
            {
                return 0;
            }
        }
    }
}

impl Default for IoReactor {
    fn default() -> Self {
        // Prefer Arc::new via IoReactor::new for sharing; Default for tests.
        Self {
            inner: Inner {
                next_token: AtomicU64::new(1),
                waits: Mutex::new(HashMap::new()),
                ready: Mutex::new(Vec::new()),
                cvar: Condvar::new(),
            },
        }
    }
}

fn poll_one(
    handle: WaitHandle,
    interest: Interest,
    timeout: Option<Duration>,
) -> Result<(), IoErrorTag> {
    let timeout_ms = match timeout {
        None => -1,
        Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
    };
    match poll_one_ms(handle, interest, timeout_ms) {
        Ok(true) => Ok(()),
        Ok(false) => Err(IoErrorTag::TimedOut),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::raw::HANDLE;

    pub const POLLIN: i16 = 0x0100 | 0x0200; // POLLRDNORM | POLLRDBAND
    pub const POLLOUT: i16 = 0x0010; // POLLWRNORM
    pub const INFINITE: u32 = 0xFFFF_FFFF;
    pub const WAIT_OBJECT_0: u32 = 0;
    pub const WAIT_TIMEOUT: u32 = 258;

    #[repr(C)]
    pub struct WSAPOLLFD {
        pub fd: usize,
        pub events: i16,
        pub revents: i16,
    }

    #[link(name = "ws2_32")]
    unsafe extern "system" {
        pub fn WSAPoll(fds: *mut WSAPOLLFD, nfds: u32, timeout: i32) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn WaitForSingleObject(handle: HANDLE, millis: u32) -> u32;
    }
}

/// `Ok(true)` ready, `Ok(false)` timeout, `Err` hard failure.
fn poll_one_ms(
    handle: WaitHandle,
    interest: Interest,
    timeout_ms: i32,
) -> Result<bool, IoErrorTag> {
    #[cfg(unix)]
    {
        let mut pfd = libc::pollfd {
            fd: handle.as_raw_fd(),
            events: interest.poll_events(),
            revents: 0,
        };
        loop {
            let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if rc < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(IoErrorTag::from_kind(err.kind()));
            }
            return Ok(rc > 0);
        }
    }
    #[cfg(windows)]
    {
        if let Some(sock) = handle.as_raw_socket() {
            let mut pfd = win::WSAPOLLFD {
                fd: sock as usize,
                events: interest.poll_events(),
                revents: 0,
            };
            loop {
                let rc = unsafe { win::WSAPoll(&mut pfd, 1, timeout_ms) };
                if rc < 0 {
                    let err = io::Error::last_os_error();
                    if err.kind() == ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(IoErrorTag::from_kind(err.kind()));
                }
                return Ok(rc > 0);
            }
        }
        let Some(raw) = handle.as_raw_handle() else {
            return Err(IoErrorTag::Other);
        };
        let ms = if timeout_ms < 0 {
            win::INFINITE
        } else {
            timeout_ms as u32
        };
        let rc = unsafe { win::WaitForSingleObject(raw, ms) };
        if rc == win::WAIT_OBJECT_0 {
            Ok(true)
        } else if rc == win::WAIT_TIMEOUT {
            Ok(false)
        } else {
            Err(IoErrorTag::Other)
        }
    }
}

fn poll_many(snapshot: &[(WaitToken, WaitHandle, Interest)], timeout_ms: i32) -> Vec<usize> {
    #[cfg(unix)]
    {
        let mut pfds: Vec<libc::pollfd> = snapshot
            .iter()
            .map(|(_, handle, interest)| libc::pollfd {
                fd: handle.as_raw_fd(),
                events: interest.poll_events(),
                revents: 0,
            })
            .collect();
        let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };
        if rc <= 0 {
            return Vec::new();
        }
        pfds.iter()
            .enumerate()
            .filter(|(_, p)| p.revents != 0)
            .map(|(i, _)| i)
            .collect()
    }
    #[cfg(windows)]
    {
        let mut ready = Vec::new();
        let mut sock_idx = Vec::new();
        let mut pfds = Vec::new();
        for (i, (_, handle, interest)) in snapshot.iter().enumerate() {
            if let Some(sock) = handle.as_raw_socket() {
                sock_idx.push(i);
                pfds.push(win::WSAPOLLFD {
                    fd: sock as usize,
                    events: interest.poll_events(),
                    revents: 0,
                });
            } else if let Some(raw) = handle.as_raw_handle() {
                let rc = unsafe { win::WaitForSingleObject(raw, 0) };
                if rc == win::WAIT_OBJECT_0 {
                    ready.push(i);
                }
            }
        }
        let sock_timeout = if ready.is_empty() { timeout_ms } else { 0 };
        if !pfds.is_empty() {
            let rc = unsafe { win::WSAPoll(pfds.as_mut_ptr(), pfds.len() as u32, sock_timeout) };
            if rc > 0 {
                for (j, pfd) in pfds.iter().enumerate() {
                    if pfd.revents != 0 {
                        ready.push(sock_idx[j]);
                    }
                }
            }
        } else if ready.is_empty() && timeout_ms > 0 {
            std::thread::sleep(
                Duration::from_millis(timeout_ms as u64).min(Duration::from_millis(1)),
            );
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    fn wait_of(stream: &TcpStream) -> WaitHandle {
        WaitHandle::from_tcp(stream)
    }

    #[test]
    fn wait_fd_times_out_on_empty_pipe_read() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        let err = io
            .wait_fd(
                wait_of(&r),
                Interest::Readable,
                Some(Duration::from_millis(20)),
            )
            .expect_err("empty socket must not be readable");
        assert_eq!(err, IoErrorTag::TimedOut);
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_fd_succeeds_when_pipe_has_data() {
        let (r, mut w) = tcp_pair();
        w.write_all(b"x").expect("write");
        let io = IoReactor::new();
        io.wait_fd(
            wait_of(&r),
            Interest::Readable,
            Some(Duration::from_millis(50)),
        )
        .expect("socket with data should be readable");
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_fd_helping_invokes_help_until_timeout() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        let mut helps = 0usize;
        let err = io
            .wait_fd_helping(
                wait_of(&r),
                Interest::Readable,
                Some(Duration::from_millis(5)),
                || helps += 1,
            )
            .expect_err("must time out");
        assert_eq!(err, IoErrorTag::TimedOut);
        assert!(helps > 0, "help callback should run between poll slices");
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_fd_helping_returns_when_ready_after_help() {
        let (r, mut w) = tcp_pair();
        let io = IoReactor::new();
        let mut helps = 0usize;
        let err = io.wait_fd_helping(
            wait_of(&r),
            Interest::Readable,
            Some(Duration::from_millis(200)),
            || {
                helps += 1;
                if helps == 1 {
                    w.write_all(b"y").expect("write");
                }
            },
        );
        assert!(err.is_ok(), "should become readable after help writes");
        assert!(helps >= 1);
        drop(r);
        drop(w);
    }

    #[test]
    fn register_poll_marks_ready_when_readable() {
        let (r, mut w) = tcp_pair();
        let io = IoReactor::new();
        let tok = io.register_wait(wait_of(&r), Interest::Readable);
        assert_eq!(io.poll_once(Some(Duration::ZERO)), 0);
        w.write_all(b"z").expect("write");
        assert_eq!(io.poll_once(Some(Duration::ZERO)), 1);
        // Second poll should not re-count the already-done waiter.
        assert_eq!(io.poll_once(Some(Duration::ZERO)), 0);
        io.wait_token(tok, Some(Duration::from_millis(10)))
            .expect("token already ready");
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_token_after_cancel_returns_other() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        let tok = io.register_wait(wait_of(&r), Interest::Readable);
        io.cancel_wait(tok);
        let err = io
            .wait_token(tok, Some(Duration::from_millis(20)))
            .expect_err("cancelled token");
        assert_eq!(err, IoErrorTag::Other);
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_token_times_out_when_never_ready() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        let tok = io.register_wait(wait_of(&r), Interest::Readable);
        let err = io
            .wait_token(tok, Some(Duration::from_millis(30)))
            .expect_err("must time out");
        assert_eq!(err, IoErrorTag::TimedOut);
        // Timed-out wait cancels the registration.
        let err2 = io
            .wait_token(tok, Some(Duration::from_millis(10)))
            .expect_err("already cancelled");
        assert_eq!(err2, IoErrorTag::Other);
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_any_returns_zero_without_waiters() {
        let io = IoReactor::new();
        assert_eq!(io.wait_any(Some(Duration::from_millis(5))), 0);
    }

    #[test]
    fn wait_any_times_out_with_unready_waiter() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        let tok = io.register_wait(wait_of(&r), Interest::Readable);
        assert!(io.has_waiters());
        assert_eq!(
            io.wait_any(Some(Duration::from_millis(30))),
            0,
            "empty socket must not become readable within timeout"
        );
        assert!(
            io.has_waiters(),
            "timed-out wait_any must leave the waiter registered"
        );
        io.cancel_wait(tok);
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_any_batches_two_pipe_waiters() {
        let (r1, mut w1) = tcp_pair();
        let (r2, mut w2) = tcp_pair();
        let io = IoReactor::new();
        let _t1 = io.register_wait(wait_of(&r1), Interest::Readable);
        let _t2 = io.register_wait(wait_of(&r2), Interest::Readable);
        assert_eq!(io.poll_once(Some(Duration::ZERO)), 0);
        w1.write_all(b"a").expect("write");
        w2.write_all(b"b").expect("write");
        let ready = io.wait_any(Some(Duration::from_millis(100)));
        assert!(
            ready >= 1,
            "expected at least one ready waiter, got {ready}"
        );
        drop(r1);
        drop(w1);
        drop(r2);
        drop(w2);
    }

    #[test]
    fn poll_once_empty_returns_zero() {
        let io = IoReactor::new();
        assert_eq!(io.poll_once(Some(Duration::ZERO)), 0);
        assert_eq!(io.poll_once(None), 0);
    }

    #[test]
    fn wait_fd_writable_succeeds_for_connected_tcp() {
        let (r, w) = tcp_pair();
        let io = IoReactor::new();
        io.wait_fd(
            wait_of(&w),
            Interest::Writable,
            Some(Duration::from_millis(50)),
        )
        .expect("connected TCP write end should be writable");
        drop(r);
        drop(w);
    }

    #[test]
    fn wait_fd_file_handle_reports_readable() {
        // Regular files use WaitHandle::File on Windows (WaitForSingleObject) and
        // a pollable fd on Unix — exercise NativeHandle::File → reactor, not only TCP.
        let path = std::env::temp_dir().join("coil_io_reactor_file_wait.bin");
        std::fs::write(&path, b"ready").expect("write");
        let handle = crate::io_handle::NativeHandle::open_file(path.to_str().unwrap(), "r")
            .expect("open file");
        let io = IoReactor::new();
        io.wait_fd(
            handle.wait_handle(),
            Interest::Readable,
            Some(Duration::from_millis(100)),
        )
        .expect("open regular file should be readable");
        drop(handle);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn poll_once_marks_file_waiter_ready() {
        let path = std::env::temp_dir().join("coil_io_reactor_file_poll.bin");
        std::fs::write(&path, b"x").expect("write");
        let handle = crate::io_handle::NativeHandle::open_file(path.to_str().unwrap(), "r")
            .expect("open file");
        let io = IoReactor::new();
        let tok = io.register_wait(handle.wait_handle(), Interest::Readable);
        assert!(
            io.poll_once(Some(Duration::from_millis(20))) >= 1,
            "file waiter should become ready"
        );
        io.wait_token(tok, Some(Duration::from_millis(10)))
            .expect("token already ready");
        drop(handle);
        let _ = std::fs::remove_file(&path);
    }
}
