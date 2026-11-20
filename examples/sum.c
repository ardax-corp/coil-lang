// sum.c — a minimal C library exposed to userland via the
// FFI mechanism. Defines a single `sum` function that adds
// two integers. Compiled to a shared library and loaded by
// the runtime at startup via `dlopen` + `dlsym`.
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
/// The runtime's `dlopen("sum")` call looks for `libsum.so` on
/// the system library search path (or the current
/// directory). The `dlsym("sum")` call resolves the symbol
/// in that library. The runtime then wraps the resolved function
/// pointer in a `LibraryFn` and sends the calls
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

/// Sum `n` bytes from a read-only buffer (for `ffi::types::ConstBytes` tests).
int64_t sum_bytes(const uint8_t *buf, int64_t n) {
    int64_t total = 0;
    for (int64_t i = 0; i < n; i++) {
        total += buf[i];
    }
    return total;
}

/// Run `cb(n)`, then sum the bytes: the callback runs (and may collect)
/// while C still holds `buf`.
int64_t sum_bytes_after_cb(const uint8_t *buf, int64_t n, int64_fn_int64 cb) {
    cb(n);
    return sum_bytes(buf, n);
}

/// Copy `n` bytes from `src` into `dst`, front to back (for `Bytes` tests;
/// `dst` and `src` may be the same buffer).
void copy_bytes(uint8_t *dst, const uint8_t *src, int64_t n) {
    for (int64_t i = 0; i < n; i++) {
        dst[i] = src[i];
    }
}

/// Write `v` into `n` bytes of `buf` (for `Bytes` tests).
void fill_bytes(uint8_t *buf, int64_t n, int64_t v) {
    for (int64_t i = 0; i < n; i++) {
        buf[i] = (uint8_t)v;
    }
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

static int64_t noted = 0;

/// Add `a + b` to a running total; returns nothing (for void `extern`
/// tests: each call must leave the runtime's stack as it found it).
void sum_note(void *tag, int64_t a, int64_t b) {
    (void)tag;
    noted += a + b;
}

/// Clear the running total.
void sum_reset(void) {
    noted = 0;
}

/// The running total.
int64_t sum_noted(void) {
    return noted;
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

/// Floating and bool scalars (direct extern call tests).
double scale(double x, int64_t k) {
    return x * (double)k;
}

_Bool is_even(int64_t x) {
    return (x & 1) == 0;
}

int64_t pick(_Bool b, int64_t x, int64_t y) {
    return b ? x : y;
}

/// Strings (direct extern call tests): a length, the argument back, and a
/// constant.
int64_t str_len(const char *s) {
    int64_t n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

const char *echo(const char *s) {
    return s;
}

const char *greeting(void) {
    return "hello from C";
}

const char *no_string(void) {
    return 0;
}

/// Structs by value (direct extern call tests): two doubles, an int and
/// a double, three ints (passed in memory), and a nested struct.
typedef struct {
    double x;
    double y;
} Vec2;

Vec2 vec2_add(Vec2 a, Vec2 b) {
    Vec2 r = {a.x + b.x, a.y + b.y};
    return r;
}

typedef struct {
    int64_t id;
    double w;
} Tagged;

Tagged tagged_scale(Tagged t, int64_t k) {
    Tagged r = {t.id + 1, t.w * (double)k};
    return r;
}

typedef struct {
    int64_t a;
    int64_t b;
    int64_t c;
} Triple;

Triple triple_rot(Triple t) {
    Triple r = {t.b, t.c, t.a};
    return r;
}

int64_t triple_sum(int64_t k, Triple t) {
    return k * (t.a + t.b + t.c);
}

Point point_swap(Point p) {
    Point r = {p.y, p.x};
    return r;
}

typedef struct {
    Point at;
    _Bool on;
} Flag;

Flag flag_flip(Flag f) {
    Flag r = {{f.at.x + 1, f.at.y + 1}, !f.on};
    return r;
}

Vec2 vec2_make(double x, double y) {
    Vec2 r = {x, y};
    return r;
}

Tagged tagged_make(int64_t id, double w) {
    Tagged r = {id, w};
    return r;
}

Triple triple_make(int64_t a, int64_t b, int64_t c) {
    Triple r = {a, b, c};
    return r;
}

Point point_make(int64_t x, int64_t y) {
    Point r = {(int32_t)x, (int32_t)y};
    return r;
}

Flag flag_make(int64_t x, int64_t y, _Bool on) {
    Flag r = {{(int32_t)x, (int32_t)y}, on};
    return r;
}

/// One number of each struct's fields, to check them from coil.
double vec2_code(Vec2 v) {
    return v.x * 100.0 + v.y;
}

double tagged_code(Tagged t) {
    return (double)t.id * 100.0 + t.w;
}

int64_t triple_code(Triple t) {
    return t.a * 100 + t.b * 10 + t.c;
}

int64_t point_code(Point p) {
    return p.x * 10 + p.y;
}

int64_t flag_code(Flag f) {
    return f.at.x * 100 + f.at.y * 10 + f.on;
}

/// Seven `int64_t`s, then a struct: the struct finds no registers left and
/// goes on the stack (x86-64), after the seventh.
int64_t late_vec2(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g, Tagged t) {
    return a + b + c + d + e + f + g + t.id;
}
