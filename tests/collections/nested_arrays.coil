// Arrays of arrays and Vecs of Vecs: row and cell reads, cell writes
// through a chained index, structural equality and `len` of each level.
use tests::assert;

fn trace([[int; 3]; 3] m) -> int {
    return m[0][0] + m[1][1] + m[2][2];
}

test("fixed grid reads and writes") {
    let m = [[1, 2, 3], [4, 5, 6], [7, 8, 9]];
    assert!(m[1][2] == 6);
    assert!(trace(m) == 15);
    m[2][0] = 70;
    m[0][1] += 10;
    assert!(m[2][0] == 70);
    assert!(m[0] == [1, 12, 3]);
    assert!(m.len() == 3 && m[0].len() == 3);
}

test("grids compare structurally") {
    let a = [[1, 2], [3, 4]];
    let b = [[1, 2], [3, 4]];
    assert!(a == b);
    b[1][1] = 0;
    assert!(a != b);
}

test("Vec of Vec grows per row") {
    let rows: Vec<Vec<int>> = Vec::new();
    let i = 0;
    while i < 3 {
        let row: Vec<int> = Vec::new();
        let j = 0;
        while j <= i {
            row.push(i * 10 + j);
            j = j + 1;
        }
        rows.push(row);
        i = i + 1;
    }
    assert!(rows.len() == 3);
    assert!(rows[2].len() == 3);
    assert!(rows[2][1] == 21);
    rows[0].push(5);
    assert!(rows[0].len() == 2 && rows[0][1] == 5);
}

test("loop over nested arrays") {
    let total = 0;
    for row in [[1, 2], [3, 4], [5, 6]] {
        for x in row {
            total = total + x;
        }
    }
    assert!(total == 21);
}
