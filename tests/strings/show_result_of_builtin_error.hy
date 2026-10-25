// `%v` / `show` of an `Option` or `Result` holding a builtin error enum
// (#1984, #1996).
use io::IoError;
use thread::ThreadError;
use string::format;
use tests::assert;

test("Result and Option of IoError") {
    let ok: Result<(), IoError> = Result::Ok(());
    let bad: Result<int, IoError> = Result::Err(IoError::AlreadyClosed);
    let some: Option<IoError> = Option::Some(IoError::AlreadyClosed);
    assert!(format("%v", ok) == "Ok(())");
    assert!(format("%v", bad) == "Err(AlreadyClosed)");
    assert!(show(some) == "Some(AlreadyClosed)");
}

test("Result and Option of ThreadError") {
    let bad: Result<int, ThreadError> = Result::Err(ThreadError::NotSendable);
    let some: Option<ThreadError> = Option::Some(ThreadError::Disconnected);
    assert!(format("%v", bad) == "Err(NotSendable)");
    assert!(show(some) == "Some(Disconnected)");
}
