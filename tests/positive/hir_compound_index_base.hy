// Compound assigns through an indexed base: the base is read twice.
test("compound assign through computed index bases") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    let i = 1;
    rows[i][0] *= 10;
    rows[i - 1][i] -= 2;
    assert(rows[1][0] == 30)?;
    assert(rows[0][1] == 0)?;
    let grid = [[1.5, 2.0], [3.0, 4.0]];
    grid[1][1] += 0.5;
    assert(grid[1][1] == 4.5)?;
}
