// Expected: compile failure — no bound on `T` declares `from_val`.
class Val { pub i: int, }
trait FromVal<T> {
    static fn from_val(Val v) -> T {}
}
fn decode<T>(Val v) -> T {
    return T::from_val(v);
}
fn main() {}
