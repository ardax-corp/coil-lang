// The always-included core prelude: `Option` and `Result` and their
// methods. Every program depends on this module (module `core`); the
// names are in scope everywhere without a `use`.
//
// `#[lang(option)]` and `#[lang(result)]` mark the two enums the compiler
// builds on: `?`, `??`, `?.` and `raise` desugar to matches on them, and
// their values get the niche / two-slot layouts. The variant order is the
// runtime tag order host natives rely on (None = 0, Some = 1; Ok = 0,
// Err = 1).

/// An optional value: either `Some(value)` or `None`.
#[lang(option)]
enum Option<T> {
    /// No value.
    None,
    /// Some value of type `T`.
    Some(T),
}

/// The outcome of an operation that can fail: `Ok(value)` or `Err(error)`.
#[lang(result)]
enum Result<T, E> {
    /// The operation succeeded with a value of type `T`.
    Ok(T),
    /// The operation failed with an error of type `E`.
    Err(E),
}

impl Option<T> {
    /// True when this is `Some`.
    pub fn is_some() -> bool {
        return match self {
            Some(_) => true,
            None => false,
        };
    }

    /// True when this is `None`.
    pub fn is_none() -> bool {
        return match self {
            Some(_) => false,
            None => true,
        };
    }

    /// The value, or `fallback` when `None`. `x ?? fallback` is the same.
    pub fn unwrap_or(T fallback) -> T {
        return match self {
            Some(v) => v,
            None => fallback,
        };
    }

    /// The value, or the result of `f` when `None`.
    pub fn unwrap_or_else(fn() -> T f) -> T {
        return match self {
            Some(v) => v,
            None => f(),
        };
    }

    /// `Some(f(value))`, or `None`.
    pub fn map<U>(T -> U f) -> Option<U> {
        return match self {
            Some(v) => Some(f(v)),
            None => None,
        };
    }

    /// `f(value)`, or `fallback` when `None`.
    pub fn map_or<U>(U fallback, T -> U f) -> U {
        return match self {
            Some(v) => f(v),
            None => fallback,
        };
    }

    /// `f(value)` (itself an `Option`), or `None`.
    pub fn and_then<U>(T -> Option<U> f) -> Option<U> {
        return match self {
            Some(v) => f(v),
            None => None,
        };
    }

    /// This option if it is `Some`, otherwise `other`.
    pub fn or(Option<T> other) -> Option<T> {
        return match self {
            Some(v) => Some(v),
            None => other,
        };
    }

    /// This option if it is `Some`, otherwise the result of `f`.
    pub fn or_else(fn() -> Option<T> f) -> Option<T> {
        return match self {
            Some(v) => Some(v),
            None => f(),
        };
    }

    /// This option if its value passes `pred`, otherwise `None`.
    pub fn filter(T -> bool pred) -> Option<T> {
        let keep = match self {
            Some(v) => pred(v),
            None => false,
        };
        if keep {
            return self;
        }
        return None;
    }

    /// `Ok(value)`, or `Err(err)` when `None`.
    pub fn ok_or<E>(E err) -> Result<T, E> {
        return match self {
            Some(v) => Ok(v),
            None => Err(err),
        };
    }
}

impl Result<T, E> {
    /// True when this is `Ok`.
    pub fn is_ok() -> bool {
        return match self {
            Ok(_) => true,
            Err(_) => false,
        };
    }

    /// True when this is `Err`.
    pub fn is_err() -> bool {
        return match self {
            Ok(_) => false,
            Err(_) => true,
        };
    }

    /// The `Ok` value as an `Option`.
    pub fn ok() -> Option<T> {
        return match self {
            Ok(v) => Some(v),
            Err(_) => None,
        };
    }

    /// The `Err` value as an `Option`.
    pub fn err() -> Option<E> {
        return match self {
            Ok(_) => None,
            Err(e) => Some(e),
        };
    }

    /// The `Ok` value, or `fallback` on `Err`.
    pub fn unwrap_or(T fallback) -> T {
        return match self {
            Ok(v) => v,
            Err(_) => fallback,
        };
    }

    /// The `Ok` value, or `f` applied to the error.
    pub fn unwrap_or_else(E -> T f) -> T {
        return match self {
            Ok(v) => v,
            Err(e) => f(e),
        };
    }

    /// `Ok(f(value))`, or the same error.
    pub fn map<U>(T -> U f) -> Result<U, E> {
        return match self {
            Ok(v) => Ok(f(v)),
            Err(e) => Err(e),
        };
    }

    /// The same value, or `Err(f(error))`.
    pub fn map_err<F>(E -> F f) -> Result<T, F> {
        return match self {
            Ok(v) => Ok(v),
            Err(e) => Err(f(e)),
        };
    }

    /// `f(value)` (itself a `Result`), or the same error.
    pub fn and_then<U>(T -> Result<U, E> f) -> Result<U, E> {
        return match self {
            Ok(v) => f(v),
            Err(e) => Err(e),
        };
    }

    /// This result if it is `Ok`, otherwise `f(error)`.
    pub fn or_else<F>(E -> Result<T, F> f) -> Result<T, F> {
        return match self {
            Ok(v) => Ok(v),
            Err(e) => f(e),
        };
    }
}
