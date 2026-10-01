// sum.c — a minimal C library exposed to userland via the
// FFI mechanism. Defines a single `sum` function that adds
// two integers. Compiled to a shared library and loaded by
// the VM at startup via `dlopen` + `dlsym`.
//
// Build:
//   cc -shared -fPIC -o libsum.so sum.c
// then put `libsum.so` somewhere the dynamic linker can find
// (e.g. the current directory) so `dlopen("sum")` resolves
// to it.

/// `int sum(int a, int b)` — return the sum of two integers.
///
/// Symbol name: `sum` (the function's identifier as it
/// appears in userland). The coil FFI declaration
/// matches by name:
///
/// ```0s
/// extern "sum" {
///     fn sum(int a, int b) -> int;
/// }
/// ```
///
/// The VM's `dlopen("sum")` call looks for `libsum.so` on
/// the system library search path (or the current
/// directory). The `dlsym("sum")` call resolves the symbol
/// in that library. The VM then wraps the resolved function
/// pointer in a `LibraryFn` and dispatches `NATIVE` opcodes
/// that name `sum` to it.
#include <stdarg.h>
#include <stdint.h>

int64_t sum(int64_t a, int64_t b) {
    return a + b;
}

/// Sum `n` elements from a C int array (for `[int]` → Ptr FFI tests).
int64_t sum_array(const int64_t *arr, int64_t n) {
    int64_t total = 0;
    for (int64_t i = 0; i < n; i++) {
        total += arr[i];
    }
    return total;
}

typedef int64_t (*int64_fn_int64)(int64_t);

/// Invoke a C callback (for FFIType::Callback tests).
int64_t apply_cb(int64_fn_int64 cb, int64_t x) {
    return cb(x);
}

typedef struct {
    int32_t x;
    int32_t y;
} Point;

/// Return a small C struct by value (for FFI struct-return tests).
Point make_point(int32_t x, int32_t y) {
    Point p;
    p.x = x;
    p.y = y;
    return p;
}

static int64_t doubler(int64_t x) {
    return x * 2;
}

/// Return a C function pointer (opaque Ptr / Callback to coil).
int64_fn_int64 get_doubler(void) {
    return &doubler;
}

/// C varargs: sum `n` trailing `int64_t` arguments (for extern `...` tests).
int64_t sum_n(int64_t n, ...) {
    va_list args;
    va_start(args, n);
    int64_t total = 0;
    for (int64_t i = 0; i < n; i++) {
        total += va_arg(args, int64_t);
    }
    va_end(args);
    return total;
}
