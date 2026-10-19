// A module whose own functions call a `... xs` function with several
// argument lists, next to the declarations a re-check must not repeat
// (tests/modules/variadic_in_module.hy).
class Pt {
    pub x: int,
}

enum Shade {
    Dark,
    Light,
}

trait Named {
    fn name() -> string {}
}

impl Named for Pt {
    fn name() -> string {
        return "pt";
    }
}

fn twice(int a) -> int {
    return a * 2;
}

fn twice(string a) -> string {
    return a + a;
}

fn count(... xs) -> int {
    return xs.len();
}

fn summary(int a, string b) -> int {
    return count(a, b, new Pt(1), Shade::Dark) + count() + twice(a);
}
