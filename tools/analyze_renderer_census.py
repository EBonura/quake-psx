#!/usr/bin/env python3
"""Summarize QRC1-QRC5 renderer census lines from PSoXide logs.

The guest emits hexadecimal positional fields. Passing both deterministic
route logs makes this tool reject any frame-level census mismatch before it
reports structural optimization bounds.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Iterable, Sequence


FIELDS_V1 = (
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
)

FIELDS_V2 = FIELDS_V1[:-2] + (
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
)

FIELDS_V3 = FIELDS_V2[:-6] + (
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
) + FIELDS_V2[-6:]

SUBDIVISION_CACHE_BUDGETS_KIB = (16, 32, 48, 64)
SUBDIVISION_CACHE_SLOT_BYTES = 748
SUBDIVISION_LEVEL1_SLOT_BYTES = 252
SUBDIVISION_LEVEL2_SLOT_BYTES = 748
SUBDIVISION_CACHE_METRICS = (
    "requests",
    "hits",
    "allocations",
    "replacements",
    "fallbacks",
    "resident",
    "requested_packet_bytes",
    "hit_packet_bytes",
    "hit_invariant_bytes",
)
SUBDIVISION_CACHE_FIELDS = tuple(
    f"subdiv_cache_{budget}k_{metric}"
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB
    for metric in SUBDIVISION_CACHE_METRICS
)
FIELDS_V4 = FIELDS_V3[:-6] + SUBDIVISION_CACHE_FIELDS + FIELDS_V3[-6:]
SUBDIVISION_SLAB_FIELDS = tuple(
    f"subdiv_slab_{budget}k_{metric}"
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB
    for metric in SUBDIVISION_CACHE_METRICS
)
FIELDS = FIELDS_V3[:-6] + SUBDIVISION_SLAB_FIELDS + FIELDS_V3[-6:]
ALL_FIELDS = tuple(dict.fromkeys(FIELDS_V4 + SUBDIVISION_SLAB_FIELDS))


def _parse_census_payload(payload: str, source: str, line_number: int) -> dict[str, int]:
    fields = payload.split(",")
    schema = fields[0]
    if schema == "QRC5":
        schema_fields = FIELDS
        schema_version = 5
    elif schema == "QRC4":
        schema_fields = FIELDS_V4
        schema_version = 4
    elif schema == "QRC3":
        schema_fields = FIELDS_V3
        schema_version = 3
    elif schema == "QRC2":
        schema_fields = FIELDS_V2
        schema_version = 2
    elif schema == "QRC1":
        schema_fields = FIELDS_V1
        schema_version = 1
    else:
        raise ValueError(f"{source}:{line_number}: unknown renderer census schema {schema}")
    expected = len(schema_fields) + 1
    if len(fields) != expected:
        raise ValueError(
            f"{source}:{line_number}: {schema} has {len(fields) - 1} fields; "
            f"expected {len(schema_fields)}"
        )
    try:
        row = {name: 0 for name in ALL_FIELDS}
        row.update(
            {name: int(value, 16) for name, value in zip(schema_fields, fields[1:])}
        )
        row["_schema_version"] = schema_version
    except ValueError as error:
        raise ValueError(f"{source}:{line_number}: invalid hexadecimal {schema} field") from error
    _validate_row(row, source, line_number)
    return row


def _validate_row(row: dict[str, int], source: str, line_number: int) -> None:
    where = f"{source}:{line_number}"
    funnel = (
        row["policy_rejects"]
        + row["backface_rejects"]
        + row["frustum_rejects"]
        + row["selected_faces"]
    )
    if funnel != row["pvs_faces"]:
        raise ValueError(f"{where}: selection funnel does not equal PVS face count")
    if row["aabb_tests"] != row["frustum_rejects"] + row["selected_faces"]:
        raise ValueError(f"{where}: AABB test count does not match the selection funnel")
    if row["plane_tests_saved"] != row["plane_tests"] - row["plane_run_tests"]:
        raise ValueError(f"{where}: plane-run saving is internally inconsistent")
    if row["near_faces"] > row["selected_faces"]:
        raise ValueError(f"{where}: near face count exceeds selected face count")
    if row["unique_positions"] > row["candidate_corners"]:
        raise ValueError(f"{where}: unique projected positions exceed candidate corners")
    available_reuses = row["candidate_corners"] - row["unique_positions"]
    if not (
        row["previous_face_reuses"]
        <= row["previous_two_face_reuses"]
        <= available_reuses
    ):
        raise ValueError(f"{where}: adjacent projection reuse counts are impossible")
    for size in (4, 8, 16):
        if row[f"block{size}_aabb_tests_saved"] > row["aabb_tests"]:
            raise ValueError(f"{where}: block-{size} saves more AABB tests than exist")
        if row[f"block{size}_rejected_groups"] > row[f"block{size}_groups"]:
            raise ValueError(f"{where}: block-{size} rejected group count is impossible")
    if row["resident_template_faces"] > row["selected_faces"]:
        raise ValueError(f"{where}: resident-template face count exceeds selected faces")
    if row["resident_template_packet_bytes"] > row["ordinary_base_packet_bytes"]:
        raise ValueError(f"{where}: resident-template bytes exceed ordinary base packets")
    if row["packet_overflow_avoided"] > 1:
        raise ValueError(f"{where}: packet-overflow flag is not boolean")
    if row["_schema_version"] >= 3:
        classified_roots = (
            row["topology_depth_rejects"]
            + row["topology_level0_root_triangles"]
            + row["topology_level1_root_triangles"]
            + row["topology_level2_root_triangles"]
        )
        if classified_roots > row["topology_root_triangles"]:
            raise ValueError(f"{where}: topology classifies more roots than exist")
        if 2 * row["topology_paired_level0_packets"] > row["topology_level0_root_triangles"]:
            raise ValueError(f"{where}: level-zero pairing count is impossible")
        if row["topology_level1_underdraw_roots"] > row["topology_level1_root_triangles"]:
            raise ValueError(f"{where}: level-one underdraw count is impossible")
        if row["topology_level2_underdraw_roots"] > row["topology_level2_root_triangles"]:
            raise ValueError(f"{where}: level-two underdraw count is impossible")
        if row["ordinary_output_packet_bytes"] > row["topology_theoretical_packet_bytes"]:
            raise ValueError(f"{where}: ordinary output exceeds theoretical packet bytes")
        if row["ordinary_output_packets"] > row["topology_theoretical_packets"]:
            raise ValueError(f"{where}: ordinary output exceeds theoretical packet count")
        if row["ordinary_output_hardware_triangles"] > row["topology_theoretical_hardware_triangles"]:
            raise ValueError(f"{where}: ordinary output exceeds theoretical triangle count")
    if row["_schema_version"] == 4:
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            prefix = f"subdiv_cache_{budget}k_"
            requests = row[prefix + "requests"]
            hits = row[prefix + "hits"]
            allocations = row[prefix + "allocations"]
            replacements = row[prefix + "replacements"]
            fallbacks = row[prefix + "fallbacks"]
            resident = row[prefix + "resident"]
            if hits + allocations + fallbacks != requests:
                raise ValueError(f"{where}: {budget} KiB cache request partition is invalid")
            if replacements > allocations:
                raise ValueError(f"{where}: {budget} KiB cache replacements exceed allocations")
            capacity = budget * 1024 // SUBDIVISION_CACHE_SLOT_BYTES
            if resident > capacity:
                raise ValueError(f"{where}: {budget} KiB cache resident count exceeds capacity")
            if row[prefix + "hit_packet_bytes"] > row[prefix + "requested_packet_bytes"]:
                raise ValueError(f"{where}: {budget} KiB cache hit bytes exceed requested bytes")
    if row["_schema_version"] >= 5:
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            prefix = f"subdiv_slab_{budget}k_"
            requests = row[prefix + "requests"]
            hits = row[prefix + "hits"]
            allocations = row[prefix + "allocations"]
            replacements = row[prefix + "replacements"]
            fallbacks = row[prefix + "fallbacks"]
            resident = row[prefix + "resident"]
            if hits + allocations + fallbacks != requests:
                raise ValueError(f"{where}: {budget} KiB slab request partition is invalid")
            if replacements > allocations:
                raise ValueError(f"{where}: {budget} KiB slab replacements exceed allocations")
            budget_bytes = budget * 1024
            level1_capacity = budget_bytes * 3 // 5 // SUBDIVISION_LEVEL1_SLOT_BYTES
            level2_capacity = (
                budget_bytes - level1_capacity * SUBDIVISION_LEVEL1_SLOT_BYTES
            ) // SUBDIVISION_LEVEL2_SLOT_BYTES
            if resident > level1_capacity + level2_capacity:
                raise ValueError(f"{where}: {budget} KiB slab resident count exceeds capacity")
            if row[prefix + "hit_packet_bytes"] > row[prefix + "requested_packet_bytes"]:
                raise ValueError(f"{where}: {budget} KiB slab hit bytes exceed requested bytes")


def parse_lines(lines: Iterable[str], source: str = "<memory>") -> list[dict[str, int]]:
    rows: list[dict[str, int]] = []
    for line_number, line in enumerate(lines, 1):
        marker = line.find("QRC5,")
        if marker < 0:
            marker = line.find("QRC4,")
        if marker < 0:
            marker = line.find("QRC3,")
        if marker < 0:
            marker = line.find("QRC2,")
        if marker < 0:
            marker = line.find("QRC1,")
        if marker < 0:
            continue
        payload = line[marker:].strip().split()[0]
        rows.append(_parse_census_payload(payload, source, line_number))
    if not rows:
        raise ValueError(f"{source}: no QRC1-QRC5 renderer census lines found")
    return rows


def parse_log(path: Path) -> list[dict[str, int]]:
    return parse_lines(path.read_text(encoding="utf-8", errors="replace").splitlines(), str(path))


def require_deterministic(
    first: Sequence[dict[str, int]], second: Sequence[dict[str, int]]
) -> None:
    if len(first) != len(second):
        raise ValueError(
            f"renderer census row count differs: run-a={len(first)}, run-b={len(second)}"
        )
    for index, (left, right) in enumerate(zip(first, second)):
        if left["_schema_version"] != right["_schema_version"]:
            raise ValueError(f"renderer census schema differs at row {index}")
        if left != right:
            differing = [name for name in ALL_FIELDS if left[name] != right[name]]
            details = ", ".join(
                f"{name}=0x{left[name]:x}/0x{right[name]:x}" for name in differing[:6]
            )
            raise ValueError(f"renderer census differs at row {index}: {details}")


def _sum(rows: Sequence[dict[str, int]], field: str) -> int:
    return sum(row[field] for row in rows)


def _percent(numerator: int, denominator: int) -> float:
    return 0.0 if denominator == 0 else 100.0 * numerator / denominator


def _percentile(values: Sequence[int], quantile: float) -> int:
    if not values:
        return 0
    ordered = sorted(values)
    index = max(0, math.ceil(quantile * len(ordered)) - 1)
    return ordered[index]


def _fingerprint_stability(rows: Sequence[dict[str, int]]) -> dict[str, int | float]:
    active = [row for row in rows if row["pvs_faces"] != 0]
    same = 0
    longest = 0
    run = 0
    previous: tuple[int, int, int] | None = None
    for row in active:
        fingerprint = (
            row["selected_faces"],
            row["selected_hash_a"],
            row["selected_hash_b"],
        )
        if fingerprint == previous:
            same += 1
            run += 1
        else:
            run = 1
        longest = max(longest, run)
        previous = fingerprint
    transitions = max(0, len(active) - 1)
    return {
        "active_frames": len(active),
        "same_as_previous": same,
        "transitions": transitions,
        "same_percent": _percent(same, transitions),
        "longest_identical_run": longest,
    }


def _topology_stability(rows: Sequence[dict[str, int]]) -> dict[str, int | float]:
    active = [row for row in rows if row["topology_surfaces"] != 0]
    same_topology = 0
    same_selection_transitions = 0
    same_topology_given_selection = 0
    longest = 0
    run = 0
    previous_topology: tuple[int, int, int, int] | None = None
    previous_selection: tuple[int, int, int] | None = None
    for row in active:
        topology = (
            row["topology_root_triangles"],
            row["topology_theoretical_packet_bytes"],
            row["topology_hash_a"],
            row["topology_hash_b"],
        )
        selection = (
            row["selected_faces"],
            row["selected_hash_a"],
            row["selected_hash_b"],
        )
        if topology == previous_topology:
            same_topology += 1
            run += 1
        else:
            run = 1
        if previous_selection is not None and selection == previous_selection:
            same_selection_transitions += 1
            if topology == previous_topology:
                same_topology_given_selection += 1
        longest = max(longest, run)
        previous_topology = topology
        previous_selection = selection
    transitions = max(0, len(active) - 1)
    return {
        "active_frames": len(active),
        "same_as_previous": same_topology,
        "transitions": transitions,
        "same_percent": _percent(same_topology, transitions),
        "longest_identical_run": longest,
        "same_selection_transitions": same_selection_transitions,
        "same_topology_given_selection": same_topology_given_selection,
        "same_given_selection_percent": _percent(
            same_topology_given_selection, same_selection_transitions
        ),
    }


def summarize(rows: Sequence[dict[str, int]]) -> dict[str, object]:
    pvs = _sum(rows, "pvs_faces")
    selection = {
        "pvs_faces": pvs,
        "policy_rejects": _sum(rows, "policy_rejects"),
        "backface_rejects": _sum(rows, "backface_rejects"),
        "frustum_rejects": _sum(rows, "frustum_rejects"),
        "selected_faces": _sum(rows, "selected_faces"),
        "near_faces": _sum(rows, "near_faces"),
        "water_blend_faces": _sum(rows, "water_blend_faces"),
        "selected_p50": _percentile([row["selected_faces"] for row in rows], 0.50),
        "selected_p95": _percentile([row["selected_faces"] for row in rows], 0.95),
    }
    selection["policy_reject_percent"] = _percent(selection["policy_rejects"], pvs)
    selection["backface_reject_percent"] = _percent(selection["backface_rejects"], pvs)
    selection["frustum_reject_percent"] = _percent(selection["frustum_rejects"], pvs)
    selection["selected_percent"] = _percent(selection["selected_faces"], pvs)
    selection["near_selected_percent"] = _percent(
        selection["near_faces"], selection["selected_faces"]
    )

    plane_tests = _sum(rows, "plane_tests")
    plane_run_tests = _sum(rows, "plane_run_tests")
    plane = {
        "current_tests": plane_tests,
        "run_cached_tests": plane_run_tests,
        "tests_saved": _sum(rows, "plane_tests_saved"),
        "saving_percent": _percent(plane_tests - plane_run_tests, plane_tests),
        "max_run": max(row["max_plane_run"] for row in rows),
    }

    baseline_aabb = _sum(rows, "aabb_tests")
    blocks: dict[str, object] = {}
    for size in (4, 8, 16):
        groups = _sum(rows, f"block{size}_groups")
        saved = _sum(rows, f"block{size}_aabb_tests_saved")
        candidate = groups + baseline_aabb - saved
        blocks[str(size)] = {
            "baseline_aabb_tests": baseline_aabb,
            "group_tests": groups,
            "rejected_groups": _sum(rows, f"block{size}_rejected_groups"),
            "rejected_faces": _sum(rows, f"block{size}_rejected_faces"),
            "individual_tests_saved": saved,
            "candidate_aabb_tests": candidate,
            "net_tests_saved": baseline_aabb - candidate,
            "net_saving_percent": _percent(baseline_aabb - candidate, baseline_aabb),
        }

    candidate_corners = _sum(rows, "candidate_corners")
    unique_positions = _sum(rows, "unique_positions")
    projection = {
        "candidate_corners": candidate_corners,
        "unique_positions": unique_positions,
        "batches": _sum(rows, "projection_batches"),
        "transforms_saved": candidate_corners - unique_positions,
        "transform_saving_percent": _percent(
            candidate_corners - unique_positions, candidate_corners
        ),
        "corners_per_unique_position": (
            0.0 if unique_positions == 0 else candidate_corners / unique_positions
        ),
        "previous_face_reuses": _sum(rows, "previous_face_reuses"),
        "previous_two_face_reuses": _sum(rows, "previous_two_face_reuses"),
        "near_fallback_corners": _sum(rows, "near_corners"),
        "special_fallback_corners": _sum(rows, "special_corners"),
        "layered_sky_corners": _sum(rows, "layered_sky_corners"),
        "oversized_fallback_corners": _sum(rows, "oversized_corners"),
    }
    ordinary_base_packet_bytes = _sum(rows, "ordinary_base_packet_bytes")
    resident_template_packet_bytes = _sum(rows, "resident_template_packet_bytes")
    resident_packets = {
        "ordinary_base_packet_bytes": ordinary_base_packet_bytes,
        "ordinary_base_p50": _percentile(
            [row["ordinary_base_packet_bytes"] for row in rows], 0.50
        ),
        "ordinary_base_p95": _percentile(
            [row["ordinary_base_packet_bytes"] for row in rows], 0.95
        ),
        "ordinary_base_max": max(row["ordinary_base_packet_bytes"] for row in rows),
        "template_faces": _sum(rows, "resident_template_faces"),
        "template_packet_bytes": resident_template_packet_bytes,
        "template_p50": _percentile(
            [row["resident_template_packet_bytes"] for row in rows], 0.50
        ),
        "template_p95": _percentile(
            [row["resident_template_packet_bytes"] for row in rows], 0.95
        ),
        "template_max": max(row["resident_template_packet_bytes"] for row in rows),
        "template_coverage_percent": _percent(
            resident_template_packet_bytes, ordinary_base_packet_bytes
        ),
        "dynamic_light_reject_bytes": _sum(
            rows, "dynamic_light_template_reject_bytes"
        ),
    }
    arena = {
        "words_p50": _percentile([row["packet_arena_words"] for row in rows], 0.50),
        "words_p95": _percentile([row["packet_arena_words"] for row in rows], 0.95),
        "words_max": max(row["packet_arena_words"] for row in rows),
        "bytes_p50": 4
        * _percentile([row["packet_arena_words"] for row in rows], 0.50),
        "bytes_p95": 4
        * _percentile([row["packet_arena_words"] for row in rows], 0.95),
        "bytes_max": 4 * max(row["packet_arena_words"] for row in rows),
        "emitted_packets": _sum(rows, "emitted_packets"),
        "hardware_triangles": _sum(rows, "hardware_triangles"),
        "overflow_frames": _sum(rows, "packet_overflow_avoided"),
    }
    topology_roots = _sum(rows, "topology_root_triangles")
    theoretical_packet_bytes = _sum(rows, "topology_theoretical_packet_bytes")
    ordinary_output_packet_bytes = _sum(rows, "ordinary_output_packet_bytes")
    classified_roots = sum(
        row["topology_depth_rejects"]
        + row["topology_level0_root_triangles"]
        + row["topology_level1_root_triangles"]
        + row["topology_level2_root_triangles"]
        for row in rows
    )
    surface_clip_rejected_roots = topology_roots - classified_roots
    topology_prefix_candidates = [
        4 * row["packet_arena_words"]
        + row["topology_theoretical_packet_bytes"]
        - row["ordinary_output_packet_bytes"]
        for row in rows
    ]
    topology = {
        "surfaces": _sum(rows, "topology_surfaces"),
        "root_triangles": topology_roots,
        "surface_clip_rejects": _sum(rows, "topology_surface_clip_rejects"),
        "surface_clip_rejected_roots": surface_clip_rejected_roots,
        "surface_clip_rejected_root_percent": _percent(
            surface_clip_rejected_roots, topology_roots
        ),
        "depth_rejects": _sum(rows, "topology_depth_rejects"),
        "level0_root_triangles": _sum(rows, "topology_level0_root_triangles"),
        "level1_root_triangles": _sum(rows, "topology_level1_root_triangles"),
        "level2_root_triangles": _sum(rows, "topology_level2_root_triangles"),
        "paired_level0_packets": _sum(rows, "topology_paired_level0_packets"),
        "level1_underdraw_roots": _sum(rows, "topology_level1_underdraw_roots"),
        "level2_underdraw_roots": _sum(rows, "topology_level2_underdraw_roots"),
        "theoretical_packets": _sum(rows, "topology_theoretical_packets"),
        "theoretical_hardware_triangles": _sum(
            rows, "topology_theoretical_hardware_triangles"
        ),
        "theoretical_packet_bytes": theoretical_packet_bytes,
        "theoretical_bytes_p50": _percentile(
            [row["topology_theoretical_packet_bytes"] for row in rows], 0.50
        ),
        "theoretical_bytes_p95": _percentile(
            [row["topology_theoretical_packet_bytes"] for row in rows], 0.95
        ),
        "theoretical_bytes_max": max(
            row["topology_theoretical_packet_bytes"] for row in rows
        ),
        "actual_packets": _sum(rows, "ordinary_output_packets"),
        "actual_hardware_triangles": _sum(
            rows, "ordinary_output_hardware_triangles"
        ),
        "actual_packet_bytes": ordinary_output_packet_bytes,
        "actual_bytes_p50": _percentile(
            [row["ordinary_output_packet_bytes"] for row in rows], 0.50
        ),
        "actual_bytes_p95": _percentile(
            [row["ordinary_output_packet_bytes"] for row in rows], 0.95
        ),
        "actual_bytes_max": max(row["ordinary_output_packet_bytes"] for row in rows),
        "screen_rejected_packet_bytes": (
            theoretical_packet_bytes - ordinary_output_packet_bytes
        ),
        "screen_rejected_percent": _percent(
            theoretical_packet_bytes - ordinary_output_packet_bytes,
            theoretical_packet_bytes,
        ),
        "actual_vs_base_percent": _percent(
            ordinary_output_packet_bytes, ordinary_base_packet_bytes
        ),
        "topology_prefix_candidate_p50": _percentile(
            topology_prefix_candidates, 0.50
        ),
        "topology_prefix_candidate_p95": _percentile(
            topology_prefix_candidates, 0.95
        ),
        "topology_prefix_candidate_max": max(topology_prefix_candidates),
        "topology_prefix_candidate_over_120k_frames": sum(
            value > 120 * 1024 for value in topology_prefix_candidates
        ),
        "level0_percent": _percent(
            _sum(rows, "topology_level0_root_triangles"), topology_roots
        ),
        "level1_percent": _percent(
            _sum(rows, "topology_level1_root_triangles"), topology_roots
        ),
        "level2_percent": _percent(
            _sum(rows, "topology_level2_root_triangles"), topology_roots
        ),
    }
    subdivision_caches: dict[str, object] = {}
    if rows[0]["_schema_version"] == 4:
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            prefix = f"subdiv_cache_{budget}k_"
            requests = _sum(rows, prefix + "requests")
            hits = _sum(rows, prefix + "hits")
            allocations = _sum(rows, prefix + "allocations")
            fallbacks = _sum(rows, prefix + "fallbacks")
            requested_packet_bytes = _sum(rows, prefix + "requested_packet_bytes")
            hit_packet_bytes = _sum(rows, prefix + "hit_packet_bytes")
            hit_invariant_bytes = _sum(rows, prefix + "hit_invariant_bytes")
            resident = [row[prefix + "resident"] for row in rows]
            subdivision_caches[str(budget)] = {
                "per_pool_kib": budget,
                "dual_pool_kib": 2 * budget,
                "slot_bytes": SUBDIVISION_CACHE_SLOT_BYTES,
                "capacity": budget * 1024 // SUBDIVISION_CACHE_SLOT_BYTES,
                "requests": requests,
                "hits": hits,
                "hit_percent": _percent(hits, requests),
                "allocations": allocations,
                "replacements": _sum(rows, prefix + "replacements"),
                "fallbacks": fallbacks,
                "fallback_percent": _percent(fallbacks, requests),
                "resident_p50": _percentile(resident, 0.50),
                "resident_p95": _percentile(resident, 0.95),
                "resident_max": max(resident),
                "requested_packet_bytes": requested_packet_bytes,
                "hit_packet_bytes": hit_packet_bytes,
                "hit_packet_byte_percent": _percent(
                    hit_packet_bytes, requested_packet_bytes
                ),
                "hit_invariant_bytes": hit_invariant_bytes,
                "invariant_reuse_percent_of_requested_bytes": _percent(
                    hit_invariant_bytes, requested_packet_bytes
                ),
            }
    subdivision_slab_caches: dict[str, object] = {}
    if rows[0]["_schema_version"] >= 5:
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            prefix = f"subdiv_slab_{budget}k_"
            requests = _sum(rows, prefix + "requests")
            hits = _sum(rows, prefix + "hits")
            allocations = _sum(rows, prefix + "allocations")
            fallbacks = _sum(rows, prefix + "fallbacks")
            requested_packet_bytes = _sum(rows, prefix + "requested_packet_bytes")
            hit_packet_bytes = _sum(rows, prefix + "hit_packet_bytes")
            hit_invariant_bytes = _sum(rows, prefix + "hit_invariant_bytes")
            resident = [row[prefix + "resident"] for row in rows]
            budget_bytes = budget * 1024
            level1_capacity = budget_bytes * 3 // 5 // SUBDIVISION_LEVEL1_SLOT_BYTES
            level2_capacity = (
                budget_bytes - level1_capacity * SUBDIVISION_LEVEL1_SLOT_BYTES
            ) // SUBDIVISION_LEVEL2_SLOT_BYTES
            subdivision_slab_caches[str(budget)] = {
                "per_pool_kib": budget,
                "dual_pool_kib": 2 * budget,
                "level1_capacity": level1_capacity,
                "level2_capacity": level2_capacity,
                "capacity": level1_capacity + level2_capacity,
                "requests": requests,
                "hits": hits,
                "hit_percent": _percent(hits, requests),
                "allocations": allocations,
                "replacements": _sum(rows, prefix + "replacements"),
                "fallbacks": fallbacks,
                "fallback_percent": _percent(fallbacks, requests),
                "resident_p50": _percentile(resident, 0.50),
                "resident_p95": _percentile(resident, 0.95),
                "resident_max": max(resident),
                "requested_packet_bytes": requested_packet_bytes,
                "hit_packet_bytes": hit_packet_bytes,
                "hit_packet_byte_percent": _percent(
                    hit_packet_bytes, requested_packet_bytes
                ),
                "hit_invariant_bytes": hit_invariant_bytes,
                "invariant_reuse_percent_of_requested_bytes": _percent(
                    hit_invariant_bytes, requested_packet_bytes
                ),
            }

    return {
        "schema": f"QRC{rows[0]['_schema_version']}",
        "frames": len(rows),
        "visibility_rebuilds": _sum(rows, "visibility_rebuilt"),
        "selection": selection,
        "plane_runs": plane,
        "block_aabb": blocks,
        "projection": projection,
        "resident_packets": resident_packets,
        "arena": arena,
        "selected_fingerprint": _fingerprint_stability(rows),
        "adaptive_topology": topology,
        "topology_fingerprint": _topology_stability(rows),
        "subdivision_caches": subdivision_caches,
        "subdivision_slab_caches": subdivision_slab_caches,
    }


def render_text(summary: dict[str, object], deterministic_runs: int) -> str:
    selection = summary["selection"]
    plane = summary["plane_runs"]
    blocks = summary["block_aabb"]
    projection = summary["projection"]
    resident_packets = summary["resident_packets"]
    arena = summary["arena"]
    fingerprint = summary["selected_fingerprint"]
    topology = summary["adaptive_topology"]
    topology_fingerprint = summary["topology_fingerprint"]
    subdivision_caches = summary["subdivision_caches"]
    subdivision_slab_caches = summary["subdivision_slab_caches"]
    assert isinstance(selection, dict)
    assert isinstance(plane, dict)
    assert isinstance(blocks, dict)
    assert isinstance(projection, dict)
    assert isinstance(resident_packets, dict)
    assert isinstance(arena, dict)
    assert isinstance(fingerprint, dict)
    assert isinstance(topology, dict)
    assert isinstance(topology_fingerprint, dict)
    assert isinstance(subdivision_caches, dict)
    assert isinstance(subdivision_slab_caches, dict)

    lines = [
        "quake-psx renderer census (diagnostic work; timing is invalid)",
        f"frames={summary['frames']} deterministic_runs={deterministic_runs} "
        f"visibility_rebuilds={summary['visibility_rebuilds']}",
        "selection: "
        f"pvs={selection['pvs_faces']} policy={selection['policy_rejects']} "
        f"({selection['policy_reject_percent']:.2f}%) "
        f"backface={selection['backface_rejects']} "
        f"({selection['backface_reject_percent']:.2f}%) "
        f"frustum={selection['frustum_rejects']} "
        f"({selection['frustum_reject_percent']:.2f}%) "
        f"selected={selection['selected_faces']} ({selection['selected_percent']:.2f}%)",
        "selected/frame: "
        f"p50={selection['selected_p50']} p95={selection['selected_p95']} "
        f"near={selection['near_faces']} ({selection['near_selected_percent']:.2f}% of selected) "
        f"water_blend={selection['water_blend_faces']}",
        "same-plane runs: "
        f"tests={plane['current_tests']} cached={plane['run_cached_tests']} "
        f"saved={plane['tests_saved']} ({plane['saving_percent']:.2f}%) "
        f"max_run={plane['max_run']}",
    ]
    for size in (4, 8, 16):
        block = blocks[str(size)]
        lines.append(
            f"block-{size} AABB: baseline={block['baseline_aabb_tests']} "
            f"group_tests={block['group_tests']} rejected_groups={block['rejected_groups']} "
            f"rejected_faces={block['rejected_faces']} "
            f"individual_saved={block['individual_tests_saved']} "
            f"candidate={block['candidate_aabb_tests']} "
            f"net_saved={block['net_tests_saved']} ({block['net_saving_percent']:.2f}%)"
        )
    lines.extend(
        [
            "batch shared projection: "
            f"corners={projection['candidate_corners']} "
            f"unique_positions={projection['unique_positions']} "
            f"batches={projection['batches']} "
            f"transforms_saved={projection['transforms_saved']} "
            f"({projection['transform_saving_percent']:.2f}%) "
            f"corners/unique={projection['corners_per_unique_position']:.3f}",
            "adjacent projection reuse: "
            f"previous_face={projection['previous_face_reuses']} "
            f"({100.0 * projection['previous_face_reuses'] / max(1, projection['candidate_corners']):.2f}% of corners) "
            f"previous_two={projection['previous_two_face_reuses']} "
            f"({100.0 * projection['previous_two_face_reuses'] / max(1, projection['candidate_corners']):.2f}% of corners)",
            "projection fallbacks: "
            f"near={projection['near_fallback_corners']} "
            f"special={projection['special_fallback_corners']} "
            f"layered_sky={projection['layered_sky_corners']} "
            f"oversized={projection['oversized_fallback_corners']}",
            "selected fingerprint (diagnostic, not proof): "
            f"active_frames={fingerprint['active_frames']} "
            f"same_as_previous={fingerprint['same_as_previous']}/"
            f"{fingerprint['transitions']} ({fingerprint['same_percent']:.2f}%) "
            f"longest_run={fingerprint['longest_identical_run']}",
        ]
    )
    if summary["schema"] in ("QRC2", "QRC3", "QRC4", "QRC5"):
        lines.append(
            "resident base-packet candidate: "
            f"ordinary={resident_packets['ordinary_base_packet_bytes']} bytes "
            f"p50/p95/max={resident_packets['ordinary_base_p50']}/"
            f"{resident_packets['ordinary_base_p95']}/"
            f"{resident_packets['ordinary_base_max']}; "
            f"stable={resident_packets['template_packet_bytes']} bytes "
            f"({resident_packets['template_coverage_percent']:.2f}%) "
            f"faces={resident_packets['template_faces']} "
            f"p50/p95/max={resident_packets['template_p50']}/"
            f"{resident_packets['template_p95']}/"
            f"{resident_packets['template_max']} "
            f"dynamic-light-reject={resident_packets['dynamic_light_reject_bytes']}"
        )
        lines.append(
            "packet arena: "
            f"p50/p95/max={arena['bytes_p50']}/{arena['bytes_p95']}/"
            f"{arena['bytes_max']} bytes; emitted={arena['emitted_packets']} "
            f"hardware_triangles={arena['hardware_triangles']} "
            f"overflow_frames={arena['overflow_frames']}"
        )
    if summary["schema"] in ("QRC3", "QRC4", "QRC5"):
        lines.append(
            "ordinary adaptive topology: "
            f"surfaces={topology['surfaces']} roots={topology['root_triangles']} "
            f"level0/1/2={topology['level0_root_triangles']}/"
            f"{topology['level1_root_triangles']}/"
            f"{topology['level2_root_triangles']} "
            f"({topology['level0_percent']:.2f}%/"
            f"{topology['level1_percent']:.2f}%/"
            f"{topology['level2_percent']:.2f}%) "
            f"paired_l0={topology['paired_level0_packets']} "
            f"underdraw_l1/l2={topology['level1_underdraw_roots']}/"
            f"{topology['level2_underdraw_roots']} "
            f"surface-clip-roots={topology['surface_clip_rejected_roots']} "
            f"({topology['surface_clip_rejected_root_percent']:.2f}%)"
        )
        lines.append(
            "ordinary final stream: "
            f"actual={topology['actual_packet_bytes']} bytes "
            f"p50/p95/max={topology['actual_bytes_p50']}/"
            f"{topology['actual_bytes_p95']}/{topology['actual_bytes_max']} "
            f"({topology['actual_vs_base_percent']:.2f}% of base bytes); "
            f"theoretical={topology['theoretical_packet_bytes']} "
            f"p50/p95/max={topology['theoretical_bytes_p50']}/"
            f"{topology['theoretical_bytes_p95']}/"
            f"{topology['theoretical_bytes_max']} "
            f"screen-rejected={topology['screen_rejected_packet_bytes']} "
            f"({topology['screen_rejected_percent']:.2f}%); "
            f"actual packets/triangles={topology['actual_packets']}/"
            f"{topology['actual_hardware_triangles']}"
        )
        lines.append(
            "topology fingerprint: "
            f"same_as_previous={topology_fingerprint['same_as_previous']}/"
            f"{topology_fingerprint['transitions']} "
            f"({topology_fingerprint['same_percent']:.2f}%) "
            f"longest_run={topology_fingerprint['longest_identical_run']}; "
            f"when selection unchanged="
            f"{topology_fingerprint['same_topology_given_selection']}/"
            f"{topology_fingerprint['same_selection_transitions']} "
            f"({topology_fingerprint['same_given_selection_percent']:.2f}%)"
        )
        lines.append(
            "all-ordinary theoretical-prefix bound: "
            f"p50/p95/max={topology['topology_prefix_candidate_p50']}/"
            f"{topology['topology_prefix_candidate_p95']}/"
            f"{topology['topology_prefix_candidate_max']} bytes; "
            f"frames_over_120KiB="
            f"{topology['topology_prefix_candidate_over_120k_frames']}"
        )
    if summary["schema"] == "QRC4":
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            cache = subdivision_caches[str(budget)]
            lines.append(
                f"subdivision cache {budget} KiB/pool "
                f"({cache['dual_pool_kib']} KiB dual, {cache['capacity']} slots): "
                f"requests={cache['requests']} hits={cache['hits']} "
                f"({cache['hit_percent']:.2f}%) alloc={cache['allocations']} "
                f"replace={cache['replacements']} fallback={cache['fallbacks']} "
                f"({cache['fallback_percent']:.2f}%); resident p50/p95/max="
                f"{cache['resident_p50']}/{cache['resident_p95']}/{cache['resident_max']}; "
                f"hit packet bytes={cache['hit_packet_bytes']}/"
                f"{cache['requested_packet_bytes']} "
                f"({cache['hit_packet_byte_percent']:.2f}%), invariant reuse="
                f"{cache['hit_invariant_bytes']} "
                f"({cache['invariant_reuse_percent_of_requested_bytes']:.2f}% of requested bytes)"
            )
    if summary["schema"] == "QRC5":
        for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
            cache = subdivision_slab_caches[str(budget)]
            lines.append(
                f"subdivision slabs {budget} KiB/pool "
                f"({cache['dual_pool_kib']} KiB dual, "
                f"{cache['level1_capacity']} L1 + {cache['level2_capacity']} L2 slots): "
                f"requests={cache['requests']} hits={cache['hits']} "
                f"({cache['hit_percent']:.2f}%) alloc={cache['allocations']} "
                f"replace={cache['replacements']} fallback={cache['fallbacks']} "
                f"({cache['fallback_percent']:.2f}%); resident p50/p95/max="
                f"{cache['resident_p50']}/{cache['resident_p95']}/{cache['resident_max']}; "
                f"hit packet bytes={cache['hit_packet_bytes']}/"
                f"{cache['requested_packet_bytes']} "
                f"({cache['hit_packet_byte_percent']:.2f}%), invariant reuse="
                f"{cache['hit_invariant_bytes']} "
                f"({cache['invariant_reuse_percent_of_requested_bytes']:.2f}% of requested bytes)"
            )
    return "\n".join(lines) + "\n"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+", type=Path, help="one log, or deterministic run-a/run-b logs")
    parser.add_argument("--json", action="store_true", help="emit JSON instead of the text report")
    parser.add_argument("--output", type=Path, help="write the report to this path")
    return parser


def main() -> int:
    args = build_parser().parse_args()
    if len(args.logs) not in (1, 2):
        raise SystemExit("pass exactly one log or the two deterministic run logs")
    runs = [parse_log(path) for path in args.logs]
    if len(runs) == 2:
        require_deterministic(runs[0], runs[1])
    summary = summarize(runs[0])
    report = (
        json.dumps(summary, indent=2, sort_keys=True) + "\n"
        if args.json
        else render_text(summary, len(runs))
    )
    if args.output:
        args.output.write_text(report, encoding="utf-8")
    else:
        print(report, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
