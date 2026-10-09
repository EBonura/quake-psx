//! Summarize QRC1-QRC5 renderer census lines from PSoXide logs.
//!
//! The guest emits hexadecimal positional fields. Passing both deterministic
//! route logs makes this tool reject any frame-level census mismatch before it
//! reports structural optimization bounds.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::csv::read_text;
use crate::json::Json;
use crate::num::{parse_int, split_lines};

const FIELDS_V1: [&str; 39] = [
    "frame",
    "leaf",
    "portal_leaf",
    "visibility_rebuilt",
    "pvs_faces",
    "policy_rejects",
    "backface_rejects",
    "frustum_rejects",
    "selected_faces",
    "near_faces",
    "water_blend_faces",
    "plane_tests",
    "plane_run_tests",
    "plane_tests_saved",
    "max_plane_run",
    "aabb_tests",
    "block4_groups",
    "block4_rejected_groups",
    "block4_rejected_faces",
    "block4_aabb_tests_saved",
    "block8_groups",
    "block8_rejected_groups",
    "block8_rejected_faces",
    "block8_aabb_tests_saved",
    "block16_groups",
    "block16_rejected_groups",
    "block16_rejected_faces",
    "block16_aabb_tests_saved",
    "candidate_corners",
    "unique_positions",
    "projection_batches",
    "previous_face_reuses",
    "previous_two_face_reuses",
    "near_corners",
    "special_corners",
    "layered_sky_corners",
    "oversized_corners",
    "selected_hash_a",
    "selected_hash_b",
];

const V2_TAIL: [&str; 10] = [
    "ordinary_base_packet_bytes",
    "resident_template_faces",
    "resident_template_packet_bytes",
    "dynamic_light_template_reject_bytes",
    "packet_arena_words",
    "emitted_packets",
    "hardware_triangles",
    "packet_overflow_avoided",
    "selected_hash_a",
    "selected_hash_b",
];

const V3_MIDDLE: [&str; 18] = [
    "ordinary_output_packet_bytes",
    "ordinary_output_packets",
    "ordinary_output_hardware_triangles",
    "topology_surfaces",
    "topology_root_triangles",
    "topology_surface_clip_rejects",
    "topology_depth_rejects",
    "topology_level0_root_triangles",
    "topology_level1_root_triangles",
    "topology_level2_root_triangles",
    "topology_paired_level0_packets",
    "topology_level1_underdraw_roots",
    "topology_level2_underdraw_roots",
    "topology_theoretical_packets",
    "topology_theoretical_hardware_triangles",
    "topology_theoretical_packet_bytes",
    "topology_hash_a",
    "topology_hash_b",
];

/// Subdivision cache budgets (KiB per pool) the guest measures.
pub const SUBDIVISION_CACHE_BUDGETS_KIB: [i128; 4] = [16, 32, 48, 64];
const SUBDIVISION_CACHE_SLOT_BYTES: i128 = 748;
const SUBDIVISION_LEVEL1_SLOT_BYTES: i128 = 252;
const SUBDIVISION_LEVEL2_SLOT_BYTES: i128 = 748;
const SUBDIVISION_CACHE_METRICS: [&str; 9] = [
    "requests",
    "hits",
    "allocations",
    "replacements",
    "fallbacks",
    "resident",
    "requested_packet_bytes",
    "hit_packet_bytes",
    "hit_invariant_bytes",
];

fn cache_fields(kind: &str) -> Vec<String> {
    let mut out = Vec::new();
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
        for metric in SUBDIVISION_CACHE_METRICS {
            out.push(format!("subdiv_{kind}_{budget}k_{metric}"));
        }
    }
    out
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

/// The positional field names of schema `version` (1 to 5).
pub fn fields(version: u8) -> Vec<String> {
    let v1 = strings(&FIELDS_V1);
    let mut v2 = v1[..v1.len() - 2].to_vec();
    v2.extend(strings(&V2_TAIL));
    let split = v2.len() - 6;
    let mut v3 = v2[..split].to_vec();
    v3.extend(strings(&V3_MIDDLE));
    v3.extend_from_slice(&v2[split..]);
    let with_caches = |kind: &str| {
        let split = v3.len() - 6;
        let mut out = v3[..split].to_vec();
        out.extend(cache_fields(kind));
        out.extend_from_slice(&v3[split..]);
        out
    };
    match version {
        1 => v1,
        2 => v2,
        3 => v3,
        4 => with_caches("cache"),
        _ => with_caches("slab"),
    }
}

/// Every field name any schema carries, in first-seen order.
pub fn all_fields() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in fields(4).into_iter().chain(cache_fields("slab")) {
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// One decoded census line. Fields a schema does not carry read as zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Schema number (1 to 5).
    pub schema: u8,
    /// Value of every field in [`all_fields`].
    pub values: HashMap<String, i128>,
}

impl Row {
    /// Value of the field `name`. Panics on an unknown name.
    pub fn get(&self, name: &str) -> i128 {
        *self
            .values
            .get(name)
            .unwrap_or_else(|| panic!("unknown census field {name}"))
    }

    /// Set the field `name`.
    pub fn set(&mut self, name: &str, value: i128) {
        self.values.insert(name.to_owned(), value);
    }
}

type Result<T> = std::result::Result<T, String>;

fn parse_payload(payload: &str, source: &str, line_number: usize) -> Result<Row> {
    let parts: Vec<&str> = payload.split(',').collect();
    let schema = parts[0];
    let version: u8 = match schema {
        "QRC5" => 5,
        "QRC4" => 4,
        "QRC3" => 3,
        "QRC2" => 2,
        "QRC1" => 1,
        _ => {
            return Err(format!(
                "{source}:{line_number}: unknown renderer census schema {schema}"
            ))
        }
    };
    let schema_fields = fields(version);
    if parts.len() != schema_fields.len() + 1 {
        return Err(format!(
            "{source}:{line_number}: {schema} has {} fields; expected {}",
            parts.len() - 1,
            schema_fields.len()
        ));
    }
    let mut values: HashMap<String, i128> =
        all_fields().into_iter().map(|name| (name, 0)).collect();
    for (name, value) in schema_fields.iter().zip(&parts[1..]) {
        let parsed = parse_int(value, 16)
            .ok_or_else(|| format!("{source}:{line_number}: invalid hexadecimal {schema} field"))?;
        values.insert(name.clone(), parsed);
    }
    let row = Row {
        schema: version,
        values,
    };
    validate(&row, source, line_number)?;
    Ok(row)
}

fn validate(row: &Row, source: &str, line_number: usize) -> Result<()> {
    let where_ = format!("{source}:{line_number}");
    let fail = |message: String| -> Result<()> { Err(format!("{where_}: {message}")) };
    let g = |name: &str| row.get(name);

    let funnel =
        g("policy_rejects") + g("backface_rejects") + g("frustum_rejects") + g("selected_faces");
    if funnel != g("pvs_faces") {
        return fail("selection funnel does not equal PVS face count".into());
    }
    if g("aabb_tests") != g("frustum_rejects") + g("selected_faces") {
        return fail("AABB test count does not match the selection funnel".into());
    }
    if g("plane_tests_saved") != g("plane_tests") - g("plane_run_tests") {
        return fail("plane-run saving is internally inconsistent".into());
    }
    if g("near_faces") > g("selected_faces") {
        return fail("near face count exceeds selected face count".into());
    }
    if g("unique_positions") > g("candidate_corners") {
        return fail("unique projected positions exceed candidate corners".into());
    }
    let available_reuses = g("candidate_corners") - g("unique_positions");
    if !(g("previous_face_reuses") <= g("previous_two_face_reuses")
        && g("previous_two_face_reuses") <= available_reuses)
    {
        return fail("adjacent projection reuse counts are impossible".into());
    }
    for size in [4, 8, 16] {
        if g(&format!("block{size}_aabb_tests_saved")) > g("aabb_tests") {
            return fail(format!("block-{size} saves more AABB tests than exist"));
        }
        if g(&format!("block{size}_rejected_groups")) > g(&format!("block{size}_groups")) {
            return fail(format!("block-{size} rejected group count is impossible"));
        }
    }
    if g("resident_template_faces") > g("selected_faces") {
        return fail("resident-template face count exceeds selected faces".into());
    }
    if g("resident_template_packet_bytes") > g("ordinary_base_packet_bytes") {
        return fail("resident-template bytes exceed ordinary base packets".into());
    }
    if g("packet_overflow_avoided") > 1 {
        return fail("packet-overflow flag is not boolean".into());
    }
    if row.schema >= 3 {
        let classified_roots = g("topology_depth_rejects")
            + g("topology_level0_root_triangles")
            + g("topology_level1_root_triangles")
            + g("topology_level2_root_triangles");
        if classified_roots > g("topology_root_triangles") {
            return fail("topology classifies more roots than exist".into());
        }
        if 2 * g("topology_paired_level0_packets") > g("topology_level0_root_triangles") {
            return fail("level-zero pairing count is impossible".into());
        }
        if g("topology_level1_underdraw_roots") > g("topology_level1_root_triangles") {
            return fail("level-one underdraw count is impossible".into());
        }
        if g("topology_level2_underdraw_roots") > g("topology_level2_root_triangles") {
            return fail("level-two underdraw count is impossible".into());
        }
        if g("ordinary_output_packet_bytes") > g("topology_theoretical_packet_bytes") {
            return fail("ordinary output exceeds theoretical packet bytes".into());
        }
        if g("ordinary_output_packets") > g("topology_theoretical_packets") {
            return fail("ordinary output exceeds theoretical packet count".into());
        }
        if g("ordinary_output_hardware_triangles") > g("topology_theoretical_hardware_triangles") {
            return fail("ordinary output exceeds theoretical triangle count".into());
        }
    }
    if row.schema == 4 {
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
            let p = format!("subdiv_cache_{budget}k_");
            let m = |metric: &str| g(&format!("{p}{metric}"));
            if m("hits") + m("allocations") + m("fallbacks") != m("requests") {
                return fail(format!("{budget} KiB cache request partition is invalid"));
            }
            if m("replacements") > m("allocations") {
                return fail(format!(
                    "{budget} KiB cache replacements exceed allocations"
                ));
            }
            let capacity = budget * 1024 / SUBDIVISION_CACHE_SLOT_BYTES;
            if m("resident") > capacity {
                return fail(format!(
                    "{budget} KiB cache resident count exceeds capacity"
                ));
            }
            if m("hit_packet_bytes") > m("requested_packet_bytes") {
                return fail(format!(
                    "{budget} KiB cache hit bytes exceed requested bytes"
                ));
            }
        }
    }
    if row.schema >= 5 {
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
            let p = format!("subdiv_slab_{budget}k_");
            let m = |metric: &str| g(&format!("{p}{metric}"));
            if m("hits") + m("allocations") + m("fallbacks") != m("requests") {
                return fail(format!("{budget} KiB slab request partition is invalid"));
            }
            if m("replacements") > m("allocations") {
                return fail(format!("{budget} KiB slab replacements exceed allocations"));
            }
            let (level1, level2) = slab_capacities(budget);
            if m("resident") > level1 + level2 {
                return fail(format!("{budget} KiB slab resident count exceeds capacity"));
            }
            if m("hit_packet_bytes") > m("requested_packet_bytes") {
                return fail(format!(
                    "{budget} KiB slab hit bytes exceed requested bytes"
                ));
            }
        }
    }
    Ok(())
}

fn slab_capacities(budget: i128) -> (i128, i128) {
    let budget_bytes = budget * 1024;
    let level1 = budget_bytes * 3 / 5 / SUBDIVISION_LEVEL1_SLOT_BYTES;
    let level2 =
        (budget_bytes - level1 * SUBDIVISION_LEVEL1_SLOT_BYTES) / SUBDIVISION_LEVEL2_SLOT_BYTES;
    (level1, level2)
}

/// Decode every census line in `lines`; other lines are ignored. Fails when
/// a line is malformed or inconsistent, or when no census line is present.
pub fn parse_lines<'a, I>(lines: I, source: &str) -> Result<Vec<Row>>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut rows = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        let marker = ["QRC5,", "QRC4,", "QRC3,", "QRC2,", "QRC1,"]
            .iter()
            .find_map(|m| line.find(m));
        let Some(marker) = marker else { continue };
        let payload = line[marker..].split_whitespace().next().unwrap_or("");
        rows.push(parse_payload(payload, source, index + 1)?);
    }
    if rows.is_empty() {
        return Err(format!(
            "{source}: no QRC1-QRC5 renderer census lines found"
        ));
    }
    Ok(rows)
}

/// Decode the census lines of a log file.
pub fn parse_log(path: &Path) -> Result<Vec<Row>> {
    let text = read_text(path)?;
    parse_lines(split_lines(&text), &path.display().to_string())
}

/// Require two runs to agree on every row and field.
pub fn require_deterministic(first: &[Row], second: &[Row]) -> Result<()> {
    if first.len() != second.len() {
        return Err(format!(
            "renderer census row count differs: run-a={}, run-b={}",
            first.len(),
            second.len()
        ));
    }
    let names = all_fields();
    for (index, (left, right)) in first.iter().zip(second).enumerate() {
        if left.schema != right.schema {
            return Err(format!("renderer census schema differs at row {index}"));
        }
        if left != right {
            let details: Vec<String> = names
                .iter()
                .filter(|name| left.get(name) != right.get(name))
                .take(6)
                .map(|name| format!("{name}=0x{:x}/0x{:x}", left.get(name), right.get(name)))
                .collect();
            return Err(format!(
                "renderer census differs at row {index}: {}",
                details.join(", ")
            ));
        }
    }
    Ok(())
}

fn sum(rows: &[Row], field: &str) -> i128 {
    rows.iter().map(|row| row.get(field)).sum()
}

fn column(rows: &[Row], field: &str) -> Vec<i128> {
    rows.iter().map(|row| row.get(field)).collect()
}

fn maximum(rows: &[Row], field: &str) -> i128 {
    rows.iter()
        .map(|row| row.get(field))
        .max()
        .expect("rows are non-empty")
}

fn percent(numerator: i128, denominator: i128) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        100.0 * numerator as f64 / denominator as f64
    }
}

fn percentile(values: &[i128], quantile: f64) -> i128 {
    if values.is_empty() {
        return 0;
    }
    let mut ordered = values.to_vec();
    ordered.sort_unstable();
    let index = (quantile * ordered.len() as f64).ceil() as i64 - 1;
    ordered[index.max(0) as usize]
}

fn int(value: i128) -> Json {
    Json::Int(value)
}

fn fingerprint_stability(rows: &[Row]) -> Json {
    let active: Vec<&Row> = rows
        .iter()
        .filter(|row| row.get("pvs_faces") != 0)
        .collect();
    let (mut same, mut longest, mut run) = (0i128, 0i128, 0i128);
    let mut previous: Option<[i128; 3]> = None;
    for row in &active {
        let fingerprint = [
            row.get("selected_faces"),
            row.get("selected_hash_a"),
            row.get("selected_hash_b"),
        ];
        if Some(fingerprint) == previous {
            same += 1;
            run += 1;
        } else {
            run = 1;
        }
        longest = longest.max(run);
        previous = Some(fingerprint);
    }
    let transitions = (active.len() as i128 - 1).max(0);
    Json::obj([
        ("active_frames", int(active.len() as i128)),
        ("same_as_previous", int(same)),
        ("transitions", int(transitions)),
        ("same_percent", Json::Float(percent(same, transitions))),
        ("longest_identical_run", int(longest)),
    ])
}

fn topology_stability(rows: &[Row]) -> Json {
    let active: Vec<&Row> = rows
        .iter()
        .filter(|row| row.get("topology_surfaces") != 0)
        .collect();
    let (mut same_topology, mut same_selection, mut same_given) = (0i128, 0i128, 0i128);
    let (mut longest, mut run) = (0i128, 0i128);
    let mut previous_topology: Option<[i128; 4]> = None;
    let mut previous_selection: Option<[i128; 3]> = None;
    for row in &active {
        let topology = [
            row.get("topology_root_triangles"),
            row.get("topology_theoretical_packet_bytes"),
            row.get("topology_hash_a"),
            row.get("topology_hash_b"),
        ];
        let selection = [
            row.get("selected_faces"),
            row.get("selected_hash_a"),
            row.get("selected_hash_b"),
        ];
        if Some(topology) == previous_topology {
            same_topology += 1;
            run += 1;
        } else {
            run = 1;
        }
        if previous_selection.is_some() && Some(selection) == previous_selection {
            same_selection += 1;
            if Some(topology) == previous_topology {
                same_given += 1;
            }
        }
        longest = longest.max(run);
        previous_topology = Some(topology);
        previous_selection = Some(selection);
    }
    let transitions = (active.len() as i128 - 1).max(0);
    Json::obj([
        ("active_frames", int(active.len() as i128)),
        ("same_as_previous", int(same_topology)),
        ("transitions", int(transitions)),
        (
            "same_percent",
            Json::Float(percent(same_topology, transitions)),
        ),
        ("longest_identical_run", int(longest)),
        ("same_selection_transitions", int(same_selection)),
        ("same_topology_given_selection", int(same_given)),
        (
            "same_given_selection_percent",
            Json::Float(percent(same_given, same_selection)),
        ),
    ])
}

fn cache_summary(rows: &[Row], kind: &str) -> Json {
    let mut out = BTreeMap::new();
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
        let prefix = format!("subdiv_{kind}_{budget}k_");
        let total = |metric: &str| sum(rows, &format!("{prefix}{metric}"));
        let requests = total("requests");
        let hits = total("hits");
        let fallbacks = total("fallbacks");
        let requested_packet_bytes = total("requested_packet_bytes");
        let hit_packet_bytes = total("hit_packet_bytes");
        let hit_invariant_bytes = total("hit_invariant_bytes");
        let resident = column(rows, &format!("{prefix}resident"));
        let mut entry: Vec<(&str, Json)> = vec![
            ("per_pool_kib", int(budget)),
            ("dual_pool_kib", int(2 * budget)),
        ];
        if kind == "cache" {
            entry.push(("slot_bytes", int(SUBDIVISION_CACHE_SLOT_BYTES)));
            entry.push((
                "capacity",
                int(budget * 1024 / SUBDIVISION_CACHE_SLOT_BYTES),
            ));
        } else {
            let (level1, level2) = slab_capacities(budget);
            entry.push(("level1_capacity", int(level1)));
            entry.push(("level2_capacity", int(level2)));
            entry.push(("capacity", int(level1 + level2)));
        }
        entry.extend([
            ("requests", int(requests)),
            ("hits", int(hits)),
            ("hit_percent", Json::Float(percent(hits, requests))),
            ("allocations", int(total("allocations"))),
            ("replacements", int(total("replacements"))),
            ("fallbacks", int(fallbacks)),
            (
                "fallback_percent",
                Json::Float(percent(fallbacks, requests)),
            ),
            ("resident_p50", int(percentile(&resident, 0.50))),
            ("resident_p95", int(percentile(&resident, 0.95))),
            (
                "resident_max",
                int(*resident.iter().max().expect("non-empty")),
            ),
            ("requested_packet_bytes", int(requested_packet_bytes)),
            ("hit_packet_bytes", int(hit_packet_bytes)),
            (
                "hit_packet_byte_percent",
                Json::Float(percent(hit_packet_bytes, requested_packet_bytes)),
            ),
            ("hit_invariant_bytes", int(hit_invariant_bytes)),
            (
                "invariant_reuse_percent_of_requested_bytes",
                Json::Float(percent(hit_invariant_bytes, requested_packet_bytes)),
            ),
        ]);
        out.insert(budget.to_string(), Json::obj(entry));
    }
    Json::Obj(out)
}

/// Compute the full census summary of `rows` (which must not be empty).
pub fn summarize(rows: &[Row]) -> Json {
    let schema = rows[0].schema;
    let pvs = sum(rows, "pvs_faces");
    let selected = sum(rows, "selected_faces");
    let near = sum(rows, "near_faces");
    let selected_series = column(rows, "selected_faces");
    let selection = Json::obj([
        ("pvs_faces", int(pvs)),
        ("policy_rejects", int(sum(rows, "policy_rejects"))),
        ("backface_rejects", int(sum(rows, "backface_rejects"))),
        ("frustum_rejects", int(sum(rows, "frustum_rejects"))),
        ("selected_faces", int(selected)),
        ("near_faces", int(near)),
        ("water_blend_faces", int(sum(rows, "water_blend_faces"))),
        ("selected_p50", int(percentile(&selected_series, 0.50))),
        ("selected_p95", int(percentile(&selected_series, 0.95))),
        (
            "policy_reject_percent",
            Json::Float(percent(sum(rows, "policy_rejects"), pvs)),
        ),
        (
            "backface_reject_percent",
            Json::Float(percent(sum(rows, "backface_rejects"), pvs)),
        ),
        (
            "frustum_reject_percent",
            Json::Float(percent(sum(rows, "frustum_rejects"), pvs)),
        ),
        ("selected_percent", Json::Float(percent(selected, pvs))),
        (
            "near_selected_percent",
            Json::Float(percent(near, selected)),
        ),
    ]);

    let plane_tests = sum(rows, "plane_tests");
    let plane_run_tests = sum(rows, "plane_run_tests");
    let plane = Json::obj([
        ("current_tests", int(plane_tests)),
        ("run_cached_tests", int(plane_run_tests)),
        ("tests_saved", int(sum(rows, "plane_tests_saved"))),
        (
            "saving_percent",
            Json::Float(percent(plane_tests - plane_run_tests, plane_tests)),
        ),
        ("max_run", int(maximum(rows, "max_plane_run"))),
    ]);

    let baseline_aabb = sum(rows, "aabb_tests");
    let mut blocks = BTreeMap::new();
    for size in [4, 8, 16] {
        let groups = sum(rows, &format!("block{size}_groups"));
        let saved = sum(rows, &format!("block{size}_aabb_tests_saved"));
        let candidate = groups + baseline_aabb - saved;
        blocks.insert(
            size.to_string(),
            Json::obj([
                ("baseline_aabb_tests", int(baseline_aabb)),
                ("group_tests", int(groups)),
                (
                    "rejected_groups",
                    int(sum(rows, &format!("block{size}_rejected_groups"))),
                ),
                (
                    "rejected_faces",
                    int(sum(rows, &format!("block{size}_rejected_faces"))),
                ),
                ("individual_tests_saved", int(saved)),
                ("candidate_aabb_tests", int(candidate)),
                ("net_tests_saved", int(baseline_aabb - candidate)),
                (
                    "net_saving_percent",
                    Json::Float(percent(baseline_aabb - candidate, baseline_aabb)),
                ),
            ]),
        );
    }

    let candidate_corners = sum(rows, "candidate_corners");
    let unique_positions = sum(rows, "unique_positions");
    let projection = Json::obj([
        ("candidate_corners", int(candidate_corners)),
        ("unique_positions", int(unique_positions)),
        ("batches", int(sum(rows, "projection_batches"))),
        (
            "transforms_saved",
            int(candidate_corners - unique_positions),
        ),
        (
            "transform_saving_percent",
            Json::Float(percent(
                candidate_corners - unique_positions,
                candidate_corners,
            )),
        ),
        (
            "corners_per_unique_position",
            Json::Float(if unique_positions == 0 {
                0.0
            } else {
                candidate_corners as f64 / unique_positions as f64
            }),
        ),
        (
            "previous_face_reuses",
            int(sum(rows, "previous_face_reuses")),
        ),
        (
            "previous_two_face_reuses",
            int(sum(rows, "previous_two_face_reuses")),
        ),
        ("near_fallback_corners", int(sum(rows, "near_corners"))),
        (
            "special_fallback_corners",
            int(sum(rows, "special_corners")),
        ),
        ("layered_sky_corners", int(sum(rows, "layered_sky_corners"))),
        (
            "oversized_fallback_corners",
            int(sum(rows, "oversized_corners")),
        ),
    ]);

    let ordinary_base_packet_bytes = sum(rows, "ordinary_base_packet_bytes");
    let resident_template_packet_bytes = sum(rows, "resident_template_packet_bytes");
    let base_series = column(rows, "ordinary_base_packet_bytes");
    let template_series = column(rows, "resident_template_packet_bytes");
    let resident_packets = Json::obj([
        (
            "ordinary_base_packet_bytes",
            int(ordinary_base_packet_bytes),
        ),
        ("ordinary_base_p50", int(percentile(&base_series, 0.50))),
        ("ordinary_base_p95", int(percentile(&base_series, 0.95))),
        (
            "ordinary_base_max",
            int(maximum(rows, "ordinary_base_packet_bytes")),
        ),
        ("template_faces", int(sum(rows, "resident_template_faces"))),
        ("template_packet_bytes", int(resident_template_packet_bytes)),
        ("template_p50", int(percentile(&template_series, 0.50))),
        ("template_p95", int(percentile(&template_series, 0.95))),
        (
            "template_max",
            int(maximum(rows, "resident_template_packet_bytes")),
        ),
        (
            "template_coverage_percent",
            Json::Float(percent(
                resident_template_packet_bytes,
                ordinary_base_packet_bytes,
            )),
        ),
        (
            "dynamic_light_reject_bytes",
            int(sum(rows, "dynamic_light_template_reject_bytes")),
        ),
    ]);

    let arena_series = column(rows, "packet_arena_words");
    let arena = Json::obj([
        ("words_p50", int(percentile(&arena_series, 0.50))),
        ("words_p95", int(percentile(&arena_series, 0.95))),
        ("words_max", int(maximum(rows, "packet_arena_words"))),
        ("bytes_p50", int(4 * percentile(&arena_series, 0.50))),
        ("bytes_p95", int(4 * percentile(&arena_series, 0.95))),
        ("bytes_max", int(4 * maximum(rows, "packet_arena_words"))),
        ("emitted_packets", int(sum(rows, "emitted_packets"))),
        ("hardware_triangles", int(sum(rows, "hardware_triangles"))),
        ("overflow_frames", int(sum(rows, "packet_overflow_avoided"))),
    ]);

    let topology_roots = sum(rows, "topology_root_triangles");
    let theoretical_packet_bytes = sum(rows, "topology_theoretical_packet_bytes");
    let ordinary_output_packet_bytes = sum(rows, "ordinary_output_packet_bytes");
    let classified_roots: i128 = rows
        .iter()
        .map(|row| {
            row.get("topology_depth_rejects")
                + row.get("topology_level0_root_triangles")
                + row.get("topology_level1_root_triangles")
                + row.get("topology_level2_root_triangles")
        })
        .sum();
    let surface_clip_rejected_roots = topology_roots - classified_roots;
    let prefix_candidates: Vec<i128> = rows
        .iter()
        .map(|row| {
            4 * row.get("packet_arena_words") + row.get("topology_theoretical_packet_bytes")
                - row.get("ordinary_output_packet_bytes")
        })
        .collect();
    let theoretical_series = column(rows, "topology_theoretical_packet_bytes");
    let output_series = column(rows, "ordinary_output_packet_bytes");
    let topology = Json::obj([
        ("surfaces", int(sum(rows, "topology_surfaces"))),
        ("root_triangles", int(topology_roots)),
        (
            "surface_clip_rejects",
            int(sum(rows, "topology_surface_clip_rejects")),
        ),
        (
            "surface_clip_rejected_roots",
            int(surface_clip_rejected_roots),
        ),
        (
            "surface_clip_rejected_root_percent",
            Json::Float(percent(surface_clip_rejected_roots, topology_roots)),
        ),
        ("depth_rejects", int(sum(rows, "topology_depth_rejects"))),
        (
            "level0_root_triangles",
            int(sum(rows, "topology_level0_root_triangles")),
        ),
        (
            "level1_root_triangles",
            int(sum(rows, "topology_level1_root_triangles")),
        ),
        (
            "level2_root_triangles",
            int(sum(rows, "topology_level2_root_triangles")),
        ),
        (
            "paired_level0_packets",
            int(sum(rows, "topology_paired_level0_packets")),
        ),
        (
            "level1_underdraw_roots",
            int(sum(rows, "topology_level1_underdraw_roots")),
        ),
        (
            "level2_underdraw_roots",
            int(sum(rows, "topology_level2_underdraw_roots")),
        ),
        (
            "theoretical_packets",
            int(sum(rows, "topology_theoretical_packets")),
        ),
        (
            "theoretical_hardware_triangles",
            int(sum(rows, "topology_theoretical_hardware_triangles")),
        ),
        ("theoretical_packet_bytes", int(theoretical_packet_bytes)),
        (
            "theoretical_bytes_p50",
            int(percentile(&theoretical_series, 0.50)),
        ),
        (
            "theoretical_bytes_p95",
            int(percentile(&theoretical_series, 0.95)),
        ),
        (
            "theoretical_bytes_max",
            int(maximum(rows, "topology_theoretical_packet_bytes")),
        ),
        ("actual_packets", int(sum(rows, "ordinary_output_packets"))),
        (
            "actual_hardware_triangles",
            int(sum(rows, "ordinary_output_hardware_triangles")),
        ),
        ("actual_packet_bytes", int(ordinary_output_packet_bytes)),
        ("actual_bytes_p50", int(percentile(&output_series, 0.50))),
        ("actual_bytes_p95", int(percentile(&output_series, 0.95))),
        (
            "actual_bytes_max",
            int(maximum(rows, "ordinary_output_packet_bytes")),
        ),
        (
            "screen_rejected_packet_bytes",
            int(theoretical_packet_bytes - ordinary_output_packet_bytes),
        ),
        (
            "screen_rejected_percent",
            Json::Float(percent(
                theoretical_packet_bytes - ordinary_output_packet_bytes,
                theoretical_packet_bytes,
            )),
        ),
        (
            "actual_vs_base_percent",
            Json::Float(percent(
                ordinary_output_packet_bytes,
                ordinary_base_packet_bytes,
            )),
        ),
        (
            "topology_prefix_candidate_p50",
            int(percentile(&prefix_candidates, 0.50)),
        ),
        (
            "topology_prefix_candidate_p95",
            int(percentile(&prefix_candidates, 0.95)),
        ),
        (
            "topology_prefix_candidate_max",
            int(*prefix_candidates.iter().max().expect("non-empty")),
        ),
        (
            "topology_prefix_candidate_over_120k_frames",
            int(prefix_candidates
                .iter()
                .filter(|v| **v > 120 * 1024)
                .count() as i128),
        ),
        (
            "level0_percent",
            Json::Float(percent(
                sum(rows, "topology_level0_root_triangles"),
                topology_roots,
            )),
        ),
        (
            "level1_percent",
            Json::Float(percent(
                sum(rows, "topology_level1_root_triangles"),
                topology_roots,
            )),
        ),
        (
            "level2_percent",
            Json::Float(percent(
                sum(rows, "topology_level2_root_triangles"),
                topology_roots,
            )),
        ),
    ]);

    let empty = || Json::Obj(BTreeMap::new());
    Json::obj([
        ("schema", Json::Str(format!("QRC{schema}"))),
        ("frames", int(rows.len() as i128)),
        ("visibility_rebuilds", int(sum(rows, "visibility_rebuilt"))),
        ("selection", selection),
        ("plane_runs", plane),
        ("block_aabb", Json::Obj(blocks)),
        ("projection", projection),
        ("resident_packets", resident_packets),
        ("arena", arena),
        ("selected_fingerprint", fingerprint_stability(rows)),
        ("adaptive_topology", topology),
        ("topology_fingerprint", topology_stability(rows)),
        (
            "subdivision_caches",
            if schema == 4 {
                cache_summary(rows, "cache")
            } else {
                empty()
            },
        ),
        (
            "subdivision_slab_caches",
            if schema >= 5 {
                cache_summary(rows, "slab")
            } else {
                empty()
            },
        ),
    ])
}

/// Render the human-readable report.
pub fn render_text(summary: &Json, deterministic_runs: usize) -> String {
    let selection = summary.get("selection");
    let plane = summary.get("plane_runs");
    let blocks = summary.get("block_aabb");
    let projection = summary.get("projection");
    let resident = summary.get("resident_packets");
    let arena = summary.get("arena");
    let fingerprint = summary.get("selected_fingerprint");
    let topology = summary.get("adaptive_topology");
    let topology_fingerprint = summary.get("topology_fingerprint");
    let schema = summary.text("schema");

    let mut lines: Vec<String> = vec![
        "quake-psx renderer census (diagnostic work; timing is invalid)".to_owned(),
        format!(
            "frames={} deterministic_runs={deterministic_runs} visibility_rebuilds={}",
            summary.int("frames"),
            summary.int("visibility_rebuilds")
        ),
        format!(
            "selection: pvs={} policy={} ({:.2}%) backface={} ({:.2}%) frustum={} ({:.2}%) \
             selected={} ({:.2}%)",
            selection.int("pvs_faces"),
            selection.int("policy_rejects"),
            selection.float("policy_reject_percent"),
            selection.int("backface_rejects"),
            selection.float("backface_reject_percent"),
            selection.int("frustum_rejects"),
            selection.float("frustum_reject_percent"),
            selection.int("selected_faces"),
            selection.float("selected_percent"),
        ),
        format!(
            "selected/frame: p50={} p95={} near={} ({:.2}% of selected) water_blend={}",
            selection.int("selected_p50"),
            selection.int("selected_p95"),
            selection.int("near_faces"),
            selection.float("near_selected_percent"),
            selection.int("water_blend_faces"),
        ),
        format!(
            "same-plane runs: tests={} cached={} saved={} ({:.2}%) max_run={}",
            plane.int("current_tests"),
            plane.int("run_cached_tests"),
            plane.int("tests_saved"),
            plane.float("saving_percent"),
            plane.int("max_run"),
        ),
    ];
    for size in [4, 8, 16] {
        let block = blocks.get(&size.to_string());
        lines.push(format!(
            "block-{size} AABB: baseline={} group_tests={} rejected_groups={} rejected_faces={} \
             individual_saved={} candidate={} net_saved={} ({:.2}%)",
            block.int("baseline_aabb_tests"),
            block.int("group_tests"),
            block.int("rejected_groups"),
            block.int("rejected_faces"),
            block.int("individual_tests_saved"),
            block.int("candidate_aabb_tests"),
            block.int("net_tests_saved"),
            block.float("net_saving_percent"),
        ));
    }
    let corners = projection.int("candidate_corners");
    let corner_divisor = corners.max(1);
    lines.push(format!(
        "batch shared projection: corners={corners} unique_positions={} batches={} \
         transforms_saved={} ({:.2}%) corners/unique={:.3}",
        projection.int("unique_positions"),
        projection.int("batches"),
        projection.int("transforms_saved"),
        projection.float("transform_saving_percent"),
        projection.float("corners_per_unique_position"),
    ));
    lines.push(format!(
        "adjacent projection reuse: previous_face={} ({:.2}% of corners) previous_two={} \
         ({:.2}% of corners)",
        projection.int("previous_face_reuses"),
        100.0 * projection.int("previous_face_reuses") as f64 / corner_divisor as f64,
        projection.int("previous_two_face_reuses"),
        100.0 * projection.int("previous_two_face_reuses") as f64 / corner_divisor as f64,
    ));
    lines.push(format!(
        "projection fallbacks: near={} special={} layered_sky={} oversized={}",
        projection.int("near_fallback_corners"),
        projection.int("special_fallback_corners"),
        projection.int("layered_sky_corners"),
        projection.int("oversized_fallback_corners"),
    ));
    lines.push(format!(
        "selected fingerprint (diagnostic, not proof): active_frames={} same_as_previous={}/{} \
         ({:.2}%) longest_run={}",
        fingerprint.int("active_frames"),
        fingerprint.int("same_as_previous"),
        fingerprint.int("transitions"),
        fingerprint.float("same_percent"),
        fingerprint.int("longest_identical_run"),
    ));
    if matches!(schema, "QRC2" | "QRC3" | "QRC4" | "QRC5") {
        lines.push(format!(
            "resident base-packet candidate: ordinary={} bytes p50/p95/max={}/{}/{}; stable={} \
             bytes ({:.2}%) faces={} p50/p95/max={}/{}/{} dynamic-light-reject={}",
            resident.int("ordinary_base_packet_bytes"),
            resident.int("ordinary_base_p50"),
            resident.int("ordinary_base_p95"),
            resident.int("ordinary_base_max"),
            resident.int("template_packet_bytes"),
            resident.float("template_coverage_percent"),
            resident.int("template_faces"),
            resident.int("template_p50"),
            resident.int("template_p95"),
            resident.int("template_max"),
            resident.int("dynamic_light_reject_bytes"),
        ));
        lines.push(format!(
            "packet arena: p50/p95/max={}/{}/{} bytes; emitted={} hardware_triangles={} \
             overflow_frames={}",
            arena.int("bytes_p50"),
            arena.int("bytes_p95"),
            arena.int("bytes_max"),
            arena.int("emitted_packets"),
            arena.int("hardware_triangles"),
            arena.int("overflow_frames"),
        ));
    }
    if matches!(schema, "QRC3" | "QRC4" | "QRC5") {
        lines.push(format!(
            "ordinary adaptive topology: surfaces={} roots={} level0/1/2={}/{}/{} \
             ({:.2}%/{:.2}%/{:.2}%) paired_l0={} underdraw_l1/l2={}/{} surface-clip-roots={} \
             ({:.2}%)",
            topology.int("surfaces"),
            topology.int("root_triangles"),
            topology.int("level0_root_triangles"),
            topology.int("level1_root_triangles"),
            topology.int("level2_root_triangles"),
            topology.float("level0_percent"),
            topology.float("level1_percent"),
            topology.float("level2_percent"),
            topology.int("paired_level0_packets"),
            topology.int("level1_underdraw_roots"),
            topology.int("level2_underdraw_roots"),
            topology.int("surface_clip_rejected_roots"),
            topology.float("surface_clip_rejected_root_percent"),
        ));
        lines.push(format!(
            "ordinary final stream: actual={} bytes p50/p95/max={}/{}/{} ({:.2}% of base \
             bytes); theoretical={} p50/p95/max={}/{}/{} screen-rejected={} ({:.2}%); actual \
             packets/triangles={}/{}",
            topology.int("actual_packet_bytes"),
            topology.int("actual_bytes_p50"),
            topology.int("actual_bytes_p95"),
            topology.int("actual_bytes_max"),
            topology.float("actual_vs_base_percent"),
            topology.int("theoretical_packet_bytes"),
            topology.int("theoretical_bytes_p50"),
            topology.int("theoretical_bytes_p95"),
            topology.int("theoretical_bytes_max"),
            topology.int("screen_rejected_packet_bytes"),
            topology.float("screen_rejected_percent"),
            topology.int("actual_packets"),
            topology.int("actual_hardware_triangles"),
        ));
        lines.push(format!(
            "topology fingerprint: same_as_previous={}/{} ({:.2}%) longest_run={}; when \
             selection unchanged={}/{} ({:.2}%)",
            topology_fingerprint.int("same_as_previous"),
            topology_fingerprint.int("transitions"),
            topology_fingerprint.float("same_percent"),
            topology_fingerprint.int("longest_identical_run"),
            topology_fingerprint.int("same_topology_given_selection"),
            topology_fingerprint.int("same_selection_transitions"),
            topology_fingerprint.float("same_given_selection_percent"),
        ));
        lines.push(format!(
            "all-ordinary theoretical-prefix bound: p50/p95/max={}/{}/{} bytes; \
             frames_over_120KiB={}",
            topology.int("topology_prefix_candidate_p50"),
            topology.int("topology_prefix_candidate_p95"),
            topology.int("topology_prefix_candidate_max"),
            topology.int("topology_prefix_candidate_over_120k_frames"),
        ));
    }
    if schema == "QRC4" || schema == "QRC5" {
        let (caches, label) = if schema == "QRC4" {
            (summary.get("subdivision_caches"), "subdivision cache")
        } else {
            (summary.get("subdivision_slab_caches"), "subdivision slabs")
        };
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
            let cache = caches.get(&budget.to_string());
            let slots = if schema == "QRC4" {
                format!("{} slots", cache.int("capacity"))
            } else {
                format!(
                    "{} L1 + {} L2 slots",
                    cache.int("level1_capacity"),
                    cache.int("level2_capacity")
                )
            };
            lines.push(format!(
                "{label} {budget} KiB/pool ({} KiB dual, {slots}): requests={} hits={} \
                 ({:.2}%) alloc={} replace={} fallback={} ({:.2}%); resident p50/p95/max=\
                 {}/{}/{}; hit packet bytes={}/{} ({:.2}%), invariant reuse={} ({:.2}% of \
                 requested bytes)",
                cache.int("dual_pool_kib"),
                cache.int("requests"),
                cache.int("hits"),
                cache.float("hit_percent"),
                cache.int("allocations"),
                cache.int("replacements"),
                cache.int("fallbacks"),
                cache.float("fallback_percent"),
                cache.int("resident_p50"),
                cache.int("resident_p95"),
                cache.int("resident_max"),
                cache.int("hit_packet_bytes"),
                cache.int("requested_packet_bytes"),
                cache.float("hit_packet_byte_percent"),
                cache.int("hit_invariant_bytes"),
                cache.float("invariant_reuse_percent_of_requested_bytes"),
            ));
        }
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::{
        all_fields, fields, parse_lines, require_deterministic, summarize, Row,
        SUBDIVISION_CACHE_BUDGETS_KIB,
    };

    fn hex_line(tag: &str, version: u8, row: &Row) -> String {
        let body: Vec<String> = fields(version)
            .iter()
            .map(|name| format!("{:x}", row.get(name)))
            .collect();
        format!("{tag},{}", body.join(","))
    }

    fn blank() -> Row {
        Row {
            schema: 5,
            values: all_fields().into_iter().map(|n| (n, 0)).collect(),
        }
    }

    fn qrc3_row(changes: &[(&str, i128)]) -> Row {
        let mut row = blank();
        for (name, value) in [
            ("frame", 1),
            ("leaf", 2),
            ("portal_leaf", 0xFFFF),
            ("visibility_rebuilt", 1),
            ("pvs_faces", 10),
            ("policy_rejects", 1),
            ("backface_rejects", 3),
            ("frustum_rejects", 2),
            ("selected_faces", 4),
            ("near_faces", 1),
            ("plane_tests", 9),
            ("plane_run_tests", 6),
            ("plane_tests_saved", 3),
            ("max_plane_run", 2),
            ("aabb_tests", 6),
            ("block4_groups", 3),
            ("block4_rejected_groups", 1),
            ("block4_rejected_faces", 4),
            ("block4_aabb_tests_saved", 2),
            ("block8_groups", 2),
            ("block16_groups", 1),
            ("candidate_corners", 12),
            ("unique_positions", 9),
            ("projection_batches", 1),
            ("near_corners", 4),
            ("ordinary_base_packet_bytes", 100),
            ("resident_template_faces", 2),
            ("resident_template_packet_bytes", 60),
            ("dynamic_light_template_reject_bytes", 20),
            ("ordinary_output_packet_bytes", 52),
            ("ordinary_output_packets", 1),
            ("ordinary_output_hardware_triangles", 2),
            ("topology_surfaces", 1),
            ("topology_root_triangles", 2),
            ("topology_level0_root_triangles", 2),
            ("topology_paired_level0_packets", 1),
            ("topology_theoretical_packets", 1),
            ("topology_theoretical_hardware_triangles", 2),
            ("topology_theoretical_packet_bytes", 52),
            ("topology_hash_a", 0xAAAA),
            ("topology_hash_b", 0xBBBB),
            ("packet_arena_words", 100),
            ("emitted_packets", 10),
            ("hardware_triangles", 20),
            ("selected_hash_a", 0x1234),
            ("selected_hash_b", 0x5678),
        ] {
            row.set(name, value);
        }
        for (name, value) in changes {
            row.set(name, *value);
        }
        row
    }

    fn qrc3(changes: &[(&str, i128)]) -> String {
        hex_line("QRC3", 3, &qrc3_row(changes))
    }

    fn qrc4(changes: &[(&str, i128)]) -> String {
        let mut row = qrc3_row(&[]);
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
            let prefix = format!("subdiv_cache_{budget}k_");
            for (metric, value) in [
                ("requests", 4),
                ("hits", 2),
                ("allocations", 1),
                ("replacements", 1),
                ("fallbacks", 1),
                ("resident", 1),
                ("requested_packet_bytes", 1000),
                ("hit_packet_bytes", 600),
                ("hit_invariant_bytes", 360),
            ] {
                row.set(&format!("{prefix}{metric}"), value);
            }
        }
        for (name, value) in changes {
            row.set(name, *value);
        }
        hex_line("QRC4", 4, &row)
    }

    fn qrc5(changes: &[(&str, i128)]) -> String {
        let mut row = qrc3_row(&[]);
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB {
            let prefix = format!("subdiv_slab_{budget}k_");
            for (metric, value) in [
                ("requests", 5),
                ("hits", 4),
                ("allocations", 1),
                ("replacements", 1),
                ("fallbacks", 0),
                ("resident", 2),
                ("requested_packet_bytes", 1000),
                ("hit_packet_bytes", 800),
                ("hit_invariant_bytes", 480),
            ] {
                row.set(&format!("{prefix}{metric}"), value);
            }
        }
        for (name, value) in changes {
            row.set(name, *value);
        }
        hex_line("QRC5", 5, &row)
    }

    fn rows(lines: &[String]) -> Result<Vec<Row>, String> {
        parse_lines(lines.iter().map(String::as_str), "<memory>")
    }

    #[test]
    fn parse_ignores_unrelated_log_lines_and_hex_decodes() {
        let lines = [
            "boot".to_owned(),
            format!("guest: {}", qrc3(&[])),
            "done".to_owned(),
        ];
        let parsed = rows(&lines).unwrap();
        assert_eq!(parsed[0].get("pvs_faces"), 10);
        assert_eq!(parsed[0].get("selected_hash_a"), 0x1234);
    }

    #[test]
    fn selection_funnel_is_validated() {
        let error = rows(&[qrc3(&[("selected_faces", 5)])]).unwrap_err();
        assert!(error.contains("selection funnel"), "{error}");
    }

    #[test]
    fn two_runs_must_match_every_field() {
        let first = rows(&[qrc3(&[])]).unwrap();
        let second = rows(&[qrc3(&[("selected_hash_b", 0x9999)])]).unwrap();
        let error = require_deterministic(&first, &second).unwrap_err();
        assert!(error.contains("differs at row 0"), "{error}");
    }

    #[test]
    fn summary_computes_isolated_bounds() {
        let parsed = rows(&[qrc3(&[]), qrc3(&[("frame", 2)])]).unwrap();
        let summary = summarize(&parsed);
        assert_eq!(summary.get("plane_runs").int("tests_saved"), 6);
        assert_eq!(
            summary
                .get("block_aabb")
                .get("4")
                .int("candidate_aabb_tests"),
            14
        );
        assert_eq!(
            summary.get("block_aabb").get("4").int("net_tests_saved"),
            -2
        );
        assert_eq!(summary.get("projection").int("transforms_saved"), 6);
        assert_eq!(
            summary.get("resident_packets").int("template_packet_bytes"),
            120
        );
        assert_eq!(
            summary
                .get("resident_packets")
                .float("template_coverage_percent"),
            60.0
        );
        assert_eq!(summary.get("arena").int("bytes_max"), 400);
        assert_eq!(summary.get("arena").int("emitted_packets"), 20);
        assert_eq!(
            summary.get("selected_fingerprint").int("same_as_previous"),
            1
        );
        assert_eq!(
            summary.get("adaptive_topology").int("actual_packet_bytes"),
            104
        );
        assert_eq!(
            summary
                .get("adaptive_topology")
                .int("topology_prefix_candidate_max"),
            400
        );
        assert_eq!(
            summary.get("topology_fingerprint").int("same_as_previous"),
            1
        );
    }

    #[test]
    fn legacy_qrc2_defaults_topology_fields() {
        let mut current = blank();
        for name in [
            "pvs_faces",
            "selected_faces",
            "plane_tests",
            "plane_run_tests",
            "aabb_tests",
        ] {
            current.set(name, 1);
        }
        let line = hex_line("QRC2", 2, &current);
        let parsed = rows(&[line]).unwrap();
        assert_eq!(parsed[0].schema, 2);
        assert_eq!(parsed[0].get("topology_root_triangles"), 0);
    }

    #[test]
    fn legacy_qrc1_defaults_new_packet_fields() {
        let mut current = blank();
        for name in [
            "pvs_faces",
            "selected_faces",
            "plane_tests",
            "plane_run_tests",
            "aabb_tests",
        ] {
            current.set(name, 1);
        }
        let line = hex_line("QRC1", 1, &current);
        let parsed = rows(&[line]).unwrap();
        assert_eq!(parsed[0].schema, 1);
        assert_eq!(parsed[0].get("resident_template_packet_bytes"), 0);
    }

    #[test]
    fn qrc4_summarizes_bounded_subdivision_cache_curve() {
        let parsed = rows(&[qrc4(&[]), qrc4(&[("frame", 2)])]).unwrap();
        let summary = summarize(&parsed);
        let cache = summary.get("subdivision_caches").get("32");
        assert_eq!(summary.text("schema"), "QRC4");
        assert_eq!(cache.int("capacity"), 43);
        assert_eq!(cache.int("requests"), 8);
        assert_eq!(cache.float("hit_percent"), 50.0);
        assert_eq!(cache.float("fallback_percent"), 25.0);
        assert_eq!(
            cache.float("invariant_reuse_percent_of_requested_bytes"),
            36.0
        );
    }

    #[test]
    fn qrc4_validates_cache_request_partition() {
        let error = rows(&[qrc4(&[("subdiv_cache_16k_hits", 3)])]).unwrap_err();
        assert!(error.contains("cache request partition"), "{error}");
    }

    #[test]
    fn qrc5_summarizes_segregated_subdivision_slabs() {
        let parsed = rows(&[qrc5(&[]), qrc5(&[("frame", 2)])]).unwrap();
        let summary = summarize(&parsed);
        let cache = summary.get("subdivision_slab_caches").get("32");
        assert_eq!(summary.text("schema"), "QRC5");
        assert_eq!(cache.int("level1_capacity"), 78);
        assert_eq!(cache.int("level2_capacity"), 17);
        assert_eq!(cache.int("hits"), 8);
        assert_eq!(cache.float("hit_percent"), 80.0);
        assert_eq!(
            cache.float("invariant_reuse_percent_of_requested_bytes"),
            48.0
        );
    }
}
