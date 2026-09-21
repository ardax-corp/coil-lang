// Same short names as coil-stdlib `ascii` — collides with `coi404_textmod`.
fn to_lower(byte c) -> byte {
    if c >= "A" {
        if c <= "Z" {
            let n = (c as int) + 32;
            return n as byte;
        }
    }
    return c;
}

fn to_upper(byte c) -> byte {
    if c >= "a" {
        if c <= "z" {
            let n = (c as int) - 32;
            return n as byte;
        }
    }
    return c;
}
