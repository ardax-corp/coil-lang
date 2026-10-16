// Helper for tests/modules/module_resolution.hy: a two-slot
// `Result<int, int>` returned across modules, called directly and through
// its address.

fn div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}
