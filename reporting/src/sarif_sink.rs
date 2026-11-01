//! SARIF 2.1 diagnostic sink.

use std::io::Write;
use std::path::Path;

use serde::Serialize;

use crate::diagnostic::{Diagnostic, Severity};
use crate::sink::DiagnosticSink;
use crate::source::{SourceId, SourceMap};

/// Buffers diagnostics and writes one SARIF 2.1 log on [`finish`](DiagnosticSink::finish).
pub struct SarifSink {
    sources: SourceMap,
    writer: Box<dyn Write + Send>,
    results: Vec<SarifResult>,
    error_count: usize,
    finished: bool,
}

impl SarifSink {
    pub fn new(sources: SourceMap, writer: Box<dyn Write + Send>) -> Self {
        Self {
            sources,
            writer,
            results: Vec::new(),
            error_count: 0,
            finished: false,
        }
    }

    fn path_uri(&self, file: SourceId) -> String {
        self.sources
            .path(file)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| format!("unknown:{}", file.as_u32()))
    }

    fn location_to_sarif(&self, loc: &crate::diagnostic::Location) -> SarifLocation {
        SarifLocation {
            physical_location: SarifPhysicalLocation {
                artifact_location: SarifArtifactLocation {
                    uri: self.path_uri(loc.file),
                },
                region: Some(SarifRegion {
                    start_offset: Some(loc.range.start as u64),
                    end_offset: Some(loc.range.end as u64),
                    byte_offset: Some(loc.range.start as u64),
                    byte_length: Some(loc.range.end.saturating_sub(loc.range.start) as u64),
                }),
            },
        }
    }
}

impl DiagnosticSink for SarifSink {
    fn emit(&mut self, diag: Diagnostic) {
        if diag.is_error() {
            self.error_count += 1;
        }

        let level = match diag.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "note",
            Severity::Note => "note",
        };

        let locations = diag
            .location
            .as_ref()
            .map(|loc| vec![self.location_to_sarif(loc)])
            .unwrap_or_default();

        let related_locations: Vec<SarifRelatedLocation> = diag
            .labels
            .iter()
            .map(|label| SarifRelatedLocation {
                location: self.location_to_sarif(&label.location),
                message: Some(SarifMessage {
                    text: label.message.clone(),
                }),
            })
            .collect();

        let properties = diag
            .help
            .as_ref()
            .map(|help| SarifResultProperties { help: help.clone() });

        self.results.push(SarifResult {
            rule_id: diag.code.map(|c| c.as_str().to_string()),
            level: level.to_string(),
            message: SarifMessage { text: diag.message },
            locations,
            related_locations,
            properties,
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

        let artifacts: Vec<SarifArtifact> = self
            .sources
            .iter()
            .map(|(_, path, _)| SarifArtifact {
                location: SarifArtifactLocation {
                    uri: path.display().to_string(),
                },
            })
            .collect();

        let log = SarifLog {
            schema: "https://json.schemastore.org/sarif-2.1.0.json",
            version: "2.1.0",
            runs: vec![SarifRun {
                tool: SarifTool {
                    driver: SarifDriver {
                        name: "coil".to_string(),
                    },
                },
                artifacts,
                results: std::mem::take(&mut self.results),
            }],
        };

        serde_json::to_writer(&mut self.writer, &log)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        writeln!(self.writer)?;
        self.writer.flush()
    }

    fn had_errors(&self) -> bool {
        self.error_count > 0
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifLog {
    #[serde(rename = "$schema")]
    schema: &'static str,
    version: &'static str,
    runs: Vec<SarifRun>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRun {
    tool: SarifTool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<SarifArtifact>,
    results: Vec<SarifResult>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifTool {
    driver: SarifDriver,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifDriver {
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifArtifact {
    location: SarifArtifactLocation,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifArtifactLocation {
    uri: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_id: Option<String>,
    level: String,
    message: SarifMessage,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    locations: Vec<SarifLocation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    related_locations: Vec<SarifRelatedLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    properties: Option<SarifResultProperties>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifMessage {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifLocation {
    physical_location: SarifPhysicalLocation,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRelatedLocation {
    location: SarifLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<SarifMessage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifPhysicalLocation {
    artifact_location: SarifArtifactLocation,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<SarifRegion>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifRegion {
    #[serde(skip_serializing_if = "Option::is_none")]
    start_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_length: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SarifResultProperties {
    help: String,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;

    use super::*;
    use crate::codes::ErrorCode;
    use crate::config::ReportConfig;
    use crate::diagnostic::Diagnostic;
    use crate::message::{Label as MsgLabel, Message};
    use crate::sink::{DiagnosticSink, create_sink};

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
    fn sarif_spanned_message_shaped_diagnostic() {
        let mut sources = SourceMap::new();
        let file = sources.insert("test.coil", "let x = 1;\n");

        let mut msg = Message::error(ErrorCode::TypeMismatch, "Type mismatch".into(), 4..5);
        msg.with_help("expected int".into());
        msg.push(MsgLabel::new("here".into(), 8..9));
        let diag = Diagnostic::from_message(&msg, file);

        let shared = SharedBuf::new();
        let mut sink = SarifSink::new(sources, Box::new(shared.clone()));
        sink.emit(diag);
        sink.finish().unwrap();

        let v: Value = serde_json::from_str(&shared.into_string()).unwrap();
        assert_eq!(v["version"], "2.1.0");
        assert_eq!(v["runs"][0]["tool"]["driver"]["name"], "coil");
        let result = &v["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "E0102");
        assert_eq!(result["level"], "error");
        assert_eq!(result["message"]["text"], "Type mismatch");
        assert_eq!(result["properties"]["help"], "expected int");
    }

    #[test]
    fn sarif_spanless_cli_error() {
        let shared = SharedBuf::new();
        let mut sink = SarifSink::new(SourceMap::new(), Box::new(shared.clone()));
        sink.emit(
            Diagnostic::error("Bytecode archive version mismatch")
                .with_code(ErrorCode::ArchiveVersionMismatch),
        );
        assert!(sink.had_errors());
        sink.finish().unwrap();

        let v: Value = serde_json::from_str(&shared.into_string()).unwrap();
        let result = &v["runs"][0]["results"][0];
        assert_eq!(result["ruleId"], "E0901");
        assert!(
            result.get("locations").is_none()
                || result["locations"].as_array().is_some_and(|a| a.is_empty())
        );
    }

    #[test]
    fn create_sink_sarif_via_factory() {
        let config = ReportConfig::from_log_json_flag(true);
        let shared = SharedBuf::new();
        let mut sink = create_sink(&config, SourceMap::new(), Box::new(shared.clone()));
        sink.emit(Diagnostic::warning("unused binding"));
        sink.finish().unwrap();

        let v: Value = serde_json::from_str(&shared.into_string()).unwrap();
        assert_eq!(v["runs"][0]["results"][0]["level"], "warning");
        assert!(!sink.had_errors());
    }
}
