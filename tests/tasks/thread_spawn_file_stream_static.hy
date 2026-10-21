// #1684: a static holding a file or socket stream cannot be copied into a
// `thread::spawn` worker. Using it there panics, so the worker's `join` is
// `Err(JoinFailed)`, instead of reading null and dropping the write.
use tests::assert;
use io::close;
use io::open;
use io::write;
use io::Stream;
use io::fs::remove_file;
use io::file::read_text;
use string::to_bytes;
use thread::{join, spawn, ThreadError};

const PATH = "coil_lang_thread_file_stream_static.txt";

static let LOG: Stream = match open(PATH, "w") {
    Result::Ok(s) => s,
    Result::Err(_) => panic "open failed",
};

fn log_line() -> int {
    return match write(LOG, to_bytes("from worker\n")) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn close_log() -> int {
    return match close(LOG) {
        Result::Ok(_) => 1,
        Result::Err(_) => -1,
    };
}

fn panics(() -> int f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Err(ThreadError::JoinFailed) => true,
        default => false,
    };
}

test("a file stream static panics in a worker and still works in main") {
    assert!(panics(log_line));
    assert!(panics(close_log));
    write(LOG, to_bytes("from main\n"))?;
    close(LOG)?;
    let text = read_text(PATH)?;
    remove_file(PATH)?;
    assert!(text == "from main\n");
}
