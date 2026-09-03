//! Untagged runtime values: immediates and heap pointers in one word.

/// Runtime type tag used by the generics boxing opcodes (`BoxValue` / `UnboxValue`).
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ValueTag {
    Int = 0,
    Float = 1,
    Bool = 2,
    String = 3,
    Enum = 4,
    Instance = 5,
    Tuple = 6,
    Array = 7,
    Record = 8,
    Coroutine = 9,
    Ptr = 10,
    Unit = 11,
    PolyFn = 12,
}

impl ValueTag {
    /// Convert a raw `u16` to `ValueTag`; returns `None` for unknown tags.
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0 => Some(Self::Int),
            1 => Some(Self::Float),
            2 => Some(Self::Bool),
            3 => Some(Self::String),
            4 => Some(Self::Enum),
            5 => Some(Self::Instance),
            6 => Some(Self::Tuple),
            7 => Some(Self::Array),
            8 => Some(Self::Record),
            9 => Some(Self::Coroutine),
            10 => Some(Self::Ptr),
            11 => Some(Self::Unit),
            12 => Some(Self::PolyFn),
            _ => None,
        }
    }
}

type Storage = u64;

#[derive(Default, Copy, Clone, Eq)]
pub struct Value(*mut u8);

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::new(value as _)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self::new(value as _)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::new(value.to_bits() as _)
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self::new(value as u8 as _)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::new(value as _)
    }
}

impl<T> From<*mut T> for Value {
    fn from(value: *mut T) -> Self {
        Self::new(value as _)
    }
}

impl<'a> Value {
    const fn new(raw: Storage) -> Self {
        Self(raw as _)
    }

    pub const fn replace(&mut self, value: Storage) {
        self.0 = value as _;
    }
}

impl<'a> Value {
    /// ```
    /// use common::Value;
    /// assert_eq!(Value::from(42).as_int(), 42);
    /// ```
    #[inline]
    pub fn as_int(&self) -> i64 {
        self.0 as _
    }

    /// ```
    /// use common::Value;
    /// assert_eq!(Value::from(true).as_bool(), true);
    /// ```
    #[inline]
    pub fn as_bool(&self) -> bool {
        self.0 as u8 == 1
    }

    /// ```
    /// use common::Value;
    /// assert_eq!(1.2, Value::from(1.2).as_float());
    /// ```
    #[inline]
    pub fn as_float(&self) -> f64 {
        f64::from_bits(self.0 as _)
    }

    #[inline]
    pub fn as_ptr<T>(&self) -> *mut T {
        self.raw() as _
        // NonNull::without_provenance(
        //     NonZero::new(self.raw() as _).expect("Invalid pointer address"),
        // )
    }

    /// ```
    /// use common::Value;
    /// assert_eq!(Value::from(42).raw(), 42 as _);
    /// ```
    #[inline]
    pub fn raw(&self) -> *mut u8 {
        self.0
        // (self.0 as usize >> 3) as _
    }

    /// Heap address with the Result `Err` low bit cleared (`pointer | 1`).
    ///
    /// `Ok` is an aligned object pointer; `Err` sets bit 0. GC and root
    /// marking must look up the aligned address. Immediates and `Option`
    /// (`None` = 0) are unchanged: `0 & !1 == 0`.
    #[inline]
    pub fn heap_addr(&self) -> u64 {
        (self.0 as u64) & !1
    }

    pub fn inc_int(&mut self) -> &Self {
        self.replace((self.as_int() + 1) as _);
        self
    }

    pub fn dec_int(&mut self) -> &Self {
        self.replace((self.as_int() - 1) as _);
        self
    }

    pub fn inc_float(&mut self) -> &Self {
        let v = self.as_float() + 1.0;
        self.replace(v.to_bits() as _);
        self
    }

    pub fn dec_float(&mut self) -> &Self {
        let v = self.as_float() - 1.0;
        self.replace(v.to_bits() as _);
        self
    }
}

#[cfg(debug_assertions)]
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0 as Storage,)
    }
}

#[cfg(debug_assertions)]
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0 as Storage,)
    }
}

#[cfg(test)]
mod tests {
    use crate::Value;

    const MIN_FLOAT: f64 = f64::MIN;
    const MAX_FLOAT: f64 = f64::MAX;

    const MIN_INT: i64 = i64::MIN;
    const MAX_INT: i64 = i64::MAX;

    #[test]
    fn ptr_tagging() {
        assert_eq!(Value::from(0).as_int(), 0);
        assert_eq!(Value::from(0).heap_addr(), 0);
        let tagged = Value::from(0x100u64 | 1);
        assert_eq!(tagged.heap_addr(), 0x100);
        assert_eq!(Value::from(0x100u64).heap_addr(), 0x100);
        assert_eq!(Value::from(0.0).as_float(), 0.0);

        assert_eq!(Value::from(MIN_INT).as_int(), MIN_INT);
        assert_eq!(Value::from(MIN_FLOAT).as_float(), MIN_FLOAT);

        assert_eq!(Value::from(MAX_INT).as_int(), MAX_INT);
        assert_eq!(Value::from(MAX_FLOAT).as_float(), MAX_FLOAT);

        assert_eq!(Value::from(false).as_int(), 0);
        assert_eq!(Value::from(true).as_int(), 1);

        assert_eq!(Value::from(32).as_int(), 32);
        assert_eq!(Value::default().as_int(), 0);
        assert_eq!(Value::from(1.2).as_float(), 1.2);

        assert_eq!(Value::default().raw(), std::ptr::null_mut());
        assert_eq!(Value::from(13).raw(), 13 as _);
        assert_eq!(Value::from(1.2).raw(), (1.2_f64).to_bits() as _);
    }
}
