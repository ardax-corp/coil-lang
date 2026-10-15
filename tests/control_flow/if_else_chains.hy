// An `else if` chain runs the first branch whose condition holds and at
// most one branch; separate `if`s can each run; conditions may be any
// `bool` expression, including calls and grouped logic.
use tests::assert;

fn grade(int score) -> string {
    if score >= 90 {
        return "A";
    } else if score >= 80 {
        return "B";
    } else if score >= 70 {
        return "C";
    } else if score >= 60 {
        return "D";
    } else {
        return "F";
    }
}

fn is_even(int n) -> bool {
    return n % 2 == 0;
}

fn fizz(int n) -> string {
    let out = "";
    if n % 3 == 0 {
        out = out + "FIZ";
    }
    if n % 5 == 0 {
        out = out + "BUZ";
    }
    return out;
}

test("first matching branch wins") {
    assert!(grade(95) == "A");
    assert!(grade(90) == "A");
    assert!(grade(89) == "B");
    assert!(grade(70) == "C");
    assert!(grade(65) == "D");
    assert!(grade(0) == "F");
}

test("separate ifs can each run") {
    assert!(fizz(15) == "FIZBUZ");
    assert!(fizz(9) == "FIZ");
    assert!(fizz(10) == "BUZ");
    assert!(fizz(7) == "");
}

test("only one branch of a chain runs") {
    let runs = 0;
    let n = 4;
    if n > 0 {
        runs = runs + 1;
    } else if n > 1 {
        runs = runs + 10;
    } else {
        runs = runs + 100;
    }
    assert!(runs == 1);
}

test("conditions from calls and grouped logic") {
    let hits = 0;
    if is_even(4) && !is_even(3) {
        hits = hits + 1;
    }
    if (is_even(1) || is_even(2)) && (1 < 2) {
        hits = hits + 1;
    }
    if !(is_even(1) || is_even(3)) {
        hits = hits + 1;
    }
    assert!(hits == 3);
}

test("nested if inside else") {
    let x = 5;
    let tag = "";
    if x < 0 {
        tag = "neg";
    } else {
        if x == 0 {
            tag = "zero";
        } else {
            tag = "pos";
        }
    }
    assert!(tag == "pos");
}

test("an empty branch is allowed") {
    let x = 1;
    if x == 1 {} else {
        x = 2;
    }
    assert!(x == 1);
}
