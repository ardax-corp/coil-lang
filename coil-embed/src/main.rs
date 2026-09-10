//! VM-only packaged-app runner (no compiler / parser).

use std::process::exit;

fn main() {
    if let Some(panicked) = coil_cli::try_run_embedded() {
        // Q5: language `panic` aborts (exit 1). Uncaught `raise` is Result.Err, exit 0.
        exit(if panicked { 1 } else { 0 });
    }
    eprintln!("coil-embed: not a packaged executable (use `coil package` to build one)");
    exit(1);
}
