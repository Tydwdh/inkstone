//! The protocol's framing: a length, a blank line, and that many bytes.
//!
//! ```text
//! Content-Length: 42\r\n
//! \r\n
//! {"jsonrpc":"2.0",...}
//! ```
//!
//! Two headers are allowed by the specification and one is used. A header that
//! is not one of them is skipped, because a server is allowed to send more
//! information than a client knows what to do with, and refusing the message
//! over it would be the wrong kind of strict.

use std::io::{BufRead, Write};

use serde_json::Value;
use thiserror::Error;

/// Nothing a server can say is this long: a guard against a corrupt or hostile
/// length prefix.
pub const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

/// What went wrong reading or writing one message.
#[derive(Debug, Error)]
pub enum FramingError {
    /// The stream ended in the middle of a message.
    #[error("the language server closed the connection")]
    Ended,
    /// The stream is not UTF-8, or not JSON.
    #[error("the language server said something that is not JSON: {0}")]
    NotJson(String),
    /// A header this client cannot honour.
    #[error("unusable header: {0}")]
    Header(String),
    /// The message was larger than [`MAX_MESSAGE_LEN`].
    #[error("the language server sent {0} bytes, which is more than this client reads")]
    TooLong(usize),
    /// The stream itself failed.
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// Write one message, headers and all.
pub fn write_message(out: &mut impl Write, value: &Value) -> Result<(), FramingError> {
    let body = serde_json::to_vec(value).map_err(|e| FramingError::NotJson(e.to_string()))?;
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(&body)?;
    Ok(())
}

/// Read one message, or `None` when the stream has ended cleanly.
pub fn read_message(input: &mut impl BufRead) -> Result<Option<Value>, FramingError> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = input.read_line(&mut line)?;
        if read == 0 {
            return if length.is_some() {
                // The headers were there and the body never came.
                Err(FramingError::Ended)
            } else {
                Ok(None)
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            // The blank line: the body starts here.
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            return Err(FramingError::Header(header.to_string()));
        };
        if name.eq_ignore_ascii_case("content-length") {
            let value = value.trim();
            let parsed = value
                .parse::<usize>()
                .map_err(|_| FramingError::Header(header.to_string()))?;
            if parsed > MAX_MESSAGE_LEN {
                return Err(FramingError::TooLong(parsed));
            }
            length = Some(parsed);
        }
        // Anything else — `Content-Type` — is allowed and skipped.
    }
    let Some(length) = length else {
        return Err(FramingError::Header(
            "a message with no Content-Length".to_string(),
        ));
    };
    let mut body = vec![0u8; length];
    std::io::Read::read_exact(input, &mut body)?;
    let text = String::from_utf8(body)
        .map_err(|_| FramingError::NotJson("bytes that are not UTF-8".to_string()))?;
    let value = serde_json::from_str(&text).map_err(|e| FramingError::NotJson(e.to_string()))?;
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn a_message_goes_out_with_its_length_and_comes_back_the_same() {
        let value = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        let mut out = Vec::new();
        write_message(&mut out, &value).unwrap();

        let text = String::from_utf8(out.clone()).unwrap();
        let body = serde_json::to_string(&value).unwrap();
        assert!(text.starts_with(&format!("Content-Length: {}\r\n\r\n", body.len())));
        assert!(text.ends_with(&body));

        let mut input = Cursor::new(out);
        assert_eq!(read_message(&mut input).unwrap(), Some(value));
        assert_eq!(read_message(&mut input).unwrap(), None, "and then the end");
    }

    #[test]
    fn two_messages_in_one_stream_are_two_messages() {
        let one = json!({"method": "a"});
        let two = json!({"method": "b"});
        let mut out = Vec::new();
        write_message(&mut out, &one).unwrap();
        write_message(&mut out, &two).unwrap();
        let mut input = Cursor::new(out);
        assert_eq!(read_message(&mut input).unwrap(), Some(one));
        assert_eq!(read_message(&mut input).unwrap(), Some(two));
    }

    #[test]
    fn a_header_this_client_does_not_know_is_skipped() {
        let body = r#"{"method":"x"}"#;
        let text = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}",
            body.len(),
            body
        );
        let mut input = Cursor::new(text.into_bytes());
        assert_eq!(
            read_message(&mut input).unwrap(),
            Some(json!({"method": "x"}))
        );
    }

    #[test]
    fn what_is_not_a_message_is_an_error_and_not_a_panic() {
        // No length at all.
        let mut input = Cursor::new(b"Content-Type: text/plain\r\n\r\n{}".to_vec());
        assert!(matches!(
            read_message(&mut input),
            Err(FramingError::Header(_))
        ));

        // A length that is not a number.
        let mut input = Cursor::new(b"Content-Length: many\r\n\r\n{}".to_vec());
        assert!(matches!(
            read_message(&mut input),
            Err(FramingError::Header(_))
        ));

        // A length nobody could mean.
        let mut input = Cursor::new(b"Content-Length: 99999999999\r\n\r\n".to_vec());
        assert!(matches!(
            read_message(&mut input),
            Err(FramingError::Header(_)) | Err(FramingError::TooLong(_))
        ));

        // A body that is not JSON.
        let mut input = Cursor::new(b"Content-Length: 4\r\n\r\nnope".to_vec());
        assert!(matches!(
            read_message(&mut input),
            Err(FramingError::NotJson(_))
        ));

        // Headers, and then the stream ends.
        let mut input = Cursor::new(b"Content-Length: 10\r\n\r\n".to_vec());
        assert!(matches!(read_message(&mut input), Err(FramingError::Io(_))));
    }
}
