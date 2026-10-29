// Generic insertion sort and sortedness check under `T: Ord`, a generic reverse and a binary search returning `Option`.
// @output [1, 2, 3, 5, 8, 9] [9, 8, 5, 3, 2, 1] [apple, fig, kiwi] true 3\n
use io::sync::println;
use string::format;

fn insertion_sort<T: Ord>(Vec<T> v) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    for x in v {
        out.push(x);
    }
    let i = 1;
    while i < out.len() {
        let j = i;
        while j > 0 && out[j - 1] > out[j] {
            let tmp = out[j];
            out[j] = out[j - 1];
            out[j - 1] = tmp;
            j -= 1;
        }
        i += 1;
    }
    return out;
}

fn reversed<T>(Vec<T> v) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    let i = v.len();
    while i > 0 {
        i -= 1;
        out.push(v[i]);
    }
    return out;
}

fn is_sorted<T: Ord>(Vec<T> v) -> bool {
    let i = 1;
    while i < v.len() {
        if v[i - 1] > v[i] {
            return false;
        }
        i += 1;
    }
    return true;
}

fn binary_search(Vec<int> v, int target) -> Option<int> {
    let lo = 0;
    let hi = v.len();
    while lo < hi {
        let mid = (lo + hi) / 2;
        if v[mid] == target {
            return Option::Some(mid);
        }
        if v[mid] < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    return Option::None;
}

fn main() {
    let s = insertion_sort(Vec::from([5, 3, 9, 1, 8, 2]));
    let words = insertion_sort(Vec::from(["kiwi", "apple", "fig"]));
    let _ = println(
        format("%v %v %v %z %i", s, reversed(s), words, is_sorted(s), binary_search(s, 5) ?? -1),
    );
}
