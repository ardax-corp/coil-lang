// `use m::{size}` imports the free fn `size` even when a trait declared after
// it in `m` has a method `size`; the method stays a method.
use tests::assert;
use shared_fn_trait_names::{Sized, size};

test("an imported free fn is not the same-named trait method") {
    assert!(size("abc") == 3);
    assert!(12.size() == "12");
}
