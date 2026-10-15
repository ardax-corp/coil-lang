// Expected: E0406 — a static initializer runs at startup, so an `env::exec` it
// reaches needs `--allow-exec`.
use env::exec;

fn run_true() -> int {
    let args: Vec<string> = Vec::new();
    return match exec("true", args) {
        Result::Ok(code) => code,
        Result::Err(_) => -1,
    };
}

static let STATUS: int = run_true();

fn main() {}
