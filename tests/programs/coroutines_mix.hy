// A counting generator, `yield from` delegation, `resume … with` sending a value back and `done`.
// @output got:hi\n0 1 2 | 1 2 3 | 10 true\n
use io::sync::println;
use string::format;

gen fn counter(int n) {
    let i = 0;
    while i < n {
        yield i;
        i += 1;
    }
}

gen fn echo() -> int {
    let got = yield 0;
    let _ = println(format("got:%s", got));
    return 10;
}

gen fn inner() {
    yield 2;
    yield 3;
}

gen fn outer() {
    yield 1;
    yield from inner();
}

fn main() {
    let s = "";
    for x in counter(3) {
        s += format("%i ", x);
    }
    let s2 = "";
    for x in outer() {
        s2 += format("%i ", x);
    }
    let h = echo();
    let _ = resume h;
    let r = resume h with "hi";
    let _ = println(format("%s| %s| %i %z", s, s2, r, done(h)));
}
