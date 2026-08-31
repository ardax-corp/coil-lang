// Pure save payload encode/decode (2 bytes: room, has_key).
// File open/read/write stays in `main.hy` for clarity (deps may also call IO).

class SaveData {
    pub room: int,
    pub has_key: int,
}

fn encode_save(int room, int has_key) -> Vec<byte> {
    let z: byte = 0;
    let one: byte = 1;
    let two: byte = 2;
    let hi: byte = 0;
    let lo: byte = 0;
    if room == 1 {
        hi = one;
    }
    if room == 2 {
        hi = two;
    }
    if has_key == 1 {
        lo = one;
    }
    if room == 0 {
        hi = z;
    }
    let payload: Vec<byte> = Vec::new();
    payload.push(hi);
    payload.push(lo);
    return payload;
}

fn decode_save(Vec<byte> got) -> SaveData {
    if len(got) < 2 {
        panic "save too short";
    }
    let one: byte = 1;
    let two: byte = 2;
    let r = 0;
    let k = 0;
    let rb = got[0];
    let kb = got[1];
    if rb == one {
        r = 1;
    }
    if rb == two {
        r = 2;
    }
    if kb == one {
        k = 1;
    }
    return new SaveData(r, k);
}
