// #1708: a stream nested inside a static (a tuple, a class field, an array,
// an enum payload) does not zero the whole slot in a `thread::spawn`
// worker. The rest of the value is copied, and a standard stream is the
// same process stream there. A file stream: see
// thread_spawn_nested_file_stream_static.hy.
// Output: tuple 1\nclass 2\narray 3\nenum 4\nback in main\n
use io::{stdout, write, Stream};
use string::{format, to_bytes};
use thread::{join, spawn};

enum Sink {
    To(Stream, int),
    Nowhere,
}

class Logger {
    pub out: Stream,
    pub id: int,
}

static let PAIR = (stdout(), 1);

static let LOGGER = new Logger(stdout(), 2);

static let OUTS: Vec<Stream> = [stdout(), stdout()];

static let SINK = Sink::To(stdout(), 4);

fn say(Stream s, string what, int n) -> int {
    return match write(s, to_bytes(format("%s %i\n", what, n))) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn tuple_stream() -> int {
    let (s, n) = PAIR;
    return say(s, "tuple", n);
}

fn class_stream() -> int {
    return say(LOGGER.out, "class", LOGGER.id);
}

fn array_stream() -> int {
    return say(OUTS[1], "array", OUTS.len() + 1);
}

fn enum_stream() -> int {
    return match SINK {
        Sink::To(s, n) => say(s, "enum", n),
        Sink::Nowhere => -1,
    };
}

fn run(() -> int f) -> int {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Ok(n) => n,
        Result::Err(_) => -2,
    };
}

fn main() {
    if run(tuple_stream) < 0 {
        let _ = write(stdout(), to_bytes("tuple FAILED\n"));
    }
    if run(class_stream) < 0 {
        let _ = write(stdout(), to_bytes("class FAILED\n"));
    }
    if run(array_stream) < 0 {
        let _ = write(stdout(), to_bytes("array FAILED\n"));
    }
    if run(enum_stream) < 0 {
        let _ = write(stdout(), to_bytes("enum FAILED\n"));
    }
    let _ = write(stdout(), to_bytes("back in main\n"));
}
