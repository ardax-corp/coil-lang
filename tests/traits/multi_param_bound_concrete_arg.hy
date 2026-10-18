// A two-parameter bound whose second argument is concrete
// (`where Wrap<T, string>`) is the given inside the generic body: calls
// through it pick `B = string` from the bound and leave `T` to the caller,
// in both function and method form (#1442).
// Output: <7>|<9>|<12>
use io::{stdout, write};
use string::to_bytes;

trait Wrap<A, B> {
    fn wrap(A x, int b) -> B {}
}

impl Wrap<string> for int {
    fn wrap(int x, int b) -> string {
        return "<" + show(x + b) + ">";
    }
}

fn wrapped<T>(T s, int b) -> string where Wrap<T, string> {
    return wrap(s, b);
}

fn wrapped_method<T>(T s, int b) -> string where Wrap<T, string> {
    return s.wrap(b);
}

fn wrapped_let<T>(T s, int b) -> string where Wrap<T, string> {
    let r = s.wrap(b);
    return r;
}

fn main() {
    let out = wrapped(5, 2) + "|" + wrapped_method(4, 5) + "|" + wrapped_let(10, 2);
    write(stdout(), to_bytes(out));
}
