// A class local kept in frame slots whose fields are two-word enums
// (`Option<int>`, a payload enum) holds each one boxed in its slot; direct
// MIR reads, matches, writes and builds them that way.
use tests::assert;

enum Status {
    Ready,
    Done(int),
}

class Job {
    pub status: Status,
    pub limit: Option<int>,
    pub name: string,
}

fn code(Status s) -> int {
    return match s {
        Status::Ready => 0,
        Status::Done(v) => v,
    };
}

fn run(int n) -> int {
    let j = new Job(Status::Ready, None, "job");
    let total = 0;
    let i = 0;
    while i < n {
        if i % 2 == 0 {
            j.status = Status::Done(i);
            j.limit = Some(i * 10);
        } else {
            j.status = Status::Ready;
            j.limit = None;
        }
        total = total + code(j.status);
        let lim = match j.limit {
            Option::Some(v) => v,
            Option::None => 1,
        };
        total = total + lim;
        i = i + 1;
    }
    return total + j.name.len();
}

test("two-word enum fields of a frame-slot class local") {
    // i = 0, 2: Done(i) + i * 10; i = 1, 3: Ready + 1; plus len("job").
    assert!(run(4) == 0 + 0 + 1 + 2 + 20 + 1 + 3);
    assert!(run(0) == 3);
}
