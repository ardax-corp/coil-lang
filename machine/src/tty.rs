//! Terminal host natives (`io::tty`): whether a stream is a terminal, its
//! size, raw mode, a timed read and window-resize notices.
//!
//! POSIX uses `isatty` / termios / `TIOCGWINSZ` / `poll` / `SIGWINCH`;
//! Windows uses the console API (`GetConsoleMode`, `SetConsoleMode`,
//! `GetConsoleScreenBufferInfo`, `WaitForSingleObject`).
//!
//! Raw mode is process-wide state of the terminal device, not of one
//! `Stream`: `enter_raw` saves the device's settings once (two streams on
//! the same terminal share them) and keeps its own handle to restore them
//! with. Whatever is still raw is restored when the process exits (normal
//! end, `env::exit`, an uncaught panic) and on SIGINT / SIGTERM / SIGHUP /
//! SIGQUIT (Ctrl-C / close on Windows) while their handling is the default.

use common::Value;

use crate::io::{
    IoErrorTag, alloc_io_error, alloc_option_none, alloc_option_some, alloc_result_err,
    alloc_result_ok, is_capturing, stream_read, with_stream_mut,
};
use crate::memory::{EnumPayload, Heap, ObjTuple, Object, StreamKind};

/// Why an `io::tty` call failed: a `TtyError` (`NotATerminal` = 0,
/// `Io(IoError)` = 1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TtyFail {
    NotATerminal,
    Io(IoErrorTag),
}

impl From<IoErrorTag> for TtyFail {
    fn from(tag: IoErrorTag) -> Self {
        Self::Io(tag)
    }
}

/// Allocate the `TtyError` value for `fail`.
pub fn alloc_tty_error(heap: &mut Heap, fail: TtyFail) -> Value {
    let _ = common::BUILTIN_TTY_ERROR_VARIANTS;
    match fail {
        TtyFail::NotATerminal => heap.alloc_enum_value(0, EnumPayload::empty()),
        TtyFail::Io(tag) => {
            let io = alloc_io_error(heap, tag);
            heap.alloc_enum_value(1, EnumPayload::one(io))
        }
    }
}

fn tty_result_value(heap: &mut Heap, r: Result<Value, TtyFail>) -> Value {
    let mapped = r.map_err(|f| alloc_tty_error(heap, f));
    crate::host_enum::pack_result_or_panic(heap, mapped)
}

fn tty_result_unit(heap: &mut Heap, r: Result<(), TtyFail>) -> Value {
    let mapped = r.map_err(|f| alloc_tty_error(heap, f));
    crate::host_enum::pack_result_unit_or_panic(heap, mapped)
}

/// `Result<Option<int>, TtyError>`, boxed as `io::as_result_option_int`.
fn tty_result_option_int(heap: &mut Heap, r: Result<Option<usize>, TtyFail>) -> Value {
    match r {
        Ok(None) => {
            let none = alloc_option_none(heap);
            alloc_result_ok(heap, none)
        }
        Ok(Some(n)) => {
            let some = alloc_option_some(heap, Value::from(n as i64));
            alloc_result_ok(heap, some)
        }
        Err(f) => {
            let err = alloc_tty_error(heap, f);
            alloc_result_err(heap, err)
        }
    }
}

/// The OS handle under `stream` and its kind; `Err(AlreadyClosed)` once closed.
fn stream_os(heap: &mut Heap, stream: Value) -> Result<(os::Raw, StreamKind), IoErrorTag> {
    with_stream_mut(heap, stream, |s| {
        if s.closed {
            return Err(IoErrorTag::AlreadyClosed);
        }
        match s.handle.as_ref() {
            Some(h) => Ok((os::raw_of(h), s.kind)),
            None => Err(IoErrorTag::AlreadyClosed),
        }
    })?
}

fn is_std(kind: StreamKind) -> bool {
    matches!(kind, StreamKind::Stdin | StreamKind::Stdout | StreamKind::Stderr)
}

/// `io::tty::is_tty(s)`. A stdout / stderr stream whose writes an open
/// `io::capture_begin()` keeps is not a terminal.
pub fn tty_is_tty(heap: &mut Heap, stream: Value) -> bool {
    match stream_os(heap, stream) {
        Ok((_, StreamKind::Stdout | StreamKind::Stderr)) if is_capturing() => false,
        Ok((raw, _)) => os::is_tty(raw),
        Err(_) => false,
    }
}

/// `io::tty::size(s)` as `(cols, rows)`. A standard stream that is not a
/// terminal falls back to the other standard streams.
pub fn tty_size(heap: &mut Heap, stream: Value) -> Result<(i64, i64), TtyFail> {
    let (raw, kind) = stream_os(heap, stream)?;
    if let Some(size) = os::size(raw) {
        return Ok(size);
    }
    if is_std(kind)
        && let Some(size) = os::std_size()
    {
        return Ok(size);
    }
    Err(TtyFail::NotATerminal)
}

// Unix `enter_raw` returns IoErrorTag (converted by `?`); Windows and wasm already return TtyFail.
#[allow(clippy::needless_question_mark)]
pub fn tty_enter_raw(
    heap: &mut Heap,
    stream: Value,
    keep_signals: bool,
    keep_output: bool,
) -> Result<(), TtyFail> {
    let (raw, _) = stream_os(heap, stream)?;
    if !os::is_tty(raw) {
        return Err(TtyFail::NotATerminal);
    }
    Ok(os::enter_raw(raw, keep_signals, keep_output)?)
}

pub fn tty_leave_raw(heap: &mut Heap, stream: Value) -> Result<(), TtyFail> {
    let (raw, _) = stream_os(heap, stream)?;
    Ok(os::leave_raw(raw)?)
}

pub fn tty_is_raw(heap: &mut Heap, stream: Value) -> bool {
    stream_os(heap, stream).is_ok_and(|(raw, _)| os::is_raw(raw))
}

/// `io::tty::read_timeout(s, buf, ms)`: wait up to `ms` (`< 0` forever) for
/// input, then read what is there into `buf`. `Some(0)` is a timeout or a
/// wait cut short (a resize), `None` the end of input.
pub fn tty_read_timeout(
    heap: &mut Heap,
    stream: Value,
    buf: Value,
    timeout_ms: i64,
) -> Result<Option<usize>, TtyFail> {
    let (raw, _) = stream_os(heap, stream)?;
    match os::wait_readable(raw, timeout_ms)? {
        Wait::Ready => {}
        Wait::TimedOut => return Ok(Some(0)),
        Wait::Eof => return Ok(None),
    }
    match stream_read(heap, stream, buf) {
        Err(IoErrorTag::WouldBlock) => Ok(Some(0)),
        r => Ok(r?),
    }
}

/// `io::tty::resized()`: true once per window resize since the last call.
pub fn tty_resized() -> bool {
    os::resized()
}

/// `io::tty::enable_vt(s)`: Windows turns on VT processing for an output
/// console; everywhere else terminals already understand escape sequences.
pub fn tty_enable_vt(heap: &mut Heap, stream: Value) -> bool {
    match stream_os(heap, stream) {
        Ok((raw, _)) => os::enable_vt(raw),
        Err(_) => false,
    }
}

/// Restore every terminal still in raw mode (the VM's exit path and tests).
pub fn restore_all() {
    os::restore_all();
}

#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
enum Wait {
    Ready,
    TimedOut,
    Eof,
}

fn alloc_size(heap: &mut Heap, (cols, rows): (i64, i64)) -> Value {
    let (obj, _) = heap.alloc(
        ObjTuple::new(vec![Value::from(cols), Value::from(rows)]),
        Object::Tuple,
    );
    Value::from(obj.addr())
}

fn host_is_tty(heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(tty_is_tty(heap, args[0]))
}

fn host_size(heap: &mut Heap, args: &[Value]) -> Value {
    let r = tty_size(heap, args[0]).map(|s| alloc_size(heap, s));
    tty_result_value(heap, r)
}

fn host_enter_raw(heap: &mut Heap, args: &[Value]) -> Value {
    let r = tty_enter_raw(heap, args[0], args[1].as_bool(), args[2].as_bool());
    tty_result_unit(heap, r)
}

fn host_leave_raw(heap: &mut Heap, args: &[Value]) -> Value {
    let r = tty_leave_raw(heap, args[0]);
    tty_result_unit(heap, r)
}

fn host_is_raw(heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(tty_is_raw(heap, args[0]))
}

fn host_read_timeout(heap: &mut Heap, args: &[Value]) -> Value {
    let r = tty_read_timeout(heap, args[0], args[1], args[2].as_int());
    tty_result_option_int(heap, r)
}

fn host_resized(_heap: &mut Heap, _args: &[Value]) -> Value {
    Value::from(tty_resized())
}

fn host_enable_vt(heap: &mut Heap, args: &[Value]) -> Value {
    Value::from(tty_enable_vt(heap, args[0]))
}

/// Pipeline wiring: `(registry_name, arity, host_fn)`, in HostInvoke order.
pub const TTY_WIRING: &[(&str, usize, crate::HostValueFn)] = &[
    (common::TTY_IS_TTY_NATIVE, 1, host_is_tty),
    (common::TTY_SIZE_NATIVE, 1, host_size),
    (common::TTY_ENTER_RAW_NATIVE, 3, host_enter_raw),
    (common::TTY_LEAVE_RAW_NATIVE, 1, host_leave_raw),
    (common::TTY_IS_RAW_NATIVE, 1, host_is_raw),
    (common::TTY_READ_TIMEOUT_NATIVE, 3, host_read_timeout),
    (common::TTY_RESIZED_NATIVE, 0, host_resized),
    (common::TTY_ENABLE_VT_NATIVE, 1, host_enable_vt),
];

#[cfg(unix)]
mod os {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::Mutex;
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use super::Wait;
    use crate::io::IoErrorTag;
    use crate::io_handle::NativeHandle;

    pub type Raw = libc::c_int;

    pub fn raw_of(h: &NativeHandle) -> Raw {
        h.as_raw_fd()
    }

    fn last_error() -> IoErrorTag {
        IoErrorTag::from_kind(std::io::Error::last_os_error().kind())
    }

    pub fn is_tty(fd: Raw) -> bool {
        // SAFETY: `isatty` only inspects the descriptor.
        unsafe { libc::isatty(fd) == 1 }
    }

    pub fn size(fd: Raw) -> Option<(i64, i64)> {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ writes one `winsize`.
        let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
        (rc == 0 && ws.ws_col > 0 && ws.ws_row > 0).then_some((ws.ws_col as i64, ws.ws_row as i64))
    }

    pub fn std_size() -> Option<(i64, i64)> {
        [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO]
            .into_iter()
            .find_map(size)
    }

    /// The terminal device behind `fd` (two streams on one terminal share it).
    fn device(fd: Raw) -> Option<(u64, u64)> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fstat` writes one `stat`.
        (unsafe { libc::fstat(fd, &mut st) } == 0).then_some((st.st_dev as u64, st.st_ino as u64))
    }

    /// Terminals in raw mode at once; more is an error.
    const SLOTS: usize = 8;

    /// The saved settings of slot `i` live in `SAVED[i]` while `FDS[i]` holds
    /// the slot's own descriptor (`-1`: free). Writers hold [`SLOT_LOCK`]
    /// and publish `FDS[i]` after `SAVED[i]`; the exit and signal paths
    /// take the descriptor with a swap so each slot is restored once.
    struct Saved(UnsafeCell<[MaybeUninit<libc::termios>; SLOTS]>);
    // SAFETY: see the protocol above.
    unsafe impl Sync for Saved {}

    static SAVED: Saved = Saved(UnsafeCell::new([const { MaybeUninit::uninit() }; SLOTS]));
    static FDS: [AtomicI32; SLOTS] = [const { AtomicI32::new(-1) }; SLOTS];
    static SLOT_LOCK: Mutex<[Option<(u64, u64)>; SLOTS]> = Mutex::new([None; SLOTS]);
    static INSTALL: Once = Once::new();
    static RESIZED: AtomicBool = AtomicBool::new(false);
    static WINCH: Once = Once::new();

    fn slot_of(keys: &[Option<(u64, u64)>; SLOTS], key: (u64, u64)) -> Option<usize> {
        keys.iter().position(|k| *k == Some(key))
    }

    pub fn enter_raw(fd: Raw, keep_signals: bool, keep_output: bool) -> Result<(), IoErrorTag> {
        let key = device(fd).ok_or_else(last_error)?;
        install_restore();
        install_winch();
        let mut keys = SLOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (slot, saved) = match slot_of(&keys, key) {
            // Already raw: apply the new flags to the settings saved first.
            // SAFETY: a used slot's `SAVED` entry is initialized.
            Some(i) => (i, unsafe { (*SAVED.0.get())[i].assume_init() }),
            None => {
                let mut t = MaybeUninit::<libc::termios>::uninit();
                // SAFETY: `tcgetattr` fills `t` on success.
                if unsafe { libc::tcgetattr(fd, t.as_mut_ptr()) } != 0 {
                    return Err(last_error());
                }
                let saved = unsafe { t.assume_init() };
                let Some(i) = keys.iter().position(Option::is_none) else {
                    return Err(IoErrorTag::Other);
                };
                // SAFETY: `dup` of a valid descriptor; the slot owns it.
                let own = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
                if own < 0 {
                    return Err(last_error());
                }
                // SAFETY: slot `i` is free (FDS[i] == -1), so no reader
                // looks at SAVED[i] until FDS[i] is published below.
                unsafe { (*SAVED.0.get())[i] = MaybeUninit::new(saved) };
                FDS[i].store(own, Ordering::Release);
                keys[i] = Some(key);
                (i, saved)
            }
        };
        let mut t = saved;
        t.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
        if !keep_output {
            t.c_oflag &= !libc::OPOST;
        }
        t.c_cflag = (t.c_cflag & !(libc::CSIZE | libc::PARENB)) | libc::CS8;
        t.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN);
        if !keep_signals {
            t.c_lflag &= !libc::ISIG;
        }
        t.c_cc[libc::VMIN] = 1;
        t.c_cc[libc::VTIME] = 0;
        // SAFETY: `t` is a full termios from `tcgetattr`.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) } != 0 {
            let err = last_error();
            release(&mut keys, slot);
            return Err(err);
        }
        Ok(())
    }

    /// Restore slot `i` and free it (caller holds [`SLOT_LOCK`]).
    fn release(keys: &mut [Option<(u64, u64)>; SLOTS], i: usize) -> bool {
        keys[i] = None;
        restore_slot(i)
    }

    /// Async-signal-safe: `tcsetattr` and `close` only.
    fn restore_slot(i: usize) -> bool {
        let fd = FDS[i].swap(-1, Ordering::AcqRel);
        if fd < 0 {
            return true;
        }
        // SAFETY: FDS[i] was published after SAVED[i] was written.
        let ok = unsafe { libc::tcsetattr(fd, libc::TCSANOW, (*SAVED.0.get())[i].as_ptr()) } == 0;
        unsafe { libc::close(fd) };
        ok
    }

    pub fn leave_raw(fd: Raw) -> Result<(), IoErrorTag> {
        let Some(key) = device(fd) else {
            return Ok(());
        };
        let mut keys = SLOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        match slot_of(&keys, key) {
            Some(i) if !release(&mut keys, i) => Err(last_error()),
            _ => Ok(()),
        }
    }

    pub fn is_raw(fd: Raw) -> bool {
        let Some(key) = device(fd) else {
            return false;
        };
        let keys = SLOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        slot_of(&keys, key).is_some()
    }

    pub fn restore_all() {
        // Free the keys first when the lock is to be had (not from a signal).
        if let Ok(mut keys) = SLOT_LOCK.try_lock() {
            *keys = [None; SLOTS];
        }
        for i in 0..SLOTS {
            restore_slot(i);
        }
    }

    extern "C" fn restore_at_exit() {
        restore_all();
    }

    extern "C" fn restore_on_signal(sig: libc::c_int) {
        for i in 0..SLOTS {
            restore_slot(i);
        }
        // SAFETY: back to the default action, then deliver it again.
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }

    /// Restore at exit, and on the terminating signals whose handling is
    /// still the default (a host's own handler is left alone).
    fn install_restore() {
        INSTALL.call_once(|| unsafe {
            libc::atexit(restore_at_exit);
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) != 0
                    || old.sa_sigaction != libc::SIG_DFL
                {
                    continue;
                }
                let mut act: libc::sigaction = std::mem::zeroed();
                act.sa_sigaction = restore_on_signal as *const () as usize;
                libc::sigemptyset(&mut act.sa_mask);
                libc::sigaction(sig, &act, std::ptr::null_mut());
            }
        });
    }

    extern "C" fn on_winch(_sig: libc::c_int) {
        RESIZED.store(true, Ordering::Relaxed);
    }

    /// SIGWINCH sets [`RESIZED`]. No `SA_RESTART`, so a `read_timeout`
    /// waiting in `poll` returns at once.
    fn install_winch() {
        WINCH.call_once(|| unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGWINCH, std::ptr::null(), &mut old) != 0
                || (old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN)
            {
                return;
            }
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = on_winch as *const () as usize;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(libc::SIGWINCH, &act, std::ptr::null_mut());
        });
    }

    pub fn resized() -> bool {
        install_winch();
        RESIZED.swap(false, Ordering::Relaxed)
    }

    pub fn wait_readable(fd: Raw, timeout_ms: i64) -> Result<Wait, IoErrorTag> {
        install_winch();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = if timeout_ms < 0 {
            -1
        } else {
            timeout_ms.min(i32::MAX as i64) as libc::c_int
        };
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(Wait::TimedOut);
            }
            return Err(IoErrorTag::from_kind(e.kind()));
        }
        if rc == 0 {
            return Ok(Wait::TimedOut);
        }
        if pfd.revents & libc::POLLNVAL != 0 {
            return Err(IoErrorTag::AlreadyClosed);
        }
        if pfd.revents & libc::POLLIN == 0 && pfd.revents & libc::POLLHUP != 0 {
            return Ok(Wait::Eof);
        }
        Ok(Wait::Ready)
    }

    pub fn enable_vt(_fd: Raw) -> bool {
        true
    }
}

#[cfg(windows)]
mod os {
    use std::os::windows::io::AsRawHandle;
    use std::sync::Mutex;
    use std::sync::Once;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::Wait;
    use crate::io::IoErrorTag;
    use crate::io_handle::NativeHandle;

    pub type Raw = Handle;

    type Handle = *mut core::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ScreenBufferInfo {
        size: Coord,
        cursor: Coord,
        attributes: u16,
        window: SmallRect,
        max_window: Coord,
    }

    /// `INPUT_RECORD`: a 2-byte event type, padding, then a 16-byte union.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct InputRecord {
        event_type: u16,
        _pad: u16,
        event: [u32; 4],
    }

    const KEY_EVENT: u16 = 0x0001;

    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;
    const INFINITE: u32 = 0xFFFF_FFFF;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
        fn GetStdHandle(which: u32) -> Handle;
        fn GetConsoleScreenBufferInfo(h: Handle, info: *mut ScreenBufferInfo) -> i32;
        fn WaitForSingleObject(h: Handle, ms: u32) -> u32;
        fn GetNumberOfConsoleInputEvents(h: Handle, n: *mut u32) -> i32;
        fn PeekConsoleInputW(h: Handle, buf: *mut InputRecord, len: u32, read: *mut u32) -> i32;
        fn ReadConsoleInputW(h: Handle, buf: *mut InputRecord, len: u32, read: *mut u32) -> i32;
        fn PeekNamedPipe(
            h: Handle,
            buf: *mut u8,
            size: u32,
            read: *mut u32,
            avail: *mut u32,
            left: *mut u32,
        ) -> i32;
        fn GetTickCount64() -> u64;
        fn Sleep(ms: u32);
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }

    unsafe extern "C" {
        fn atexit(cb: extern "C" fn()) -> i32;
    }

    pub fn raw_of(h: &NativeHandle) -> Raw {
        match h {
            NativeHandle::File(f) => f.as_raw_handle(),
            _ => std::ptr::null_mut(),
        }
    }

    fn console_mode(h: Handle) -> Option<u32> {
        let mut mode = 0u32;
        (!h.is_null() && unsafe { GetConsoleMode(h, &mut mode) } != 0).then_some(mode)
    }

    pub fn is_tty(h: Raw) -> bool {
        console_mode(h).is_some()
    }

    fn window_size(h: Handle) -> Option<(i64, i64)> {
        let mut info = ScreenBufferInfo::default();
        if h.is_null() || unsafe { GetConsoleScreenBufferInfo(h, &mut info) } == 0 {
            return None;
        }
        let cols = info.window.right as i64 - info.window.left as i64 + 1;
        let rows = info.window.bottom as i64 - info.window.top as i64 + 1;
        (cols > 0 && rows > 0).then_some((cols, rows))
    }

    pub fn size(h: Raw) -> Option<(i64, i64)> {
        window_size(h)
    }

    pub fn std_size() -> Option<(i64, i64)> {
        [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .into_iter()
            .find_map(|w| window_size(unsafe { GetStdHandle(w) }))
    }

    /// Saved `(input, output)` console modes while raw (one console per process).
    static SAVED: Mutex<Option<(Option<u32>, Option<u32>)>> = Mutex::new(None);
    static INSTALL: Once = Once::new();
    /// Last seen window size, `cols << 32 | rows` (`0`: not seen yet).
    static LAST_SIZE: AtomicU64 = AtomicU64::new(0);

    fn std_handles() -> (Handle, Handle) {
        unsafe { (GetStdHandle(STD_INPUT_HANDLE), GetStdHandle(STD_OUTPUT_HANDLE)) }
    }

    pub fn enter_raw(_h: Raw, keep_signals: bool, _keep_output: bool) -> Result<(), super::TtyFail> {
        install_restore();
        let (input, output) = std_handles();
        let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
        let modes = match *saved {
            Some(modes) => modes,
            None => (console_mode(input), console_mode(output)),
        };
        let (Some(in_mode), out_mode) = modes else {
            return Err(super::TtyFail::NotATerminal);
        };
        let mut m = (in_mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
            | ENABLE_VIRTUAL_TERMINAL_INPUT;
        if keep_signals {
            m |= ENABLE_PROCESSED_INPUT;
        }
        if unsafe { SetConsoleMode(input, m) } == 0 {
            return Err(IoErrorTag::from_kind(std::io::Error::last_os_error().kind()).into());
        }
        if let Some(out_mode) = out_mode {
            let _ = unsafe {
                SetConsoleMode(
                    output,
                    out_mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
                )
            };
        }
        *saved = Some(modes);
        Ok(())
    }

    fn restore(saved: &mut Option<(Option<u32>, Option<u32>)>) -> bool {
        let Some((in_mode, out_mode)) = saved.take() else {
            return true;
        };
        let (input, output) = std_handles();
        let mut ok = true;
        if let Some(m) = in_mode {
            ok &= unsafe { SetConsoleMode(input, m) } != 0;
        }
        if let Some(m) = out_mode {
            ok &= unsafe { SetConsoleMode(output, m) } != 0;
        }
        ok
    }

    pub fn leave_raw(_h: Raw) -> Result<(), IoErrorTag> {
        let mut saved = SAVED.lock().unwrap_or_else(|e| e.into_inner());
        if restore(&mut saved) {
            Ok(())
        } else {
            Err(IoErrorTag::from_kind(std::io::Error::last_os_error().kind()))
        }
    }

    pub fn is_raw(h: Raw) -> bool {
        is_tty(h) && SAVED.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn restore_all() {
        if let Ok(mut saved) = SAVED.try_lock() {
            restore(&mut saved);
        }
    }

    extern "C" fn restore_at_exit() {
        restore_all();
    }

    unsafe extern "system" fn restore_on_ctrl(_event: u32) -> i32 {
        restore_all();
        0 // let the next handler (the default: exit) run
    }

    fn install_restore() {
        INSTALL.call_once(|| unsafe {
            atexit(restore_at_exit);
            SetConsoleCtrlHandler(Some(restore_on_ctrl), 1);
        });
    }

    fn pack(size: (i64, i64)) -> u64 {
        ((size.0 as u64) << 32) | (size.1 as u64 & 0xFFFF_FFFF)
    }

    pub fn resized() -> bool {
        let Some(now) = std_size().map(pack) else {
            return false;
        };
        let last = LAST_SIZE.swap(now, Ordering::Relaxed);
        last != 0 && last != now
    }

    fn now_ms() -> u64 {
        unsafe { GetTickCount64() }
    }

    /// Whether a console input buffer holds a key press (`ReadFile` would
    /// not block); other events are dropped so waiting does not spin.
    fn console_has_key(h: Handle) -> Result<bool, IoErrorTag> {
        let mut n = 0u32;
        if unsafe { GetNumberOfConsoleInputEvents(h, &mut n) } == 0 {
            return Err(IoErrorTag::from_kind(std::io::Error::last_os_error().kind()));
        }
        if n == 0 {
            return Ok(false);
        }
        let mut recs = vec![InputRecord { event_type: 0, _pad: 0, event: [0; 4] }; n as usize];
        let mut got = 0u32;
        if unsafe { PeekConsoleInputW(h, recs.as_mut_ptr(), n, &mut got) } == 0 {
            return Err(IoErrorTag::from_kind(std::io::Error::last_os_error().kind()));
        }
        // KEY_EVENT_RECORD: bKeyDown (BOOL) first, then repeat/vk/scan,
        // then the UTF-16 char in the low half of the third word. Window
        // size events are dropped too: `resized()` compares sizes.
        let is_key = |r: &InputRecord| {
            r.event_type == KEY_EVENT && r.event[0] != 0 && (r.event[2] & 0xFFFF) != 0
        };
        if recs[..got as usize].iter().any(is_key) {
            return Ok(true);
        }
        let mut dropped = 0u32;
        unsafe { ReadConsoleInputW(h, recs.as_mut_ptr(), got, &mut dropped) };
        Ok(false)
    }

    pub fn wait_readable(h: Raw, timeout_ms: i64) -> Result<Wait, IoErrorTag> {
        let deadline = (timeout_ms >= 0).then(|| now_ms() + timeout_ms as u64);
        let left = || match deadline {
            None => INFINITE,
            Some(d) => d.saturating_sub(now_ms()).min(INFINITE as u64 - 1) as u32,
        };
        if console_mode(h).is_some() {
            loop {
                if console_has_key(h)? {
                    return Ok(Wait::Ready);
                }
                let ms = left();
                if ms == 0 {
                    return Ok(Wait::TimedOut);
                }
                match unsafe { WaitForSingleObject(h, ms) } {
                    WAIT_OBJECT_0 => continue,
                    WAIT_TIMEOUT => return Ok(Wait::TimedOut),
                    _ => return Err(IoErrorTag::Other),
                }
            }
        }
        // A pipe: poll how much is waiting. Anything else (a file) is ready.
        loop {
            let mut avail = 0u32;
            let ok = unsafe {
                PeekNamedPipe(
                    h,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut avail,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let e = std::io::Error::last_os_error();
                // ERROR_BROKEN_PIPE: the writer is gone.
                if e.raw_os_error() == Some(109) {
                    return Ok(Wait::Eof);
                }
                return Ok(Wait::Ready);
            }
            if avail > 0 {
                return Ok(Wait::Ready);
            }
            let ms = left();
            if ms == 0 {
                return Ok(Wait::TimedOut);
            }
            unsafe { Sleep(ms.min(10)) };
        }
    }

    pub fn enable_vt(h: Raw) -> bool {
        match console_mode(h) {
            Some(m) => unsafe {
                SetConsoleMode(h, m | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
                    != 0
            },
            None => false,
        }
    }
}

/// No terminal: wasm and other targets without one. Streams are never
/// terminals, raw mode is refused and a timed read just reads.
#[cfg(not(any(unix, windows)))]
mod os {
    use super::Wait;
    use crate::io::IoErrorTag;
    use crate::io_handle::NativeHandle;

    pub type Raw = ();

    pub fn raw_of(_h: &NativeHandle) -> Raw {}

    pub fn is_tty(_raw: Raw) -> bool {
        false
    }

    pub fn size(_raw: Raw) -> Option<(i64, i64)> {
        None
    }

    pub fn std_size() -> Option<(i64, i64)> {
        None
    }

    pub fn enter_raw(_raw: Raw, _keep_signals: bool, _keep_output: bool) -> Result<(), super::TtyFail> {
        Err(super::TtyFail::NotATerminal)
    }

    pub fn leave_raw(_raw: Raw) -> Result<(), IoErrorTag> {
        Ok(())
    }

    pub fn is_raw(_raw: Raw) -> bool {
        false
    }

    pub fn restore_all() {}

    pub fn resized() -> bool {
        false
    }

    pub fn wait_readable(_raw: Raw, _timeout_ms: i64) -> Result<Wait, IoErrorTag> {
        Ok(Wait::Ready)
    }

    pub fn enable_vt(_raw: Raw) -> bool {
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::time::Instant;

    use super::*;
    use crate::io::alloc_stream;
    use crate::io_handle::NativeHandle;
    use crate::memory::ObjArray;

    /// A pseudo-terminal: `(master, slave as a Stream)`.
    fn pty(heap: &mut Heap) -> (File, Value) {
        // SAFETY: plain libc pty setup; every descriptor is owned below.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(master >= 0, "posix_openpt");
            assert_eq!(libc::grantpt(master), 0);
            assert_eq!(libc::unlockpt(master), 0);
            let name = std::ffi::CStr::from_ptr(libc::ptsname(master)).to_owned();
            let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            assert!(slave >= 0, "open slave");
            let slave = File::from(OwnedFd::from_raw_fd(slave));
            let stream = alloc_stream(heap, NativeHandle::File(slave), StreamKind::File).unwrap();
            (File::from(OwnedFd::from_raw_fd(master)), stream)
        }
    }

    fn termios_of(heap: &mut Heap, s: Value) -> libc::termios {
        let (fd, _) = stream_os(heap, s).unwrap();
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut t) }, 0);
        t
    }

    fn bytes(heap: &mut Heap, n: usize) -> Value {
        let (obj, _) = heap.alloc(ObjArray::from_bytes(vec![0; n]), Object::Array);
        Value::from(obj.addr())
    }

    #[test]
    fn pty_is_a_terminal_and_reports_its_size() {
        let mut heap = Heap::default();
        let (master, slave) = pty(&mut heap);
        assert!(tty_is_tty(&mut heap, slave));
        let ws = libc::winsize { ws_row: 40, ws_col: 100, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCSWINSZ reads one `winsize`.
        assert_eq!(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &ws) }, 0);
        assert_eq!(tty_size(&mut heap, slave), Ok((100, 40)));
    }

    #[test]
    fn raw_mode_sets_and_restores_termios() {
        let mut heap = Heap::default();
        let (_master, slave) = pty(&mut heap);
        let before = termios_of(&mut heap, slave);
        assert!(before.c_lflag & libc::ECHO != 0 && before.c_lflag & libc::ICANON != 0);
        assert!(!tty_is_raw(&mut heap, slave));

        tty_enter_raw(&mut heap, slave, false, false).unwrap();
        let raw = termios_of(&mut heap, slave);
        assert_eq!(raw.c_lflag & (libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG), 0);
        assert_eq!(raw.c_iflag & (libc::ICRNL | libc::IXON), 0);
        assert_eq!(raw.c_oflag & libc::OPOST, 0);
        assert_eq!(raw.c_cc[libc::VMIN], 1);
        assert!(tty_is_raw(&mut heap, slave));

        // Again with other flags: applied to the first saved settings.
        tty_enter_raw(&mut heap, slave, true, true).unwrap();
        let again = termios_of(&mut heap, slave);
        assert!(again.c_lflag & libc::ISIG != 0);
        assert_eq!(again.c_oflag & libc::OPOST, before.c_oflag & libc::OPOST);
        assert_eq!(again.c_lflag & libc::ECHO, 0);

        tty_leave_raw(&mut heap, slave).unwrap();
        let after = termios_of(&mut heap, slave);
        // PENDIN is kernel state: macOS sets it when leaving non-canonical mode.
        assert_eq!(after.c_lflag & !libc::PENDIN, before.c_lflag & !libc::PENDIN);
        assert_eq!(after.c_iflag, before.c_iflag);
        assert_eq!(after.c_oflag, before.c_oflag);
        assert!(!tty_is_raw(&mut heap, slave));
        // Leaving twice is a no-op.
        tty_leave_raw(&mut heap, slave).unwrap();
    }

    #[test]
    fn read_timeout_times_out_then_reads() {
        let mut heap = Heap::default();
        let (mut master, slave) = pty(&mut heap);
        tty_enter_raw(&mut heap, slave, true, true).unwrap();
        let buf = bytes(&mut heap, 16);
        let start = Instant::now();
        assert_eq!(tty_read_timeout(&mut heap, slave, buf, 30), Ok(Some(0)));
        assert!(start.elapsed().as_millis() >= 25);
        master.write_all(b"\x1b[A").unwrap();
        assert_eq!(tty_read_timeout(&mut heap, slave, buf, 1000), Ok(Some(3)));
        tty_leave_raw(&mut heap, slave).unwrap();
    }

    #[test]
    fn resized_reports_each_sigwinch_once() {
        let _ = tty_resized();
        // SAFETY: the handler only stores to an atomic.
        unsafe { libc::raise(libc::SIGWINCH) };
        assert!(tty_resized());
        assert!(!tty_resized());
    }

    #[test]
    fn a_file_is_not_a_terminal() {
        let mut heap = Heap::default();
        let path = std::env::temp_dir().join("coil_tty_unit_file.txt");
        std::fs::write(&path, b"x").unwrap();
        let f = crate::io::stream_open(&mut heap, path.to_str().unwrap(), "r").unwrap();
        assert!(!tty_is_tty(&mut heap, f));
        assert_eq!(tty_size(&mut heap, f), Err(TtyFail::NotATerminal));
        assert_eq!(tty_enter_raw(&mut heap, f, true, true), Err(TtyFail::NotATerminal));
        assert_eq!(tty_leave_raw(&mut heap, f), Ok(()));
        let _ = std::fs::remove_file(&path);
    }
}
