// #1708: a file stream nested inside a static does not zero the whole slot
// in a `thread::spawn` worker: the rest of the value is copied, and using
// the stream there panics (#1684), so `join` is `Err(JoinFailed)`.
use tests::assert;
use io::close;
use io::open;
use io::write;
use io::Stream;
use io::fs::remove_file;
use io::file::read_text;
use string::to_bytes;
use thread::{join, spawn, ThreadError};

const PATH = "coil_lang_thread_nested_file_stream_static.txt";

class Log {
    pub file: Stream,
    pub id: int,
}

static let LOG = new Log(
    match open(PATH, "w") {
        Result::Ok(s) => s,
        Result::Err(_) => panic "open failed",
    },
    5,
);

fn log_id() -> int {
    return LOG.id;
}

fn log_line() -> int {
    return match write(LOG.file, to_bytes("from worker\n")) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn joined(() -> int f) -> Result<int, ThreadError> {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return join(t);
}

test("a file stream in a static class field panics in a worker, the rest copies") {
    assert!(match joined(log_id) {
        Result::Ok(n) => n == 5,
        default => false,
    });
    assert!(
        match joined(log_line) {
            Result::Err(ThreadError::JoinFailed) => true,
            default => false,
        },
    );
    write(LOG.file, to_bytes("from main\n"))?;
    close(LOG.file)?;
    let text = read_text(PATH)?;
    remove_file(PATH)?;
    assert!(text == "from main\n");
}
