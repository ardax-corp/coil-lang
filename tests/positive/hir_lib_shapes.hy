use hir_lib_shapes::{Gate, Level, Raw};

test("ordered comparison on a module's scalar enum") {
    let g = Gate::new(Level::Info);
    assert(g.pass(Level::Warn))?;
    assert(g.pass(Level::Info))?;
    assert(!g.pass(Level::Debug))?;
    assert(g.below(Level::Debug))?;
    assert(!g.below(Level::Warn))?;
}

test("a match whose arms all raise fits the value's word") {
    assert(hir_lib_shapes::decode(Raw::Good{ value: 7 })? == 7)?;
    let first = match hir_lib_shapes::decode(Raw::Bad{ code: 1 }) {
        Result::Ok(_) => "ok",
        Result::Err(e) => hir_lib_shapes::message(e),
    };
    assert(first == "one")?;
    let second = match hir_lib_shapes::decode(Raw::Bad{ code: 5 }) {
        Result::Ok(_) => "ok",
        Result::Err(e) => hir_lib_shapes::message(e),
    };
    assert(second == "other")?;
}

fn byte_sum([byte] bytes, int n) -> int {
    let total = 0;
    for i in 0..n {
        total = total + (bytes[i] as int);
    }
    return total;
}

test("a byte vector cast to a byte array is the same array") {
    let v: Vec<byte> = Vec::new();
    v.push(1 as byte);
    v.push(2 as byte);
    v.push(4 as byte);
    assert(byte_sum(v as [byte], v.len()) == 7)?;
}
