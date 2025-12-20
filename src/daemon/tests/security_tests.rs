//! Extended tests for security module

use crate::daemon::security::*;
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

#[test]
fn test_validate_regular_file() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.png");
    File::create(&file_path).unwrap();

    let result = validate_image_path(file_path.to_str().unwrap());
    assert!(result.is_ok());
}

#[test]
fn test_validate_file_in_subdirectory() {
    let dir = TempDir::new().unwrap();
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();
    let file_path = subdir.join("test.png");
    File::create(&file_path).unwrap();

    let result = validate_image_path(file_path.to_str().unwrap());
    assert!(result.is_ok());
}

#[test]
fn test_validate_symlink_chain() {
    let dir = TempDir::new().unwrap();

    // Create: real.png -> link1.png -> link2.png
    let real_path = dir.path().join("real.png");
    File::create(&real_path).unwrap();

    let link1_path = dir.path().join("link1.png");
    std::os::unix::fs::symlink(&real_path, &link1_path).unwrap();

    let link2_path = dir.path().join("link2.png");
    std::os::unix::fs::symlink(&link1_path, &link2_path).unwrap();

    // All should resolve to the real file
    let result = validate_image_path(link2_path.to_str().unwrap());
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), real_path.canonicalize().unwrap());
}

#[test]
fn test_validate_broken_symlink() {
    let dir = TempDir::new().unwrap();
    let link_path = dir.path().join("broken.png");
    std::os::unix::fs::symlink("/nonexistent/file.png", &link_path).unwrap();

    let result = validate_image_path(link_path.to_str().unwrap());
    assert!(matches!(result, Err(SecurityError::FileNotFound(_))));
}

#[test]
fn test_validate_relative_path_with_dots() {
    let dir = TempDir::new().unwrap();
    let subdir = dir.path().join("subdir");
    fs::create_dir(&subdir).unwrap();
    let file_path = subdir.join("test.png");
    File::create(&file_path).unwrap();

    // Use a path with .. that still resolves to a valid file
    let relative_path = format!("{}/subdir/../subdir/test.png", dir.path().display());
    let result = validate_image_path(&relative_path);
    assert!(result.is_ok());
}

#[test]
fn test_validate_hidden_file() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join(".hidden.png");
    File::create(&file_path).unwrap();

    let result = validate_image_path(file_path.to_str().unwrap());
    assert!(result.is_ok());
}

#[test]
fn test_validate_file_with_special_permissions() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.png");
    File::create(&file_path).unwrap();

    // Make file read-only
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o444)).unwrap();

    // Should still be valid (we only need to read)
    let result = validate_image_path(file_path.to_str().unwrap());
    assert!(result.is_ok());
}

#[test]
fn test_validate_fifo() {
    let dir = TempDir::new().unwrap();
    let fifo_path = dir.path().join("test.fifo");

    // Create a FIFO
    nix::unistd::mkfifo(&fifo_path, nix::sys::stat::Mode::S_IRWXU).unwrap();

    // FIFO is not a regular file
    let result = validate_image_path(fifo_path.to_str().unwrap());
    assert!(matches!(result, Err(SecurityError::NotAFile(_))));
}

#[test]
fn test_set_socket_permissions() {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");
    File::create(&socket_path).unwrap();

    // Start with insecure permissions
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o777)).unwrap();

    // Set secure permissions
    set_socket_permissions(&socket_path).unwrap();

    // Verify actual mode
    let metadata = fs::metadata(&socket_path).unwrap();
    let mode = metadata.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn test_validate_path_at_root() {
    // This test checks /tmp which should always exist
    let result = validate_image_path("/tmp");
    // /tmp is a directory, not a file
    assert!(matches!(result, Err(SecurityError::NotAFile(_))));
}

#[test]
fn test_validate_path_max_length() {
    // Create a path that's exactly at the limit
    let long_name = "a".repeat(255); // Max filename length on most filesystems
    let long_path = format!("/tmp/{}", long_name);

    // This should be under our MAX_PATH_LENGTH
    let result = validate_image_path(&long_path);
    // Will fail because file doesn't exist, but not because path is too long
    assert!(matches!(result, Err(SecurityError::FileNotFound(_))));
}
