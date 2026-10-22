// @expected E0403 — an `extern struct` field type must be an FFI type tag, not an arbitrary name.
extern struct Point {
    x: Banana,
    y: int32,
};

fn main() {
}
