import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from analyze_renderer_census import (
    FIELDS,
    FIELDS_V1,
    FIELDS_V2,
    FIELDS_V3,
    FIELDS_V4,
    SUBDIVISION_CACHE_BUDGETS_KIB,
    parse_lines,
    require_deterministic,
    summarize,
)


def qrc3(**changes: int) -> str:
    row = {name: 0 for name in FIELDS}
    row.update(
        frame=1,
        leaf=2,
        portal_leaf=0xFFFF,
        visibility_rebuilt=1,
        pvs_faces=10,
        policy_rejects=1,
        backface_rejects=3,
        frustum_rejects=2,
        selected_faces=4,
        near_faces=1,
        plane_tests=9,
        plane_run_tests=6,
        plane_tests_saved=3,
        max_plane_run=2,
        aabb_tests=6,
        block4_groups=3,
        block4_rejected_groups=1,
        block4_rejected_faces=4,
        block4_aabb_tests_saved=2,
        block8_groups=2,
        block16_groups=1,
        candidate_corners=12,
        unique_positions=9,
        projection_batches=1,
        near_corners=4,
        ordinary_base_packet_bytes=100,
        resident_template_faces=2,
        resident_template_packet_bytes=60,
        dynamic_light_template_reject_bytes=20,
        ordinary_output_packet_bytes=52,
        ordinary_output_packets=1,
        ordinary_output_hardware_triangles=2,
        topology_surfaces=1,
        topology_root_triangles=2,
        topology_level0_root_triangles=2,
        topology_paired_level0_packets=1,
        topology_theoretical_packets=1,
        topology_theoretical_hardware_triangles=2,
        topology_theoretical_packet_bytes=52,
        topology_hash_a=0xAAAA,
        topology_hash_b=0xBBBB,
        packet_arena_words=100,
        emitted_packets=10,
        hardware_triangles=20,
        selected_hash_a=0x1234,
        selected_hash_b=0x5678,
    )
    row.update(changes)
    return "QRC3," + ",".join(f"{row[name]:x}" for name in FIELDS_V3)


def qrc4(**changes: int) -> str:
    row = parse_lines([qrc3()])[0]
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
        prefix = f"subdiv_cache_{budget}k_"
        row[prefix + "requests"] = 4
        row[prefix + "hits"] = 2
        row[prefix + "allocations"] = 1
        row[prefix + "replacements"] = 1
        row[prefix + "fallbacks"] = 1
        row[prefix + "resident"] = 1
        row[prefix + "requested_packet_bytes"] = 1000
        row[prefix + "hit_packet_bytes"] = 600
        row[prefix + "hit_invariant_bytes"] = 360
    row.update(changes)
    return "QRC4," + ",".join(f"{row[name]:x}" for name in FIELDS_V4)


def qrc5(**changes: int) -> str:
    row = parse_lines([qrc4()])[0]
    for budget in SUBDIVISION_CACHE_BUDGETS_KIB:
        prefix = f"subdiv_slab_{budget}k_"
        row[prefix + "requests"] = 5
        row[prefix + "hits"] = 4
        row[prefix + "allocations"] = 1
        row[prefix + "replacements"] = 1
        row[prefix + "fallbacks"] = 0
        row[prefix + "resident"] = 2
        row[prefix + "requested_packet_bytes"] = 1000
        row[prefix + "hit_packet_bytes"] = 800
        row[prefix + "hit_invariant_bytes"] = 480
    row.update(changes)
    return "QRC5," + ",".join(f"{row[name]:x}" for name in FIELDS)


class RendererCensusTests(unittest.TestCase):
    def test_parse_ignores_unrelated_log_lines_and_hex_decodes(self) -> None:
        rows = parse_lines(["boot", "guest: " + qrc3(), "done"])
        self.assertEqual(rows[0]["pvs_faces"], 10)
        self.assertEqual(rows[0]["selected_hash_a"], 0x1234)

    def test_selection_funnel_is_validated(self) -> None:
        with self.assertRaisesRegex(ValueError, "selection funnel"):
            parse_lines([qrc3(selected_faces=5)])

    def test_two_runs_must_match_every_field(self) -> None:
        first = parse_lines([qrc3()])
        second = parse_lines([qrc3(selected_hash_b=0x9999)])
        with self.assertRaisesRegex(ValueError, "differs at row 0"):
            require_deterministic(first, second)

    def test_summary_computes_isolated_bounds(self) -> None:
        rows = parse_lines([qrc3(), qrc3(frame=2)])
        summary = summarize(rows)
        self.assertEqual(summary["plane_runs"]["tests_saved"], 6)
        self.assertEqual(summary["block_aabb"]["4"]["candidate_aabb_tests"], 14)
        self.assertEqual(summary["block_aabb"]["4"]["net_tests_saved"], -2)
        self.assertEqual(summary["projection"]["transforms_saved"], 6)
        self.assertEqual(summary["resident_packets"]["template_packet_bytes"], 120)
        self.assertEqual(summary["resident_packets"]["template_coverage_percent"], 60.0)
        self.assertEqual(summary["arena"]["bytes_max"], 400)
        self.assertEqual(summary["arena"]["emitted_packets"], 20)
        self.assertEqual(summary["selected_fingerprint"]["same_as_previous"], 1)
        self.assertEqual(summary["adaptive_topology"]["actual_packet_bytes"], 104)
        self.assertEqual(
            summary["adaptive_topology"]["topology_prefix_candidate_max"], 400
        )
        self.assertEqual(summary["topology_fingerprint"]["same_as_previous"], 1)

    def test_legacy_qrc2_defaults_topology_fields(self) -> None:
        current = {name: 0 for name in FIELDS}
        current.update(
            pvs_faces=1,
            selected_faces=1,
            plane_tests=1,
            plane_run_tests=1,
            aabb_tests=1,
        )
        line = "QRC2," + ",".join(f"{current[name]:x}" for name in FIELDS_V2)
        rows = parse_lines([line])
        self.assertEqual(rows[0]["_schema_version"], 2)
        self.assertEqual(rows[0]["topology_root_triangles"], 0)

    def test_legacy_qrc1_defaults_new_packet_fields(self) -> None:
        current = {name: 0 for name in FIELDS}
        current.update(
            pvs_faces=1,
            selected_faces=1,
            plane_tests=1,
            plane_run_tests=1,
            aabb_tests=1,
        )
        line = "QRC1," + ",".join(f"{current[name]:x}" for name in FIELDS_V1)
        rows = parse_lines([line])
        self.assertEqual(rows[0]["_schema_version"], 1)
        self.assertEqual(rows[0]["resident_template_packet_bytes"], 0)

    def test_qrc4_summarizes_bounded_subdivision_cache_curve(self) -> None:
        rows = parse_lines([qrc4(), qrc4(frame=2)])
        summary = summarize(rows)
        cache = summary["subdivision_caches"]["32"]
        self.assertEqual(summary["schema"], "QRC4")
        self.assertEqual(cache["capacity"], 43)
        self.assertEqual(cache["requests"], 8)
        self.assertEqual(cache["hit_percent"], 50.0)
        self.assertEqual(cache["fallback_percent"], 25.0)
        self.assertEqual(cache["invariant_reuse_percent_of_requested_bytes"], 36.0)

    def test_qrc4_validates_cache_request_partition(self) -> None:
        with self.assertRaisesRegex(ValueError, "cache request partition"):
            parse_lines([qrc4(subdiv_cache_16k_hits=3)])

    def test_qrc5_summarizes_segregated_subdivision_slabs(self) -> None:
        rows = parse_lines([qrc5(), qrc5(frame=2)])
        summary = summarize(rows)
        cache = summary["subdivision_slab_caches"]["32"]
        self.assertEqual(summary["schema"], "QRC5")
        self.assertEqual(cache["level1_capacity"], 78)
        self.assertEqual(cache["level2_capacity"], 17)
        self.assertEqual(cache["hits"], 8)
        self.assertEqual(cache["hit_percent"], 80.0)
        self.assertEqual(cache["invariant_reuse_percent_of_requested_bytes"], 48.0)


if __name__ == "__main__":
    unittest.main()
