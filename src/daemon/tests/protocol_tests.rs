//! Extended tests for protocol module

use crate::daemon::protocol::*;

#[test]
fn test_request_serialization_roundtrip() {
    let request = DaemonRequest {
        filename: "/tmp/image.png".into(),
        output_filename: Some("/tmp/output.png".into()),
        copy_command: Some("wl-copy".into()),
        initial_tool: Some("arrow".into()),
        fullscreen: Some(true),
        early_exit: Some(false),
        corner_roundness: Some(15.0),
        annotation_size_factor: Some(2.0),
        default_hide_toolbars: Some(true),
        no_window_decoration: Some(false),
        stdin_data: None,
    };

    let bytes = request.to_bytes().unwrap();
    let parsed = DaemonRequest::from_bytes(&bytes).unwrap();

    assert_eq!(parsed.filename, request.filename);
    assert_eq!(parsed.output_filename, request.output_filename);
    assert_eq!(parsed.copy_command, request.copy_command);
    assert_eq!(parsed.initial_tool, request.initial_tool);
    assert_eq!(parsed.fullscreen, request.fullscreen);
    assert_eq!(parsed.early_exit, request.early_exit);
    assert_eq!(parsed.corner_roundness, request.corner_roundness);
    assert_eq!(
        parsed.annotation_size_factor,
        request.annotation_size_factor
    );
    assert_eq!(parsed.default_hide_toolbars, request.default_hide_toolbars);
    assert_eq!(parsed.no_window_decoration, request.no_window_decoration);
}

#[test]
fn test_response_serialization_roundtrip() {
    // Test Ok response
    let ok_response = DaemonResponse::ok(42);
    let bytes = ok_response.to_bytes().unwrap();
    let parsed = DaemonResponse::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.status, ResponseStatus::Ok);
    assert_eq!(parsed.window_id, Some(42));
    assert!(parsed.message.is_none());

    // Test Error response
    let err_response = DaemonResponse::error("Something went wrong");
    let bytes = err_response.to_bytes().unwrap();
    let parsed = DaemonResponse::from_bytes(&bytes).unwrap();
    assert_eq!(parsed.status, ResponseStatus::Error);
    assert!(parsed.window_id.is_none());
    assert_eq!(parsed.message, Some("Something went wrong".into()));
}

#[test]
fn test_invalid_json() {
    let invalid_json = b"not valid json at all";
    let result = DaemonRequest::from_bytes(invalid_json);
    assert!(matches!(result, Err(ProtocolError::InvalidJson(_))));
}

#[test]
fn test_incomplete_json() {
    let incomplete_json = b"{\"filename\": \"/tmp/test.png\"";
    let result = DaemonRequest::from_bytes(incomplete_json);
    assert!(matches!(result, Err(ProtocolError::InvalidJson(_))));
}

#[test]
fn test_json_missing_required_field() {
    let json = b"{}";
    let result = DaemonRequest::from_bytes(json);
    // serde will fail to deserialize without filename
    assert!(result.is_err());
}

#[test]
fn test_json_with_null_optional_fields() {
    let json = r#"{
        "filename": "/tmp/test.png",
        "output_filename": null,
        "copy_command": null
    }"#;
    let result = DaemonRequest::from_bytes(json.as_bytes());
    assert!(result.is_ok());
    let req = result.unwrap();
    assert_eq!(req.filename, "/tmp/test.png");
    assert!(req.output_filename.is_none());
}

#[test]
fn test_message_framing_multiple() {
    // Test multiple messages in sequence
    let mut buffer = Vec::new();

    let data1 = b"first message";
    let data2 = b"second message with more data";
    let data3 = b"third";

    write_message(&mut buffer, data1).unwrap();
    write_message(&mut buffer, data2).unwrap();
    write_message(&mut buffer, data3).unwrap();

    let mut reader = std::io::Cursor::new(buffer);

    assert_eq!(read_message(&mut reader).unwrap(), data1);
    assert_eq!(read_message(&mut reader).unwrap(), data2);
    assert_eq!(read_message(&mut reader).unwrap(), data3);
}

#[test]
fn test_message_framing_empty() {
    let mut buffer = Vec::new();
    write_message(&mut buffer, b"").unwrap();

    let mut reader = std::io::Cursor::new(buffer);
    let result = read_message(&mut reader).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_special_characters_in_paths() {
    let special_paths = [
        "/tmp/file with spaces.png",
        "/tmp/file\twith\ttabs.png",
        "/tmp/file'with'quotes.png",
        "/tmp/file\"with\"doublequotes.png",
        "/tmp/файл.png",     // Russian
        "/tmp/文件.png",     // Chinese
        "/tmp/ファイル.png", // Japanese
    ];

    for path in special_paths {
        let req = DaemonRequest::new(path);
        let bytes = req.to_bytes().unwrap();
        let parsed = DaemonRequest::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.filename, path, "Failed for path: {}", path);
    }
}

#[test]
fn test_large_stdin_data() {
    use base64::Engine;

    // Test with moderately large base64 data (1MB)
    let image_data = vec![0u8; 1024 * 1024];
    let encoded = base64::engine::general_purpose::STANDARD.encode(&image_data);

    let mut req = DaemonRequest::new("-");
    req.stdin_data = Some(encoded);

    let bytes = req.to_bytes().unwrap();
    assert!(bytes.len() < MAX_MESSAGE_SIZE);

    let parsed = DaemonRequest::from_bytes(&bytes).unwrap();
    assert!(parsed.stdin_data.is_some());
}

#[test]
fn test_connection_closed_on_empty_read() {
    let empty: &[u8] = &[];
    let mut reader = std::io::Cursor::new(empty);
    let result = read_message(&mut reader);
    assert!(matches!(result, Err(ProtocolError::ConnectionClosed)));
}
