// `for` over ranges, `while` with `break` / `continue`, early `return` from a search and a `match` on its `Option`.
// @output 45 5 4 0123 even odd end 5\n2 4 6 8 10 found\n
use io::sync::println;
use string::format;

fn classify(int n) -> string {
    if n % 2 == 0 {
        return "even";
    } else {
        return "odd";
    }
}

fn find(Vec<int> xs, int target) -> Option<int> {
    let i = 0;
    while i < xs.len() {
        if xs[i] == target {
            return Option::Some(i);
        }
        i += 1;
    }
    return Option::None;
}

fn main() {
    let total = 0;
    for i in 0..10 {
        total += i;
    }
    let count = 0;
    let i = 0;
    while true {
        i += 1;
        if i > 5 {
            break;
        }
        count += 1;
    }
    let odd_sum = 0;
    for k in 0..=3 {
        if k % 2 == 0 {
            continue;
        }
        odd_sum += k;
    }
    let s = "";
    for k in 0..4 {
        s += format("%i", k);
    }
    let evens = "";
    let v: Vec<int> = Vec::new();
    for k in 1..=10 {
        if k % 2 == 0 {
            v.push(k);
        }
    }
    for e in v {
        evens += format("%i ", e);
    }
    let r = match find(v, 4) {
        Option::Some(_) => "found",
        Option::None => "missing",
    };
    let _ = println(
        format(
            "%i %i %i %s %s %s end %i%s%s",
            total,
            count,
            odd_sum,
            s,
            classify(2),
            classify(3),
            v.len(),
            "",
            "",
        ),
    );
    let _ = println(format("%s%s", evens, r));
}
