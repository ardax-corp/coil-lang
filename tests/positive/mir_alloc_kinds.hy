// Allocations built by direct MIR carry the same construction kinds as the
// stack-IL path: a `Vec<byte>` literal is packed one byte per element.
use tests::assert;
use gc::{collect, heap_bytes};

fn row(int i) -> Vec<byte> {
    return [
        (i % 256) as byte,
        1 as byte,
        2 as byte,
        3 as byte,
        4 as byte,
        5 as byte,
        6 as byte,
        7 as byte,
        8 as byte,
        9 as byte,
        10 as byte,
        11 as byte,
        12 as byte,
        13 as byte,
        14 as byte,
        15 as byte,
    ];
}

test("byte literals built in a direct MIR body are packed") {
    collect();
    let before = heap_bytes();
    let rows: Vec<Vec<byte>> = Vec::with_capacity(10000);
    let i = 0;
    while i < 10000 {
        rows.push(row(i));
        i = i + 1;
    }
    // Packed rows cost about 96 bytes each (header + 16 bytes); word rows
    // cost about 208.
    let grown = heap_bytes() - before;
    assert!(grown < 1400000);
    assert!(rows[9999][0] as int == 9999 % 256 && rows[5][15] as int == 15);
}
