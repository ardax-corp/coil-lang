// @flags --comptime-env PATH --comptime-env COIL_COMPTIME_UNSET_VAR
// A `comptime` expression reads granted environment variables.
use env::var;
use tests::assert;

test("a set variable") {
    let path = comptime var("PATH") ?? "";
    assert!(path.len() > 0);
}

test("an unset variable") {
    let missing = comptime var("COIL_COMPTIME_UNSET_VAR") ?? "unset";
    assert!(missing == "unset");
}
