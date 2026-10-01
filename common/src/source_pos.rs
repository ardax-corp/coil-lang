//! Byte-offset → line/column (1-based line, 0-based column).

/// 1-based line, 0-based UTF-8 character column on that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub line: u32,
    pub column: u32,
}

/// Map a UTF-8 byte offset in `text` to a source position.
/// An offset inside a multi-byte character (stale debug info against an
/// edited file) rounds down to that character's start instead of panicking.
pub fn byte_to_position(text: &str, byte: usize) -> SourcePosition {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    let mut line: u32 = 0;
    let mut line_start = 0usize;
    for (idx, ch) in text.char_indices() {
        if idx >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = idx + ch.len_utf8();
        }
    }
    let column = text[line_start..byte].chars().count() as u32;
    SourcePosition {
        line: line + 1,
        column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_position_rounds_down_inside_a_char() {
        let text = "a\n·b";
        // Byte 3 is the second byte of `·` (bytes 2..4).
        let pos = byte_to_position(text, 3);
        assert_eq!((pos.line, pos.column), (2, 0));
    }

    #[test]
    fn byte_to_position_tracks_newlines() {
        let text = "fn main() {\n    panic \"x\";\n}\n";
        assert_eq!(byte_to_position(text, 0).line, 1);
        assert_eq!(byte_to_position(text, 13).line, 2);
    }
}
