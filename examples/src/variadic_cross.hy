// `... xs` functions other modules call: each importer's argument lists get
// copies made here (tests/modules/variadic_across_modules.hy, coil-lang#1386).
class Box {
    pub v: int,
}

fn bump(int a) -> int {
    return a + 1;
}

fn count(... xs) -> int {
    return bump(xs.len()) - 1;
}

fn labelled(string label, ... xs) -> int {
    return label.len() * 10 + xs.len();
}

fn total(... xs) -> int {
    if xs.len() == 0 {
        return 0;
    }
    return new Box(xs.len()).v + count(xs[0]);
}

fn own() -> int {
    return count(1, "a");
}

impl Box {
    // A method's `... xs` gets copies only for this module's calls
    // (tests/modules/compile_fail/variadic_method_across_modules.hy,
    // coil-lang#1754).
    pub fn tally(... xs) -> int {
        return self.v + xs.len();
    }
}

fn own_tally() -> int {
    return new Box(1).tally(2, "b");
}
