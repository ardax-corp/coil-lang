//! DAP JSON message types and Content-Length framing.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub seq: i64,
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

impl Message {
    /// Built by the protocol unit test `request_roundtrip_preserves_arguments`.
    /// The adapter answers requests; it does not send them.
    #[allow(dead_code)]
    pub fn request(seq: i64, command: &str, arguments: Option<Value>) -> Self {
        Self {
            seq,
            msg_type: "request".into(),
            command: Some(command.into()),
            event: None,
            request_seq: None,
            success: None,
            message: None,
            arguments,
            body: None,
        }
    }

    pub fn response(seq: i64, request_seq: i64, command: &str, body: Value) -> Self {
        Self {
            seq,
            msg_type: "response".into(),
            command: Some(command.into()),
            event: None,
            request_seq: Some(request_seq),
            success: Some(true),
            message: None,
            arguments: None,
            body: Some(body),
        }
    }

    pub fn error_response(seq: i64, request_seq: i64, command: &str, message: &str) -> Self {
        Self {
            seq,
            msg_type: "response".into(),
            command: Some(command.into()),
            event: None,
            request_seq: Some(request_seq),
            success: Some(false),
            message: Some(message.into()),
            arguments: None,
            body: None,
        }
    }

    pub fn event(seq: i64, event: &str, body: Option<Value>) -> Self {
        Self {
            seq,
            msg_type: "event".into(),
            command: None,
            event: Some(event.into()),
            request_seq: None,
            success: None,
            message: None,
            arguments: None,
            body,
        }
    }
}

pub fn read_message<R: Read>(reader: &mut R) -> io::Result<Option<Message>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        loop {
            let mut buf = [0u8; 1];
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                    return if content_length.is_none() {
                        Ok(None)
                    } else {
                        Err(e)
                    };
                }
                Err(e) => return Err(e),
            }
            if buf[0] == b'\n' {
                break;
            }
            if buf[0] != b'\r' {
                line.push(buf[0] as char);
            }
        }
        if line.is_empty() {
            break;
        }
        if let Some((key, val)) = line.split_once(':')
            && key.trim() == "Content-Length"
        {
            content_length = Some(val.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length")
            })?);
        }
    }
    let Some(len) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let msg: Message = serde_json::from_slice(&body)?;
    Ok(Some(msg))
}

pub fn write_message<W: Write>(writer: &mut W, msg: &Message) -> io::Result<()> {
    let body = serde_json::to_vec(msg)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_framing() {
        let msg = Message::event(1, "initialized", None);
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded.event.as_deref(), Some("initialized"));
    }

    #[test]
    fn empty_reader_yields_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        assert!(read_message(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn invalid_content_length_errors() {
        let raw = b"Content-Length: nope\r\n\r\n";
        let mut cursor = std::io::Cursor::new(&raw[..]);
        let err = read_message(&mut cursor).expect_err("bad length");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_body_errors() {
        let raw = b"Content-Length: 50\r\n\r\n{\"seq\":1}";
        let mut cursor = std::io::Cursor::new(&raw[..]);
        let err = read_message(&mut cursor).expect_err("truncated");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn error_response_shape() {
        let msg = Message::error_response(3, 2, "launch", "compile failed");
        assert_eq!(msg.success, Some(false));
        assert_eq!(msg.message.as_deref(), Some("compile failed"));
        assert_eq!(msg.request_seq, Some(2));
        assert_eq!(msg.command.as_deref(), Some("launch"));
    }

    #[test]
    fn request_roundtrip_preserves_arguments() {
        let msg = Message::request(
            1,
            "setBreakpoints",
            Some(serde_json::json!({ "breakpoints": [{ "line": 3 }] })),
        );
        let mut buf = Vec::new();
        write_message(&mut buf, &msg).unwrap();
        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_message(&mut cursor).unwrap().unwrap();
        assert_eq!(decoded.msg_type, "request");
        assert_eq!(decoded.command.as_deref(), Some("setBreakpoints"));
        let line = decoded
            .arguments
            .as_ref()
            .and_then(|a| a.get("breakpoints"))
            .and_then(|b| b.as_array())
            .and_then(|arr| arr.first())
            .and_then(|bp| bp.get("line"))
            .and_then(|l| l.as_u64());
        assert_eq!(line, Some(3));
    }
}
