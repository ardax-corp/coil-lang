// Expected: compile failure — `Nope` is not an item of the known module
// `qpath_provider` ("Cannot find type `Nope` in module `qpath_provider`").
use qpath_provider::marker;

fn main() {
    let p: qpath_provider::Nope = marker();
}
