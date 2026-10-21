// No `contract:` case is generated for a function with type parameters,
// a parameter type without an `Arbitrary` instance, effects besides `read`
// and `mutate`, or for an instance method. Each function below would fail
// such a case, so the file passes only if none is generated.
use tests::assert;
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

class Plain {
    pub v: int,
}

fn generic_bad<T>(T x) -> int
    ensures result > 0
{
    return 0;
}

fn no_instance_bad(Plain p) -> int
    ensures result > p.v
{
    return p.v;
}

fn writes_bad(int x) -> int
    ensures result > x
{
    let _ = write_all(stdout(), to_bytes(""));
    return x;
}

impl Plain {
    pub fn method_bad(int x) -> int
        ensures result > x
    {
        return x;
    }
}

fn ok(int x) -> int
    requires x > 0
{
    return x;
}

test("the skipped functions still run when their clauses hold") {
    assert!(ok(1) == 1);
    let p = new Plain(1);
    assert!(p.v == 1);
}
