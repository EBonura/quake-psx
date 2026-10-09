//! Command-line helpers shared by the three tools.

use std::process;

/// Print a usage error in the conventional format and exit with status 2.
pub fn usage_error(program: &str, usage: &str, message: &str) -> ! {
    eprintln!("usage: {program} {usage}");
    eprintln!("{program}: error: {message}");
    process::exit(2)
}

/// Print an error and exit with status 1.
pub fn fail(message: &str) -> ! {
    eprintln!("error: {message}");
    process::exit(1)
}

/// Split `--name=value` into `(name, Some(value))`; other arguments pass
/// through with no value.
pub fn split_long(argument: &str) -> (&str, Option<&str>) {
    match argument.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (argument, None),
    }
}
