// A guard callee inlined into another guard callee: `single_exit` rebuilds
// the blocks it folds and leaves the old ones behind, sharing statements with
// live blocks. A temp hoisted into a left-behind block never ran (#859).

fn is_leap(int year) -> bool {
    return year % 4 == 0;
}

fn month_days(int year, int month) -> int {
    if month == 2 {
        if is_leap(year) {
            return 29;
        }
        return 28;
    }
    return 31;
}

test("nested guard callees") {
    assert(month_days(3, 4) == 31, "april")?;
    assert(month_days(4, 2) == 29, "leap feb")?;
    assert(month_days(5, 2) == 28, "feb")?;
}
