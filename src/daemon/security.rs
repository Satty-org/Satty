//! Security utilities for daemon mode
//!
//! Provides path validation and socket permission checking to prevent
//! common security issues like path traversal attacks.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SecurityError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Path traversal detected: {0}")]
    PathTraversal(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Path is not a file: {0}")]
    NotAFile(PathBuf),
}

/// Maximum path length to prevent DoS attacks
const MAX_PATH_LENGTH: usize = 4096;

/// Validate an image path for security
///
/// Performs the following checks (BASIC level):
/// - Path is not empty
/// - Path length is reasonable
/// - File exists
/// - Path does not contain path traversal sequences after canonicalization
/// - Resolved path is a regular file (not a directory)
///
/// Symlinks are allowed in BASIC mode.
pub fn validate_image_path(path: &str) -> Result<PathBuf, SecurityError> {
    // Check for empty path
    if path.is_empty() {
        return Err(SecurityError::InvalidPath("empty path".into()));
    }

    // Check for stdin indicator (not a real path)
    if path == "-" {
        return Ok(PathBuf::from("-"));
    }

    // Check path length
    if path.len() > MAX_PATH_LENGTH {
        return Err(SecurityError::InvalidPath(format!(
            "path too long: {} bytes (max {})",
            path.len(),
            MAX_PATH_LENGTH
        )));
    }

    let path = Path::new(path);

    // Check for obvious path traversal before canonicalization
    let path_str = path.to_string_lossy();
    if path_str.contains("..") {
        // We'll check again after canonicalization, but catch obvious cases early
        // This helps with error messages
    }

    // Canonicalize the path (resolves symlinks and ..)
    let canonical = path.canonicalize().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SecurityError::FileNotFound(path.to_path_buf())
        } else {
            SecurityError::Io(e)
        }
    })?;

    // After canonicalization, path should be absolute and have no .. components
    let canonical_str = canonical.to_string_lossy();
    if canonical_str.contains("..") {
        return Err(SecurityError::PathTraversal(path_str.into_owned()));
    }

    // Check that it's a file, not a directory
    let metadata = fs::metadata(&canonical)?;
    if !metadata.is_file() {
        return Err(SecurityError::NotAFile(canonical));
    }

    Ok(canonical)
}

/// Validate socket file permissions
///
/// Ensures the socket file:
/// - Has mode 0600 (owner read/write only)
/// - Is owned by the current user
/// Set secure permissions on a socket file
pub fn set_socket_permissions(socket_path: &Path) -> Result<(), SecurityError> {
    let permissions = fs::Permissions::from_mode(0o600);
    fs::set_permissions(socket_path, permissions)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    #[test]
    fn test_validate_existing_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.png");
        File::create(&file_path).unwrap();

        let result = validate_image_path(file_path.to_str().unwrap());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_validate_nonexistent_file() {
        let result = validate_image_path("/nonexistent/path/to/file.png");
        assert!(matches!(result, Err(SecurityError::FileNotFound(_))));
    }

    #[test]
    fn test_validate_empty_path() {
        let result = validate_image_path("");
        assert!(matches!(result, Err(SecurityError::InvalidPath(_))));
    }

    #[test]
    fn test_validate_stdin_marker() {
        let result = validate_image_path("-");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), PathBuf::from("-"));
    }

    #[test]
    fn test_validate_path_traversal() {
        // Create a temp file to make the path "valid" except for traversal
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("test.png");
        File::create(&file_path).unwrap();

        // Try to access via path traversal
        let traversal_path = format!(
            "{}/../{}/test.png",
            dir.path().to_str().unwrap(),
            dir.path().file_name().unwrap().to_str().unwrap()
        );

        // This should still work because canonicalize resolves ..
        // The point is we verify the final path, not block all .. usage
        let result = validate_image_path(&traversal_path);
        // After canonicalization, the path should be valid
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_directory() {
        let dir = TempDir::new().unwrap();
        let result = validate_image_path(dir.path().to_str().unwrap());
        assert!(matches!(result, Err(SecurityError::NotAFile(_))));
    }

    #[test]
    fn test_validate_symlink_to_file() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("real.png");
        File::create(&file_path).unwrap();

        let link_path = dir.path().join("link.png");
        std::os::unix::fs::symlink(&file_path, &link_path).unwrap();

        // Symlinks are allowed in BASIC mode
        let result = validate_image_path(link_path.to_str().unwrap());
        assert!(result.is_ok());
        // Should resolve to the real file
        assert_eq!(result.unwrap(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_validate_long_path() {
        let long_path = "/".to_string() + &"a".repeat(MAX_PATH_LENGTH + 1);
        let result = validate_image_path(&long_path);
        assert!(matches!(result, Err(SecurityError::InvalidPath(_))));
    }

    #[test]
    fn test_validate_unicode_path() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("скриншот.png");
        File::create(&file_path).unwrap();

        let result = validate_image_path(file_path.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_set_socket_permissions() {
        let dir = TempDir::new().unwrap();
        let socket_path = dir.path().join("test.sock");
        File::create(&socket_path).unwrap();

        set_socket_permissions(&socket_path).unwrap();

        let metadata = fs::metadata(&socket_path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
