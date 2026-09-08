// COI-290 W4: allowlisted math HostInvoke inside a dense float loop.
fn hot(float a, int n) -> float {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        s = s + sin(xf) * a;
        i = i + 1;
    }
    return s;
}

test("dense host sin n1") {
    assert(hot(2.0, 1) == 0.0)?;
}

test("dense host sin n2") {
    let got = hot(1.0, 2);
    let want = sin(1.0);
    let d = got - want;
    if d < 0.0 {
        assert((0.0 - d) < 0.0000001)?;
    } else {
        assert(d < 0.0000001)?;
    }
}
