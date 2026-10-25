// A `match` arm that ignores a payload whose type never resolved (a
// thread's or channel's value type nothing pins down): the arm still
// tests the variant, and nothing reads the payload's word.
use tests::assert;
use thread::{channel, close, detach, join, spawn, try_recv, ThreadError};

fn work() -> int {
    return 1;
}

fn detached() -> Result<string, ThreadError> {
    let t = spawn(work)?;
    detach(t)?;
    let msg = match join(t) {
        Result::Ok(_) => "joined",
        Result::Err(e) => match e {
            ThreadError::JoinFailed => "jf",
            default => "other",
        },
    };
    return Result::Ok(msg);
}

fn polled(bool shut) -> Result<string, ThreadError> {
    let pair = channel()?;
    if shut {
        close(pair[0])?;
    }
    let msg = match try_recv(pair[1]) {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            ThreadError::Disconnected => "disc",
            ThreadError::WouldBlock => "wb",
            default => "other",
        },
    };
    return Result::Ok(msg);
}

test("join after detach fails") {
    assert!((detached().ok() ?? "error") == "jf");
}

test("try_recv on an empty channel") {
    assert!((polled(false).ok() ?? "error") == "wb");
    assert!((polled(true).ok() ?? "error") == "disc");
}
