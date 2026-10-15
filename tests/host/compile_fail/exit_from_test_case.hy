// Expected: E0407 — under `coil test` each case is an entry point; `env::exit`
// reached from one needs `--allow-exit`.
use env::exit;

fn stop() {
    exit(0);
}

test("stops early") {
    stop();
}
