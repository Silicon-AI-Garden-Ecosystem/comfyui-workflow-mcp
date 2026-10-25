//! One run, one directory: a directory is the only shape that answers "what did this
//! run produce", and the server names it, so the artifacts land somewhere predictable.
//! The stamp is a hand-rolled UTC civil date: the machine's timezone must not change
//! the layout, and a nix-built binary should not carry a time dependency.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn utc_stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}-{:02}{:02}{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's public-domain algorithm; the constants are the published ones.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn create_run_dir(root: &Path, name: &str, now: SystemTime) -> io::Result<PathBuf> {
    let base = root.join(name);
    fs::create_dir_all(&base)?;
    let stamp = utc_stamp(now);
    for attempt in 1..=10_000u32 {
        let dir = if attempt == 1 {
            base.join(&stamp)
        } else {
            base.join(format!("{stamp}-{attempt}"))
        };
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(
        "too many run directories within one second",
    ))
}

/// `create_new`, never overwrite: a collision must not destroy an earlier run.
pub fn write_slot(dir: &Path, index: usize, ext: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let ext = ext.trim_start_matches('.');
    let ext = if ext.is_empty() { "bin" } else { ext };
    for i in index..index + 10_000 {
        let file = dir.join(format!("{i}.{ext}"));
        match OpenOptions::new().write(true).create_new(true).open(&file) {
            Ok(mut handle) => {
                handle.write_all(bytes)?;
                return Ok(file);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other(format!(
        "too many files in {}",
        dir.display()
    )))
}

pub fn extension_of(filename: &str) -> String {
    Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "png".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn stamp_is_utc() {
        // 2026-10-23T14:23:31Z
        assert_eq!(utc_stamp(at(1_792_765_411)), "20261023-142331");
        assert_eq!(utc_stamp(at(0)), "19700101-000000");
        // leap day, February 29
        assert_eq!(utc_stamp(at(1_709_164_800)), "20240229-000000");
    }

    #[test]
    fn run_dir_collides_by_suffix() {
        let root = tempfile::tempdir().unwrap();
        let now = at(1_792_765_411);
        let a = create_run_dir(root.path(), "sdxl", now).unwrap();
        let b = create_run_dir(root.path(), "sdxl", now).unwrap();
        assert!(a.ends_with("20261023-142331"));
        assert!(b.ends_with("20261023-142331-2"));
        assert_ne!(a, b);
    }

    #[test]
    fn write_slot_never_overwrites() {
        let root = tempfile::tempdir().unwrap();
        let dir = create_run_dir(root.path(), "w", at(0)).unwrap();
        let a = write_slot(&dir, 0, "png", b"one").unwrap();
        let b = write_slot(&dir, 0, "png", b"two").unwrap();
        assert_eq!(a.file_name().unwrap(), "0.png");
        assert_eq!(b.file_name().unwrap(), "1.png");
        assert_eq!(fs::read(&a).unwrap(), b"one");
    }

    #[test]
    fn extension_from_filename() {
        assert_eq!(extension_of("ComfyUI_00001_.PNG"), "png");
        assert_eq!(extension_of("clip.mp4"), "mp4");
        assert_eq!(extension_of("no-extension"), "png");
    }
}
