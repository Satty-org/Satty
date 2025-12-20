//! Integration tests for daemon socket communication
//!
//! These tests verify end-to-end communication between client and server.

use crate::daemon::protocol::{DaemonRequest, DaemonResponse, ResponseStatus};
use crate::daemon::socket::{DaemonClient, DaemonServer};
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
async fn test_client_server_valid_request() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    // Create a test image file path (we just test the protocol, not actual image loading)
    let image_path = dir.path().join("test.png");
    // Create a dummy file for the test
    std::fs::write(&image_path, [0u8; 100]).unwrap();

    // Start server
    let server = DaemonServer::new(&socket_path).await.unwrap();

    // Spawn server handler
    let server_path = server.socket_path().to_path_buf();
    tokio::spawn(async move {
        let (request, mut conn) = server.accept().await.unwrap();
        // Verify request fields
        assert!(!request.filename.is_empty());
        conn.send_response(&DaemonResponse::ok(1)).await.unwrap();
    });

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send request
    let client = DaemonClient::new(&server_path);
    let request = DaemonRequest::new(image_path.to_str().unwrap());
    let response = client.send_request_async(&request).await.unwrap();

    assert_eq!(response.status, ResponseStatus::Ok);
    assert_eq!(response.window_id, Some(1));
}

#[tokio::test]
async fn test_client_server_error_response() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    // Start server
    let server = DaemonServer::new(&socket_path).await.unwrap();
    let server_path = server.socket_path().to_path_buf();

    // Spawn server handler that returns error
    tokio::spawn(async move {
        let (_request, mut conn) = server.accept().await.unwrap();
        conn.send_response(&DaemonResponse::error("Test error message"))
            .await
            .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send request
    let client = DaemonClient::new(&server_path);
    let request = DaemonRequest::new("/tmp/test.png");
    let response = client.send_request_async(&request).await.unwrap();

    assert_eq!(response.status, ResponseStatus::Error);
    assert_eq!(response.message, Some("Test error message".into()));
    assert!(response.window_id.is_none());
}

#[tokio::test]
async fn test_multiple_sequential_requests() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let server = DaemonServer::new(&socket_path).await.unwrap();
    let server_path = server.socket_path().to_path_buf();

    // Spawn server that handles multiple requests
    tokio::spawn(async move {
        for i in 1..=3 {
            let (_request, mut conn) = server.accept().await.unwrap();
            conn.send_response(&DaemonResponse::ok(i)).await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(&server_path);

    // Send 3 sequential requests
    for i in 1..=3 {
        let request = DaemonRequest::new(format!("/tmp/test_{}.png", i));
        let response = client.send_request_async(&request).await.unwrap();
        assert_eq!(response.status, ResponseStatus::Ok);
        assert_eq!(response.window_id, Some(i));
    }
}

#[tokio::test]
async fn test_request_with_all_options() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let server = DaemonServer::new(&socket_path).await.unwrap();
    let server_path = server.socket_path().to_path_buf();

    tokio::spawn(async move {
        let (request, mut conn) = server.accept().await.unwrap();

        // Verify all options were received
        assert_eq!(request.filename, "/tmp/input.png");
        assert_eq!(request.output_filename, Some("/tmp/output.png".into()));
        assert_eq!(request.copy_command, Some("wl-copy".into()));
        assert_eq!(request.fullscreen, Some(true));
        assert_eq!(request.early_exit, Some(false));

        conn.send_response(&DaemonResponse::ok(42)).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(&server_path);

    let mut request = DaemonRequest::new("/tmp/input.png");
    request.output_filename = Some("/tmp/output.png".into());
    request.copy_command = Some("wl-copy".into());
    request.fullscreen = Some(true);
    request.early_exit = Some(false);

    let response = client.send_request_async(&request).await.unwrap();
    assert_eq!(response.status, ResponseStatus::Ok);
    assert_eq!(response.window_id, Some(42));
}

#[tokio::test]
async fn test_request_with_stdin_data() {
    use base64::Engine;

    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let server = DaemonServer::new(&socket_path).await.unwrap();
    let server_path = server.socket_path().to_path_buf();

    let test_data = vec![1u8, 2, 3, 4, 5];
    let encoded = base64::engine::general_purpose::STANDARD.encode(&test_data);
    let encoded_clone = encoded.clone();

    tokio::spawn(async move {
        let (request, mut conn) = server.accept().await.unwrap();

        assert_eq!(request.filename, "-");
        assert_eq!(request.stdin_data, Some(encoded_clone));

        conn.send_response(&DaemonResponse::ok(1)).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = DaemonClient::new(&server_path);

    let mut request = DaemonRequest::new("-");
    request.stdin_data = Some(encoded);

    let response = client.send_request_async(&request).await.unwrap();
    assert_eq!(response.status, ResponseStatus::Ok);
}

#[test]
fn test_client_is_daemon_running_no_socket() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("nonexistent.sock");

    let client = DaemonClient::new(&socket_path);
    assert!(!client.is_daemon_running());
}

#[tokio::test]
async fn test_client_is_daemon_running_with_server() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let _server = DaemonServer::new(&socket_path).await.unwrap();

    let client = DaemonClient::new(&socket_path);
    assert!(client.is_daemon_running());
}

#[tokio::test]
async fn test_server_cleanup_on_drop() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    {
        let _server = DaemonServer::new(&socket_path).await.unwrap();
        assert!(socket_path.exists());
    }

    // Socket should be cleaned up after server is dropped
    assert!(!socket_path.exists());
}

#[tokio::test]
async fn test_stale_socket_replacement() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    // Create a stale socket file
    std::fs::write(&socket_path, "stale").unwrap();
    assert!(socket_path.exists());

    // Server should replace the stale socket
    let server = DaemonServer::new(&socket_path).await.unwrap();
    assert!(socket_path.exists());

    // Verify it's a real socket now
    let client = DaemonClient::new(&socket_path);
    assert!(client.is_daemon_running());

    drop(server);
}

#[tokio::test]
async fn test_request_validation() {
    // Test validation of DaemonRequest
    let valid_request = DaemonRequest::new("/tmp/test.png");
    assert!(valid_request.validate().is_ok());

    // Request with empty filename should fail
    let empty_request = DaemonRequest::new("");
    assert!(empty_request.validate().is_err());
}

#[tokio::test]
async fn test_concurrent_connections() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let server = DaemonServer::new(&socket_path).await.unwrap();
    let server_path = server.socket_path().to_path_buf();

    // Spawn server handler for multiple connections
    tokio::spawn(async move {
        for i in 1..=5 {
            let (_request, mut conn) = server.accept().await.unwrap();
            conn.send_response(&DaemonResponse::ok(i)).await.unwrap();
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send multiple concurrent requests
    let mut handles = vec![];
    for i in 1..=5 {
        let path = server_path.clone();
        handles.push(tokio::spawn(async move {
            let client = DaemonClient::new(&path);
            let request = DaemonRequest::new(format!("/tmp/test_{}.png", i));
            client.send_request_async(&request).await
        }));
    }

    // Verify all requests succeeded
    for handle in handles {
        let response = handle.await.unwrap().unwrap();
        assert_eq!(response.status, ResponseStatus::Ok);
        assert!(response.window_id.is_some());
    }
}
