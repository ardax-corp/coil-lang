// 02-adventure — text adventure over stdin/stdout.
//
// Modules: world (rooms, player), commands (parse a line into a `Command`),
// save (encode/decode). Stdin, stdout and the save file live here.
//
//   printf 'look\ngo north\ntake key\ninventory\ngo south\ngo east\nlook\nquit\n' | \
//     coil --root examples/projects/02-adventure/src --root <coil-stdlib>/src \
//       --allow-read --allow-write examples/projects/02-adventure/src/main.hy

use io::close;
use io::open;
use io::stdin;
use io::sync::println;
use io::sync::read_line;
use io::sync::read_to_end;
use io::sync::write_all;
use string::format;

use commands::{Command, parse_command};
use save::{decode_save, encode_save};
use world::{Player, Room, key_here, new_player, room_exits, room_title, try_move, try_take_key};

const SAVE_PATH: string = "coil_adventure_save.dat";

const HELP: string = "Commands: look, go north/south/east/west, take [key], inventory, save, load, help, quit";

fn save_player(Player p) -> Result<(), IoError> {
    let s = open(SAVE_PATH, "w")?;
    write_all(s, encode_save(p))?;
    close(s)?;
    return Ok(());
}

fn load_player(Player p) -> Result<bool, IoError> {
    let s = open(SAVE_PATH, "r")?;
    let got = read_to_end(s)?;
    close(s)?;
    if let Result::Ok(data) = decode_save(got) {
        p.room = data.room;
        p.has_key = data.has_key;
        return Ok(true);
    }
    return Ok(false);
}

fn look(Player p) -> Result<(), IoError> {
    println(format("You are in the %s. Exits: %s.", room_title(p.room), room_exits(p.room)))?;
    if key_here(p) {
        println("A brass key glints on a shelf.")?;
    }
    if p.has_key && p.room == Room::Garden {
        println("The garden gate unlocks - you win!")?;
    }
    return Ok(());
}

/// Runs one command; false once the player quits.
fn handle(Player p, Command c) -> Result<bool, IoError> {
    let playing = true;
    match c {
        Command::Look => {
            look(p)?;
        },
        Command::Go(d) => {
            if try_move(p, d) {
                look(p)?;
            } else {
                println("You cannot go that way.")?;
            }
        },
        Command::Take => {
            if try_take_key(p) {
                println("Taken: brass key.")?;
            } else {
                println("Nothing to take.")?;
            }
        },
        Command::Inventory => {
            if p.has_key {
                println("Inventory: brass key.")?;
            } else {
                println("Inventory: (empty).")?;
            }
        },
        Command::Save => {
            if save_player(p).is_ok() {
                println("Saved.")?;
            } else {
                println("Save failed.")?;
            }
        },
        Command::Load => {
            if load_player(p) ?? false {
                look(p)?;
            } else {
                println("Load failed.")?;
            }
        },
        Command::Help => {
            println(HELP)?;
        },
        Command::Quit => {
            playing = false;
        },
        Command::Unknown => {
            println("Unknown command. Type help.")?;
        },
    }
    return Ok(playing);
}

fn main() -> Result<(), IoError> {
    println("=== Tiny Adventure ===")?;
    println(HELP)?;
    let p = new_player();
    look(p)?;
    let input = stdin();
    while let Option::Some(line) = read_line(input)? {
        if line.len() == 0 {
            continue;
        }
        println("> " + line)?;
        if !handle(p, parse_command(line))? {
            break;
        }
    }
    println("Bye.")?;
    return Ok(());
}
