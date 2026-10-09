//! Aggregate PSoXide PC samples using a rust-lld guest link map.
//!
//! The PS-X EXE is a flat binary, but rust-lld's `-Map` output retains exact
//! function ranges without changing one byte of the image. This accepts either
//! the aggregate `--pc-sample-log` CSV or the windowed form and attributes
//! each sample to the smallest enclosing named map range.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::csv::{dict_rows, read_text, Row};
use crate::num::{parse_int, split_lines};

/// A named address range from the link map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// First address.
    pub start: u64,
    /// One past the last address.
    pub end: u64,
    /// Symbol name.
    pub name: String,
}

/// Take a leading run of `accept` characters that is followed by whitespace,
/// and advance `rest` past that whitespace.
fn take_token(rest: &mut &str, accept: fn(char) -> bool) -> Option<String> {
    let end = rest.find(|c: char| !accept(c)).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let (word, tail) = rest.split_at(end);
    if !tail.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    *rest = tail.trim_start();
    Some(word.to_owned())
}

/// Parse one map row: three hex columns (VMA, LMA, size), a decimal
/// alignment, then the name.
fn map_row(line: &str) -> Option<(u64, u64, String)> {
    let hex: fn(char) -> bool = |c| c.is_ascii_hexdigit();
    let mut rest = line.trim_start();
    let start = take_token(&mut rest, hex)?;
    take_token(&mut rest, hex)?;
    let size = take_token(&mut rest, hex)?;
    // The alignment is followed by one whitespace character and then at least
    // one more character, which (trimmed) becomes the name.
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let tail = &rest[end..];
    let mut chars = tail.chars();
    if !chars.next()?.is_whitespace() || chars.as_str().is_empty() {
        return None;
    }
    Some((
        u64::from_str_radix(&start, 16).ok()?,
        u64::from_str_radix(&size, 16).ok()?,
        tail.trim().to_owned(),
    ))
}

/// Read exact named function ranges from a rust-lld map.
pub fn load_symbols(path: &Path) -> Result<Vec<Symbol>, String> {
    let text = read_text(path)?;
    Ok(symbols_from_text(&text))
}

/// Parse map text. See [`load_symbols`].
pub fn symbols_from_text(text: &str) -> Vec<Symbol> {
    let mut by_range: BTreeMap<(u64, u64), String> = BTreeMap::new();
    for line in split_lines(text) {
        let Some((start, size, name)) = map_row(line) else {
            continue;
        };
        if size == 0 || start < 0x8001_0000 {
            continue;
        }
        // Input-section rows duplicate the following readable symbol row.
        if name.contains(":(") || name.starts_with('.') || name.ends_with(" = .") {
            continue;
        }
        by_range.insert((start, start + size), name);
    }
    by_range
        .into_iter()
        .map(|((start, end), name)| Symbol { start, end, name })
        .collect()
}

/// The narrowest named range containing `pc`, searching the 15 symbols that
/// start at or before it.
pub fn enclosing_symbol<'a>(symbols: &'a [Symbol], starts: &[u64], pc: u64) -> Option<&'a Symbol> {
    let cursor = starts.partition_point(|&start| start <= pc);
    let mut best: Option<&Symbol> = None;
    let lowest = cursor.saturating_sub(15);
    for index in (lowest..cursor).rev() {
        let candidate = &symbols[index];
        if candidate.start <= pc && pc < candidate.end {
            let narrower = best.is_none_or(|b| candidate.end - candidate.start < b.end - b.start);
            if narrower {
                best = Some(candidate);
            }
        }
    }
    best
}

/// Samples attributed to each symbol, and the hottest PC seen for each.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Aggregate {
    /// Total samples counted.
    pub grand: i128,
    /// Samples per symbol name (`<unmapped>` for PCs outside every range).
    pub totals: HashMap<String, i128>,
    /// For each name, the largest single-row count and its PC.
    pub hottest: HashMap<String, (i128, u64)>,
}

/// Attribute every sample row of a CSV to a symbol. The window bounds select
/// rows of a windowed CSV by `window_start_tick`.
pub fn aggregate(
    symbols: &[Symbol],
    samples_path: &Path,
    minimum_window_start: Option<i128>,
    maximum_window_start: Option<i128>,
) -> Result<Aggregate, String> {
    let text = read_text(samples_path)?;
    let (header, rows) =
        dict_rows(&text).map_err(|e| format!("{}: {e}", samples_path.display()))?;
    let windowed = header.iter().any(|name| name == "window_start_tick");
    let starts: Vec<u64> = symbols.iter().map(|s| s.start).collect();
    let cell = |row: &Row, name: &str| -> Result<String, String> {
        row.get(name)
            .cloned()
            .ok_or_else(|| format!("missing column or cell {name:?}"))
    };
    let window_tick = |row: &Row| -> Result<i128, String> {
        parse_int(&cell(row, "window_start_tick")?, 10)
            .ok_or_else(|| "invalid window_start_tick".to_owned())
    };
    let mut out = Aggregate::default();
    for row in &rows {
        if let Some(bound) = minimum_window_start {
            if !windowed {
                return Err("--min-window-start requires a windowed sample CSV".to_owned());
            }
            if window_tick(row)? < bound {
                continue;
            }
        }
        if let Some(bound) = maximum_window_start {
            if !windowed {
                return Err("--max-window-start requires a windowed sample CSV".to_owned());
            }
            if window_tick(row)? > bound {
                continue;
            }
        }
        let pc = parse_int(&cell(row, "pc")?, 16).ok_or("invalid pc")?;
        let pc = u64::try_from(pc).map_err(|_| "pc out of range".to_owned())?;
        let count = parse_int(&cell(row, "samples")?, 10).ok_or("invalid samples")?;
        let name = enclosing_symbol(symbols, &starts, pc)
            .map_or("<unmapped>", |symbol| symbol.name.as_str())
            .to_owned();
        *out.totals.entry(name.clone()).or_insert(0) += count;
        if count > out.hottest.get(&name).map_or(0, |hot| hot.0) {
            out.hottest.insert(name, (count, pc));
        }
        out.grand += count;
    }
    Ok(out)
}

/// Render the ranked report for the `top` hottest symbols.
pub fn render(aggregate: &Aggregate, top: i64) -> Result<String, String> {
    let mut ranked: Vec<(&String, &i128)> = aggregate.totals.iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    let keep = if top >= 0 {
        (top as usize).min(ranked.len())
    } else {
        ranked.len().saturating_sub(top.unsigned_abs() as usize)
    };
    ranked.truncate(keep);

    let mut out = format!(
        "{} samples over {} symbols\n",
        aggregate.grand,
        aggregate.totals.len()
    );
    out.push_str(&format!(
        "{:>9} {:>7}  {:>10}  symbol\n",
        "samples", "pct", "hot pc"
    ));
    for (name, count) in ranked {
        let (_, pc) = aggregate
            .hottest
            .get(name)
            .ok_or_else(|| format!("no positive sample count for {name}"))?;
        if aggregate.grand == 0 {
            return Err("division by zero: no samples".to_owned());
        }
        let percent = (100 * *count) as f64 / aggregate.grand as f64;
        out.push_str(&format!("{count:>9} {percent:>6.2}%  0x{pc:08x}  {name}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{aggregate, enclosing_symbol, symbols_from_text};
    use std::fs;

    #[test]
    fn named_map_rows_win_over_duplicate_input_sections() {
        let symbols = symbols_from_text(
            " VMA LMA Size Align Out In Symbol\n\
             80010000 80010000 20 4 /tmp/a.o:(.text.foo)\n\
             80010000 80010000 20 1 crate::foo\n\
             80010020 80010020 10 1 crate::bar\n",
        );
        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["crate::foo", "crate::bar"]);
        let starts: Vec<u64> = symbols.iter().map(|s| s.start).collect();
        assert_eq!(
            enclosing_symbol(&symbols, &starts, 0x8001_001C).map(|s| s.name.as_str()),
            Some("crate::foo")
        );
        assert!(enclosing_symbol(&symbols, &starts, 0x8001_0040).is_none());
    }

    #[test]
    fn window_filter_and_unmapped_samples() {
        let dir = std::env::temp_dir().join(format!("quake-analysis-sym-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("samples.csv");
        fs::write(
            &path,
            "window_start_tick,pc,samples,percent_window\n\
             0,0x80010004,3,0\n\
             300,0x80010004,5,0\n\
             300,0x70000000,2,0\n\
             600,0x80010004,11,0\n",
        )
        .unwrap();
        let symbols = symbols_from_text("80010000 80010000 20 1 crate::foo\n");
        let result = aggregate(&symbols, &path, Some(300), Some(300)).unwrap();
        assert_eq!(result.grand, 7);
        assert_eq!(result.totals.len(), 2);
        assert_eq!(result.totals["crate::foo"], 5);
        assert_eq!(result.totals["<unmapped>"], 2);
        fs::remove_dir_all(&dir).unwrap();
    }
}
