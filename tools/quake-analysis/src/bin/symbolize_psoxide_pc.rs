//! Aggregate PSoXide PC samples using a rust-lld guest link map.
//!
//! ```text
//! symbolize-psoxide-pc --map GUEST.map --samples SAMPLES.csv
//!     [--top N] [--min-window-start TICK] [--max-window-start TICK]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use quake_analysis::args::{split_long, usage_error};
use quake_analysis::num::parse_int;
use quake_analysis::symbolize::{aggregate, load_symbols, render};

const PROGRAM: &str = "symbolize-psoxide-pc";
const USAGE: &str = "[-h] --map MAP --samples SAMPLES [--top TOP] \
[--min-window-start MIN_WINDOW_START] [--max-window-start MAX_WINDOW_START]";

fn value(option: &str, inline: Option<&str>, args: &mut impl Iterator<Item = String>) -> String {
    inline
        .map(str::to_owned)
        .or_else(|| args.next())
        .unwrap_or_else(|| {
            usage_error(
                PROGRAM,
                USAGE,
                &format!("argument {option}: expected one argument"),
            )
        })
}

fn number(option: &str, text: &str) -> i128 {
    parse_int(text, 10).unwrap_or_else(|| {
        usage_error(
            PROGRAM,
            USAGE,
            &format!("argument {option}: invalid int value: '{text}'"),
        )
    })
}

fn main() -> ExitCode {
    let mut map: Option<PathBuf> = None;
    let mut samples: Option<PathBuf> = None;
    let mut top: i64 = 40;
    let mut minimum: Option<i128> = None;
    let mut maximum: Option<i128> = None;
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        let (name, inline) = split_long(&argument);
        match name {
            "-h" | "--help" => {
                println!("usage: {PROGRAM} {USAGE}");
                println!("\nAggregate PSoXide PC samples using a rust-lld guest link map.");
                return ExitCode::SUCCESS;
            }
            "--map" => map = Some(PathBuf::from(value(name, inline, &mut args))),
            "--samples" => samples = Some(PathBuf::from(value(name, inline, &mut args))),
            "--top" => {
                top = number(name, &value(name, inline, &mut args)) as i64;
            }
            "--min-window-start" => {
                minimum = Some(number(name, &value(name, inline, &mut args)));
            }
            "--max-window-start" => {
                maximum = Some(number(name, &value(name, inline, &mut args)));
            }
            _ => usage_error(
                PROGRAM,
                USAGE,
                &format!("unrecognized arguments: {argument}"),
            ),
        }
    }
    let (Some(map), Some(samples)) = (map, samples) else {
        usage_error(
            PROGRAM,
            USAGE,
            "the following arguments are required: --map, --samples",
        );
    };

    let symbols = match load_symbols(&map) {
        Ok(symbols) => symbols,
        Err(message) => quake_analysis::args::fail(&message),
    };
    if symbols.is_empty() {
        usage_error(
            PROGRAM,
            USAGE,
            "the link map contains no named function ranges",
        );
    }
    let result = match aggregate(&symbols, &samples, minimum, maximum) {
        Ok(result) => result,
        Err(message) if message.contains("requires a windowed") => {
            usage_error(PROGRAM, USAGE, &message)
        }
        Err(message) => quake_analysis::args::fail(&message),
    };
    match render(&result, top) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(message) => quake_analysis::args::fail(&message),
    }
}
