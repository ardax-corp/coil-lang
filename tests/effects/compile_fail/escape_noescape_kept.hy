// @expected E0139 — `keep` stores `t` in a static, so `#[noescape(t)]` does not hold.
static let SEEN: Vec<string> = Vec::new();

#[noescape(t)]
fn keep(string t) {
    SEEN.push(t);
}

fn main() {
    keep("a");
}
