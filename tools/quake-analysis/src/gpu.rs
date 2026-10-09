//! Summarize PSoXide GP0 counters over complete gameplay presentations.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::csv::{dict_rows, read_text, records};
use crate::json::Json;
use crate::num::parse_int;

/// The counters reported per presentation, in output order.
pub const GPU_FIELDS: [&str; 16] = [
    "frame_draw_words",
    "commands",
    "draws",
    "fills",
    "textured_tris",
    "textured_quads",
    "textured_rects",
    "texture_windows",
    "texture_window_changes",
    "texture_window_redundant",
    "gpu_cycles",
    "fill_cycles",
    "textured_tri_cycles",
    "textured_quad_cycles",
    "textured_rect_cycles",
    "other_cycles",
];

/// The cycle fields whose share of `gpu_cycles` is reported.
const SHARE_FIELDS: [&str; 5] = [
    "fill_cycles",
    "textured_tri_cycles",
    "textured_quad_cycles",
    "textured_rect_cycles",
    "other_cycles",
];

/// Counters of one route tick, keyed by column name.
pub type TickRow = HashMap<String, i128>;
/// Rows keyed by route tick.
pub type Ticks = BTreeMap<i128, TickRow>;

fn integer(value: &str) -> Result<i128, String> {
    if value.is_empty() {
        return Ok(0);
    }
    parse_int(value, 0).ok_or_else(|| format!("invalid integer literal {value:?}"))
}

/// Read a per-tick CSV into rows keyed by `route_tick`. A later row for the
/// same tick replaces an earlier one.
pub fn read_rows(path: &Path) -> Result<Ticks, String> {
    let text = read_text(path)?;
    let (_, rows) = dict_rows(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Ticks::new();
    for raw in rows {
        let tick = match raw.get("route_tick") {
            Some(value) => integer(value)?,
            None => 0,
        };
        let mut row = TickRow::new();
        for (key, value) in &raw {
            if key != "route_tick" {
                row.insert(key.clone(), integer(value)?);
            }
        }
        out.insert(tick, row);
    }
    Ok(out)
}

/// Cycle stamps of the `ReadN` (0x06) commands in a CD command log.
pub fn readn_cycles(path: &Path) -> Result<Vec<i128>, String> {
    let text = read_text(path)?;
    let mut out = Vec::new();
    for row in records(&text).iter().skip(1) {
        if row.len() > 1 && row[1] == "0x06" {
            let cycle = parse_int(&row[0], 10)
                .ok_or_else(|| format!("invalid integer literal {:?}", row[0]))?;
            out.push(cycle);
        }
    }
    Ok(out)
}

fn field(row: &TickRow, name: &str) -> i128 {
    row.get(name).copied().unwrap_or(0)
}

/// The first and last display-start change inside the longest gap between
/// two `ReadN` cycles (the gameplay window between the load and the next
/// map transition).
pub fn gameplay_bounds(route: &Ticks, cd_path: &Path) -> Result<(i128, i128), String> {
    let reads = readn_cycles(cd_path)?;
    if reads.len() < 2 {
        return Err("CD log has fewer than two ReadN sessions".to_owned());
    }
    // Strict comparison keeps the first of equal gaps.
    let (mut initial_last, mut transition) = (reads[0], reads[1]);
    for pair in reads.windows(2) {
        if pair[1] - pair[0] > transition - initial_last {
            initial_last = pair[0];
            transition = pair[1];
        }
    }
    let presents: Vec<i128> = route
        .iter()
        .filter(|(_, row)| {
            field(row, "display_start_changed") != 0 && {
                let cycles = field(row, "bus_cycles");
                initial_last < cycles && cycles < transition
            }
        })
        .map(|(tick, _)| *tick)
        .collect();
    if presents.len() < 2 {
        return Err("gameplay window has fewer than two presentations".to_owned());
    }
    Ok((presents[0], presents[presents.len() - 1]))
}

/// Nearest-rank percentile of `values` for a fraction in `0..=1`.
pub fn nearest_rank(values: &[i128], fraction: f64) -> i128 {
    if values.is_empty() {
        return 0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    let rank = (ordered.len() as f64 * fraction).ceil() as i64 - 1;
    ordered[rank.max(0) as usize]
}

/// Mean, median, 95th percentile and maximum of `values`.
pub fn distribution(values: &[i128]) -> Json {
    if values.is_empty() {
        return Json::obj([
            ("mean", Json::Float(0.0)),
            ("p50", Json::Int(0)),
            ("p95", Json::Int(0)),
            ("max", Json::Int(0)),
        ]);
    }
    let sum: i128 = values.iter().sum();
    Json::obj([
        ("mean", Json::Float(sum as f64 / values.len() as f64)),
        ("p50", Json::Int(nearest_rank(values, 0.50))),
        ("p95", Json::Int(nearest_rank(values, 0.95))),
        ("max", Json::Int(*values.iter().max().expect("non-empty"))),
    ])
}

/// Summarize the complete presentation intervals between two ticks.
pub fn summarize(
    gpu: &Ticks,
    route: &Ticks,
    first_present: i128,
    last_present: i128,
) -> Result<Json, String> {
    let presents: Vec<i128> = route
        .iter()
        .filter(|(tick, row)| {
            field(row, "display_start_changed") != 0
                && first_present <= **tick
                && **tick <= last_present
        })
        .map(|(tick, _)| *tick)
        .collect();
    let frames: Vec<(i128, i128)> = presents.windows(2).map(|w| (w[0], w[1])).collect();
    if frames.is_empty() {
        return Err("selected window has no complete presentation intervals".to_owned());
    }
    let values = |name: &str| -> Vec<i128> {
        frames
            .iter()
            .map(|&(previous, current)| {
                gpu.range(previous + 1..=current)
                    .map(|(_, row)| field(row, name))
                    .sum()
            })
            .collect()
    };
    let mut per_present = BTreeMap::new();
    let mut totals = BTreeMap::new();
    for name in GPU_FIELDS {
        let series = values(name);
        per_present.insert(name.to_owned(), distribution(&series));
        totals.insert(name.to_owned(), series.iter().sum::<i128>());
    }
    let gpu_cycles = totals["gpu_cycles"];
    let share: Vec<(&str, Json)> = SHARE_FIELDS
        .iter()
        .map(|name| {
            let value = if gpu_cycles != 0 {
                100.0 * totals[*name] as f64 / gpu_cycles as f64
            } else {
                0.0
            };
            (*name, Json::Float(value))
        })
        .collect();
    Ok(Json::obj([
        ("first_present_tick", Json::Int(first_present)),
        ("last_present_tick", Json::Int(last_present)),
        (
            "complete_present_intervals",
            Json::Int(frames.len() as i128),
        ),
        ("per_present", Json::Obj(per_present)),
        (
            "totals",
            Json::Obj(totals.into_iter().map(|(k, v)| (k, Json::Int(v))).collect()),
        ),
        ("gpu_cycle_share_percent", Json::obj(share)),
        (
            "notes",
            Json::Arr(vec![
                Json::Str("GP0 counts are direct PSoXide observations.".into()),
                Json::Str(
                    "GPU cycle fields are emulator estimates, not original-silicon timing.".into(),
                ),
            ]),
        ),
    ]))
}

/// Render the human-readable report.
pub fn render_text(summary: &Json) -> String {
    let mut out = format!(
        "PSoXide GP0 gameplay census: ticks={}..{} presents={}\n",
        summary.int("first_present_tick"),
        summary.int("last_present_tick"),
        summary.int("complete_present_intervals"),
    );
    let per_present = summary.get("per_present");
    for name in GPU_FIELDS {
        let row = per_present.get(name);
        out.push_str(&format!(
            "{name}: mean={:.2} p50={} p95={} max={}\n",
            row.float("mean"),
            row.int("p50"),
            row.int("p95"),
            row.int("max"),
        ));
    }
    let shares = summary.get("gpu_cycle_share_percent");
    let line: Vec<String> = SHARE_FIELDS
        .iter()
        .map(|name| format!("{name}={:.2}%", shares.float(name)))
        .collect();
    out.push_str(&format!("gpu cycle share: {}\n", line.join(" ")));
    out
}

#[cfg(test)]
mod tests {
    use super::{gameplay_bounds, read_rows, summarize};
    use std::fs;

    #[test]
    fn complete_present_intervals_and_cd_bounds() {
        let root = std::env::temp_dir().join(format!("quake-analysis-gpu-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let route_path = root.join("route.csv");
        let gpu_path = root.join("gpu.csv");
        let cd_path = root.join("cd.csv");

        let mut route = String::from("route_tick,bus_cycles,display_start_changed\n");
        for (tick, cycle, present) in [
            (1, 100, 1),
            (2, 250, 1),
            (3, 300, 0),
            (4, 400, 1),
            (5, 650, 1),
        ] {
            route.push_str(&format!("{tick},{cycle},{present}\n"));
        }
        fs::write(&route_path, route).unwrap();
        let mut gpu = String::from("route_tick,commands,gpu_cycles\n");
        for (index, commands) in [0, 10, 20, 30, 40].iter().enumerate() {
            gpu.push_str(&format!("{},{commands},{}\n", index + 1, commands * 2));
        }
        fs::write(&gpu_path, gpu).unwrap();
        fs::write(&cd_path, "cycle,command\n200,0x06\n500,0x06\n700,0x06\n").unwrap();

        let route = read_rows(&route_path).unwrap();
        assert_eq!(gameplay_bounds(&route, &cd_path).unwrap(), (2, 4));
        let summary = summarize(&read_rows(&gpu_path).unwrap(), &route, 2, 4).unwrap();
        assert_eq!(summary.int("complete_present_intervals"), 1);
        assert_eq!(
            summary.get("per_present").get("commands").float("mean"),
            50.0
        );
        assert_eq!(
            summary.get("per_present").get("gpu_cycles").float("mean"),
            100.0
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
