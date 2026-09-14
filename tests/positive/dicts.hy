// Anonymous records (dicts): create, field access, mutation, for-in.
test("field access") {
    let d = { foo: 42, bar: 100 };
    assert(d.foo == 42)?;
    assert(d.bar == 100)?;
}

test("field mutation") {
    let d = { val: 1 };
    d.val = 10;
    assert(d.val == 10)?;
    d.val += 32;
    assert(d.val == 42)?;
}

test("nested dict fields") {
    // Chained `d.inner.v` is not yet supported for dicts; bind the intermediate.
    let d = { inner: { v: 7 }, tag: 9 };
    assert(d.tag == 9)?;
    let inner = d.inner;
    assert(inner.v == 7)?;
}

test("string and bool fields") {
    let d = { ok: true, msg: "hi" };
    assert(d.ok == true)?;
    // String fields use interned literals at construction — identity eq works.
    assert(d.msg == "hi")?;
}

test("for in dict yields key-value pairs") {
    let seen = 0;
    for p in { a: 1, b: 2 } {
        seen = seen + p[1];
    }
    assert(seen == 3)?;
}

test("for in dict binds key and value") {
    let seen = 0;
    for (k, v) in { a: 1, b: 2 } {
        seen = seen + v;
    }
    assert(seen == 3)?;
}
