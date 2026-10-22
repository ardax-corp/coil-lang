// @output high\nlow\nhigh\nlow\nhigh\n
// A `#[repr(int)]` enum is an immediate word, so it goes everywhere an
// `int` does: a `Vec` literal and `pop`, indexing, `push` through a generic
// class, and a tuple item (#1903).
use io::sync::println;

#[repr(int)]
enum Lvl {
    Low = 1,
    High = 2,
}

class Box<V> {
    pub vs: Vec<V>,
}

impl Box<V> {
    pub fn put(V x) {
        self.vs.push(x);
    }

    pub fn at(int i) -> V {
        return self.vs[i];
    }
}

fn say(Lvl v) -> string {
    return match v {
        Lvl::Low => "low",
        Lvl::High => "high",
    };
}

fn popped(Lvl x) -> Lvl {
    let vs: Vec<Lvl> = [x];
    return match vs.pop() {
        Some(v) => v,
        None => panic "empty",
    };
}

fn indexed(Lvl x) -> Lvl {
    let vs: Vec<Lvl> = [Lvl::Low, x];
    return vs[1];
}

fn boxed(Lvl x) -> Lvl {
    let b: Box<Lvl> = new Box(Vec::new());
    b.put(x);
    return b.at(0);
}

fn tupled<W>(W w) -> (W, int) {
    return (w, 1);
}

fn untupled(Lvl x) -> Lvl {
    let (w, _) = tupled(x);
    return w;
}

fn main() {
    println(say(popped(Lvl::High)))?;
    println(say(indexed(Lvl::Low)))?;
    println(say(boxed(Lvl::High)))?;
    println(say(boxed(Lvl::Low)))?;
    println(say(untupled(Lvl::High)))?;
}
