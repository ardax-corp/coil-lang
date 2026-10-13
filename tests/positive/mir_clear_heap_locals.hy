// Typed inlining clears a spliced callee's heap locals after the splicing
// statement. A body lowered straight to MIR zeroes their frame words too,
// so a collection right after frees the callee's objects (and runs the
// finalizer of what they hold).

use gc::collect;

class Handle {
    pub fd: int,
}

static let closed: int = 0;

impl Handle {
    fn drop() {
        closed = closed + 1;
    }
}

class Item {
    pub fd: int,
    pub h: Handle,
}

// Builds a finalizer object, so it keeps its call.
fn wrap(int fd) -> Item {
    return new Item(fd, new Handle(fd));
}

fn use_one(int fd) -> int {
    let it = wrap(fd);
    return it.fd;
}

fn label(int n) -> string {
    let s = "n";
    let t = s + "=";
    return t;
}

// Each spliced `it` is dead once its field is read: the collection frees
// them and their handles.
fn closed_after_splice(int n) -> int {
    let before = closed;
    let sum = 0;
    let i = 0;
    while i < n {
        sum = sum + use_one(i);
        collect();
        i = i + 1;
    }
    return (closed - before) * 100 + sum;
}

// A cleared local whose value another local still holds stays alive.
fn kept_by_result() -> int {
    let total = 0;
    let i = 0;
    while i < 3 {
        let t = label(i);
        collect();
        if t == "n=" {
            total = total + 1;
        }
        i = i + 1;
    }
    return total;
}

test("a spliced heap local dies at its clear") {
    assert(closed_after_splice(4) == 406)?;
}

test("a cleared value another local holds stays live") {
    assert(kept_by_result() == 3)?;
}

fn pick() -> Result<string, string> {
    return Result::Ok("coil");
}

fn sep(string a, string b) -> string {
    return a + "/" + b;
}

// The `?` payload is cleared after the `let`; when the joined string is
// built inline at the return, it must still read the payload first.
fn joined_after_clear() -> Result<string, string> {
    let j = sep("a", pick()?);
    return Result::Ok(j);
}

test("an inline use is not moved past its value's clear") {
    let got = match joined_after_clear() {
        Result::Ok(s) => s,
        Result::Err(e) => e,
    };
    assert(got == "a/coil")?;
}
