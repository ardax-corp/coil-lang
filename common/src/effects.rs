//! Effect bits shared by the compiler's purity analysis and the host-native
//! table ([`crate::HOST_NATIVES`] carries one [`EffectFlags`] per row).

/// Observable effects that kill purity. Empty flags are pure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectFlags(u16);

impl EffectFlags {
    /// Reads the clock or other nondeterministic host state (not IO).
    pub const HOST: u16 = 1 << 0;
    pub const FFI: u16 = 1 << 1;
    pub const HEAP_MUT: u16 = 1 << 2;
    /// Generator `yield`.
    pub const YIELD: u16 = 1 << 3;
    pub const THREAD: u16 = 1 << 4;
    pub const GC: u16 = 1 << 5;
    /// Reads a stream or the file system.
    pub const READ: u16 = 1 << 6;
    pub const ATTACH_PARK: u16 = 1 << 7;
    pub const UNKNOWN: u16 = 1 << 8;
    /// May change the length (or buffer) of an array it can reach: `Vec`
    /// grow/shrink methods, unknown or indirect code, yield, FFI. Always set
    /// alongside another impure bit, so purity is unchanged.
    pub const RESIZE: u16 = 1 << 9;
    /// Writes a stream or the file system.
    pub const WRITE: u16 = 1 << 10;
    /// Opens or uses a socket.
    pub const NET: u16 = 1 << 11;
    /// Reads or changes the process environment (args, vars, cwd).
    pub const ENV: u16 = 1 << 12;
    /// Runs another program or ends this one.
    pub const EXEC: u16 = 1 << 13;
    /// May park the caller until IO is ready (`wait_readable`, `wait_ready`).
    pub const SUSPEND: u16 = 1 << 14;
    /// Returns a fresh mutable object each call (byte and string
    /// conversions): not pure, so LICM and CSE keep every call, but no
    /// effect a user sees or declares.
    pub const ALLOC: u16 = 1 << 15;

    /// Any IO: a mask, so `contains(IO)` is true for any of its bits.
    pub const IO: u16 = Self::READ | Self::WRITE | Self::NET;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }

    /// Effects a userland lock might cover for shared-heap steal (F3).
    ///
    /// IO / FFI / heap mutation / mutex. Yield, GC, clocks, and unknown
    /// (`panic`) are not lockable edges.
    pub const fn is_lockable_escape(self) -> bool {
        self.contains(Self::IO)
            || self.contains(Self::FFI)
            || self.contains(Self::HEAP_MUT)
            || self.contains(Self::THREAD)
    }

    pub const fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn insert(&mut self, bit: u16) {
        self.0 |= bit;
    }
}
