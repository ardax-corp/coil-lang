// Expected: compile failure — two modules need each other's derives.
use macro_cycle_a::{CycleA};

fn main() {}
