//! `coil-dissect` entry point (`coil dissect …` re-execs this with the same flags).

fn main() {
    coil_build_id::check_parent("coil-dissect");
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    coil_dissect::run(&raw);
}
