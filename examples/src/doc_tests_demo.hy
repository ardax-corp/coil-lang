// Documented items whose `///` examples `coil test` runs as doc-tests when a
// test file imports this module (tests/testing/doc_tests.hy).

/// Adds one.
///
/// # Examples
///
/// ```
/// # use tests::assert;
/// use doc_tests_demo::inc;
///
/// assert!(inc(1) == 2);
/// ```
fn inc(int x) -> int {
    return x + 1;
}

/// A counter.
///
/// ```coil
/// # use tests::assert;
/// use doc_tests_demo::Counter;
///
/// let c = new Counter(3);
/// assert!(c.n == 3);
/// ```
class Counter {
    /// The count.
    ///
    /// ```
    /// # use tests::assert;
    /// # use doc_tests_demo::Counter;
    /// assert!(new Counter(0).n == 0);
    /// ```
    pub n: int,
}

impl Counter {
    /// The count after one more.
    ///
    /// ```
    /// # use tests::assert;
    /// # use doc_tests_demo::Counter;
    /// assert!(new Counter(1).next() == 2);
    /// ```
    pub fn next() -> int {
        return self.n + 1;
    }
}

/// A light.
enum Light {
    /// Stop.
    ///
    /// ```
    /// # use doc_tests_demo::Light;
    /// let l = Light::Red;
    /// ```
    Red,
    Green,
}
