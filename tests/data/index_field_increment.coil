// `xs[i].field++` / `--` on a Vec of objects, as a statement and as a
// value (#1978).
use tests::assert;

class Counter {
    pub value: int,
}

test("postfix and prefix on an indexed field") {
    let counters: Vec<Counter> = [new Counter(0), new Counter(10)];
    counters[0].value++;
    counters[0].value++;
    counters[1].value--;
    assert!(counters[0].value == 2);
    assert!(counters[1].value == 9);
    let i = 1;
    ++counters[i].value;
    assert!(counters[i].value == 10);
}

test("an indexed field adjust used as a value") {
    let counters: Vec<Counter> = [new Counter(5)];
    let old = counters[0].value++;
    let new = ++counters[0].value;
    assert!(old == 5);
    assert!(new == 7);
    assert!(counters[0].value == 7);
}
