// A derive that allocates far more than `--comptime-mem` in total, but keeps
// little of it live, still runs: the cap is on live objects.
// Flags: --comptime-mem 1
use tests::assert;
use derive_macros::Churn;

#[derive(Churn)]
class Scratch {
    pub x: int,
}

test("garbage does not count toward the compile-time memory cap") {
    assert!(Scratch::churned() == 20);
}
