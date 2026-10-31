// Unit test: input lines parse into commands.
use tests::assert;
use commands::{Command, parse_command};
use world::Dir;

fn is_go(Command c, Dir want) -> bool {
    return match c {
        Command::Go(d) => d == want,
        default => false,
    };
}

test("parse look") {
    assert!(
        match parse_command("look") {
            Command::Look => true,
            default => false,
        },
    );
}

test("parse go in each direction") {
    assert!(is_go(parse_command("go north"), Dir::North));
    assert!(is_go(parse_command("go south"), Dir::South));
    assert!(is_go(parse_command("go east"), Dir::East));
    assert!(is_go(parse_command("  Go West "), Dir::West));
}

test("parse take inventory quit exit") {
    assert!(
        match parse_command("take key") {
            Command::Take => true,
            default => false,
        },
    );
    assert!(
        match parse_command("inventory") {
            Command::Inventory => true,
            default => false,
        },
    );
    assert!(
        match parse_command("quit") {
            Command::Quit => true,
            default => false,
        },
    );
    assert!(
        match parse_command("exit") {
            Command::Quit => true,
            default => false,
        },
    );
}

test("parse unknown") {
    assert!(
        match parse_command("x") {
            Command::Unknown => true,
            default => false,
        },
    );
    assert!(
        match parse_command("go up") {
            Command::Unknown => true,
            default => false,
        },
    );
}
