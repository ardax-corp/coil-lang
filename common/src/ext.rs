//! File extensions: `.coil` sources and `.ccoil` compiled archives.
//!
//! Sources were `.hy` and archives `.hyc` before the rename. The legacy
//! source extension is still read (module lookup prefers `.coil` and falls
//! back to `.hy`, and tools that walk directories pick up both) so packages
//! that have not renamed their files keep working during the transition.
//! Nothing writes the legacy extensions.

use std::path::Path;

/// Source file extension, without the dot.
pub const SOURCE_EXT: &str = "coil";

/// Compiled archive extension (`coil compile`, the build cache), without the dot.
pub const ARCHIVE_EXT: &str = "ccoil";

/// Pre-rename source extension, still read during the transition.
pub const LEGACY_SOURCE_EXT: &str = "hy";

/// Pre-rename archive extension. Only recognised, never written.
pub const LEGACY_ARCHIVE_EXT: &str = "hyc";

/// Source extensions in lookup order: a `.coil` file wins over a `.hy` one.
pub const SOURCE_EXTS: [&str; 2] = [SOURCE_EXT, LEGACY_SOURCE_EXT];

/// `path` names a source file (`.coil`, or legacy `.hy`).
pub fn is_source_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SOURCE_EXTS.contains(&e))
}

/// `path` names a compiled archive (`.ccoil`, or legacy `.hyc`).
pub fn is_archive_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e == ARCHIVE_EXT || e == LEGACY_ARCHIVE_EXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_paths_take_both_extensions() {
        assert!(is_source_path(Path::new("src/main.coil")));
        assert!(is_source_path(Path::new("src/main.hy")));
        assert!(!is_source_path(Path::new("out.ccoil")));
        assert!(!is_source_path(Path::new("src/main.coil.proof")));
        assert!(!is_source_path(Path::new("coil")));
    }

    #[test]
    fn archive_paths_take_both_extensions() {
        assert!(is_archive_path(Path::new("out.ccoil")));
        assert!(is_archive_path(Path::new("out.hyc")));
        assert!(!is_archive_path(Path::new("main.coil")));
        assert!(!is_archive_path(Path::new("main.hy")));
    }
}
