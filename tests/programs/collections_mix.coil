// Fixed arrays, `Vec` growth, nested arrays, `pop` / `remove` with `??`, broadcast arithmetic and structural equality.
// @output 3 6 [1, 2, 3] 2 9 4 1 1 0 [2, 4, 6] 3 1 true 5\n
use io::sync::println;
use string::format;

fn main() {
    let arr = [1, 2, 3];
    let total = 0;
    for x in arr {
        total += x;
    }
    let v: Vec<int> = Vec::from(arr);
    v.push(4);
    let last = v.pop() ?? -1;
    let m: [[int; 2]; 2] = [[1, 2], [3, 4]];
    let diag = m[0][0] * m[1][1] + 5 - 0;
    let grid = [[0, 0, 0], [0, 0, 0]];
    let z = grid[1][2];
    let w = v.remove(0) ?? -1;
    let d = { a: 1, b: 2 };
    let dv = d.a + d.b;
    let keys = "";
    keys += format("%i", dv);
    let doubled = [1, 2, 3] * 2;
    let t = (1, 2, 3);
    let tl = t.len();
    let vv: Vec<Vec<int>> = Vec::new();
    vv.push(Vec::from([1]));
    let eq = [1, 2] == [1, 2];
    let strlen = "hello".len();
    let _ = println(
        format(
            "%i %i %v %i %i %i %i %i %i %v %i %i %z %i",
            arr.len(),
            total,
            arr,
            v.len(),
            diag,
            last,
            w,
            vv[0][0],
            z,
            doubled,
            tl,
            vv.len(),
            eq,
            strlen,
        ),
    );
    let _ = keys;
}
