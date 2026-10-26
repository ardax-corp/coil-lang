// #627: the builtin error enums implement `Show` and render as their
// variant name, so an error can be formatted (`%v`) or passed to a `T: Show`.
use tests::assert;
use io::{IoError, open};
use thread::{ThreadError, ChannelError, LockError};
use env::EnvError;
use ffi::ErrorKind;
use io::tty::TtyError;
use string::{format, StringError};

fn shown<T: Show>(T x) -> string {
    return format("%v", x);
}

test("IoError variants") {
    assert!(format("%v", IoError::WouldBlock) == "WouldBlock");
    assert!(format("%v", IoError::NotFound) == "NotFound");
    assert!(format("%v", IoError::PermissionDenied) == "PermissionDenied");
    assert!(format("%v", IoError::AlreadyClosed) == "AlreadyClosed");
    assert!(format("%v", IoError::InvalidInput) == "InvalidInput");
    assert!(format("%v", IoError::Other) == "Other");
    assert!(format("%v", IoError::NotADirectory) == "NotADirectory");
    assert!(format("%v", IoError::AlreadyExists) == "AlreadyExists");
    assert!(format("%v", IoError::TimedOut) == "TimedOut");
    assert!(format("%v", IoError::Truncated) == "Truncated");
    assert!(format("%v", IoError::Protocol) == "Protocol");
}

test("TtyError and StringError") {
    assert!(format("%v", TtyError::NotATerminal) == "NotATerminal");
    assert!(format("%v", TtyError::Io(IoError::NotFound)) == "Io(NotFound)");
    assert!(format("%v", StringError::InvalidUtf8) == "InvalidUtf8");
    assert!(format("%v", StringError::NotCharBoundary) == "NotCharBoundary");
    assert!(shown(TtyError::Io(IoError::AlreadyClosed)) == "Io(AlreadyClosed)");
}

test("an error returned by an io call") {
    let text = match open("/nonexistent/dir/file", "r") {
        Result::Ok(_) => "opened",
        Result::Err(e) => format("open failed: %v", e),
    };
    assert!(text == "open failed: NotFound");
}

test("thread errors, EnvError and ffi ErrorKind") {
    assert!(format("%v", ThreadError::NotSendable) == "NotSendable");
    assert!(format("%v", ChannelError::Closed) == "Closed");
    assert!(format("%v", ChannelError::Full) == "Full");
    assert!(format("%v", LockError::Poisoned) == "Poisoned");
    assert!(format("%v", LockError::AlreadyHeld) == "AlreadyHeld");
    assert!(format("%v", EnvError::ExecFailed) == "ExecFailed");
    assert!(format("%v", EnvError::NotFound) == "NotFound");
    assert!(format("%v", ErrorKind::SymbolNotFound) == "SymbolNotFound");
    assert!(format("%v", ErrorKind::Other) == "Other");
}

test("through a Show bound") {
    assert!(shown(IoError::TimedOut) == "TimedOut");
    assert!(shown(ThreadError::JoinFailed) == "JoinFailed");
    assert!(shown(ChannelError::Empty) == "Empty");
    assert!(shown(LockError::NotHeld) == "NotHeld");
}
