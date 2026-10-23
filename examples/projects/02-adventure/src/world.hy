// Adventure world: rooms, the player, movement and the key.

#[derive(Eq)]
enum Room {
    Hall,
    Library,
    Garden,
}

#[derive(Eq)]
enum Dir {
    North,
    South,
    East,
    West,
}

class Player {
    pub room: Room,
    pub has_key: bool,
}

fn new_player() -> Player {
    return new Player(Room::Hall, false);
}

/// The room through `dir` from `room`, when there is a way.
fn exit_to(Room room, Dir dir) -> Option<Room> {
    return match room {
        Room::Hall => match dir {
            Dir::North => Option::Some(Room::Library),
            Dir::East => Option::Some(Room::Garden),
            default => Option::None,
        },
        Room::Library => match dir {
            Dir::South => Option::Some(Room::Hall),
            default => Option::None,
        },
        Room::Garden => match dir {
            Dir::West => Option::Some(Room::Hall),
            default => Option::None,
        },
    };
}

/// Moves the player through `dir`; false when there is no exit that way.
fn try_move(Player p, Dir dir) -> bool {
    if let Option::Some(next) = exit_to(p.room, dir) {
        p.room = next;
        return true;
    }
    return false;
}

fn key_here(Player p) -> bool {
    return p.room == Room::Library && !p.has_key;
}

/// Picks up the key; false when it is not here.
fn try_take_key(Player p) -> bool {
    if !key_here(p) {
        return false;
    }
    p.has_key = true;
    return true;
}

fn room_title(Room room) -> string {
    return match room {
        Room::Hall => "Hall",
        Room::Library => "Library",
        Room::Garden => "Garden",
    };
}

fn room_exits(Room room) -> string {
    return match room {
        Room::Hall => "north, east",
        Room::Library => "south",
        Room::Garden => "west",
    };
}
