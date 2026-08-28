//! Self-contained executable packaging (append `.hyc` archive + trailer).

use crate::opcode::{Byte, Instruction};

/// Magic at the end of a packaged `coil` binary.
pub const PACKAGE_MAGIC: &[u8; 8] = b"COILAPP\0";

/// Trailer size in bytes (little-endian fields).
pub const PACKAGE_TRAILER_SIZE: usize = 32;

/// [`PackageTrailer::flags`]: program bytecode uses dynamic FFI (`dload` / `extern`).
pub const PACKAGE_FLAG_USES_FFI: u32 = 1;

/// [`PackageTrailer::flags`]: a [`NativeLock`] blob sits between the `.hyc` archive and the trailer.
pub const PACKAGE_FLAG_HAS_NATIVE_LOCK: u32 = 2;

/// Metadata stored in the last 32 bytes of a packaged executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageTrailer {
    pub archive_offset: u64,
    pub archive_len: u64,
    pub flags: u32,
    pub archive_version: u32,
}

impl PackageTrailer {
    pub fn uses_ffi(self) -> bool {
        self.flags & PACKAGE_FLAG_USES_FFI != 0
    }

    pub fn has_native_lock(self) -> bool {
        self.flags & PACKAGE_FLAG_HAS_NATIVE_LOCK != 0
    }

    pub fn encode(self) -> [u8; PACKAGE_TRAILER_SIZE] {
        let mut buf = [0u8; PACKAGE_TRAILER_SIZE];
        buf[..8].copy_from_slice(PACKAGE_MAGIC);
        buf[8..16].copy_from_slice(&self.archive_offset.to_le_bytes());
        buf[16..24].copy_from_slice(&self.archive_len.to_le_bytes());
        buf[24..28].copy_from_slice(&self.flags.to_le_bytes());
        buf[28..32].copy_from_slice(&self.archive_version.to_le_bytes());
        buf
    }

    pub fn decode(trailer: &[u8; PACKAGE_TRAILER_SIZE]) -> Option<Self> {
        if &trailer[..8] != PACKAGE_MAGIC {
            return None;
        }
        Some(Self {
            archive_offset: u64::from_le_bytes(trailer[8..16].try_into().ok()?),
            archive_len: u64::from_le_bytes(trailer[16..24].try_into().ok()?),
            flags: u32::from_le_bytes(trailer[24..28].try_into().ok()?),
            archive_version: u32::from_le_bytes(trailer[28..32].try_into().ok()?),
        })
    }
}

/// One direct shared-library artifact declared for packaging / `spool download`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLockEntry {
    pub package: String,
    pub version: String,
    pub stem: String,
    pub filename: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub requires: Vec<String>,
    pub requires_hint: String,
}

/// Host triple + direct native artifacts embedded beside the `.hyc` archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLock {
    pub os: String,
    pub arch: String,
    pub entries: Vec<NativeLockEntry>,
}

impl NativeLock {
    /// Serialize as JSON for embedding and `coil natives dump`.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n");
        out.push_str(&format!("  \"os\": {},\n", json_string(&self.os)));
        out.push_str(&format!("  \"arch\": {},\n", json_string(&self.arch)));
        out.push_str("  \"entries\": [\n");
        for (i, e) in self.entries.iter().enumerate() {
            out.push_str("    {\n");
            out.push_str(&format!("      \"package\": {},\n", json_string(&e.package)));
            out.push_str(&format!("      \"version\": {},\n", json_string(&e.version)));
            out.push_str(&format!("      \"stem\": {},\n", json_string(&e.stem)));
            out.push_str(&format!("      \"filename\": {},\n", json_string(&e.filename)));
            out.push_str(&format!("      \"url\": {},\n", json_string(&e.url)));
            out.push_str(&format!("      \"sha256\": {},\n", json_string(&e.sha256)));
            out.push_str(&format!("      \"size\": {},\n", e.size));
            out.push_str("      \"requires\": [");
            for (j, r) in e.requires.iter().enumerate() {
                if j > 0 {
                    out.push_str(", ");
                }
                out.push_str(&json_string(r));
            }
            out.push_str("],\n");
            out.push_str(&format!(
                "      \"requires_hint\": {}\n",
                json_string(&e.requires_hint)
            ));
            out.push_str("    }");
            if i + 1 < self.entries.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push_str("  ]\n}\n");
        out
    }

    /// Parse JSON produced by [`Self::to_json`] (tolerant of whitespace).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v = Json::parse(text)?;
        let obj = v.as_object()?;
        let os = json_obj_str(obj, "os")?;
        let arch = json_obj_str(obj, "arch")?;
        let entries_v = json_obj_get(obj, "entries").ok_or("missing entries")?;
        let arr = entries_v.as_array()?;
        let mut entries = Vec::new();
        for item in arr {
            let e = item.as_object()?;
            let requires = match json_obj_get(e, "requires") {
                Some(r) => r
                    .as_array()?
                    .iter()
                    .map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Result<Vec<_>, _>>()?,
                None => Vec::new(),
            };
            entries.push(NativeLockEntry {
                package: json_obj_str(e, "package")?,
                version: json_obj_str(e, "version")?,
                stem: json_obj_str(e, "stem")?,
                filename: json_obj_str(e, "filename")?,
                url: json_obj_str(e, "url")?,
                sha256: json_obj_str(e, "sha256")?,
                size: json_obj_u64(e, "size")?,
                requires,
                requires_hint: json_obj_str(e, "requires_hint").unwrap_or_default(),
            });
        }
        Ok(Self { os, arch, entries })
    }

    /// TSV lines for bash `spool download`: package, version, filename, url, sha256, size.
    /// Prefixed with `# os=…` / `# arch=…` comments for host checks.
    pub fn to_fetch_tsv(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# os={}\n", self.os));
        out.push_str(&format!("# arch={}\n", self.arch));
        for e in &self.entries {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                e.package, e.version, e.filename, e.url, e.sha256, e.size
            ));
        }
        out
    }

    /// Cache directory for one entry under `natives_root`.
    pub fn entry_cache_dir(natives_root: &std::path::Path, entry: &NativeLockEntry) -> std::path::PathBuf {
        let hash16: String = entry.sha256.chars().take(16).collect();
        natives_root
            .join("cache")
            .join(&entry.package)
            .join(&entry.version)
            .join(hash16)
    }

    /// Full path to the cached artifact for `entry`.
    pub fn entry_cache_path(natives_root: &std::path::Path, entry: &NativeLockEntry) -> std::path::PathBuf {
        Self::entry_cache_dir(natives_root, entry).join(&entry.filename)
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Minimal JSON value for [`NativeLock::from_json`].
#[derive(Debug, Clone)]
enum Json {
    Null,
    #[allow(dead_code)]
    Bool(bool),
    Number(u64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn parse(input: &str) -> Result<Self, String> {
        let mut p = JsonParser {
            bytes: input.as_bytes(),
            i: 0,
        };
        let v = p.parse_value()?;
        p.skip_ws();
        if p.i != p.bytes.len() {
            return Err("trailing junk after JSON".into());
        }
        Ok(v)
    }

    fn as_object(&self) -> Result<&[(String, Json)], String> {
        match self {
            Json::Object(o) => Ok(o),
            _ => Err("expected object".into()),
        }
    }

    fn as_array(&self) -> Result<&[Json], String> {
        match self {
            Json::Array(a) => Ok(a),
            _ => Err("expected array".into()),
        }
    }

    fn as_str(&self) -> Result<&str, String> {
        match self {
            Json::String(s) => Ok(s),
            _ => Err("expected string".into()),
        }
    }
}

fn json_obj_get<'a>(obj: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn json_obj_str(obj: &[(String, Json)], key: &str) -> Result<String, String> {
    json_obj_get(obj, key)
        .ok_or_else(|| format!("missing {key}"))?
        .as_str()
        .map(|s| s.to_string())
}

fn json_obj_u64(obj: &[(String, Json)], key: &str) -> Result<u64, String> {
    match json_obj_get(obj, key).ok_or_else(|| format!("missing {key}"))? {
        Json::Number(n) => Ok(*n),
        _ => Err(format!("expected number for {key}")),
    }
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    i: usize,
}

impl<'a> JsonParser<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.bytes.len() && self.bytes[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.i).copied()
    }

    fn bump(&mut self) -> Result<u8, String> {
        let b = self.peek().ok_or("unexpected end of JSON")?;
        self.i += 1;
        Ok(b)
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek().ok_or("unexpected end of JSON")? {
            b'n' => self.parse_null(),
            b't' | b'f' => self.parse_bool(),
            b'"' => Ok(Json::String(self.parse_string()?)),
            b'[' => self.parse_array(),
            b'{' => self.parse_object(),
            b'0'..=b'9' => self.parse_number(),
            other => Err(format!("unexpected JSON byte {}", other as char)),
        }
    }

    fn parse_null(&mut self) -> Result<Json, String> {
        for c in b"null" {
            if self.bump()? != *c {
                return Err("invalid null".into());
            }
        }
        Ok(Json::Null)
    }

    fn parse_bool(&mut self) -> Result<Json, String> {
        if self.peek() == Some(b't') {
            for c in b"true" {
                if self.bump()? != *c {
                    return Err("invalid true".into());
                }
            }
            Ok(Json::Bool(true))
        } else {
            for c in b"false" {
                if self.bump()? != *c {
                    return Err("invalid false".into());
                }
            }
            Ok(Json::Bool(false))
        }
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.i += 1;
        }
        let s = std::str::from_utf8(&self.bytes[start..self.i]).map_err(|_| "bad number utf8")?;
        let n: u64 = s.parse().map_err(|_| format!("bad number {s}"))?;
        Ok(Json::Number(n))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        if self.bump()? != b'"' {
            return Err("expected string".into());
        }
        let mut out = String::new();
        loop {
            match self.bump()? {
                b'"' => return Ok(out),
                b'\\' => match self.bump()? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let mut hex = String::new();
                        for _ in 0..4 {
                            hex.push(self.bump()? as char);
                        }
                        let cp = u32::from_str_radix(&hex, 16).map_err(|_| "bad \\u escape")?;
                        out.push(char::from_u32(cp).ok_or("invalid unicode escape")?);
                    }
                    other => return Err(format!("bad escape {}", other as char)),
                },
                c => out.push(c as char),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.bump()?; // [
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.bump()?;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump()? {
                b']' => return Ok(Json::Array(items)),
                b',' => {
                    self.skip_ws();
                    continue;
                }
                other => return Err(format!("expected , or ] in array, got {}", other as char)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.bump()?; // {
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b'}') {
            self.bump()?;
            return Ok(Json::Object(items));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bump()? != b':' {
                return Err("expected : in object".into());
            }
            let val = self.parse_value()?;
            items.push((key, val));
            self.skip_ws();
            match self.bump()? {
                b'}' => return Ok(Json::Object(items)),
                b',' => continue,
                other => return Err(format!("expected , or }} in object, got {}", other as char)),
            }
        }
    }
}

/// Read trailer from the end of `data`, if present.
pub fn read_package_trailer(data: &[u8]) -> Option<PackageTrailer> {
    if data.len() < PACKAGE_TRAILER_SIZE {
        return None;
    }
    let start = data.len() - PACKAGE_TRAILER_SIZE;
    let trailer: &[u8; PACKAGE_TRAILER_SIZE] = data[start..].try_into().ok()?;
    PackageTrailer::decode(trailer)
}

/// Slice of embedded archive bytes inside a packaged executable.
pub fn embedded_archive_slice(data: &[u8], trailer: PackageTrailer) -> Option<&[u8]> {
    let off = usize::try_from(trailer.archive_offset).ok()?;
    let len = usize::try_from(trailer.archive_len).ok()?;
    let end = off.checked_add(len)?;
    if end > data.len().saturating_sub(PACKAGE_TRAILER_SIZE) {
        return None;
    }
    if !trailer.has_native_lock() && end + PACKAGE_TRAILER_SIZE != data.len() {
        return None;
    }
    if trailer.has_native_lock() && end + PACKAGE_TRAILER_SIZE > data.len() {
        return None;
    }
    data.get(off..end)
}

/// Slice of the embedded native lock JSON, if the trailer flag is set.
pub fn embedded_native_lock_slice(data: &[u8], trailer: PackageTrailer) -> Option<&[u8]> {
    if !trailer.has_native_lock() {
        return None;
    }
    let off = usize::try_from(trailer.archive_offset).ok()?;
    let len = usize::try_from(trailer.archive_len).ok()?;
    let lock_start = off.checked_add(len)?;
    let lock_end = data.len().checked_sub(PACKAGE_TRAILER_SIZE)?;
    if lock_start > lock_end {
        return None;
    }
    data.get(lock_start..lock_end)
}

/// Parse the embedded [`NativeLock`], if present.
pub fn read_embedded_native_lock(data: &[u8], trailer: PackageTrailer) -> Result<Option<NativeLock>, String> {
    let Some(slice) = embedded_native_lock_slice(data, trailer) else {
        return Ok(None);
    };
    if slice.is_empty() {
        return Ok(Some(NativeLock {
            os: String::new(),
            arch: String::new(),
            entries: Vec::new(),
        }));
    }
    let text = std::str::from_utf8(slice).map_err(|_| "native lock is not UTF-8".to_string())?;
    NativeLock::from_json(text).map(Some)
}

/// Whether `data` already ends with a package trailer (template is already packaged).
pub fn is_packaged_executable(data: &[u8]) -> bool {
    read_package_trailer(data).is_some()
}

/// rkyv `access` requires the blob pointer to meet `ArchivedArchivedProgram`
/// alignment. PE/ELF runner sizes are often only 4- or 8-byte aligned.
const PACKAGE_ARCHIVE_ALIGN: usize = 16;

/// Append `archive`, optional `native_lock` JSON, and trailer to `runner_bytes`.
///
/// Zero-pads so the overlay starts at a 16-byte offset (rkyv alignment).
pub fn append_package_payload(
    runner_bytes: &[u8],
    archive: &[u8],
    flags: u32,
    archive_version: u32,
) -> Vec<u8> {
    append_package_payload_with_natives(runner_bytes, archive, None, flags, archive_version)
}

/// Like [`append_package_payload`], optionally embedding a native lock blob.
pub fn append_package_payload_with_natives(
    runner_bytes: &[u8],
    archive: &[u8],
    native_lock: Option<&[u8]>,
    flags: u32,
    archive_version: u32,
) -> Vec<u8> {
    let pad = (PACKAGE_ARCHIVE_ALIGN - (runner_bytes.len() % PACKAGE_ARCHIVE_ALIGN))
        % PACKAGE_ARCHIVE_ALIGN;
    let offset = u64::try_from(runner_bytes.len() + pad).expect("runner too large");
    let len = u64::try_from(archive.len()).expect("archive too large");
    let mut out = runner_bytes.to_vec();
    out.resize(out.len() + pad, 0);
    out.extend_from_slice(archive);
    let mut flags = flags;
    if let Some(lock) = native_lock {
        if !lock.is_empty() {
            out.extend_from_slice(lock);
            flags |= PACKAGE_FLAG_HAS_NATIVE_LOCK;
        }
    }
    let trailer = PackageTrailer {
        archive_offset: offset,
        archive_len: len,
        flags,
        archive_version,
    };
    out.extend_from_slice(&trailer.encode());
    out
}

/// True when `name` refers to the portable C library (never bundled / fetched).
pub fn is_system_ffi_stem(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "c" | "libc" | "libc.so.6" | "libsystem" | "libsystem.b.dylib" | "ucrtbase" | "msvcrt"
    ) || {
        let mut stem = lower.clone();
        if let Some(idx) = stem.find(".so.") {
            stem.truncate(idx);
        } else if let Some(stripped) = stem.strip_suffix(".so") {
            stem = stripped.to_string();
        } else if let Some(stripped) = stem.strip_suffix(".dylib") {
            stem = stripped.to_string();
        } else if let Some(stripped) = stem.strip_suffix(".dll") {
            stem = stripped.to_string();
        }
        if let Some(stripped) = stem.strip_prefix("lib") {
            if !stripped.is_empty() {
                stem = stripped.to_string();
            }
        }
        matches!(
            stem.as_str(),
            "c" | "system" | "system.b" | "ucrtbase" | "msvcrt"
        )
    }
}

fn is_ffi_opcode(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::FfiLoad
            | Instruction::FfiInvoke
            | Instruction::DeclareFFI
            | Instruction::NATIVE
    )
}

/// True when bytecode may call into shared libraries at runtime.
pub fn bytecode_uses_ffi(bytecode: &[Byte]) -> bool {
    bytecode.iter().any(|b| is_ffi_opcode(*b.bytecode()))
}

/// Decode a `STRING` table index at `i`. Returns `(text, index_after)`.
fn decode_string_literal(
    bytecode: &[Byte],
    strings: &[String],
    i: usize,
) -> Option<(String, usize)> {
    let b = bytecode.get(i)?;
    if *b.bytecode() != Instruction::STRING {
        return None;
    }
    let idx = b.operand_u32() as usize;
    let text = strings.get(idx)?.clone();
    Some((text, i + 1))
}

/// Library names passed to `FfiLoad` (`STRING` immediately before each `FfiLoad`).
pub fn ffi_library_names_from_bytecode(bytecode: &[Byte], strings: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    let mut i = 0;
    while i < bytecode.len() {
        if let Some((name, end)) = decode_string_literal(bytecode, strings, i) {
            if bytecode
                .get(end)
                .is_some_and(|b| *b.bytecode() == Instruction::FfiLoad)
                && !names.iter().any(|n| n == &name)
            {
                names.push(name);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    names
}

/// Default natives cache root (`$COIL_NATIVES_DIR` or `~/.coil/natives`).
pub fn default_natives_root() -> std::path::PathBuf {
    if let Ok(p) = std::env::var("COIL_NATIVES_DIR") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".coil").join("natives")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::Byte;

    #[test]
    fn trailer_round_trip() {
        let t = PackageTrailer {
            archive_offset: 1_234,
            archive_len: 56_789,
            flags: PACKAGE_FLAG_USES_FFI | PACKAGE_FLAG_HAS_NATIVE_LOCK,
            archive_version: 26,
        };
        let enc = t.encode();
        assert_eq!(PackageTrailer::decode(&enc), Some(t));
        assert!(t.has_native_lock());
    }

    #[test]
    fn append_and_read_embedded_archive() {
        let runner = b"#!/bin/fake\nELF...";
        let archive = b"rkyv-bytes-here";
        let out = append_package_payload(runner, archive, 0, 26);
        let trailer = read_package_trailer(&out).expect("trailer");
        assert_eq!(trailer.archive_offset % PACKAGE_ARCHIVE_ALIGN as u64, 0);
        assert!(trailer.archive_offset as usize >= runner.len());
        assert_eq!(trailer.archive_len, archive.len() as u64);
        assert_eq!(
            embedded_archive_slice(&out, trailer),
            Some(archive.as_slice())
        );
    }

    #[test]
    fn append_with_native_lock_round_trip() {
        let runner = b"ELF";
        let archive = b"ARCHIVE";
        let lock = NativeLock {
            os: "linux".into(),
            arch: "x86_64".into(),
            entries: vec![NativeLockEntry {
                package: "regex".into(),
                version: "0.3.0".into(),
                stem: "regex".into(),
                filename: "libregex.so".into(),
                url: "https://example.com/libregex.so".into(),
                sha256: "abcd".into(),
                size: 12,
                requires: vec!["libpcre2-8.so.0".into()],
                requires_hint: "pacman -S pcre2".into(),
            }],
        };
        let json = lock.to_json();
        let out = append_package_payload_with_natives(
            runner,
            archive,
            Some(json.as_bytes()),
            PACKAGE_FLAG_USES_FFI,
            26,
        );
        let trailer = read_package_trailer(&out).expect("trailer");
        assert!(trailer.has_native_lock());
        assert_eq!(embedded_archive_slice(&out, trailer), Some(archive.as_slice()));
        let got = read_embedded_native_lock(&out, trailer)
            .expect("parse")
            .expect("present");
        assert_eq!(got, lock);
    }

    #[test]
    fn native_lock_json_round_trip() {
        let lock = NativeLock {
            os: "linux".into(),
            arch: "x86_64".into(),
            entries: vec![],
        };
        let again = NativeLock::from_json(&lock.to_json()).unwrap();
        assert_eq!(again, lock);
    }

    #[test]
    fn append_pads_unaligned_runner_to_16() {
        let runner = [0u8; 1];
        let archive = b"blob";
        let out = append_package_payload(&runner, archive, 0, 26);
        let trailer = read_package_trailer(&out).expect("trailer");
        assert_eq!(trailer.archive_offset, PACKAGE_ARCHIVE_ALIGN as u64);
        assert_eq!(
            embedded_archive_slice(&out, trailer),
            Some(archive.as_slice())
        );
    }

    #[test]
    fn detects_ffi_opcodes() {
        let bc = vec![
            Byte::new(Instruction::HALT),
            Byte::new(Instruction::FfiLoad),
        ];
        assert!(bytecode_uses_ffi(&bc));
        assert!(!bytecode_uses_ffi(&[Byte::new(Instruction::HALT)]));
    }

    #[test]
    fn extracts_dload_library_name_before_ffi_load() {
        let strings = vec!["sum".to_string()];
        let bc = vec![
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::FfiLoad),
        ];
        assert_eq!(
            ffi_library_names_from_bytecode(&bc, &strings),
            vec!["sum".to_string()]
        );
    }

    #[test]
    fn system_ffi_stems() {
        assert!(is_system_ffi_stem("c"));
        assert!(is_system_ffi_stem("libc.so.6"));
        assert!(!is_system_ffi_stem("regex"));
        assert!(!is_system_ffi_stem("tls"));
    }
}
