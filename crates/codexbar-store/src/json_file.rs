//! Strict reading of the JSON object files in `~/.codexbar` that CodexBar rewrites (#314): a file that exists but
//! can't be used is an error, never an empty document, so a save never replaces it.

use std::io;
use std::path::Path;

use serde_json::{Map, Value};

/// The JSON object in `path`, or `Ok(None)` when the file doesn't exist. A leading UTF-8 byte order mark is skipped,
/// as the WPF app's `File.ReadAllText` does. A file that can't be read, isn't valid JSON, or holds anything other than
/// an object is an error.
pub(crate) fn read_object(path: &Path) -> io::Result<Option<Map<String, Value>>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        _ => Err(io::Error::new(io::ErrorKind::InvalidData, "not a JSON object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn with(name: &str, bytes: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!("codexbar-json-file-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("file.json");
            std::fs::write(&file, bytes).unwrap();
            Self(file)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn a_missing_file_is_none() {
        let file = TempFile::with("missing", b"{}");
        std::fs::remove_file(&file.0).unwrap();
        assert!(read_object(&file.0).unwrap().is_none());
    }

    #[test]
    fn an_object_reads_with_or_without_a_byte_order_mark() {
        for (case, bytes) in [
            ("plain", b"{\"a\":1}".to_vec()),
            ("bom", b"\xef\xbb\xbf{\"a\":1}".to_vec()),
        ] {
            let file = TempFile::with(case, &bytes);
            let map = read_object(&file.0).unwrap().expect(case);
            assert_eq!(map.get("a"), Some(&Value::from(1)), "{case}");
        }
    }

    #[test]
    fn anything_but_a_readable_object_is_an_error() {
        for (case, bytes) in [
            ("syntax", b"{ not json".to_vec()),
            ("array", b"[]".to_vec()),
            ("null", b"null".to_vec()),
            ("empty", Vec::new()),
            ("utf8", b"{\"a\":\"\xff\"}".to_vec()),
        ] {
            let file = TempFile::with(case, &bytes);
            assert!(read_object(&file.0).is_err(), "{case}");
        }
    }

    #[test]
    fn a_read_error_other_than_not_found_is_an_error() {
        // A folder where the file should be can't be read as text.
        let file = TempFile::with("folder", b"{}");
        std::fs::remove_file(&file.0).unwrap();
        std::fs::create_dir(&file.0).unwrap();
        let err = read_object(&file.0).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::NotFound);
    }
}
