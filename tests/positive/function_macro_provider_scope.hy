// A macro's output may call its provider's other macros: `quad!` expands to
// `square!(square!(…))`, and `square` is not imported here.
use fn_macros::quad;

test("macros in output resolve in the provider") {
    assert(quad!(3) == 81)?;
}
