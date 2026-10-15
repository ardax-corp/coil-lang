// Blocks scope `let`: an inner `let` of an outer name hides it until the
// block ends (it may even change the type), assignment without `let`
// writes the outer binding, and loop / match bindings shadow the same way.
use tests::assert;

test("inner block shadow is restored") {
    let x = 1;
    {
        let x = 2;
        assert!(x == 2);

        {
            let x = 3;
            assert!(x == 3);
        }

        assert!(x == 2);
    }
    assert!(x == 1);
}

test("shadow with a different type") {
    let v = 10;
    {
        let v = "ten";
        assert!(v == "ten");
    }
    assert!(v + 1 == 11);
}

test("assignment in a block writes the outer binding") {
    let x = 1;
    {
        x = 5;
    }
    assert!(x == 5);
    if x > 0 {
        x = x * 2;
    }
    assert!(x == 10);
}

test("shadow inside if and while bodies") {
    let n = 1;
    if true {
        let n = 100;
        assert!(n == 100);
    }
    assert!(n == 1);
    let i = 0;
    while i < 3 {
        let n = i * 10;
        assert!(n == i * 10);
        i = i + 1;
    }
    assert!(n == 1);
}

test("for binding shadows an outer name") {
    let x = 99;
    let sum = 0;
    for x in [1, 2, 3] {
        sum = sum + x;
    }
    assert!(sum == 6);
    assert!(x == 99);
}

test("match binding shadows an outer name") {
    let v = 7;
    let r = match Option::Some(3) {
        Option::Some(v) => v * 2,
        Option::None => 0,
    };
    assert!(r == 6);
    assert!(v == 7);
}

fn param_shadow(int a) -> (int, int) {
    let inner = 0;
    {
        let a = a + 1;
        inner = a;
    }
    return (inner, a);
}

test("parameter shadowed in a block") {
    assert!(param_shadow(4) == (5, 4));
}

test("shadow reads the outer value in its initializer") {
    let x = 2;
    {
        let x = x * 10;
        assert!(x == 20);
    }
    assert!(x == 2);
}
