// `... xs` functions other modules call: each importer's argument lists get
// copies made here (tests/modules/variadic_across_modules.hy, coil-lang#1386).
class Box {
    pub v: int,
}

fn bump(int a) -> int {
    return a + 1;
}

fn count(... xs) -> int {
    return bump(len(xs)) - 1;
}

fn labelled(string label, ... xs) -> int {
    return len(label) * 10 + len(xs);
}

fn total(... xs) -> int {
    if len(xs) == 0 {
        return 0;
    }
    return new Box(len(xs)).v + count(xs[0]);
}

fn own() -> int {
    return count(1, "a");
}
