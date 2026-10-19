// Doc stubs for the builtin `prelude::math` module (hover docs; never compiled).

/// Returns the sine of `x` (in radians).
///
/// Like every scalar function in `prelude::math`, it takes and returns
/// `float` and follows IEEE-754: invalid inputs give NaN and overflow gives
/// infinity, never a clamped value. Auto-imported.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let pi = 3.141592653589793;
/// assert!(sin(0.0) == 0.0);
/// assert!(sin(pi / 2.0) == 1.0);
/// ```
fn sin(float x) -> float {}

/// Returns the cosine of `x` (in radians).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(cos(0.0) == 1.0);
/// ```
fn cos(float x) -> float {}

/// Returns the tangent of `x` (in radians).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(tan(0.0) == 0.0);
/// let t = tan(3.141592653589793 / 4.0);
/// assert!(t > 0.9999);
/// assert!(t < 1.5);
/// ```
fn tan(float x) -> float {}

/// Returns the square root of `x`.
///
/// A negative `x` gives NaN.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(sqrt(16.0) == 4.0);
/// assert!(sqrt(2.0) * sqrt(2.0) > 1.9999);
/// ```
fn sqrt(float x) -> float {}

/// Returns the largest whole number less than or equal to `x`, as a `float`.
///
/// Use `floor(x) as int` to get an `int`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(floor(2.7) == 2.0);
/// assert!(floor(-2.1) == -3.0);
/// ```
fn floor(float x) -> float {}

/// Returns the smallest whole number greater than or equal to `x`, as a `float`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(ceil(2.1) == 3.0);
/// assert!(ceil(-2.7) == -2.0);
/// ```
fn ceil(float x) -> float {}

/// Returns `e` raised to the power `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(exp(0.0) == 1.0);
/// assert!(ln(exp(2.0)) == 2.0);
/// ```
fn exp(float x) -> float {}

/// Returns the natural logarithm of `x`.
///
/// `ln(0.0)` is negative infinity and a negative `x` gives NaN.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(ln(1.0) == 0.0);
/// assert!(ln(0.0) < -1000000.0);
/// ```
fn ln(float x) -> float {}

/// Returns the arctangent of `x`, in radians in `-pi/2..=pi/2`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(atan(0.0) == 0.0);
/// assert!(atan(1.0) * 4.0 == 3.141592653589793);
/// ```
fn atan(float x) -> float {}

/// Returns the angle of the point `(x, y)` from the positive x axis, in radians in `-pi..=pi`.
///
/// Note the argument order: `y` first, then `x`. Unlike `atan(y / x)` it
/// uses the signs of both arguments to pick the right quadrant.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let pi = 3.141592653589793;
/// assert!(atan2(0.0, 1.0) == 0.0);
/// assert!(atan2(1.0, 0.0) == pi / 2.0);
/// assert!(atan2(0.0, -1.0) == pi);
/// ```
fn atan2(float y, float x) -> float {}

/// Returns the arcsine of `x`, in radians in `-pi/2..=pi/2`.
///
/// `x` outside `-1.0..=1.0` gives NaN.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(asin(0.0) == 0.0);
/// assert!(asin(1.0) == 3.141592653589793 / 2.0);
/// ```
fn asin(float x) -> float {}

/// Returns the arccosine of `x`, in radians in `0..=pi`.
///
/// `x` outside `-1.0..=1.0` gives NaN.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(acos(1.0) == 0.0);
/// assert!(acos(-1.0) == 3.141592653589793);
/// ```
fn acos(float x) -> float {}

/// Returns the base-10 logarithm of `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(log10(1000.0) == 3.0);
/// ```
fn log10(float x) -> float {}

/// Returns the base-2 logarithm of `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(log2(8.0) == 3.0);
/// ```
fn log2(float x) -> float {}

/// Returns the cube root of `x`.
///
/// Defined for negative `x` too (`cbrt(-8.0)` is `-2.0`).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(cbrt(27.0) == 3.0);
/// assert!(cbrt(-8.0) == -2.0);
/// ```
fn cbrt(float x) -> float {}

/// Returns the hyperbolic sine of `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(sinh(0.0) == 0.0);
/// ```
fn sinh(float x) -> float {}

/// Returns the hyperbolic cosine of `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(cosh(0.0) == 1.0);
/// ```
fn cosh(float x) -> float {}

/// Returns the hyperbolic tangent of `x`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(tanh(0.0) == 0.0);
/// assert!(tanh(20.0) > 0.9999);
/// ```
fn tanh(float x) -> float {}

/// Returns the dot product of two vectors of the same length.
///
/// `a` and `b` are both tuples or both fixed arrays (`[T; N]`) of the same
/// numeric element type (`int`, `float` or `byte`) and length. The result is
/// the sum of the element-wise products, of the element type. `*` on tuples
/// and arrays is element-wise, not a dot product.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(dot((1, 2, 3), (4, 5, 6)) == 32);
/// assert!(dot([1.5, 2.0], [2.0, 4.0]) == 11.0);
/// ```
fn dot<V, T>(V a, V b) -> T {}

/// Returns the cross product of two 3-element vectors.
///
/// `a` and `b` are both 3-tuples or both `[T; 3]` arrays of the same numeric
/// element type; the result has the same shape.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// assert!(cross((1, 0, 0), (0, 1, 0)) == (0, 0, 1));
/// let z = cross([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]);
/// assert!(z[0] == 1.0);
/// ```
fn cross<V>(V a, V b) -> V {}

/// Multiplies two matrices written as nested fixed-length arrays.
///
/// `a` is `[[T; K]; M]` and `b` is `[[T; N]; K]`; the result is the row-major
/// product `[[T; N]; M]`. A mismatched inner dimension is a compile-time
/// error. For `*` as matrix multiplication, wrap the data with `matrix`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let p = matmul([[1, 2], [3, 4]], [[5, 6], [7, 8]]);
/// assert!(p[0][0] == 19);
/// assert!(p[0][1] == 22);
/// assert!(p[1][0] == 43);
/// assert!(p[1][1] == 50);
/// ```
fn matmul<A, B, C>(A a, B b) -> C {}

/// Wraps nested fixed-length rows as a `Matrix`.
///
/// `rows` is nested numeric data such as `[[int; 2]; 2]` (or a tuple of
/// equal-length rows). There is no runtime cost: the `Matrix` is the same
/// data with a type that gives operators matrix meaning (see `Matrix`).
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let a = matrix([[1, 2], [3, 4]]);
/// let sq = a * a;
/// assert!(sq[0][0] == 7);
/// assert!(sq[1][1] == 22);
/// ```
fn matrix<D>(D rows) -> Matrix<D> {}

/// Returns a `byte` mask that is `1` where both matrices have a non-zero cell.
///
/// `a` and `b` must have the same shape. A cell counts as present when it is
/// not `0` (NaN counts as present). The result is a `Matrix` of `byte`
/// cells with the same shape; there is no scalar broadcast.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let a = matrix([[1, 0], [2, 3]]);
/// let b = matrix([[5, 6], [0, 7]]);
/// let m = intersect(a, b);
/// assert!(m[0][0] == 1 as byte);
/// assert!(m[0][1] == 0 as byte);
/// assert!(m[1][0] == 0 as byte);
/// assert!(m[1][1] == 1 as byte);
/// ```
fn intersect<D, M>(Matrix<D> a, Matrix<D> b) -> Matrix<M> {}

/// Returns a `byte` mask that is `1` where `a` has a non-zero cell and `b` has `0`.
///
/// Same shape rules as `intersect`. The result is a `Matrix` of `byte`
/// cells.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let a = matrix([[1, 2], [3, 0]]);
/// let b = matrix([[1, 0], [0, 0]]);
/// let d = diff(a, b);
/// assert!(d[0][0] == 0 as byte);
/// assert!(d[0][1] == 1 as byte);
/// assert!(d[1][0] == 1 as byte);
/// assert!(d[1][1] == 0 as byte);
/// ```
fn diff<D, M>(Matrix<D> a, Matrix<D> b) -> Matrix<M> {}

/// A matrix: nested fixed-length numeric rows with matrix operators.
///
/// Build one with `matrix(rows)`; `D` is the nested data type, for example
/// `Matrix<[[int; 2]; 2]>`. Cells are read with `m[row][col]`. On `Matrix`
/// values:
///
/// - `*` is matrix multiplication.
/// - `+`, `-` and unary `-` are element-wise.
/// - `==`, `!=`, `<`, `<=`, `>`, `>=` compare cell by cell and return a
///   `Matrix` of `byte` (`1` where the test holds). A scalar of the cell type
///   is broadcast (`m == 1`).
/// - `&`, `|`, `^`, `<<`, `>>`, `~` work cell by cell on `int` or `byte`
///   cells, with scalar broadcast.
/// - `/`, `%` and `**` are rejected: `Matrix` is not `Num`.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// let a = matrix([[1, 2], [3, 4]]);
/// let b = matrix([[5, 6], [7, 8]]);
/// let p = a * b;
/// assert!(p[0][0] == 19);
/// let s = a + a;
/// assert!(s[1][1] == 8);
/// let ones = a == 1;
/// assert!(ones[0][0] == 1 as byte);
/// assert!(ones[0][1] == 0 as byte);
/// ```
class Matrix<D> {}
