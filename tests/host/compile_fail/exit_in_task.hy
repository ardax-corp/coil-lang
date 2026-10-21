// Expected: E0407 — a task body `main` spawns is reached code.
use env::exit;
use task::{scope, Scope};

fn main() {
    let _ = scope(
        fn (Scope s) {
            s.spawn(fn () {
                exit(1);
                0
            });
            0
        },
    );
}
