//! Capabilities a program must be granted (`--allow-read`, …) to acquire a
//! resource. A host native that needs one says so in [`crate::HOST_NATIVES`].

/// A set of capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Caps(u8);

impl Caps {
    pub const NONE: Caps = Caps(0);
    /// Open a file for reading, inspect the file system.
    pub const READ: Caps = Caps(1);
    /// Open a file for writing, create, remove or rename files.
    pub const WRITE: Caps = Caps(2);
    /// Connect, listen or bind a socket.
    pub const NET: Caps = Caps(4);
    /// Read or change environment variables or the working directory.
    pub const ENV: Caps = Caps(8);
    /// `env::exec`.
    pub const EXEC: Caps = Caps(16);
    /// `env::exit`.
    pub const EXIT: Caps = Caps(32);
    /// `Stream.attach`.
    pub const ATTACH: Caps = Caps(64);

    /// Every capability, in the order messages list them, with its name
    /// (the flag is `--allow-<name>`).
    pub const ALL: [(Caps, &'static str); 7] = [
        (Caps::READ, "read"),
        (Caps::WRITE, "write"),
        (Caps::NET, "net"),
        (Caps::ENV, "env"),
        (Caps::EXEC, "exec"),
        (Caps::EXIT, "exit"),
        (Caps::ATTACH, "attach"),
    ];

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn union(self, other: Caps) -> Caps {
        Caps(self.0 | other.0)
    }

    pub const fn without(self, other: Caps) -> Caps {
        Caps(self.0 & !other.0)
    }

    pub const fn contains(self, other: Caps) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The capability names in `self`, as `--allow-<name>` takes them.
    pub fn names(self) -> Vec<&'static str> {
        Self::ALL
            .iter()
            .filter(|(c, _)| self.contains(*c))
            .map(|(_, n)| *n)
            .collect()
    }

    /// The capability named `name` (`read`, `net`, …).
    pub fn named(name: &str) -> Option<Caps> {
        Self::ALL.iter().find(|(_, n)| *n == name).map(|(c, _)| *c)
    }

    /// The flags that grant `self`: `--allow-read --allow-net`.
    pub fn flags(self) -> String {
        self.names()
            .iter()
            .map(|n| format!("--allow-{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What `open(path, mode)` needs for the literal `mode`: reading, writing
/// or both. A mode the compiler cannot see needs both.
pub fn open_mode_caps(mode: Option<&str>) -> Caps {
    let Some(mode) = mode else {
        return Caps::READ.union(Caps::WRITE);
    };
    let mut caps = Caps::NONE;
    if mode.contains('r') || mode.contains('+') {
        caps = caps.union(Caps::READ);
    }
    if mode.contains(['w', 'a', 'x', '+']) {
        caps = caps.union(Caps::WRITE);
    }
    if caps.is_empty() {
        Caps::READ.union(Caps::WRITE)
    } else {
        caps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_modes() {
        assert_eq!(open_mode_caps(Some("r")), Caps::READ);
        assert_eq!(open_mode_caps(Some("w")), Caps::WRITE);
        assert_eq!(open_mode_caps(Some("a")), Caps::WRITE);
        assert_eq!(open_mode_caps(Some("r+")), Caps::READ.union(Caps::WRITE));
        assert_eq!(open_mode_caps(None), Caps::READ.union(Caps::WRITE));
    }

    #[test]
    fn names_and_flags() {
        let c = Caps::NET.union(Caps::READ);
        assert_eq!(c.names(), vec!["read", "net"]);
        assert_eq!(c.flags(), "--allow-read --allow-net");
        assert_eq!(Caps::named("env"), Some(Caps::ENV));
    }
}
