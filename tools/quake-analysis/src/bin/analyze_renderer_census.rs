//! Summarize QRC1-QRC5 renderer census lines from PSoXide logs.
//!
//! The guest emits hexadecimal positional fields. Passing both deterministic
//! route logs makes this tool reject any frame-level census mismatch before it
//! reports structural optimization bounds.
//!
//! ```text
//! analyze-renderer-census LOG [LOG_B] [--json] [--output PATH]
//! ```

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use quake_analysis::args::{fail, split_long, usage_error};
use quake_analysis::census::{parse_log, render_text, require_deterministic, summarize};

const PROGRAM: &str = "analyze-renderer-census";
const USAGE: &str = "[-h] [--json] [--output OUTPUT] logs [logs ...]";

fn main() -> ExitCode {
    let mut logs: Vec<PathBuf> = Vec::new();
    let mut json = false;
    let mut output: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    let mut only_positional = false;
    while let Some(argument) = args.next() {
        if only_positional || !argument.starts_with('-') || argument == "-" {
            logs.push(PathBuf::from(argument));
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
                println!("\nSummarize QRC1-QRC5 renderer census lines from PSoXide logs.");
                println!("\n  logs      one log, or deterministic run-a/run-b logs");
                println!("  --json    emit JSON instead of the text report");
                println!("  --output  write the report to this path");
                return ExitCode::SUCCESS;
            }
            "--json" => json = true,
            "--output" => {
                let value = inline.map(str::to_owned).or_else(|| args.next());
                match value {
                    Some(value) => output = Some(PathBuf::from(value)),
                    None => usage_error(PROGRAM, USAGE, "argument --output: expected one argument"),
                }
            }
            _ => usage_error(
                PROGRAM,
                USAGE,
                &format!("unrecognized arguments: {argument}"),
            ),
        }
    }
    if logs.is_empty() {
        usage_error(PROGRAM, USAGE, "the following arguments are required: logs");
    }
    if logs.len() != 1 && logs.len() != 2 {
        eprintln!("pass exactly one log or the two deterministic run logs");
        return ExitCode::from(1);
    }

    let run = || -> Result<String, String> {
        let mut runs = Vec::new();
        for path in &logs {
            runs.push(parse_log(path)?);
        }
        if runs.len() == 2 {
            require_deterministic(&runs[0], &runs[1])?;
        }
        let summary = summarize(&runs[0]);
        Ok(if json {
            summary.pretty() + "\n"
        } else {
            render_text(&summary, runs.len())
        })
    };
    match run() {
        Ok(report) => {
            match output {
                Some(path) => {
                    if let Err(error) = fs::write(&path, report) {
                        fail(&format!("{}: {error}", path.display()));
                    }
                }
                None => print!("{report}"),
            }
            ExitCode::SUCCESS
        }
        Err(message) => fail(&message),
    }
}
