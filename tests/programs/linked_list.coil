// A generic cons list: push, length, reverse, nth and sum through recursion and `while true` with `match` + `break`.
// @output [3, 2, 1] 3 [1, 2, 3] Some(2) 6\n
use io::sync::println;
use string::format;

enum List<T> {
    Nil,
    Cons(T, List<T>),
}

fn push<T>(List<T> l, T x) -> List<T> {
    return List::Cons(x, l);
}

fn to_vec<T>(List<T> l) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    let cur = l;
    while true {
        match cur {
            List::Nil => {
                break;
            },
            List::Cons(x, rest) => {
                out.push(x);
                cur = rest;
            },
        }
    }
    return out;
}

fn len<T>(List<T> l) -> int {
    return match l {
        List::Nil => 0,
        List::Cons(_, r) => 1 + len(r),
    };
}

fn reverse<T>(List<T> l) -> List<T> {
    let acc: List<T> = List::Nil;
    let cur = l;
    while true {
        match cur {
            List::Nil => {
                break;
            },
            List::Cons(x, r) => {
                acc = List::Cons(x, acc);
                cur = r;
            },
        }
    }
    return acc;
}

fn nth<T>(List<T> l, int n) -> Option<T> {
    return match l {
        List::Nil => Option::None,
        List::Cons(x, r) => if n == 0 {
            Option::Some(x)
        } else {
            nth(r, n - 1)
        },
    };
}

fn sum(List<int> l) -> int {
    return match l {
        List::Nil => 0,
        List::Cons(x, r) => x + sum(r),
    };
}

fn main() {
    let l: List<int> = List::Nil;
    l = push(l, 1);
    l = push(l, 2);
    l = push(l, 3);
    let _ = println(
        format("%v %i %v %v %i", to_vec(l), len(l), to_vec(reverse(l)), nth(l, 1), sum(l)),
    );
}
