//! `HOME` is the one environment variable still read for paths: a model names files as
//! `~/...`, and config expansion has already turned every configured path into an absolute
//! one before it gets here.

use std::path::PathBuf;

pub fn dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}
