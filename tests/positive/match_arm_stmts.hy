// Match-arm blocks accept statements and an optional trailing value.
use tests::assert;

enum Choice {
    Value(int),
    Stop,
}

fn choose(Choice choice) -> int {
    return match choice {
        Choice::Value(x) => {
            let adjusted = x + 1;
            adjusted
        },
        Choice::Stop => {
            return 40;
        },
    };
}

test("match arm let and trailing value") {
    assert!(choose(Choice::Value(41)) == 42);
}

test("match arm early return") {
    assert!(choose(Choice::Stop) == 40);
}
