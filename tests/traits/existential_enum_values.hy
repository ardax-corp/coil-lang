// Output: Dir::Down(3)|Dir::Up|Two::Q(1, 2)|Unit::B|d down 3|d up
// An enum value packs as a bare-class existential with its instance, as a
// class value does: unit and payload variants, one-word and two-word
// layouts, derived `Show` and a user trait (#1184).
use io::{stdout, write};
use string::{format, to_bytes};

trait D<T> {
    fn d(T self) -> string {}
}

#[derive(Show)]
enum Dir {
    Up,
    Down(int),
}

#[derive(Show)]
enum Unit {
    A,
    B,
}

#[derive(Show)]
enum Two {
    P(string),
    Q(int, int),
}

impl D for Dir {
    fn d(Dir x) -> string {
        return match x {
            Dir::Up => "d up",
            Dir::Down(n) => "d down " + show(n),
        };
    }
}

fn wrap(Show x) -> string {
    return show(x);
}

fn via(D x) -> string {
    return d(x);
}

fn main() {
    let a = wrap(Dir::Down(3));
    let b = wrap(Dir::Up);
    let c = wrap(Two::Q(1, 2));
    let u = wrap(Unit::B);
    let e = via(Dir::Down(3));
    let f = via(Dir::Up);
    write(stdout(), to_bytes(format("%s|%s|%s|%s|%s|%s", a, b, c, u, e, f)));
}
