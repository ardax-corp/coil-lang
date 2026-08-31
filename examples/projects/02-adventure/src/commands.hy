// Command parse for the adventure (byte-line equality).
//
// Cmd kind: 0=look 1=go 2=take 3=inv 4=save 5=load 6=help 7=quit 8=bad
// Dir: 0=north 1=south 2=east 3=west; unused sentinel = 99.

use string::{to_bytes};

class Cmd {
    pub kind: int,
    pub dir: int,
}

fn bytes_eq(Vec<byte> a, Vec<byte> b) -> int {
    if len(a) != len(b) {
        return 0;
    }
    let i = 0;
    let ok = 1;
    while i < len(a) {
        if a[i] != b[i] {
            ok = 0;
        }
        i = i + 1;
    }
    return ok;
}

fn parse_line(Vec<byte> line) -> Cmd {
    let look = to_bytes("look");
    let inv = to_bytes("inventory");
    let take = to_bytes("take");
    let take_key = to_bytes("take key");
    let save = to_bytes("save");
    let load = to_bytes("load");
    let help = to_bytes("help");
    let quit = to_bytes("quit");
    let exit = to_bytes("exit");
    let go_n = to_bytes("go north");
    let go_s = to_bytes("go south");
    let go_e = to_bytes("go east");
    let go_w = to_bytes("go west");

    if bytes_eq(line, look) == 1 {
        return new Cmd(0, 99);
    }
    if bytes_eq(line, inv) == 1 {
        return new Cmd(3, 99);
    }
    if bytes_eq(line, take_key) == 1 {
        return new Cmd(2, 99);
    }
    if bytes_eq(line, take) == 1 {
        return new Cmd(2, 99);
    }
    if bytes_eq(line, save) == 1 {
        return new Cmd(4, 99);
    }
    if bytes_eq(line, load) == 1 {
        return new Cmd(5, 99);
    }
    if bytes_eq(line, help) == 1 {
        return new Cmd(6, 99);
    }
    if bytes_eq(line, quit) == 1 {
        return new Cmd(7, 99);
    }
    if bytes_eq(line, exit) == 1 {
        return new Cmd(7, 99);
    }
    if bytes_eq(line, go_n) == 1 {
        return new Cmd(1, 0);
    }
    if bytes_eq(line, go_s) == 1 {
        return new Cmd(1, 1);
    }
    if bytes_eq(line, go_e) == 1 {
        return new Cmd(1, 2);
    }
    if bytes_eq(line, go_w) == 1 {
        return new Cmd(1, 3);
    }
    return new Cmd(8, 99);
}

fn cmd_kind(Cmd c) -> int {
    return c.kind;
}

fn cmd_dir(Cmd c) -> int {
    return c.dir;
}
