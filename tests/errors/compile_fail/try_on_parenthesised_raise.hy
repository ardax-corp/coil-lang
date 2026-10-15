// Expected: E0114 — `(raise e)?` puts `?` on a `raise`, which has no value.
fn f() {
    (raise "err")?;
}

fn main() {}
