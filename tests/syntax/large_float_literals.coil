// #1142: `coil fmt` keeps float literals of 1e16 and above in decimal
// form (Coil has no exponent syntax).
use tests::assert;

test("large float literals") {
    let big = 10000000000000000.0;
    let huge = 123456789012345680000.0;
    assert!(big > 9999999999999998.0);
    assert!(huge > big);
}
