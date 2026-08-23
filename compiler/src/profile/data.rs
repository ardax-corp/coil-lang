//! Versioned PGO profile (COI-132 / COI-180).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Bump when the JSON/binary schema changes incompatibly. Loaders refuse a
/// newer version; older JSON (v1) is still accepted with empty `fn_checksums`.
pub const PROFILE_VERSION: u32 = 2;

/// Runtime counts keyed by function name, block id, and branch site id.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileData {
    pub version: u32,
    pub function_counts: BTreeMap<String, u64>,
    pub block_counts: BTreeMap<u32, u64>,
    /// `(taken, not_taken)` for a conditional jump site id.
    pub branch_counts: BTreeMap<u32, (u64, u64)>,
    /// Unix seconds when the profile was created or last written.
    #[serde(default)]
    pub timestamp: u64,
    /// Cleanup mid-IR shape fingerprints; missing entries skip matching.
    #[serde(default)]
    pub fn_checksums: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    Version { found: u32, expected: u32 },
    Parse(String),
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version { found, expected } => {
                write!(f, "profile version {found} != {expected}")
            }
            Self::Parse(msg) => write!(f, "profile parse error: {msg}"),
            Self::Io(msg) => write!(f, "profile io error: {msg}"),
        }
    }
}

impl ProfileData {
    pub fn new() -> Self {
        Self {
            version: PROFILE_VERSION,
            timestamp: unix_secs(),
            ..Self::default()
        }
    }

    pub fn hit_function(&mut self, name: &str) {
        *self.function_counts.entry(name.to_string()).or_insert(0) += 1;
    }

    pub fn hit_block(&mut self, id: u32) {
        *self.block_counts.entry(id).or_insert(0) += 1;
    }

    pub fn hit_branch(&mut self, id: u32, taken: bool) {
        let e = self.branch_counts.entry(id).or_insert((0, 0));
        if taken {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }

    pub fn function_is_hot(&self, name: &str) -> bool {
        match self.function_counts.get(name) {
            Some(&n) if n > 0 => {
                let max = self.function_counts.values().copied().max().unwrap_or(0);
                n * 2 >= max && n >= 8
            }
            _ => false,
        }
    }

    pub fn function_is_cold(&self, name: &str) -> bool {
        if self.function_counts.is_empty() {
            return false;
        }
        self.function_counts.get(name).copied().unwrap_or(0) == 0
    }

    pub fn to_json(&self) -> String {
        let mut fns = String::new();
        for (i, (k, v)) in self.function_counts.iter().enumerate() {
            if i > 0 {
                fns.push(',');
            }
            let _ = write!(fns, "\"{}\":{}", json_escape(k), v);
        }
        let mut blocks = String::new();
        for (i, (k, v)) in self.block_counts.iter().enumerate() {
            if i > 0 {
                blocks.push(',');
            }
            let _ = write!(blocks, "\"{k}\":{v}");
        }
        let mut branches = String::new();
        for (i, (k, (t, n))) in self.branch_counts.iter().enumerate() {
            if i > 0 {
                branches.push(',');
            }
            let _ = write!(branches, "\"{k}\":[{t},{n}]");
        }
        let mut checksums = String::new();
        for (i, (k, v)) in self.fn_checksums.iter().enumerate() {
            if i > 0 {
                checksums.push(',');
            }
            let _ = write!(checksums, "\"{}\":{}", json_escape(k), v);
        }
        format!(
            "{{\"version\":{},\"function_counts\":{{{fns}}},\"block_counts\":{{{blocks}}},\"branch_counts\":{{{branches}}},\"timestamp\":{},\"fn_checksums\":{{{checksums}}}}}",
            self.version, self.timestamp
        )
    }

    pub fn from_json(s: &str) -> Result<Self, LoadError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(LoadError::Parse("empty profile".into()));
        }
        let version = json_u32_field(s, "version").unwrap_or(0);
        if version == 0 || version > PROFILE_VERSION {
            return Err(LoadError::Version {
                found: version,
                expected: PROFILE_VERSION,
            });
        }
        let mut data = ProfileData {
            version: PROFILE_VERSION,
            ..Self::default()
        };
        if let Some(obj) = json_object_field(s, "function_counts") {
            parse_string_u64_map(obj, &mut data.function_counts)?;
        }
        if let Some(obj) = json_object_field(s, "block_counts") {
            parse_u32_u64_map(obj, &mut data.block_counts)?;
        }
        if let Some(obj) = json_object_field(s, "branch_counts") {
            parse_branch_map(obj, &mut data.branch_counts)?;
        }
        data.timestamp = json_u64_field(s, "timestamp").unwrap_or(0);
        if let Some(obj) = json_object_field(s, "fn_checksums") {
            parse_string_u64_map(obj, &mut data.fn_checksums)?;
        }
        Ok(data)
    }

    /// bincode blob (COI-180).
    pub fn to_binary(&self) -> Result<Vec<u8>, LoadError> {
        bincode::serialize(self).map_err(|e| LoadError::Parse(e.to_string()))
    }

    pub fn from_binary(bytes: &[u8]) -> Result<Self, LoadError> {
        let data: Self =
            bincode::deserialize(bytes).map_err(|e| LoadError::Parse(e.to_string()))?;
        if data.version == 0 || data.version > PROFILE_VERSION {
            return Err(LoadError::Version {
                found: data.version,
                expected: PROFILE_VERSION,
            });
        }
        Ok(data)
    }

    fn io_err(path: &Path, err: std::io::Error) -> LoadError {
        LoadError::Io(format!("{}: {err}", path.display()))
    }

    pub fn to_json_file(&self, path: &Path) -> Result<(), LoadError> {
        std::fs::write(path, self.to_json()).map_err(|e| Self::io_err(path, e))
    }

    pub fn from_json_file(path: &Path) -> Result<Self, LoadError> {
        let s = std::fs::read_to_string(path).map_err(|e| Self::io_err(path, e))?;
        Self::from_json(&s)
    }

    pub fn to_binary_file(&self, path: &Path) -> Result<(), LoadError> {
        let bytes = self.to_binary()?;
        std::fs::write(path, bytes).map_err(|e| Self::io_err(path, e))
    }

    pub fn from_binary_file(path: &Path) -> Result<Self, LoadError> {
        let bytes = std::fs::read(path).map_err(|e| Self::io_err(path, e))?;
        Self::from_binary(&bytes)
    }
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn json_u64_field(s: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let rest = s[i + pat.len()..].trim_start();
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok()
}

fn json_u32_field(s: &str, key: &str) -> Option<u32> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let rest = s[i + pat.len()..].trim_start();
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse().ok()
}

fn json_object_field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let i = s.find(&pat)?;
    let rest = s[i + pat.len()..].trim_start();
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0i32;
    for (j, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=j]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_string_u64_map(obj: &str, out: &mut BTreeMap<String, u64>) -> Result<(), LoadError> {
    let inner = obj.trim().trim_start_matches('{').trim_end_matches('}');
    if inner.trim().is_empty() {
        return Ok(());
    }
    for part in split_top_commas(inner) {
        let (k, v) = split_kv(part)?;
        let key = k.trim().trim_matches('"').to_string();
        let n = v.trim().parse::<u64>().map_err(|e| LoadError::Parse(e.to_string()))?;
        out.insert(key, n);
    }
    Ok(())
}

fn parse_u32_u64_map(obj: &str, out: &mut BTreeMap<u32, u64>) -> Result<(), LoadError> {
    let inner = obj.trim().trim_start_matches('{').trim_end_matches('}');
    if inner.trim().is_empty() {
        return Ok(());
    }
    for part in split_top_commas(inner) {
        let (k, v) = split_kv(part)?;
        let key = k.trim().trim_matches('"').parse::<u32>().map_err(|e| LoadError::Parse(e.to_string()))?;
        let n = v.trim().parse::<u64>().map_err(|e| LoadError::Parse(e.to_string()))?;
        out.insert(key, n);
    }
    Ok(())
}

fn parse_branch_map(obj: &str, out: &mut BTreeMap<u32, (u64, u64)>) -> Result<(), LoadError> {
    let inner = obj.trim().trim_start_matches('{').trim_end_matches('}');
    if inner.trim().is_empty() {
        return Ok(());
    }
    for part in split_top_commas(inner) {
        let (k, v) = split_kv(part)?;
        let key = k.trim().trim_matches('"').parse::<u32>().map_err(|e| LoadError::Parse(e.to_string()))?;
        let v = v.trim();
        let v = v.trim_start_matches('[').trim_end_matches(']');
        let mut it = v.split(',');
        let t = it
            .next()
            .ok_or_else(|| LoadError::Parse("branch pair".into()))?
            .trim()
            .parse::<u64>()
            .map_err(|e| LoadError::Parse(e.to_string()))?;
        let n = it
            .next()
            .ok_or_else(|| LoadError::Parse("branch pair".into()))?
            .trim()
            .parse::<u64>()
            .map_err(|e| LoadError::Parse(e.to_string()))?;
        out.insert(key, (t, n));
    }
    Ok(())
}

fn split_kv(part: &str) -> Result<(&str, &str), LoadError> {
    let i = part.find(':').ok_or_else(|| LoadError::Parse("missing :".into()))?;
    Ok((&part[..i], &part[i + 1..]))
}

fn split_top_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = s[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}
