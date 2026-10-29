// An index the compiler proves in bounds is read unchecked, so a proof
// that holds too often reads outside the array. Each loop here only looks
// counted: the start is a parameter or negative, or a fact from before
// the loop stops holding on a later trip. Every read out of range must
// still panic.
use tests::assert;
use thread::{join, spawn, ThreadError};

fn sum_from(Vec<int> v, int from) -> int {
    let t = 0;
    let i = from;
    while i < v.len() {
        t = t + v[i];
        i = i + 1;
    }
    return t;
}

fn after_step_up(Vec<int> v, int start) -> int {
    let i = start;
    i += 1;
    if i < v.len() {
        return v[i];
    }
    return 0;
}

fn after_step_down(Vec<int> v) -> int {
    let i = 0;
    i--;
    if i < v.len() {
        return v[i];
    }
    return 0;
}

fn walk_down(Vec<int> v) -> int {
    let t = 0;
    let j = 1;
    let k = 0;
    while k < 4 {
        if j < v.len() {
            t = t + v[j];
        }
        j = j - 1;
        k = k + 1;
    }
    return t;
}

fn walk_past_end(Vec<int> v, int n) -> int {
    let t = 0;
    let i = 0;
    if i < v.len() {
        let k = 0;
        while k < n {
            t = t + v[i];
            i = i + 1;
            k = k + 1;
        }
    }
    return t;
}

fn two() -> Vec<int> {
    let v: Vec<int> = Vec::new();
    v.push(5);
    v.push(6);
    return v;
}

fn negative_start() -> int {
    return sum_from(two(), -1);
}

fn negative_step_up() -> int {
    return after_step_up(two(), -3);
}

fn negative_step_down() -> int {
    return after_step_down(two());
}

fn below_zero_later() -> int {
    return walk_down(two());
}

fn past_the_end_later() -> int {
    return walk_past_end(two(), 5);
}

fn panics(() -> int f) -> bool {
    let t = match spawn(f) {
        Result::Ok(t) => t,
        Result::Err(_) => panic "spawn failed",
    };
    return match join(t) {
        Result::Err(ThreadError::JoinFailed) => true,
        default => false,
    };
}

test("reads out of range panic") {
    assert!(panics(negative_start));
    assert!(panics(negative_step_up));
    assert!(panics(negative_step_down));
    assert!(panics(below_zero_later));
    assert!(panics(past_the_end_later));
}

test("the same loops in range") {
    assert!(sum_from(two(), 0) == 11);
    assert!(sum_from(two(), 1) == 6);
    assert!(after_step_up(two(), 0) == 6);
    assert!(walk_past_end(two(), 2) == 11);
}
