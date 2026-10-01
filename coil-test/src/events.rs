//! `--json`: NDJSON events on stdout, one object per line, flushed as they
//! happen so a reader can render progress live.

use std::fmt::Display;
use std::fmt::Write as _;
use std::io::Write as _;

use crate::coverage::json_str;

/// One event object, built field by field (`{"event":"…",…}`).
pub struct Event(String);

impl Event {
    pub fn new(kind: &str) -> Self {
        Event(String::from("{")).str("event", kind)
    }

    /// A plain object (nested in an event; no `event` key).
    pub fn object() -> Self {
        Event(String::from("{"))
    }

    /// A field whose value is already JSON text.
    pub fn raw(mut self, key: &str, json: &str) -> Self {
        if self.0.len() > 1 {
            self.0.push(',');
        }
        let _ = write!(self.0, "{}:{json}", json_str(key));
        self
    }

    pub fn str(self, key: &str, value: &str) -> Self {
        let v = json_str(value);
        self.raw(key, &v)
    }

    /// `null` for `None`.
    pub fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.str(key, v),
            None => self.raw(key, "null"),
        }
    }

    pub fn num(self, key: &str, value: impl Display) -> Self {
        self.raw(key, &value.to_string())
    }

    pub fn bool(self, key: &str, value: bool) -> Self {
        self.raw(key, if value { "true" } else { "false" })
    }

    /// The object as JSON text (no trailing newline).
    pub fn finish(mut self) -> String {
        self.0.push('}');
        self.0
    }

    /// Write the event as one line on stdout and flush it.
    pub fn emit(self) {
        let line = self.finish();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// `[a,b,…]` from JSON texts.
pub fn array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

/// A harness error as an event.
pub fn emit_error(message: &str) {
    Event::new("error").str("message", message).emit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_flat_objects() {
        let e = Event::new("file")
            .str("file", "tests/a \"b\".hy")
            .bool("ok", false)
            .num("n", 3)
            .opt_str("message", None)
            .raw(
                "cases",
                &array(&[Event::object().str("name", "t").finish()]),
            )
            .finish();
        assert_eq!(
            e,
            "{\"event\":\"file\",\"file\":\"tests/a \\\"b\\\".hy\",\"ok\":false,\"n\":3,\"message\":null,\"cases\":[{\"name\":\"t\"}]}"
        );
    }
}
