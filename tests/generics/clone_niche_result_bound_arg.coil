// A mono clone passes a niche `Result` word to a bounded generic's shared
// body as its bare type parameter, and gets it back (#1565).
use tests::assert;

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Result<A, B> {
    fn sz(Result<A, B> r) -> int {
        return match r {
            Ok(_) => 1,
            Err(_) => 2,
        };
    }
}

fn size<T: Sz>(T x) -> int {
    return x.sz();
}

fn keep<T: Sz>(T x) -> T {
    return x;
}

fn hr<E>(E e) -> int {
    return size(Err(e)) + size(Ok(e));
}

fn hk<E>(E e) -> int {
    let r: Result<(), E> = Err(e);
    return match keep(r) {
        Ok(_) => 10,
        Err(_) => 20,
    };
}

fn hk2<E>(E e) -> int {
    let r: Result<E, string> = Err("q");
    return match keep(r) {
        Ok(_) => 10,
        Err(m) => m.len(),
    };
}

test("results") {
    assert!(hr("s") == 3);
    assert!(hk("s") == 20);
    assert!(hk2("s") == 1);
    assert!(hk2(3) == 1);
}
