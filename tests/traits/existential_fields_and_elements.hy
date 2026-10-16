// Output: 12 14|Pt { x: 1 }|pt 2|3|Pt { x: 4 }
// A bare-class existential works as a class field and as a `Vec` element:
// a value packs where the field or element type asks for it, as it does at
// a parameter, and each element keeps its own instance.
use io::{stdout, write};
use string::{format, to_bytes};

trait Describe<D> {
    fn describe(D self) -> string {}
}

#[derive(Show)]
class Pt {
    pub x: int,
}

impl Describe for Pt {
    fn describe(Pt p) -> string {
        return "pt " + show(p.x);
    }
}

class Tagged {
    pub what: Show,
}

class Labeled {
    pub what: Describe,
}

fn all(Vec<Show> xs) -> int {
    let n = 0;
    for x in xs {
        n = n + len(show(x));
    }
    return n;
}

fn main() {
    let d: Show = 12;
    let t = new Tagged(d);
    let xs: Vec<Show> = Vec::new();
    let a: Show = 1;
    xs.push(a);
    let p: Show = new Pt(5);
    xs.push(p);
    xs.push(t.what);
    let first = format("%s %i", show(t.what), all(xs));
    let u = new Tagged(new Pt(1));
    let l = new Labeled(new Pt(2));
    let ys: Vec<Show> = Vec::new();
    ys.push(3);
    ys.push(new Pt(4));
    let rest = format("%s|%s|%s|%s", show(u.what), describe(l.what), show(ys[0]), show(ys[1]));
    write(stdout(), to_bytes(first + "|" + rest));
}
