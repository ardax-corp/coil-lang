// 02-adventure — text adventure over stdin/stdout.
//
// Modules: world (rooms/player), commands (byte parse), save (encode/decode).
// File IO and stdin live here for clarity (deps may also call IO + `?`).
// Input: `read_to_end` once, then split on `\n` (batch / Ctrl+D on a TTY).
//
//   rm -f out.hyc
//   printf 'look\ngo north\ntake key\ninventory\ngo south\ngo east\nlook\nquit\n' | \
//     timeout 10s ./target/release/coil examples/projects/02-adventure/src/main.hy

use io::close;
use io::open;
use io::stdin;
use io::stdout;
use io::sync::read_to_end;
use io::sync::write_all;

use world::{Player, key_here, move_ok, new_player, player_has_key, player_room, room_exits, room_title, try_move, try_take_key};

use commands::{cmd_dir, cmd_kind, parse_line};

use save::{SaveData, decode_save, encode_save};

use string::{format, to_bytes};

fn save_player(string path, int room, int has_key) {
    let payload = encode_save(room, has_key);
    let s = open(path, "w")?;
    write_all(s, payload)?;
    close(s)?;
    return 0;
}

fn load_player(string path) {
    let s = open(path, "r")?;
    let got = read_to_end(s)?;
    close(s)?;
    return decode_save(got);
}

fn print_look(Player p) {
    let room = player_room(p);
    write_all(stdout(), to_bytes("You are in the "));
    write_all(stdout(), to_bytes(format("%s", room_title(room))));
    write_all(stdout(), to_bytes(". Exits: "));
    write_all(stdout(), to_bytes(format("%s", room_exits(room))));
    write_all(stdout(), to_bytes(". "));
    if key_here(p) == 1 {
        write_all(stdout(), to_bytes("A brass key glints on a shelf. "));
    }
    if player_has_key(p) == 1 {
        if room == 2 {
            write_all(stdout(), to_bytes("The garden gate unlocks - you win! "));
        }
    }
}

fn print_help() {
    write_all(
        stdout(),
        to_bytes(
            "Commands: look, go north/south/east/west, take [key], inventory, save, load, help, quit",
        ),
    );
}

fn handle_line(Player p, Vec<byte> line, string save_path) -> int {
    if len(line) == 0 {
        return 1;
    }
    let c = parse_line(line);
    let k = cmd_kind(c);

    if k == 0 {
        print_look(p);
        write_all(stdout(), to_bytes(" "));
    }
    if k == 1 {
        let d = cmd_dir(c);
        if move_ok(p, d) == 1 {
            try_move(p, d);
            write_all(stdout(), to_bytes("OK. "));
            print_look(p);
            write_all(stdout(), to_bytes(" "));
        } else {
            write_all(stdout(), to_bytes("You cannot go that way. "));
        }
    }
    if k == 2 {
        if key_here(p) == 1 {
            try_take_key(p);
            write_all(stdout(), to_bytes("Taken: brass key. "));
        } else {
            write_all(stdout(), to_bytes("Nothing to take. "));
        }
    }
    if k == 3 {
        if player_has_key(p) == 1 {
            write_all(stdout(), to_bytes("Inventory: brass key. "));
        } else {
            write_all(stdout(), to_bytes("Inventory: (empty). "));
        }
    }
    if k == 4 {
        let r = save_player(save_path, player_room(p), player_has_key(p));
        write_all(
            stdout(),
            to_bytes(
                format(
                    "%s",
                    match r {
                        Result::Ok(_) => "Saved. ",
                        Result::Err(_) => "Save failed. ",
                    },
                ),
            ),
        );
    }
    if k == 5 {
        let r = load_player(save_path);
        let ok = match r {
            Result::Ok(_) => 1,
            Result::Err(_) => 0,
        };
        if ok == 1 {
            let data = match r {
                Result::Ok(d) => d,
                Result::Err(_) => new SaveData(0, 0),
            };
            p.room = data.room;
            p.has_key = data.has_key;
            write_all(stdout(), to_bytes("Loaded. "));
            print_look(p);
            write_all(stdout(), to_bytes(" "));
        } else {
            write_all(stdout(), to_bytes("Load failed. "));
        }
    }
    if k == 6 {
        print_help();
        write_all(stdout(), to_bytes(" "));
    }
    if k == 7 {
        write_all(stdout(), to_bytes("Bye."));
        return 0;
    }
    if k == 8 {
        write_all(stdout(), to_bytes("Unknown command. Type help. "));
    }
    return 1;
}

fn main() {
    write_all(stdout(), to_bytes("=== Tiny Adventure === "));
    write_all(stdout(), to_bytes("Type help for commands (end input with Ctrl+D / EOF). "));
    print_help();
    write_all(stdout(), to_bytes(" "));

    let p = new_player();
    print_look(p);
    write_all(stdout(), to_bytes(" "));

    let save_path = "coil_adventure_save.dat";
    let z: byte = 0;
    let nl: byte = 10;
    let cr: byte = 13;

    let input = stdin();
    let raw = match read_to_end(input) {
        Result::Ok(b) => b,
        Result::Err(_) => Vec::from([z]),
    };

    let i = 0;
    let running = 1;
    while running == 1 {
        if i >= len(raw) {
            write_all(stdout(), to_bytes("Bye."));
            running = 0;
        }
        if running == 1 {
            let line: Vec<byte> = Vec::new();
            line.push(z);
            let collecting = 1;
            while collecting == 1 {
                if i >= len(raw) {
                    collecting = 0;
                }
                if collecting == 1 {
                    let b = raw[i];
                    i = i + 1;
                    if b == nl {
                        collecting = 0;
                    }
                    if b != nl {
                        if b != cr {
                            line.push(b);
                        }
                    }
                }
            }
            write_all(stdout(), to_bytes("> "));
            if len(line) > 1 {
                let out: Vec<byte> = Vec::new();
                out.push(line[1]);
                let j = 2;
                while j < len(line) {
                    out.push(line[j]);
                    j = j + 1;
                }
                running = handle_line(p, out, save_path);
            }
        }
    }
}
