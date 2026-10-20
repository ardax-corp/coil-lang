//! Hash the sources `coil` and its helpers are built from into one build id
//! (`COIL_BUILD_ID`), so `coil` can tell a helper built from other sources.
//!
//! In the workspace that is every member's `src/`, `Cargo.toml` and
//! `build.rs`, the root package's and `Cargo.lock`: not tests, benches or
//! docs, which change no binary. Outside it (a lone crate) only this crate.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().filter(|ws| members(ws).is_some());
    let mut inputs = Vec::new();
    match workspace {
        Some(ws) => {
            inputs.extend(["src", "Cargo.toml", "Cargo.lock", "build.rs"].map(|p| ws.join(p)));
            for member in members(ws).unwrap_or_default() {
                inputs.extend(["src", "Cargo.toml", "build.rs"].map(|p| ws.join(&member).join(p)));
            }
        }
        None => inputs.extend(["src", "Cargo.toml", "build.rs"].map(|p| manifest.join(p))),
    }
    let base = workspace.unwrap_or(&manifest);
    let mut files = Vec::new();
    for input in &inputs {
        if input.exists() {
            // A directory reruns this script when anything under it changes.
            println!("cargo:rerun-if-changed={}", input.display());
            collect(input, &mut files);
        }
    }
    files.sort();
    let mut hash = Fnv128::default();
    for file in &files {
        let rel = file.strip_prefix(base).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(file).unwrap_or_default();
        hash.write(rel.as_bytes());
        hash.write(&[0]);
        hash.write(&(bytes.len() as u64).to_le_bytes());
        hash.write(&bytes);
    }
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    println!("cargo:rustc-env=COIL_BUILD_ID={version}-{:032x}", hash.finish());
}

/// The `members` of `dir/Cargo.toml` when it declares a `[workspace]`.
fn members(dir: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let ws = &text[text.find("[workspace]")?..];
    let list = &ws[ws.find("members")?..];
    let list = &list[list.find('[')? + 1..list.find(']')?];
    Some(
        list.lines()
            .map(|line| line.split('#').next().unwrap_or(""))
            .flat_map(|line| line.split(','))
            .map(|item| item.trim().trim_matches('"').to_string())
            .filter(|item| !item.is_empty())
            .collect(),
    )
}

/// Every file at or under `path`, skipping hidden ones (editor swap files).
fn collect(path: &Path, out: &mut Vec<PathBuf>) {
    let hidden = path.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
    if hidden {
        return;
    }
    if path.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            collect(&entry.path(), out);
        }
    } else if path.is_file() {
        out.push(path.to_path_buf());
    }
}

/// FNV-1a over 128 bits: deterministic across hosts and toolchains, which
/// `std`'s hasher does not promise.
struct Fnv128(u128);

impl Default for Fnv128 {
    fn default() -> Self {
        Self(0x6c62272e07bb014262b821756295c58d)
    }
}

impl Fnv128 {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(0x0000000001000000000000000000013B);
        }
    }

    fn finish(&self) -> u128 {
        self.0
    }
}
