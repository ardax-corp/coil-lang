// Non-tail recursion in several shapes: mutual recursion, nested recursive
// arguments (Ackermann), recursion that builds a string, and recursion that
// returns an Option or a Result and stops at the first failure.
use tests::assert;
use string::format;

fn is_even(int n) -> bool {
    if n == 0 {
        return true;
    }
    return is_odd(n - 1);
}

fn is_odd(int n) -> bool {
    if n == 0 {
        return false;
    }
    return is_even(n - 1);
}

fn ack(int m, int n) -> int {
    if m == 0 {
        return n + 1;
    }
    if n == 0 {
        return ack(m - 1, 1);
    }
    return ack(m - 1, ack(m, n - 1));
}

fn binary(int n) -> string {
    if n < 2 {
        return format("%i", n);
    }
    return binary(n / 2) + format("%i", n % 2);
}

fn halvings(int n) -> Option<int> {
    if n == 1 {
        return Option::Some(0);
    }
    if n % 2 == 1 {
        return Option::None;
    }
    return match halvings(n / 2) {
        Option::Some(rest) => Option::Some(rest + 1),
        Option::None => Option::None,
    };
}

fn steps(int n) -> Result<int, string> {
    if n < 0 {
        raise "negative";
    }
    if n == 0 {
        return 0;
    }
    let rest = steps(n - 2)?;
    return rest + 1;
}

fn depth(int n) -> int {
    if n == 0 {
        return 0;
    }
    return 1 + depth(n - 1);
}

test("mutual recursion") {
    assert!(is_even(0));
    assert!(is_even(10));
    assert!(!is_even(7));
    assert!(is_odd(7));
    assert!(!is_odd(100));
}

test("recursive call as an argument") {
    assert!(ack(0, 0) == 1);
    assert!(ack(1, 2) == 4);
    assert!(ack(2, 3) == 9);
    assert!(ack(3, 3) == 61);
}

test("recursion that builds a string") {
    assert!(binary(0) == "0");
    assert!(binary(1) == "1");
    assert!(binary(2) == "10");
    assert!(binary(3) == "11");
    assert!(binary(6) == "110");
    assert!(binary(255) == "11111111");
}

test("recursion through Option") {
    assert!(halvings(1) == Option::Some(0));
    assert!(halvings(8) == Option::Some(3));
    assert!(halvings(1024) == Option::Some(10));
    assert!(halvings(12) == Option::None);
}

test("recursion through Result and ?") {
    assert!(steps(0) == Result::Ok(0));
    assert!(steps(6) == Result::Ok(3));
    assert!(steps(5) == Result::Err("negative"));
}

test("deep non-tail recursion") {
    assert!(depth(50000) == 50000);
}
