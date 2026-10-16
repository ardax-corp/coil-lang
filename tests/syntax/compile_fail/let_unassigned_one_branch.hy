// Expected: E0132 — assigning on only one branch leaves the read unassigned on the other.
fn f(bool b) -> string {
    let s: string;
    if b {
        s = "yes";
    }
    return s;
}

fn main() {
    let x = f(true);
}
