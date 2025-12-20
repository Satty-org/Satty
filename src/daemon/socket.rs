//! Unix socket server and client for daemon mode
//!
//! Uses tokio for async I/O with length-prefixed message framing.

use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::protocol::{
    read_message, write_message, DaemonRequest, DaemonResponse, ProtocolError, LENGTH_PREFIX_SIZE,
    MAX_MESSAGE_SIZE,
};
use super::security::set_socket_permissions;

/// Connection timeout for client
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Read timeout for client waiting for response
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Daemon server that listens for requests
pub struct DaemonServer {
    listener: UnixListener,
    socket_path: PathBuf,
}

impl DaemonServer {
    /// Create a new daemon server listening on the given path
    pub async fn new(socket_path: &Path) -> Result<Self, std::io::Error> {
        // Remove stale socket if it exists
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;

        // Set secure permissions on the socket
        set_socket_permissions(socket_path).map_err(|e| std::io::Error::other(e.to_string()))?;

        Ok(Self {
            listener,
            socket_path: socket_path.to_path_buf(),
        })
    }

    /// Accept a new connection and read the request
    pub async fn accept(&self) -> Result<(DaemonRequest, DaemonConnection), ProtocolError> {
        let (stream, _addr) = self.listener.accept().await?;
        let mut connection = DaemonConnection { stream };

        let request = connection.read_request().await?;
        Ok((request, connection))
    }

    /// Get the socket path
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        // Clean up socket file
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// A connection to a client
pub struct DaemonConnection {
    stream: UnixStream,
}

impl DaemonConnection {
    /// Read a request from the client
    pub async fn read_request(&mut self) -> Result<DaemonRequest, ProtocolError> {
        // Read length prefix
        let mut len_buf = [0u8; LENGTH_PREFIX_SIZE];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;

        if len > MAX_MESSAGE_SIZE {
            return Err(ProtocolError::MessageTooLarge(len));
        }

        // Read message body
        let mut data = vec![0u8; len];
        self.stream.read_exact(&mut data).await?;

        DaemonRequest::from_bytes(&data)
    }

    /// Send a response to the client
    pub async fn send_response(&mut self, response: &DaemonResponse) -> Result<(), ProtocolError> {
        let data = response.to_bytes()?;

        // Write length prefix
        let len = data.len() as u32;
        self.stream.write_all(&len.to_le_bytes()).await?;

        // Write message body
        self.stream.write_all(&data).await?;
        self.stream.flush().await?;

        Ok(())
    }
}

/// Client for connecting to the daemon
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    /// Create a new client targeting the given socket path
    pub fn new(socket_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
        }
    }

    /// Check if the daemon is running (socket exists and accepts connections)
    pub fn is_daemon_running(&self) -> bool {
        if !self.socket_path.exists() {
            return false;
        }

        // Try to connect with a short timeout
        StdUnixStream::connect(&self.socket_path).is_ok()
    }

    /// Send a request to the daemon and wait for response
    ///
    /// Uses synchronous I/O because the client is typically a short-lived process.
    pub fn send_request(&self, request: &DaemonRequest) -> Result<DaemonResponse, ProtocolError> {
        use std::io::Write;

        let mut stream = StdUnixStream::connect(&self.socket_path)?;

        // Set timeouts
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(CONNECTION_TIMEOUT))?;

        // Send request
        let data = request.to_bytes()?;
        write_message(&mut stream, &data)?;
        stream.flush()?;

        // Read response
        let response_data = read_message(&mut stream)?;
        DaemonResponse::from_bytes(&response_data)
    }

    /// Send a request asynchronously (for use in async contexts)
    #[allow(dead_code)] // Used in tests
    pub async fn send_request_async(
        &self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, ProtocolError> {
        // Connect
        let mut stream =
            tokio::time::timeout(CONNECTION_TIMEOUT, UnixStream::connect(&self.socket_path))
                .await
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, "connection timeout")
                })??;

        // Send request
        let data = request.to_bytes()?;
        let len = data.len() as u32;
        stream.write_all(&len.to_le_bytes()).await?;
        stream.write_all(&data).await?;
        stream.flush().await?;

        // Read response with timeout
        let response_data = tokio::time::timeout(READ_TIMEOUT, async {
            let mut len_buf = [0u8; LENGTH_PREFIX_SIZE];
            stream.read_exact(&mut len_buf).await?;
            let len = u32::from_le_bytes(len_buf) as usize;

            if len > MAX_MESSAGE_SIZE {
                return Err(ProtocolError::MessageTooLarge(len));
            }

            let mut data = vec![0u8; len];
            stream.read_exact(&mut data).await?;
            Ok::<_, ProtocolError>(data)
        })
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))??;

        DaemonResponse::from_bytes(&response_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_server_client_communication() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        // Start server
        let server = DaemonServer::new(&socket_path).await.unwrap();
        let server_path = server.socket_path().to_path_buf();

        // Spawn server handler
        tokio::spawn(async move {
            let (request, mut conn) = server.accept().await.unwrap();
            assert_eq!(request.filename, "/tmp/test.png");
            conn.send_response(&DaemonResponse::ok(1)).await.unwrap();
        });

        // Give server time to start
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Send request
        let client = DaemonClient::new(&server_path);
        let request = DaemonRequest::new("/tmp/test.png");
        let response = client.send_request_async(&request).await.unwrap();

        assert_eq!(response.status, super::super::protocol::ResponseStatus::Ok);
        assert_eq!(response.window_id, Some(1));
    }

    #[tokio::test]
    async fn test_server_creates_socket() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let server = DaemonServer::new(&socket_path).await.unwrap();
        assert!(socket_path.exists());

        // Check permissions
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&socket_path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        drop(server);
        // Socket should be cleaned up
        assert!(!socket_path.exists());
    }

    #[test]
    fn test_client_daemon_not_running() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("nonexistent.sock");

        let client = DaemonClient::new(&socket_path);
        assert!(!client.is_daemon_running());
    }

    #[tokio::test]
    async fn test_client_daemon_running() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");

        let _server = DaemonServer::new(&socket_path).await.unwrap();

        let client = DaemonClient::new(&socket_path);
        assert!(client.is_daemon_running());
    }
}
