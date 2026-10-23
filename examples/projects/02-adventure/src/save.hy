// Save payload encode/decode: two bytes, the room and whether the player
// has the key. File IO stays in `main.hy`.

use world::{Player, Room};

class SaveData {
    pub room: Room,
    pub has_key: bool,
}

fn encode_save(Player p) -> Vec<byte> {
    let room: byte = match p.room {
        Room::Hall => 0,
        Room::Library => 1,
        Room::Garden => 2,
    };
    let key: byte = 0;
    if p.has_key {
        key = 1;
    }
    return [room, key];
}

fn decode_save(Vec<byte> got) -> Result<SaveData, string> {
    if got.len() != 2 {
        raise "save file has the wrong size";
    }
    let room = match got[0] as int {
        0 => Room::Hall,
        1 => Room::Library,
        2 => Room::Garden,
        default => Room::Hall,
    };
    if (got[0] as int) > 2 {
        raise "save file names no room";
    }
    return new SaveData(room, (got[1] as int) == 1);
}
