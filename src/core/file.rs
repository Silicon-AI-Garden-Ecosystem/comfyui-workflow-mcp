//! Image parameters are content-addressed: the same bytes always become the same name, so
//! an upload is idempotent, `workflow.json` records which bytes were used, and a re-run is
//! served from ComfyUI's input directory without touching the network.
//!
//! The extension is taken from the caller's file and lowercased: ComfyUI's `LoadImage`
//! filters the input directory by extension, so a name without one is invisible there.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::core::error::FileError;
use crate::core::home;

const CHUNK: usize = 64 * 1024;
const NAME_BYTES: usize = 16;

/// Resolves the caller's path and hashes the file: the name is what the workflow JSON has
/// to hold, the path is what still has to be uploaded.
pub fn locate(raw: &str) -> Result<(PathBuf, String), FileError> {
    locate_in(raw, &home::dir())
}

#[cfg(test)]
fn identify(raw: &str) -> Result<String, FileError> {
    locate(raw).map(|(_, name)| name)
}

pub(crate) fn locate_in(raw: &str, home: &Path) -> Result<(PathBuf, String), FileError> {
    let path = expand(raw, home)?;
    let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => FileError::NotFound {
            path: path.display().to_string(),
        },
        _ => FileError::Read {
            path: path.display().to_string(),
            detail: err.to_string(),
        },
    })?;
    if !metadata.is_file() {
        return Err(FileError::NotAFile {
            path: path.display().to_string(),
        });
    }

    let name = format!("{}.{}", digest(&path)?, extension(&path)?);
    Ok((path, name))
}

#[cfg(test)]
pub(crate) fn identify_in(raw: &str, home: &Path) -> Result<String, FileError> {
    locate_in(raw, home).map(|(_, name)| name)
}

fn expand(raw: &str, home: &Path) -> Result<PathBuf, FileError> {
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    };

    if !path.is_absolute() {
        return Err(FileError::NotAbsolute {
            raw: raw.to_string(),
        });
    }
    Ok(path)
}

fn extension(path: &Path) -> Result<String, FileError> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or_else(|| FileError::NoExtension {
            path: path.display().to_string(),
        })
}

/// 16 bytes of SHA-256 in lowercase hex: 32 characters, short enough to copy by hand and
/// long enough that two different files never collide by accident.
fn digest(path: &Path) -> Result<String, FileError> {
    let mut file = std::fs::File::open(path).map_err(|err| FileError::Read {
        path: path.display().to_string(),
        detail: err.to_string(),
    })?;

    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer).map_err(|err| FileError::Read {
            path: path.display().to_string(),
            detail: err.to_string(),
        })?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
    }

    Ok(context.finish().as_ref()[..NAME_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn file(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_same_bytes_give_the_same_name() {
        let dir = TempDir::new().unwrap();
        let a = file(&dir, "a.png", b"same");
        let b = file(&dir, "b.png", b"same");
        assert_eq!(
            identify(a.to_str().unwrap()).unwrap(),
            identify(b.to_str().unwrap()).unwrap()
        );
    }

    #[test]
    fn one_changed_byte_changes_the_name() {
        let dir = TempDir::new().unwrap();
        let a = file(&dir, "a.png", b"same");
        let b = file(&dir, "b.png", b"samf");
        assert_ne!(
            identify(a.to_str().unwrap()).unwrap(),
            identify(b.to_str().unwrap()).unwrap()
        );
    }

    #[test]
    fn the_name_is_hash_dot_lowercased_extension() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "shot.PNG", b"x");
        let name = identify(path.to_str().unwrap()).unwrap();
        let (hash, ext) = name.split_once('.').unwrap();
        assert_eq!(hash.len(), NAME_BYTES * 2);
        assert!(
            hash.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(ext, "png");
    }

    #[test]
    fn a_file_without_extension_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = file(&dir, "noext", b"x");
        let err = identify(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, FileError::NoExtension { .. }), "{err}");
    }

    #[test]
    fn a_relative_path_is_rejected() {
        let err = identify("pics/a.png").unwrap_err();
        assert_eq!(
            err.to_string(),
            "file path must be absolute or start with ~: pics/a.png"
        );
    }

    #[test]
    fn a_missing_file_is_reported_as_missing() {
        let err = identify("/nonexistent/a.png").unwrap_err();
        assert!(matches!(err, FileError::NotFound { .. }), "{err}");
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("folder.png");
        std::fs::create_dir(&path).unwrap();
        let err = identify(path.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, FileError::NotAFile { .. }), "{err}");
    }

    #[test]
    fn tilde_is_expanded_against_home() {
        let dir = TempDir::new().unwrap();
        file(&dir, "a.png", b"x");
        let name = identify_in("~/a.png", dir.path()).unwrap();
        assert_eq!(name.len(), NAME_BYTES * 2 + 4);
    }
}
