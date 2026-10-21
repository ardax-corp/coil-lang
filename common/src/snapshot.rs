//! Heap snapshots: an object graph copied out of the compile-time VM.
//!
//! A `const` whose value is computed at compile time keeps that value as a
//! snapshot: the objects it reaches, in an order-free table, plus the static
//! slots that hold them. The snapshot is stored as words in the constant
//! pool and `RestoreSnapshot` rebuilds it once at startup, so shared and
//! cyclic references come back exactly as they were. Type ids are the
//! compiling program's own, so no remapping is needed.
//!
//! Encoding (all `u64`):
//!
//! ```text
//! MAGIC, total_words, n_objects, n_statics,
//! n_statics slot words, word list of the n_statics values,
//! n_objects objects
//! ```
//!
//! An object is a header word `kind | aux << 8 | extra << 32` and its body.
//! A word list is its length, its 2-bit tags (32 per word) and one payload
//! word per entry; a byte string is its length and its bytes packed
//! little-endian.

/// First word of every encoded snapshot.
pub const SNAPSHOT_MAGIC: u64 = 0x534e_4150_434f_494c; // "SNAPCOIL"

/// Words before the statics: magic, total length, object count, static count.
const HEADER_WORDS: usize = 4;

const OBJ_STRING: u64 = 0;
const OBJ_BYTES: u64 = 1;
const OBJ_ARRAY: u64 = 2;
const OBJ_TUPLE: u64 = 3;
const OBJ_ENUM: u64 = 4;
const OBJ_INSTANCE: u64 = 5;
const OBJ_BOXED: u64 = 6;

const TAG_RAW: u64 = 0;
const TAG_REF: u64 = 1;
const TAG_UNIT_ENUM: u64 = 2;
const TAG_EMPTY_TUPLE: u64 = 3;

/// One word of a snapshot object (or of a static slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapWord {
    /// A scalar, kept bit for bit (ints, floats, bools, `0`).
    Raw(u64),
    /// Object `index` of the snapshot. `err_bit` keeps a niche `Result`
    /// `Err` (pointer `| 1`).
    Ref { index: u32, err_bit: bool },
    /// The shared unit variant with this tag; `err_bit` as for `Ref`.
    UnitEnum { tag: u32, err_bit: bool },
    /// The shared `()`; `err_bit` as for `Ref`.
    EmptyTuple { err_bit: bool },
}

/// One heap object of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapObject {
    String(String),
    /// A packed byte array (`Vec<byte>` / `Vec<bool>`).
    Bytes(Vec<u8>),
    Array { elem_kind: u8, words: Vec<SnapWord> },
    Tuple { kinds: u8, words: Vec<SnapWord> },
    Enum { tag: u32, type_id: u32, kinds: u8, words: Vec<SnapWord> },
    /// A typed class instance's field slots.
    Instance { type_id: u32, words: Vec<SnapWord> },
    /// A generic-runtime box: its tag and payload.
    Boxed { tag: u16, word: SnapWord },
}

/// Objects plus the static slots that hold the roots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeapSnapshot {
    pub objects: Vec<SnapObject>,
    /// `(static slot, value)` in store order.
    pub statics: Vec<(u32, SnapWord)>,
}

/// One step from a root to the value that could not be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapStep {
    /// Array / `Vec` element.
    Element(usize),
    /// Tuple element.
    TupleElement(usize),
    /// Enum variant payload word.
    Payload(usize),
    /// Field slot `index` of an instance of class or record `type_id`.
    Field { type_id: u32, index: usize },
    /// The value inside a generic-runtime box.
    Boxed,
}

/// A value reachable from a root that has no snapshot form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotError {
    /// Index into the roots the snapshot was captured from.
    pub root: usize,
    /// Path from the root to the offending value.
    pub path: Vec<SnapStep>,
    /// What it is, as a phrase ("a function value").
    pub what: &'static str,
}

/// Why encoded words are not a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotDecodeError {
    BadMagic,
    Truncated,
    /// The header's length disagrees with what the body used.
    Length,
    BadObjectKind(u64),
    /// A reference past the object table.
    BadRef(u32),
    BadUtf8,
}

impl std::fmt::Display for SnapshotDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a heap snapshot"),
            Self::Truncated => write!(f, "heap snapshot is truncated"),
            Self::Length => write!(f, "heap snapshot length does not match its contents"),
            Self::BadObjectKind(k) => write!(f, "heap snapshot has unknown object kind {k}"),
            Self::BadRef(i) => write!(f, "heap snapshot refers to missing object {i}"),
            Self::BadUtf8 => write!(f, "heap snapshot string is not UTF-8"),
        }
    }
}

impl HeapSnapshot {
    /// Words for the constant pool.
    pub fn encode(&self) -> Vec<u64> {
        let mut out = vec![SNAPSHOT_MAGIC, 0, self.objects.len() as u64, self.statics.len() as u64];
        out.extend(self.statics.iter().map(|(slot, _)| u64::from(*slot)));
        let roots: Vec<SnapWord> = self.statics.iter().map(|(_, w)| *w).collect();
        encode_words(&mut out, &roots);
        for object in &self.objects {
            encode_object(&mut out, object);
        }
        out[1] = out.len() as u64;
        out
    }

    /// Total encoded length read from the header at `words[0..]`, for the
    /// bytecode verifier (which checks it fits the pool).
    pub fn encoded_len(words: &[u64]) -> Option<usize> {
        match words {
            [SNAPSHOT_MAGIC, len, ..] => usize::try_from(*len).ok(),
            _ => None,
        }
    }

    /// Decode and check a snapshot: every reference must name an object.
    pub fn decode(words: &[u64]) -> Result<Self, SnapshotDecodeError> {
        if words.first() != Some(&SNAPSHOT_MAGIC) {
            return Err(SnapshotDecodeError::BadMagic);
        }
        if words.len() < HEADER_WORDS {
            return Err(SnapshotDecodeError::Truncated);
        }
        let total = usize::try_from(words[1]).map_err(|_| SnapshotDecodeError::Length)?;
        let words = words.get(..total).ok_or(SnapshotDecodeError::Truncated)?;
        let n_objects = usize::try_from(words[2]).map_err(|_| SnapshotDecodeError::Length)?;
        let n_statics = usize::try_from(words[3]).map_err(|_| SnapshotDecodeError::Length)?;
        let mut r = Reader { words, at: HEADER_WORDS };
        let slots = r.take(n_statics)?.to_vec();
        let roots = r.words()?;
        if roots.len() != n_statics {
            return Err(SnapshotDecodeError::Length);
        }
        let statics = slots
            .iter()
            .zip(roots)
            .map(|(slot, w)| Ok((u32::try_from(*slot).map_err(|_| SnapshotDecodeError::Length)?, w)))
            .collect::<Result<Vec<_>, _>>()?;
        // Each object takes at least one word, so a forged count can't
        // make the table allocate more than the input.
        if n_objects > words.len() {
            return Err(SnapshotDecodeError::Length);
        }
        let mut objects = Vec::with_capacity(n_objects);
        for _ in 0..n_objects {
            objects.push(r.object()?);
        }
        if r.at != total {
            return Err(SnapshotDecodeError::Length);
        }
        let snapshot = Self { objects, statics };
        snapshot.check_refs()?;
        Ok(snapshot)
    }

    fn check_refs(&self) -> Result<(), SnapshotDecodeError> {
        let n = self.objects.len();
        let check = |w: &SnapWord| match w {
            SnapWord::Ref { index, .. } if *index as usize >= n => Err(SnapshotDecodeError::BadRef(*index)),
            _ => Ok(()),
        };
        self.statics.iter().try_for_each(|(_, w)| check(w))?;
        for object in &self.objects {
            match object {
                SnapObject::String(_) | SnapObject::Bytes(_) => {}
                SnapObject::Array { words, .. }
                | SnapObject::Tuple { words, .. }
                | SnapObject::Enum { words, .. }
                | SnapObject::Instance { words, .. } => words.iter().try_for_each(check)?,
                SnapObject::Boxed { word, .. } => check(word)?,
            }
        }
        Ok(())
    }
}

fn encode_object(out: &mut Vec<u64>, object: &SnapObject) {
    let header = |kind: u64, aux: u64, extra: u64| kind | aux << 8 | extra << 32;
    match object {
        SnapObject::String(s) => {
            out.push(header(OBJ_STRING, 0, 0));
            encode_bytes(out, s.as_bytes());
        }
        SnapObject::Bytes(b) => {
            out.push(header(OBJ_BYTES, 0, 0));
            encode_bytes(out, b);
        }
        SnapObject::Array { elem_kind, words } => {
            out.push(header(OBJ_ARRAY, u64::from(*elem_kind), 0));
            encode_words(out, words);
        }
        SnapObject::Tuple { kinds, words } => {
            out.push(header(OBJ_TUPLE, u64::from(*kinds), 0));
            encode_words(out, words);
        }
        SnapObject::Enum { tag, type_id, kinds, words } => {
            out.push(header(OBJ_ENUM, u64::from(*kinds), u64::from(*tag)));
            out.push(u64::from(*type_id));
            encode_words(out, words);
        }
        SnapObject::Instance { type_id, words } => {
            out.push(header(OBJ_INSTANCE, 0, u64::from(*type_id)));
            encode_words(out, words);
        }
        SnapObject::Boxed { tag, word } => {
            out.push(header(OBJ_BOXED, 0, u64::from(*tag)));
            encode_words(out, std::slice::from_ref(word));
        }
    }
}

fn encode_bytes(out: &mut Vec<u64>, bytes: &[u8]) {
    out.push(bytes.len() as u64);
    for chunk in bytes.chunks(8) {
        let mut word = [0u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        out.push(u64::from_le_bytes(word));
    }
}

fn encode_words(out: &mut Vec<u64>, words: &[SnapWord]) {
    out.push(words.len() as u64);
    for chunk in words.chunks(32) {
        let mut tags = 0u64;
        for (i, w) in chunk.iter().enumerate() {
            let tag = match w {
                SnapWord::Raw(_) => TAG_RAW,
                SnapWord::Ref { .. } => TAG_REF,
                SnapWord::UnitEnum { .. } => TAG_UNIT_ENUM,
                SnapWord::EmptyTuple { .. } => TAG_EMPTY_TUPLE,
            };
            tags |= tag << (2 * i);
        }
        out.push(tags);
    }
    out.extend(words.iter().map(|w| match *w {
        SnapWord::Raw(raw) => raw,
        SnapWord::Ref { index, err_bit } => u64::from(index) << 1 | u64::from(err_bit),
        SnapWord::UnitEnum { tag, err_bit } => u64::from(tag) << 1 | u64::from(err_bit),
        SnapWord::EmptyTuple { err_bit } => u64::from(err_bit),
    }));
}

struct Reader<'a> {
    words: &'a [u64],
    at: usize,
}

impl<'a> Reader<'a> {
    fn next(&mut self) -> Result<u64, SnapshotDecodeError> {
        let w = *self.words.get(self.at).ok_or(SnapshotDecodeError::Truncated)?;
        self.at += 1;
        Ok(w)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u64], SnapshotDecodeError> {
        let end = self.at.checked_add(n).ok_or(SnapshotDecodeError::Truncated)?;
        let out = self.words.get(self.at..end).ok_or(SnapshotDecodeError::Truncated)?;
        self.at = end;
        Ok(out)
    }

    fn len(&mut self) -> Result<usize, SnapshotDecodeError> {
        usize::try_from(self.next()?).map_err(|_| SnapshotDecodeError::Truncated)
    }

    fn bytes(&mut self) -> Result<Vec<u8>, SnapshotDecodeError> {
        let len = self.len()?;
        let packed = self.take(len.div_ceil(8))?;
        let mut out: Vec<u8> = packed.iter().flat_map(|w| w.to_le_bytes()).collect();
        out.truncate(len);
        Ok(out)
    }

    fn words(&mut self) -> Result<Vec<SnapWord>, SnapshotDecodeError> {
        let len = self.len()?;
        let tags = self.take(len.div_ceil(32))?;
        let payload = self.take(len)?;
        payload
            .iter()
            .enumerate()
            .map(|(i, &raw)| {
                Ok(match (tags[i / 32] >> (2 * (i % 32))) & 3 {
                    TAG_RAW => SnapWord::Raw(raw),
                    TAG_REF => SnapWord::Ref {
                        index: u32::try_from(raw >> 1).map_err(|_| SnapshotDecodeError::BadRef(u32::MAX))?,
                        err_bit: raw & 1 != 0,
                    },
                    TAG_UNIT_ENUM => SnapWord::UnitEnum {
                        tag: (raw >> 1) as u32,
                        err_bit: raw & 1 != 0,
                    },
                    _ => SnapWord::EmptyTuple { err_bit: raw & 1 != 0 },
                })
            })
            .collect()
    }

    fn object(&mut self) -> Result<SnapObject, SnapshotDecodeError> {
        let header = self.next()?;
        let aux = (header >> 8) as u8;
        let extra = (header >> 32) as u32;
        Ok(match header & 0xff {
            OBJ_STRING => SnapObject::String(
                String::from_utf8(self.bytes()?).map_err(|_| SnapshotDecodeError::BadUtf8)?,
            ),
            OBJ_BYTES => SnapObject::Bytes(self.bytes()?),
            OBJ_ARRAY => SnapObject::Array { elem_kind: aux, words: self.words()? },
            OBJ_TUPLE => SnapObject::Tuple { kinds: aux, words: self.words()? },
            OBJ_ENUM => {
                let type_id = self.next()? as u32;
                SnapObject::Enum { tag: extra, type_id, kinds: aux, words: self.words()? }
            }
            OBJ_INSTANCE => SnapObject::Instance { type_id: extra, words: self.words()? },
            OBJ_BOXED => {
                let words = self.words()?;
                let [word] = words[..] else {
                    return Err(SnapshotDecodeError::Length);
                };
                SnapObject::Boxed { tag: extra as u16, word }
            }
            kind => return Err(SnapshotDecodeError::BadObjectKind(kind)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HeapSnapshot {
        HeapSnapshot {
            objects: vec![
                SnapObject::String("héllo, snapshot".into()),
                SnapObject::Bytes(vec![1, 2, 3, 255, 0, 7, 8, 9, 10]),
                SnapObject::Array {
                    elem_kind: 2,
                    words: (0..40).map(|i| if i % 2 == 0 { SnapWord::Raw(i) } else { SnapWord::Ref { index: 0, err_bit: false } }).collect(),
                },
                SnapObject::Tuple { kinds: 0b1001, words: vec![SnapWord::Raw(u64::MAX), SnapWord::Ref { index: 3, err_bit: false }] },
                SnapObject::Enum { tag: 7, type_id: 42, kinds: 2, words: vec![SnapWord::Ref { index: 0, err_bit: true }] },
                SnapObject::Instance { type_id: 9, words: vec![SnapWord::UnitEnum { tag: 3, err_bit: true }, SnapWord::EmptyTuple { err_bit: false }, SnapWord::Ref { index: 5, err_bit: false }] },
                SnapObject::Boxed { tag: 4, word: SnapWord::Raw(f64::NAN.to_bits()) },
            ],
            statics: vec![(0, SnapWord::Ref { index: 5, err_bit: false }), (3, SnapWord::Raw(17))],
        }
    }

    #[test]
    fn round_trips_every_object_and_word_kind() {
        let snap = sample();
        let words = snap.encode();
        assert_eq!(HeapSnapshot::encoded_len(&words), Some(words.len()));
        assert_eq!(HeapSnapshot::decode(&words), Ok(snap));
    }

    /// Words after the snapshot (the rest of the pool) are not read.
    #[test]
    fn decode_stops_at_its_own_length() {
        let mut words = sample().encode();
        words.extend([1, 2, 3]);
        assert_eq!(HeapSnapshot::decode(&words), Ok(sample()));
    }

    #[test]
    fn rejects_damaged_input() {
        let words = sample().encode();
        assert_eq!(HeapSnapshot::decode(&words[1..]), Err(SnapshotDecodeError::BadMagic));
        assert_eq!(HeapSnapshot::decode(&words[..words.len() - 1]), Err(SnapshotDecodeError::Truncated));
        let dangling = HeapSnapshot {
            objects: vec![],
            statics: vec![(0, SnapWord::Ref { index: 1, err_bit: false })],
        };
        assert_eq!(HeapSnapshot::decode(&dangling.encode()), Err(SnapshotDecodeError::BadRef(1)));
        let mut forged = sample().encode();
        forged[2] = u64::MAX;
        assert!(HeapSnapshot::decode(&forged).is_err());
    }
}
