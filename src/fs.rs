//! Filesystem boundary.
//!
//! Production code uses [`RealFs`]. Tests construct a [`FakeFs`] populated with
//! in-memory paths so they stay hermetic.

use anyhow::Result;
use std::path::Path;

pub trait FileSystem {
    fn exists(&self, path: &Path) -> bool;

    /// Read a file's contents. Returns `Ok(None)` when the file does not exist.
    ///
    /// # Errors
    ///
    /// Propagates IO errors other than "not found".
    fn read_to_string(&self, path: &Path) -> Result<Option<String>>;
}

/// Production filesystem backed by `std::fs`.
pub struct RealFs;

impl FileSystem for RealFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn read_to_string(&self, path: &Path) -> Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::Error::from(e)),
        }
    }
}

#[cfg(test)]
pub use fake::FakeFs;

#[cfg(test)]
mod fake {
    use super::{FileSystem, Path, Result};
    use std::{collections::HashMap, path::PathBuf};

    pub struct FakeFs {
        files: HashMap<PathBuf, String>,
    }

    impl FakeFs {
        #[must_use]
        pub fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, c)| (PathBuf::from(*p), (*c).to_string()))
                    .collect(),
            }
        }
    }

    impl FileSystem for FakeFs {
        fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }

        fn read_to_string(&self, path: &Path) -> Result<Option<String>> {
            Ok(self.files.get(path).cloned())
        }
    }
}
