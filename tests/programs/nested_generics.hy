// `Option<Option<T>>`, a `Vec<Option<int>>`, `Result<Option<int>, string>` and a tuple holding an `Option`, all shown with `%v`.
// @output Some(Some(3)) [Some(1), None] Ok(Some(2)) 2 [[1], [2, 3]] (1, Some(x))\n
use io::sync::println;
use string::format;

fn wrap2<T>(T x) -> Option<Option<T>> {
    return Option::Some(Option::Some(x));
}

fn lookup(Vec<Vec<int>> grid, int r, int c) -> Result<Option<int>, string> {
    if r >= grid.len() {
        raise "row";
    }
    let row = grid[r];
    if c >= row.len() {
        return Option::None;
    }
    return Option::Some(row[c]);
}

fn count_some<T>(Vec<Option<T>> v) -> int {
    let n = 0;
    for o in v {
        match o {
            Option::Some(_) => {
                n += 1;
            },
            Option::None => {},
        }
    }
    return n;
}

fn main() {
    let w = wrap2(3);
    let opts: Vec<Option<int>> = Vec::from([Option::Some(1), Option::None]);
    let grid: Vec<Vec<int>> = Vec::new();
    grid.push(Vec::from([1]));
    grid.push(Vec::from([2, 3]));
    let l = lookup(grid, 1, 0);
    let pair = (1, Option::Some("x"));
    let _ = println(
        format(
            "%v %v %v %i %v %v",
            w,
            opts,
            l,
            count_some(Vec::from([Option::Some("a"), Option::Some("b"), Option::None])),
            grid,
            pair,
        ),
    );
}
