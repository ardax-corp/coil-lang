// Expected: E0207 — reading a field the class does not declare.
class P {
    pub x: int,
}

fn main() {
    let p = new P(1);
    let z = p.z;
}
