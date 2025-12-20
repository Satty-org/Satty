//! Protocol message types for daemon-client communication
//!
//! Messages are framed with a 4-byte little-endian length prefix followed by JSON payload.
//! Maximum message size is 16MB to support base64-encoded images via stdin.

use serde_derive::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use thiserror::Error;

/// Maximum message size (16MB for base64-encoded images)
pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Length prefix size in bytes
pub const LENGTH_PREFIX_SIZE: usize = 4;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Message too large: {0} bytes (max {MAX_MESSAGE_SIZE})")]
    MessageTooLarge(usize),

    #[error("Invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Missing required field: {0}")]
    MissingField(&'static str),

    #[error("Connection closed")]
    ConnectionClosed,
}

/// Request from client to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    /// Path to image file, or "-" for stdin data
    pub filename: String,

    /// Output filename for saving
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_filename: Option<String>,

    /// Command to use for copying to clipboard
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_command: Option<String>,

    /// Initial tool to select
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_tool: Option<String>,

    /// Start in fullscreen mode
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,

    /// Exit after first save/copy action
    #[serde(skip_serializing_if = "Option::is_none")]
    pub early_exit: Option<bool>,

    /// Corner roundness for shapes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corner_roundness: Option<f32>,

    /// Annotation size factor
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotation_size_factor: Option<f32>,

    /// Hide toolbars by default
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_hide_toolbars: Option<bool>,

    /// Disable window decoration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_window_decoration: Option<bool>,

    /// Base64-encoded image data for stdin mode
    /// Only used when filename is "-"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin_data: Option<String>,
}

impl DaemonRequest {
    /// Create a new request with only the required filename
    pub fn new(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            output_filename: None,
            copy_command: None,
            initial_tool: None,
            fullscreen: None,
            early_exit: None,
            corner_roundness: None,
            annotation_size_factor: None,
            default_hide_toolbars: None,
            no_window_decoration: None,
            stdin_data: None,
        }
    }

    /// Validate the request
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.filename.is_empty() {
            return Err(ProtocolError::MissingField("filename"));
        }

        // If filename is "-", stdin_data must be present
        if self.filename == "-" && self.stdin_data.is_none() {
            return Err(ProtocolError::MissingField(
                "stdin_data (required when filename is '-')",
            ));
        }

        Ok(())
    }

    /// Serialize to JSON bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge(json.len()));
        }
        Ok(json)
    }

    /// Deserialize from JSON bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        let request: Self = serde_json::from_slice(bytes)?;
        request.validate()?;
        Ok(request)
    }
}

/// Response status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// Request was successful
    Ok,
    /// An error occurred
    Error,
}

/// Response from daemon to client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    /// Status of the request
    pub status: ResponseStatus,

    /// Window ID (for successful requests)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,

    /// Error or informational message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DaemonResponse {
    /// Create a successful response
    pub fn ok(window_id: u64) -> Self {
        Self {
            status: ResponseStatus::Ok,
            window_id: Some(window_id),
            message: None,
        }
    }

    /// Create an error response
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: ResponseStatus::Error,
            window_id: None,
            message: Some(message.into()),
        }
    }

    /// Serialize to JSON bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        let json = serde_json::to_vec(self)?;
        Ok(json)
    }

    /// Deserialize from JSON bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

/// Write a length-prefixed message to a writer
pub fn write_message<W: Write>(writer: &mut W, data: &[u8]) -> Result<(), ProtocolError> {
    if data.len() > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge(data.len()));
    }

    let len = data.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

/// Read a length-prefixed message from a reader
pub fn read_message<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut len_buf = [0u8; LENGTH_PREFIX_SIZE];
    match reader.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::ConnectionClosed);
        }
        Err(e) => return Err(e.into()),
    }

    let len = u32::from_le_bytes(len_buf) as usize;

    if len > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge(len));
    }

    let mut data = vec![0u8; len];
    reader.read_exact(&mut data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_minimal() {
        let req = DaemonRequest::new("/tmp/test.png");
        assert_eq!(req.filename, "/tmp/test.png");
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_request_full() {
        let req = DaemonRequest {
            filename: "/tmp/test.png".into(),
            output_filename: Some("/tmp/output.png".into()),
            copy_command: Some("wl-copy".into()),
            initial_tool: Some("arrow".into()),
            fullscreen: Some(true),
            early_exit: Some(false),
            corner_roundness: Some(12.0),
            annotation_size_factor: Some(1.5),
            default_hide_toolbars: Some(false),
            no_window_decoration: Some(false),
            stdin_data: None,
        };
        assert!(req.validate().is_ok());

        let bytes = req.to_bytes().unwrap();
        let parsed = DaemonRequest::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.filename, req.filename);
        assert_eq!(parsed.output_filename, req.output_filename);
    }

    #[test]
    fn test_request_empty_filename() {
        let req = DaemonRequest::new("");
        assert!(matches!(
            req.validate(),
            Err(ProtocolError::MissingField("filename"))
        ));
    }

    #[test]
    fn test_request_stdin_without_data() {
        let req = DaemonRequest::new("-");
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_request_stdin_with_data() {
        use base64::Engine;
        let mut req = DaemonRequest::new("-");
        req.stdin_data = Some(base64::engine::general_purpose::STANDARD.encode(b"fake image data"));
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_response_ok() {
        let resp = DaemonResponse::ok(42);
        assert_eq!(resp.status, ResponseStatus::Ok);
        assert_eq!(resp.window_id, Some(42));

        let bytes = resp.to_bytes().unwrap();
        let parsed = DaemonResponse::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.status, ResponseStatus::Ok);
        assert_eq!(parsed.window_id, Some(42));
    }

    #[test]
    fn test_response_error() {
        let resp = DaemonResponse::error("File not found");
        assert_eq!(resp.status, ResponseStatus::Error);
        assert_eq!(resp.message, Some("File not found".into()));
    }

    #[test]
    fn test_message_framing() {
        let data = b"hello world";
        let mut buffer = Vec::new();
        write_message(&mut buffer, data).unwrap();

        let mut reader = std::io::Cursor::new(buffer);
        let read_data = read_message(&mut reader).unwrap();
        assert_eq!(read_data, data);
    }

    #[test]
    fn test_message_too_large() {
        let data = vec![0u8; MAX_MESSAGE_SIZE + 1];
        let mut buffer = Vec::new();
        assert!(matches!(
            write_message(&mut buffer, &data),
            Err(ProtocolError::MessageTooLarge(_))
        ));
    }

    #[test]
    fn test_json_with_unknown_fields() {
        // Unknown fields should be ignored (forward compatibility)
        let json = r#"{"filename": "/tmp/test.png", "unknown_field": "value"}"#;
        let req: DaemonRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.filename, "/tmp/test.png");
    }

    #[test]
    fn test_unicode_paths() {
        let req = DaemonRequest::new("/tmp/скриншот.png");
        assert!(req.validate().is_ok());

        let bytes = req.to_bytes().unwrap();
        let parsed = DaemonRequest::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.filename, "/tmp/скриншот.png");
    }
}
