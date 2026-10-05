// Records under HIR: built as dicts, fields read and written by name.

fn norm1(int x, int y) -> int {
    let p = { x: x, y: y };
    p.x = p.x + 10;
    p.y += 10;
    return p.x + p.y;
}

fn scaled(float s) -> float {
    let r = { w: 1.5, h: 2.0 };
    r.w = r.w * s;
    return r.w + r.h;
}

fn label(bool ok) -> string {
    let r = { ok: ok, msg: "fine", code: 7 };
    if r.ok {
        return r.msg;
    }
    return "bad";
}

fn nested(int v) -> int {
    let outer = { inner: { v: v }, tag: 9 };
    let inner = outer.inner;
    inner.v = inner.v * 2;
    let again = outer.inner;
    return again.v + outer.tag;
}

test("record fields of each kind") {
    assert(norm1(3, 4) == 27)?;
    assert(scaled(2.0) == 5.0)?;
    assert(label(true) == "fine")?;
    assert(label(false) == "bad")?;
}

test("nested records share the inner dict") {
    assert(nested(7) == 23)?;
}
