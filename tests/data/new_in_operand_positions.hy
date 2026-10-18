// A `new` used inside an operand (a call argument under an operator chain,
// a second argument, a receiver, a nested constructor, a condition) builds
// its object like a `let`-bound one would (#1443).
// Output: x... <x> 23 true true 7 3 12 -3 5 2 9 yes 6 41 no!x x!x 8 ok
use io::{stdout, write};
use string::{format, to_bytes};

class Box1 {
    pub name: string,
    pub flag: bool,
}

class Cell {
    pub v: int,
}

class Pair {
    pub a: Cell,
    pub b: Cell,
}

impl Cell {
    pub fn get() -> int {
        return self.v;
    }
}

fn f(Box1 b) -> string {
    if b.flag {
        return b.name;
    }
    return "no";
}

fn n(Cell c) -> int {
    return c.v;
}

fn h(int k, Cell c) -> int {
    return k + c.v;
}

fn both(Cell a, Cell b) -> int {
    return a.v * 10 + b.v;
}

fn sum(Pair p) -> int {
    return p.a.v + p.b.v;
}

fn yn(bool b) -> string {
    if b {
        return "true";
    }
    return "false";
}

fn id<T>(T x) -> T {
    return x;
}

fn helper() -> int {
    return n(new Cell(40)) + h(0, new Cell(1));
}

fn twice(Box1 b) -> string {
    return f(b) + "!" + f(new Box1(b.name, true));
}

fn main() {
    let s = f(new Box1("x", true)) + "...";
    let t = "<" + f(new Box1("x", true)) + ">";
    let i = n(new Cell(2)) * 3 + n(new Cell(17));
    let lt = n(new Cell(1)) < n(new Cell(2));
    let eq = n(new Cell(4)) == 4 && n(new Cell(5)) != 6;
    let second = h(1, new Cell(5)) + 1;
    let pair = both(new Cell(0), new Cell(3));
    let nested = sum(new Pair(new Cell(5), new Cell(7)));
    let neg = -n(new Cell(3));
    let recv = new Cell(4).get() + 1;
    let generic = id(new Cell(2)).v;
    let w = 0;
    while w < n(new Cell(9)) {
        w = w + 1;
    }
    let cond = "no";
    if n(new Cell(1)) > 0 {
        cond = "yes";
    }
    let arr = [n(new Cell(1)), n(new Cell(2)), n(new Cell(3))];
    let picked = match n(new Cell(2)) {
        2 => "x!x",
        default => "bad",
    };
    let fl = format("%i", n(new Cell(8)));
    let tail = "bad";
    if f(new Box1("o", true)) + "k" == "ok" {
        tail = "ok";
    }
    write(
        stdout(),
        to_bytes(
            format(
                "%s %s %i %s %s %i %i %i %i %i %i %i %s %i %i %s %s %s %s",
                s,
                t,
                i,
                yn(lt),
                yn(eq),
                second,
                pair,
                nested,
                neg,
                recv,
                generic,
                w,
                cond,
                arr[0] + arr[1] + arr[2],
                helper(),
                twice(new Box1("x", false)),
                picked,
                fl,
                tail,
            ),
        ),
    );
}
