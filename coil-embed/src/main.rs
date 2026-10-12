//! VM-only packaged-app runner (no compiler / parser).

use std::process::exit;

/// `coil package` refuses a runner without this marker (see
/// `coil_cli::is_coil_embed_runner`).
static RUNNER_MARKER: &[u8] = b"coil-embed-runner:v1";

fn main() {
    // Keep the marker in the image through LTO and section GC.
    std::hint::black_box(RUNNER_MARKER);
    if let Some(panicked) = coil_cli::try_run_embedded() {
        // Q5: language `panic` aborts (exit 1). Uncaught `raise` is Result.Err, exit 0.
        exit(if panicked { 1 } else { 0 });
    }
    eprintln!("coil-embed: not a packaged executable (use `coil package` to build one)");
    exit(1);
}
