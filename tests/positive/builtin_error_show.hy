// #627: the builtin error enums implement `Show` and render as their
// variant name, so an error can be formatted (`%v`) or passed to a `T: Show`.
use io::{IoError, open};
use thread::ThreadError;
use env::EnvError;
use ffi::ErrorKind;
use string::format;

fn shown<T: Show>(T x) -> string {
    return format("%v", x);
}

test("IoError variants") {
    assert(format("%v", IoError::WouldBlock) == "WouldBlock")?;
    assert(format("%v", IoError::NotFound) == "NotFound")?;
    assert(format("%v", IoError::PermissionDenied) == "PermissionDenied")?;
    assert(format("%v", IoError::AlreadyClosed) == "AlreadyClosed")?;
    assert(format("%v", IoError::InvalidInput) == "InvalidInput")?;
    assert(format("%v", IoError::Other) == "Other")?;
    assert(format("%v", IoError::NotADirectory) == "NotADirectory")?;
    assert(format("%v", IoError::AlreadyExists) == "AlreadyExists")?;
    assert(format("%v", IoError::TimedOut) == "TimedOut")?;
    assert(format("%v", IoError::Truncated) == "Truncated")?;
    assert(format("%v", IoError::Certificate) == "Certificate")?;
    assert(format("%v", IoError::Handshake) == "Handshake")?;
}

test("an error returned by an io call") {
    let text = match open("/nonexistent/dir/file", "r") {
        Result::Ok(_) => "opened",
        Result::Err(e) => format("open failed: %v", e),
    };
    assert(text == "open failed: NotFound")?;
}

test("ThreadError, EnvError and ffi ErrorKind") {
    assert(format("%v", ThreadError::Disconnected) == "Disconnected")?;
    assert(format("%v", ThreadError::Poisoned) == "Poisoned")?;
    assert(format("%v", EnvError::ExecDisabled) == "ExecDisabled")?;
    assert(format("%v", EnvError::NotFound) == "NotFound")?;
    assert(format("%v", ErrorKind::SymbolNotFound) == "SymbolNotFound")?;
    assert(format("%v", ErrorKind::Other) == "Other")?;
}

test("through a Show bound") {
    assert(shown(IoError::TimedOut) == "TimedOut")?;
    assert(shown(ThreadError::JoinFailed) == "JoinFailed")?;
}
