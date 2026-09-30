//! `coil-test` entry point (`coil test …` re-execs this with the same flags).

use std::process::exit;

use coil_test::args::{Parsed, parse_args, print_help};
use coil_test::runner::cmd_test;

fn main() {
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    match parse_args(&raw) {
        Ok(Parsed::Help) => print_help(),
        Ok(Parsed::Run(config, options)) => cmd_test(config, options),
        Err(msg) => {
            eprintln!("coil-test: {msg}");
            print_help();
            exit(1);
        }
    }
}
