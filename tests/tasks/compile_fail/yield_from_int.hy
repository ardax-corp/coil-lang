// Expected: E0102 — `yield from` delegates to a coroutine; `1` is an `int`.
gen fn bad() {
    yield from 1;
}

fn main() {}
