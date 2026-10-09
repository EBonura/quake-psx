//! Summarize PSoXide GP0 counters over complete gameplay presentations.
//!
//! ```text
//! analyze-psoxide-gpu GPU_CSV ROUTE_CSV CD_CSV
//!     [--first-present N] [--last-present N] [--json]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use quake_analysis::args::{fail, split_long, usage_error};
use quake_analysis::gpu::{gameplay_bounds, read_rows, render_text, summarize};
use quake_analysis::num::parse_int;

const PROGRAM: &str = "analyze-psoxide-gpu";
const USAGE: &str = "[-h] [--first-present FIRST_PRESENT] [--last-present LAST_PRESENT] \
[--json] gpu_csv route_csv cd_csv";

fn number(option: &str, value: Option<String>) -> i128 {
    let Some(value) = value else {
        usage_error(
            PROGRAM,
            USAGE,
            &format!("argument {option}: expected one argument"),
        );
    };
    parse_int(&value, 10).unwrap_or_else(|| {
        usage_error(
            PROGRAM,
            USAGE,
            &format!("argument {option}: invalid int value: '{value}'"),
        )
    })
}

fn main() -> ExitCode {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut first: Option<i128> = None;
    let mut last: Option<i128> = None;
    let mut json = false;
    let mut args = std::env::args().skip(1);
    let mut only_positional = false;
    while let Some(argument) = args.next() {
        if only_positional || !argument.starts_with('-') || argument == "-" {
            paths.push(PathBuf::from(argument));
            continue;
        }
        if argument == "--" {
            only_positional = true;
            continue;
        }
        let (name, inline) = split_long(&argument);
        match name {
            "-h" | "--help" => {
                println!("usage: {PROGRAM} {USAGE}");
                println!("\nSummarize PSoXide GP0 counters over complete gameplay presentations.");
                return ExitCode::SUCCESS;
            }
            "--json" => json = true,
            "--first-present" => {
                first = Some(number(
                    name,
                    inline.map(str::to_owned).or_else(|| args.next()),
                ))
            }
            "--last-present" => {
                last = Some(number(
                    name,
                    inline.map(str::to_owned).or_else(|| args.next()),
                ))
            }
            _ => usage_error(
                PROGRAM,
                USAGE,
                &format!("unrecognized arguments: {argument}"),
            ),
        }
    }
    if paths.len() != 3 {
        usage_error(
            PROGRAM,
            USAGE,
            "expected exactly three paths: gpu_csv route_csv cd_csv",
        );
    }

    let run = || -> Result<String, String> {
        let gpu = read_rows(&paths[0])?;
        let route = read_rows(&paths[1])?;
        let (automatic_first, automatic_last) = gameplay_bounds(&route, &paths[2])?;
        let summary = summarize(
            &gpu,
            &route,
            first.unwrap_or(automatic_first),
            last.unwrap_or(automatic_last),
        )?;
        Ok(if json {
            summary.pretty() + "\n"
        } else {
            render_text(&summary)
        })
    };
    match run() {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(message) => fail(&message),
    }
}
