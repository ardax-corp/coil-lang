// @expected E0119 — an instance method cannot add a bound the trait
// method does not declare: a caller through `Seq` has no `Eq`
// dictionary to pass. The bound goes on the head,
// `impl Seq for Vec<T: Eq>` (#2007).
trait Seq<V> {
    type Item;
    fn has(V xs, Item needle) -> bool {}
}

impl Seq for Vec<T> {
    type Item = T;
    pub fn has<T: Eq>(Vec<T> xs, T needle) -> bool {
        for x in xs {
            if x == needle {
                return true;
            }
        }
        return false;
    }
}

fn main() {
    let xs: Vec<int> = [1, 2];
    let _ = xs.has(2);
}
