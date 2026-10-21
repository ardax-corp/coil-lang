// Macros and a derive written by `comptime` blocks and by a macro, for
// `tests/comptime/`.
use macro::{Code, Expr, TypeDecl, lit_int};
use comptime_gen::{scale_macro, count_derive, doubler};

comptime {
    return scale_macro("triple", 3);
}

comptime {
    return count_derive("FieldCount");
}

// A macro's output may declare a macro too.
doubler!(double);
