use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Content-addressed object store rooted at `<root>/objects/<hh>/<hash>`,
/// where `<hh>` is the first two hex chars of the BLAKE3 hash (docs/VERSIONING.md §2.1).
pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validates and resolves `hash` to its on-disk object path. A
    /// content-addressed locator must be exactly a 64-char BLAKE3 hex
    /// digest (see `hash::hash_bytes`) - anything shorter, longer, or
    /// non-hex is a malformed/adversarial input and must be rejected here
    /// rather than allowed to slice out of bounds below.
    fn object_path(&self, hash: &str) -> io::Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid object hash: expected 64 hex chars, got {hash:?}"),
            ));
        }
        let prefix = &hash[..2];
        Ok(self.root.join("objects").join(prefix).join(hash))
    }

    /// Returns whether `hash` names an object already on disk. A malformed
    /// hash can never name a real object, so it reports `false` rather than
    /// erroring - callers use this for existence checks, not for resolving
    /// a locator they intend to read from or write to.
    pub fn has_object(&self, hash: &str) -> bool {
        matches!(self.object_path(hash), Ok(path) if path.exists())
    }

    /// Writes `data` under `hash`, skipping the write if the object already
    /// exists (content-addressed dedup). Returns whether a new object was
    /// written (`false` means it was already present).
    pub fn write_object(&self, hash: &str, data: &[u8]) -> io::Result<bool> {
        let path = self.object_path(hash)?;
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, data)?;
        Ok(true)
    }

    pub fn read_object(&self, hash: &str) -> io::Result<Vec<u8>> {
        fs::read(self.object_path(hash)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hash_returns_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        assert!(store.read_object("").is_err());
        assert!(store.write_object("", b"data").is_err());
        assert!(!store.has_object(""));
    }

    #[test]
    fn one_char_hash_returns_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());

        assert!(store.read_object("a").is_err());
        assert!(store.write_object("a", b"data").is_err());
        assert!(!store.has_object("a"));
    }

    #[test]
    fn non_hex_hash_returns_error_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        // 64 chars, but not valid hex (and includes a multi-byte char to
        // exercise the non-ASCII path too).
        let not_hex = format!("{}{}", "z".repeat(63), "\u{00e9}");

        assert!(store.read_object(&not_hex).is_err());
        assert!(store.write_object(&not_hex, b"data").is_err());
        assert!(!store.has_object(&not_hex));
    }

    #[test]
    fn valid_64_char_hex_hash_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(dir.path());
        let hash = "a".repeat(64);

        assert!(store.write_object(&hash, b"payload").unwrap());
        assert!(store.has_object(&hash));
        assert_eq!(store.read_object(&hash).unwrap(), b"payload");
    }
}
