// Pure unit test: room graph movement (no stdin / REPL).
use tests::assert;
use world::{Dir, Room, key_here, new_player, try_move, try_take_key};

test("start in Hall and move to Library") {
    let p = new_player();
    assert!(p.room == Room::Hall, "start Hall");
    assert!(try_move(p, Dir::North), "can go north");
    assert!(p.room == Room::Library, "now Library");
    assert!(!try_move(p, Dir::East), "no east exit");
}

test("take key and reach Garden") {
    let p = new_player();
    try_move(p, Dir::North);
    assert!(key_here(p), "key present");
    assert!(try_take_key(p), "took it");
    assert!(p.has_key && !key_here(p), "has key");
    assert!(try_move(p, Dir::South), "back to Hall");
    assert!(try_move(p, Dir::East), "to Garden");
    assert!(p.room == Room::Garden, "Garden");
}
