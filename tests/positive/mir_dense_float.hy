// COI-268: float loop that must run on dense MIR opcodes.
fn escape(float cr, float ci, int max_iter) -> int {
    let zr = 0.0;
    let zi = 0.0;
    let iter = 0;
    while iter < max_iter {
        let zr2 = zr * zr;
        let zi2 = zi * zi;
        if zr2 + zi2 > 4.0 {
            break;
        }
        let tr = zr2 - zi2 + cr;
        zi = 2.0 * zr * zi + ci;
        zr = tr;
        iter = iter + 1;
    }
    return iter;
}

test("dense mandel origin stays inside") {
    assert(escape(0.0, 0.0, 50) == 50)?;
}

test("dense mandel far point escapes after first step") {
    assert(escape(2.0, 2.0, 50) == 1)?;
}

test("dense mandel known interior") {
    assert(escape(-0.75, 0.1, 50) > 0)?;
}
