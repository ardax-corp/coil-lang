use commands::{cmd_dir, cmd_kind, parse_line};
use string::to_bytes;

test("parse look") {
    let look = to_bytes("look");
    let c0 = parse_line(look);
    assert(cmd_kind(c0) == 0)?;
}

test("parse go north") {
    let go_n = to_bytes("go north");
    let c1 = parse_line(go_n);
    assert(cmd_kind(c1) == 1)?;
    assert(cmd_dir(c1) == 0)?;
}

test("parse go south east west") {
    let go_s = to_bytes("go south");
    let go_e = to_bytes("go east");
    let go_w = to_bytes("go west");
    let c_s = parse_line(go_s);
    assert(cmd_kind(c_s) == 1)?;
    assert(cmd_dir(c_s) == 1)?;
    let c_e = parse_line(go_e);
    assert(cmd_kind(c_e) == 1)?;
    assert(cmd_dir(c_e) == 2)?;
    let c_w = parse_line(go_w);
    assert(cmd_kind(c_w) == 1)?;
    assert(cmd_dir(c_w) == 3)?;
}

test("parse take inventory quit exit") {
    let take_key = to_bytes("take key");
    let inv = to_bytes("inventory");
    let quit = to_bytes("quit");
    let exit = to_bytes("exit");
    let c_take = parse_line(take_key);
    assert(cmd_kind(c_take) == 2)?;
    let c_inv = parse_line(inv);
    assert(cmd_kind(c_inv) == 3)?;
    let c_quit = parse_line(quit);
    assert(cmd_kind(c_quit) == 7)?;
    let c_exit = parse_line(exit);
    assert(cmd_kind(c_exit) == 7)?;
}

test("parse unknown") {
    let bad = to_bytes("x");
    let c2 = parse_line(bad);
    assert(cmd_kind(c2) == 8)?;
}
