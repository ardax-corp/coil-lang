// A sum of each element's length (`sum += x.len()` over a `Vec<string>`)
// is not a lane-wise reduction: the length is not loop-invariant, since it
// reads the element the loop indexes (#973).
use tests::assert;

fn total(Vec<string> xs) -> int {
    let sum = 0;
    for x in xs {
        sum += x.len();
    }
    return sum;
}

fn nested(Vec<Vec<int>> rows) -> int {
    let sum = 0;
    for r in rows {
        sum += r.len();
    }
    return sum;
}

test("element lengths over many elements") {
    let xs = Vec::from(["ab", "c", "def", "g", "hh", "i", "j", "k", "l", "mmmm", "n"]);
    assert!(total(xs) == 18);
    let rows = Vec::new();
    for i in 0..12 {
        let r = Vec::new();
        for j in 0..i {
            r.push(j);
        }
        rows.push(r);
    }
    assert!(nested(rows) == 66);
}
