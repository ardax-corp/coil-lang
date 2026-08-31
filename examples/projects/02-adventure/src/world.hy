// Adventure world: rooms, player, movement, items.
//
// Rooms: 0=Hall, 1=Library (key), 2=Garden.
// Dirs: 0=north, 1=south, 2=east, 3=west.
// Dir sentinel 99 = unused (avoid negative relational compares).

class Player {
    pub room: int,
    pub has_key: int,
}

fn new_player() -> Player {
    return new Player(0, 0);
}

fn player_room(Player p) -> int {
    return p.room;
}

fn player_has_key(Player p) -> int {
    return p.has_key;
}

fn try_move(Player p, int dir) -> Player {
    let room = p.room;
    let next = 99;
    if room == 0 {
        if dir == 0 {
            next = 1;
        }
        if dir == 2 {
            next = 2;
        }
    }
    if room == 1 {
        if dir == 1 {
            next = 0;
        }
    }
    if room == 2 {
        if dir == 3 {
            next = 0;
        }
    }
    if next == 99 {
        return p;
    }
    p.room = next;
    return p;
}

fn move_ok(Player p, int dir) -> int {
    let room = p.room;
    if room == 0 {
        if dir == 0 {
            return 1;
        }
        if dir == 2 {
            return 1;
        }
        return 0;
    }
    if room == 1 {
        if dir == 1 {
            return 1;
        }
        return 0;
    }
    if room == 2 {
        if dir == 3 {
            return 1;
        }
        return 0;
    }
    return 0;
}

fn try_take_key(Player p) -> Player {
    if p.room == 1 {
        if p.has_key == 0 {
            p.has_key = 1;
        }
    }
    return p;
}

fn key_here(Player p) -> int {
    if p.room == 1 {
        if p.has_key == 0 {
            return 1;
        }
    }
    return 0;
}

fn room_title(int room) -> string {
    if room == 0 {
        return "Hall";
    }
    if room == 1 {
        return "Library";
    }
    if room == 2 {
        return "Garden";
    }
    return "?";
}

fn room_exits(int room) -> string {
    if room == 0 {
        return "north, east";
    }
    if room == 1 {
        return "south";
    }
    if room == 2 {
        return "west";
    }
    return "";
}
