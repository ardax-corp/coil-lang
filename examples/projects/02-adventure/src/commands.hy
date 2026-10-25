// Command parsing for the adventure: words in, a `Command` out.

use text::{Searchable, Text};
use world::Dir;

enum Command {
    Look,
    Go(Dir),
    Take,
    Inventory,
    Save,
    Load,
    Help,
    Quit,
    Unknown,
}

fn parse_dir(string word) -> Option<Dir> {
    if word == "north" {
        return Option::Some(Dir::North);
    }
    if word == "south" {
        return Option::Some(Dir::South);
    }
    if word == "east" {
        return Option::Some(Dir::East);
    }
    if word == "west" {
        return Option::Some(Dir::West);
    }
    return Option::None;
}

/// Reads one input line: `go north`, `take key`, `look`, ….
fn parse_command(string line) -> Command {
    let words = line.trim().to_lower().split(" ");
    let verb = words[0];
    if verb == "go" && words.len() > 1 {
        return match parse_dir(words[1]) {
            Option::Some(d) => Command::Go(d),
            Option::None => Command::Unknown,
        };
    }
    if verb == "look" {
        return Command::Look;
    }
    if verb == "take" {
        return Command::Take;
    }
    if verb == "inventory" {
        return Command::Inventory;
    }
    if verb == "save" {
        return Command::Save;
    }
    if verb == "load" {
        return Command::Load;
    }
    if verb == "help" {
        return Command::Help;
    }
    if verb == "quit" || verb == "exit" {
        return Command::Quit;
    }
    return Command::Unknown;
}
