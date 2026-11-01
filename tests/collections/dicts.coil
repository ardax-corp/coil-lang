// Records: create, field access, mutation.
use tests::assert;

test("field access") {
    let d = { foo: 42, bar: 100 };
    assert!(d.foo == 42);
    assert!(d.bar == 100);
}

test("field mutation") {
    let d = { val: 1 };
    d.val = 10;
    assert!(d.val == 10);
    d.val += 32;
    assert!(d.val == 42);
}

test("nested dict fields") {
    // Bind the intermediate record, then read its field.
    let d = { inner: { v: 7 }, tag: 9 };
    assert!(d.tag == 9);
    let inner = d.inner;
    assert!(inner.v == 7);
}

test("string and bool fields") {
    let d = { ok: true, msg: "hi" };
    assert!(d.ok == true);
    // String fields use interned literals at construction — identity eq works.
    assert!(d.msg == "hi");
}
