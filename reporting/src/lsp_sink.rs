//! LSP Diagnostic-shaped NDJSON sink (no language server).

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::diagnostic::{Diagnostic, Severity};
use crate::position::byte_offset_to_lsp_position;
use crate::sink::DiagnosticSink;
use crate::source::{SourceId, SourceMap};

/// Buffers diagnostics and writes LSP Diagnostic NDJSON on `finish`.
///
/// Position conversion uses UTF-8 byte offsets mapped to line/UTF-16
/// character columns. For BMP (ASCII + common Unicode) this matches LSP;
/// supplementary-plane code points may under-count UTF-16 length.
pub struct LspSink {
    sources: SourceMap,
    writer: Box<dyn Write + Send>,
    diags: Vec<LspDiagnostic>,
    error_count: usize,
    finished: bool,
}

impl LspSink {
    pub fn new(sources: SourceMap, writer: Box<dyn Write + Send>) -> Self {
        Self {
            sources,
            writer,
            diags: Vec::new(),
            error_count: 0,
            finished: false,
        }
    }

    fn path_uri(&self, file: SourceId) -> String {
        self.sources
            .path(file)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "untitled:coil".to_string())
    }

    fn byte_to_position(text: &str, byte: usize) -> LspPosition {
        let position = byte_offset_to_lsp_position(text, byte);
        LspPosition {
            line: position.line,
            character: position.character,
        }
    }

    fn range_for(&self, file: SourceId, range: &std::ops::Range<usize>) -> LspRange {
        let text = self.sources.text(file).unwrap_or("");
        LspRange {
            start: Self::byte_to_position(text, range.start),
            end: Self::byte_to_position(text, range.end),
        }
    }
}

impl DiagnosticSink for LspSink {
    fn emit(&mut self, diag: Diagnostic) {
        if diag.is_error() {
            self.error_count += 1;
        }

        let severity = match diag.severity {
            Severity::Error => 1u32,
            Severity::Warning => 2,
            Severity::Info | Severity::Note => 3,
        };

        let (uri, range) = if let Some(loc) = &diag.location {
            (
                Some(self.path_uri(loc.file)),
                self.range_for(loc.file, &loc.range),
            )
        } else {
            (
                Some("untitled:coil".to_string()),
                LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            )
        };

        let related_information: Vec<LspRelatedInformation> = diag
            .labels
            .iter()
            .map(|label| LspRelatedInformation {
                location: LspLocation {
                    uri: self.path_uri(label.location.file),
                    range: self.range_for(label.location.file, &label.location.range),
                },
                message: label.message.clone(),
            })
            .collect();

        self.diags.push(LspDiagnostic {
            uri,
            range,
            severity,
            code: diag.code.map(|c| LspCode::String(c.as_str().to_string())),
            source: Some("coil".to_string()),
            message: diag.message,
            related_information: if related_information.is_empty() {
                None
            } else {
                Some(related_information)
            },
            data: diag.help.map(|h| LspData { help: h }),
        });
    }

    fn register_source(&mut self, path: &Path, text: &str) -> SourceId {
        self.sources.insert(path, text)
    }

    fn finish(&mut self) -> std::io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        for d in std::mem::take(&mut self.diags) {
            serde_json::to_writer(&mut self.writer, &d)
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            writeln!(self.writer)?;
        }
        self.writer.flush()
    }

    fn had_errors(&self) -> bool {
        self.error_count > 0
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    range: LspRange,
    severity: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<LspCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    related_information: Option<Vec<LspRelatedInformation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<LspData>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum LspCode {
    String(String),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspRange {
    start: LspPosition,
    end: LspPosition,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspPosition {
    line: u32,
    character: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspRelatedInformation {
    location: LspLocation,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspLocation {
    uri: String,
    range: LspRange,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LspData {
    help: String,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;

    use super::*;
    use crate::codes::ErrorCode;
    use crate::message::{Label as MsgLabel, Message};

    #[derive(Clone, Default)]
    struct SharedBuf {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuf {
        fn new() -> Self {
            Self::default()
        }
        fn into_string(self) -> String {
            String::from_utf8_lossy(&self.inner.lock().unwrap()).into_owned()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn lsp_ndjson_spanned_and_spanless() {
        let mut sources = SourceMap::new();
        let file = sources.insert("test.coil", "let x = 1;\n");
        let shared = SharedBuf::new();
        let mut sink = LspSink::new(sources, Box::new(shared.clone()));

        let mut msg = Message::error(ErrorCode::TypeMismatch, "Type mismatch".into(), 4..5);
        msg.with_help("expected int".into());
        msg.push(MsgLabel::new("here".into(), 8..9));
        sink.emit(Diagnostic::from_message(&msg, file));
        sink.emit(Diagnostic::error("missing file").with_code(ErrorCode::MissingInputFile));
        sink.finish().unwrap();

        let out = shared.into_string();
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);

        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["severity"], 1);
        assert_eq!(first["code"], "E0102");
        assert_eq!(first["source"], "coil");
        assert_eq!(first["range"]["start"]["character"], 4);
        assert_eq!(first["relatedInformation"][0]["message"], "here");
        assert_eq!(first["data"]["help"], "expected int");

        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["code"], "E0903");
        assert_eq!(second["uri"], "untitled:coil");
    }

    #[test]
    fn byte_to_position_tracks_newlines() {
        let text = "ab\ncd\n";
        let p = LspSink::byte_to_position(text, 4); // 'd'
        assert_eq!(p.line, 1);
        assert_eq!(p.character, 1);
    }
}
