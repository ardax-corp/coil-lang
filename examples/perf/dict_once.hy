// A generic call in a loop at a type its module does not know: its ground
// dictionary is built once into a static, not as a fresh tuple on every
// call.
use dict_lib::larger;
use string::{format, to_bytes};
use io::{stdout, write};

class Score {
    pub v: int,
}

impl Eq for Score {
    pub fn eq(Score a, Score b) -> bool {
        return a.v == b.v;
    }

    pub fn ne(Score a, Score b) -> bool {
        return a.v != b.v;
    }
}

impl Lt for Score {
    pub fn lt(Score a, Score b) -> bool {
        return a.v < b.v;
    }
}

impl Le for Score {
    pub fn le(Score a, Score b) -> bool {
        return a.v <= b.v;
    }
}

impl Gt for Score {
    pub fn gt(Score a, Score b) -> bool {
        return a.v > b.v;
    }
}

impl Ge for Score {
    pub fn ge(Score a, Score b) -> bool {
        return a.v >= b.v;
    }
}

impl Ord for Score {
}

fn main() {
    let total = 0;
    for i in 0..1000 {
        total = total + larger(new Score(i), new Score(500)).v;
    }
    write(stdout(), to_bytes(format("%i\n", total)));
}
