//! Regression for libc's removal of the ENOATTR compatibility alias on Linux.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn missing_attribute_returns_none() {
    let file = tempfile::NamedTempFile::new().unwrap();
    assert_eq!(
        xattr::get(file.path(), "user.shadowroom_absent_attribute").unwrap(),
        None
    );
}
