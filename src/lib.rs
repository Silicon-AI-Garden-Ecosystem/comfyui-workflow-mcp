//! The server is a child process of an MCP client: no flags, no arguments, one fixed
//! configuration file, so every agent reads the same settings. stdout carries the
//! protocol alone; logs go to stderr and to the log file.

pub mod config;
pub mod core;
pub mod startup;
pub mod tool;
