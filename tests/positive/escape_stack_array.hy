// Non-escaping fixed arrays stay local (codegen stack slots + IL escape analysis).
test("local array does not need heap identity") {
    let a = [10, 20, 30];
    a[1] = 21;
    assert(a[0] + a[1] + a[2] == 61)?;
}
