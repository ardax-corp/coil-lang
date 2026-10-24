// A `with_lock` / `with_read` / `with_write` callback may capture locals:
// it runs on the calling thread, so it need not be sendable (#1979).
use thread::{mutex, rwlock, with_lock, with_read, with_write, spawn, join, Mutex};
use tests::assert;

fn add_name((Mutex<Vec<string>>, string) job) -> int {
    let m = job[0];
    let name = job[1];
    let _ = with_lock(
        m,
        fn (Vec<string> xs) use (name) {
            xs.push(name);
            return (xs, 0);
        },
    );
    return 0;
}

test("with_lock with a capturing lambda") {
    let m = mutex(0) ?? panic "no mutex";
    let step = 2;
    let got = with_lock(m, fn (int n) use (step) => (n + step, n + step)) ?? -1;
    assert!(got == 2);
    let again = with_lock(m, fn (int n) use (step) => (n + step, n + step)) ?? -1;
    assert!(again == 4);
}

test("with_read and with_write with capturing lambdas") {
    let l = rwlock(10) ?? panic "no rwlock";
    let k = 5;
    let _ = with_write(l, fn (int n) use (k) => (n + k, 0));
    let seen = with_read(l, fn (int n) use (k) => n - k) ?? -1;
    assert!(seen == 10);
}

test("a thread pushes its own argument into a mutex") {
    let names: Vec<string> = Vec::new();
    let m = mutex(names) ?? panic "no mutex";
    let t = spawn(add_name, (m, "worker")) ?? panic "no thread";
    let _ = join(t) ?? panic "join failed";
    let n = with_lock(m, fn (Vec<string> xs) => (xs, xs.len())) ?? -1;
    assert!(n == 1);
}
