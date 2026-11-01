// An enum's tuple and record payloads may name a type imported from another
// module: the payload is that module's type, not a local name (#1947).

use tests::assert;
use payload_cmd::{Command, go_north, face_south};
use payload_dir::{Dir, is_north};

fn heading(Command c) -> bool {
    return match c {
        Command::Go(d) => is_north(d),
        Command::Face { dir, steps } => is_north(dir) && steps > 0,
        Command::Stay => false,
    };
}

test("imported type in a tuple payload") {
    assert!(heading(go_north()));
    assert!(heading(Command::Go(Dir::North)));
    assert!(!heading(Command::Go(Dir::South)));
}

test("imported type in a record payload") {
    assert!(!heading(face_south(2)));
    assert!(heading(Command::Face { dir: Dir::North, steps: 1 }));
    assert!(!heading(Command::Stay));
}
