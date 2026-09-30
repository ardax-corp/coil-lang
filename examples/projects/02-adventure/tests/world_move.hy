// Pure unit test: room graph movement (no stdin / REPL).
use world::{key_here, move_ok, new_player, player_has_key, player_room, try_move, try_take_key};

test("start in Hall and move to Library") {
    let p = new_player();
    assert(player_room(p) == 0, "start Hall")?;
    assert(move_ok(p, 0) == 1, "can go north")?;
    try_move(p, 0);
    assert(player_room(p) == 1, "now Library")?;
}

test("take key and reach Garden") {
    let p = new_player();
    try_move(p, 0);
    assert(key_here(p) == 1, "key present")?;
    try_take_key(p);
    assert(player_has_key(p) == 1, "has key")?;
    assert(move_ok(p, 1) == 1, "can go south")?;
    try_move(p, 1);
    assert(player_room(p) == 0, "back Hall")?;
    assert(move_ok(p, 2) == 1, "can go east")?;
    try_move(p, 2);
    assert(player_room(p) == 2, "Garden")?;
}
