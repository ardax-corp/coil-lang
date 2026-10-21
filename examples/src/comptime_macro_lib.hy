// A macro and a derive written by `comptime` blocks, for `tests/comptime/`.
use macro::{Code, Expr, TypeDecl, lit_int};
use comptime_gen::{scale_macro, count_derive};

comptime {
    return scale_macro("triple", 3);
}

comptime {
    return count_derive("FieldCount");
}
