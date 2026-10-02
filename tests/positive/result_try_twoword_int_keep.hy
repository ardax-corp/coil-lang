use io::Stream;
use io::IoError;
use io::net::tcp::listen;

fn big_ok() -> Result<int, IoError> {
    return 94805378185680;
}

fn take_q(Stream s) -> Result<Stream, IoError> {
    let n = big_ok()?;
    if n != 94805378185680 {
        panic "lost two-word int after ?";
    }
    return s;
}

fn four_q(Stream s) -> Result<Stream, IoError> {
    let a = big_ok()?;
    let b = big_ok()?;
    let c = big_ok()?;
    let d = big_ok()?;
    if a != 94805378185680 {
        panic "a";
    }
    if b != 94805378185680 {
        panic "b";
    }
    if c != 94805378185680 {
        panic "c";
    }
    if d != 94805378185680 {
        panic "d";
    }
    return s;
}

test("two-word Result int ? into heap Result keeps payload") {
    let l = listen("127.0.0.1", 0)?;
    take_q(l)?;
}

test("four two-word Result int ? into heap Result") {
    let l = listen("127.0.0.1", 0)?;
    four_q(l)?;
}
