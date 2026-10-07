//! `coil-test` entry point (`coil test …` re-execs this with the same flags).

use std::process::exit;

use coil_args::print_cli_error;
use coil_test::args::{MUTATE, Parsed, parse_args, print_help, print_mutate_help};
use coil_test::mutate::cmd_mutate;
use coil_test::mutate::job::{WORKER_ARG, worker_main};
use coil_test::runner::cmd_test;

fn main() {
    comptime::install();
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some(WORKER_ARG) {
        // `coil mutate` runs each mutant here, with its own flags.
        let mut argv = vec![raw[0].clone(), MUTATE.to_string()];
        argv.extend_from_slice(&raw[2..]);
        match parse_args(&argv) {
            Ok(Parsed::Mutate(config, options)) => worker_main(config, &options.test),
            _ => exit(2),
        }
    }
    match parse_args(&raw) {
        Ok(Parsed::Help) => print_help(),
        Ok(Parsed::Run(config, options)) => exit(cmd_test(config, *options)),
        Ok(Parsed::MutateHelp) => print_mutate_help(),
        Ok(Parsed::Mutate(config, options)) => cmd_mutate(config, *options),
        Err(msg) => {
            print_cli_error(&msg);
            exit(1);
        }
    }
}
