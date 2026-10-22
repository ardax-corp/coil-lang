// @expected E0413 — reading an environment variable is the `env` effect.
use env::var;

pure fn home() -> bool {
    return match var("HOME") {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

fn main() {
    home();
}
