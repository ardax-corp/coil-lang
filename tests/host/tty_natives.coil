// `io::tty` on streams that are not terminals: files and, under the test
// harness, the standard streams. Raw-mode behaviour on a real terminal is
// covered by the Rust pty tests in `machine/src/tty.rs`.
use io::IoError;
use io::capture_begin;
use io::capture_end;
use io::close;
use io::open;
use io::stdin;
use io::stdout;
use io::write;
use io::fs::remove_file;
use io::tty::enable_vt;
use io::tty::enter_raw;
use io::tty::is_raw;
use io::tty::is_tty;
use io::tty::leave_raw;
use io::tty::read_timeout;
use io::tty::resized;
use io::tty::size;
use io::tty::TtyError;
use string::{from_bytes, to_bytes};
use tests::assert;

fn scratch(string path, string text) -> Result<(), IoError> {
    let w = open(path, "w")?;
    write(w, to_bytes(text))?;
    close(w)?;
    return;
}

test("a file is not a terminal") {
    let path = "coil_tty_natives_file.txt";
    scratch(path, "hi")?;
    let f = open(path, "r")?;
    assert!(!is_tty(f));
    assert!(!is_raw(f));
    assert!(size(f) == Result::Err(TtyError::NotATerminal));
    assert!(enter_raw(f, true, true) == Result::Err(TtyError::NotATerminal));
    assert!(!is_raw(f));
    // Leaving raw mode is a no-op when the stream is not raw.
    assert!(leave_raw(f) == Result::Ok(()));
    close(f)?;
    remove_file(path)?;
}

test("read_timeout reads what is there, then reports the end") {
    let path = "coil_tty_natives_read.txt";
    scratch(path, "hello")?;
    let f = open(path, "r")?;
    let buf = to_bytes("........");
    let n = read_timeout(f, buf, 50)?;
    assert!(n == Option::Some(5));
    assert!(from_bytes(buf)? == "hello...");
    assert!(read_timeout(f, buf, 50)? == Option::None);
    close(f)?;
    remove_file(path)?;
}

test("a closed stream") {
    let path = "coil_tty_natives_closed.txt";
    scratch(path, "")?;
    let f = open(path, "r")?;
    close(f)?;
    assert!(!is_tty(f));
    assert!(size(f) == Result::Err(TtyError::Io(IoError::AlreadyClosed)));
    assert!(read_timeout(f, to_bytes(" "), 0) == Result::Err(TtyError::Io(IoError::AlreadyClosed)));
    remove_file(path)?;
}

test("captured stdout is not a terminal") {
    capture_begin();
    let captured = is_tty(stdout());
    let _ = capture_end();
    assert!(!captured);
}

test("resized and enable_vt") {
    // No resize has happened since the start.
    assert!(!resized());
    let _ = enable_vt(stdout());
    let _ = is_tty(stdin());
}
