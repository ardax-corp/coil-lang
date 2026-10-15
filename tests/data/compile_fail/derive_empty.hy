// Expected: E0001 — `#[derive()]` must name at least one trait.
#[derive()]
class C {
    pub v: int,
}

fn main() {
}
