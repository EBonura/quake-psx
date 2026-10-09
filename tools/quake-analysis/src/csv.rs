//! Minimal CSV reader for the emulator's comma-separated logs.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Read a whole file as text, replacing invalid UTF-8 rather than failing.
pub fn read_text(path: &Path) -> Result<String, String> {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Split CSV text into records. Quoted fields may contain commas, doubled
/// quotes and line breaks. A blank line yields an empty record.
pub fn records(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut at_field_start = true;
    let mut line_has_data = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                in_quotes = true;
                at_field_start = false;
                line_has_data = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                at_field_start = true;
                line_has_data = true;
            }
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                if line_has_data {
                    record.push(std::mem::take(&mut field));
                }
                out.push(std::mem::take(&mut record));
                at_field_start = true;
                line_has_data = false;
            }
            _ => {
                field.push(c);
                at_field_start = false;
                line_has_data = true;
            }
        }
    }
    if line_has_data {
        record.push(field);
        out.push(record);
    }
    out
}

/// One data row keyed by header name. Cells missing from a short row are
/// absent from the map.
pub type Row = HashMap<String, String>;

/// Parse CSV text whose first record is the header. Blank lines are skipped
/// and a data row wider than the header is an error.
pub fn dict_rows(text: &str) -> Result<(Vec<String>, Vec<Row>), String> {
    let mut all = records(text).into_iter();
    let header = all.next().unwrap_or_default();
    let mut rows = Vec::new();
    for record in all {
        if record.is_empty() {
            continue;
        }
        if record.len() > header.len() {
            return Err("CSV row has more fields than the header".to_owned());
        }
        rows.push(header.iter().cloned().zip(record).collect());
    }
    Ok((header, rows))
}

#[cfg(test)]
mod tests {
    use super::{dict_rows, records};

    #[test]
    fn splits_plain_quoted_and_blank_records() {
        let text = "a,b\r\n1,\"x,\"\"y\"\"\"\n\n3\n";
        let all = records(text);
        assert_eq!(all[0], ["a", "b"]);
        assert_eq!(all[1], ["1", "x,\"y\""]);
        assert!(all[2].is_empty());
        assert_eq!(all[3], ["3"]);
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn dict_rows_skip_blanks_and_leave_short_cells_absent() {
        let (header, rows) = dict_rows("a,b,c\n1,2\n\n4,5,6\n").unwrap();
        assert_eq!(header, ["a", "b", "c"]);
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].contains_key("c"));
        assert_eq!(rows[1]["c"], "6");
        assert!(dict_rows("a\n1,2\n").is_err());
    }

    #[test]
    fn trailing_comma_keeps_an_empty_last_cell() {
        let all = records("a,b,\n");
        assert_eq!(all[0], ["a", "b", ""]);
    }
}
