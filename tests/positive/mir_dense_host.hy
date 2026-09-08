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
    assert(hot(1.0, 2) == sin(1.0))?;
}
