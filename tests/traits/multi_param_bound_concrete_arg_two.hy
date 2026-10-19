// With two `Wrap<_, string>` instances in scope the bound
// `where Wrap<T, string>` still resolves at the call of `wrapped`, not
// inside its body, so the body is not ambiguous (#1442).
// Output: <7>|b3|<9>|b1
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

impl Wrap<string> for bool {
    fn wrap(bool x, int b) -> string {
        return "b" + show(b);
    }
}

fn wrapped<T>(T s, int b) -> string where Wrap<T, string> {
    return wrap(s, b);
}

fn wrapped_method<T>(T s, int b) -> string where Wrap<T, string> {
    return s.wrap(b);
}

fn main() {
    let out = wrapped(5, 2) + "|" + wrapped(true, 3) + "|" + wrapped_method(4, 5) + "|" +
              wrapped_method(false, 1);
    write(stdout(), to_bytes(out));
}
