//! Shared logic for the Quake PSoXide analysis tools.
//!
//! The three binaries report on files that PSoXide and the guest emit:
//! GP0 frame counters, renderer census log lines and PC samples. Their text
//! and JSON output is stable and diffed against earlier runs, so this crate
//! keeps the number and JSON formatting those reports have always used
//! (shortest round-trip floats, two-space indented JSON with sorted keys).

pub mod args;
pub mod census;
pub mod csv;
pub mod gpu;
pub mod json;
pub mod num;
pub mod symbolize;
