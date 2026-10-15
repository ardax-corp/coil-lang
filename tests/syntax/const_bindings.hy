// A local `const` cannot be rebound but reads like a `let`: it may carry a
// type annotation, live in an inner block, be shadowed there by a `let`,
// and (being shallow) still allows interior mutation of a heap value.
// (Assigning to a `let` that shadows a local const, and `len` of a
// const, are known bugs; see findings.)
use tests::assert;

test("annotated consts") {
    const n: int = 40;
    const f: float = 0.5;
    const s: string = "c";
    assert!(n + 2 == 42);
    assert!(f * 4.0 == 2.0);
    assert!(s + s == "cc");
}

test("const in a block and a loop body") {
    let total = 0;
    {
        const step = 5;
        total = total + step;
    }
    let i = 0;
    while i < 3 {
        const k = i * 2;
        total = total + k;
        i = i + 1;
    }
    assert!(total == 5 + 0 + 2 + 4);
}

test("an inner let may shadow a const") {
    const x = 1;
    {
        let x = 2;
        assert!(x == 2);
    }
    assert!(x == 1);
}

test("const is shallow") {
    const v: Vec<int> = [1];
    v.push(2);
    v[0] = 10;
    assert!(v[0] == 10);
    assert!(v[1] == 2);
    const d = { a: 1 };
    d.a = 5;
    assert!(d.a == 5);
}

test("const from an expression") {
    let base = 6;
    const sq = base * base;
    base = 0;
    assert!(sq == 36);
}
