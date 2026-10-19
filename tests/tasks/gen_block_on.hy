// `task::block_on(h)` resumes `h` until it is done and gives its last
// resumed word, for int, string and float results and a body that yields
// first.
use task::block_on;
use tests::assert;

gen fn three() -> int {
    yield 1;
    yield 2;
    return 3;
}

gen fn word() -> string {
    yield "a";
    return "done";
}

gen fn half(float x) -> float {
    return x / 2.0;
}

test("block_on of int, string and float") {
    assert!(block_on(three()) == 3);
    assert!(block_on(word()) == "done");
    assert!(block_on(half(5.0)) == 2.5);
}
