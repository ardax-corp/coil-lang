// Expected: compile failure — Clone is not derivable on payload or scalar enums.
#[repr(int)]
#[derive(Clone)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn main() {}
