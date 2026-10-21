// Output: 10
use gc::{collect, weak, upgrade, Weak};
use io::{stdout, write};
use string::{format, to_bytes};

class Handle {
    pub fd: int,
}

static let during: int = 0;

static let held: Option<Weak<Handle>> = Option::None;

impl Handle {
    fn drop() {
        let live = match held {
            Option::Some(w) => match upgrade(w) {
                Option::Some(_) => 1,
                Option::None => 2,
            },
            Option::None => 3,
        };
        during = live;
    }
}

fn ephemeral() {
    let h = new Handle(1);
    held = Option::Some(weak(h));
}

fn main() {
    ephemeral();
    collect();
    let after = match held {
        Option::Some(w) => match upgrade(w) {
            Option::Some(_) => 1,
            Option::None => 0,
        },
        Option::None => -1,
    };
    let _ = write(stdout(), to_bytes(format("%i%i", during, after)));
}
