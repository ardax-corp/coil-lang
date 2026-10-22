// A free fn and a later trait method of the same name (tests/modules/
// imported_fn_beside_trait_method.hy).

fn size(string s) -> int {
    return s.len();
}

trait Sized<T: Show> {
    fn size(T value) -> string {
        return value.show();
    }
}

impl Sized for int {
}
