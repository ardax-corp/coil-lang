//! Diagnostic reporting for coil.
//!
//! Owns producer [`Message`]s (with stable [`ErrorCode`]s) and sink-facing
//! [`Diagnostic`]s. Render via [`DiagnosticSink`]: pretty ariadne,
//! SARIF 2.1 (`--log-json`), or LSP Diagnostic NDJSON (`--log-lsp`).

mod ariadne_sink;
mod codes;
mod config;
mod collector;
mod convert;
mod diagnostic;
mod lsp_sink;
mod message;
mod position;
pub mod progress;
mod sarif_sink;
mod sink;
mod source;

pub use codes::ErrorCode;
pub use collector::DiagnosticCollector;
pub use config::{ReportConfig, ReportFormat};
pub use diagnostic::{Diagnostic, Location, RelatedLabel, Severity};
pub use message::{Label, Message, MessageKind};
pub use position::{LspPosition, LspRange, byte_offset_to_lsp_position, byte_range_to_lsp_range};
pub use sink::{DiagnosticSink, create_sink, emit_all};
pub use source::{SourceId, SourceMap};
