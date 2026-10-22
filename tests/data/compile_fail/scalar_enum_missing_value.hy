// @expected E0213 — every scalar case needs `= literal`; there is no auto-increment.
enum Status {
    Ok = 200,
    NotFound,
}

fn main() {}
