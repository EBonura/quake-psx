import csv
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import analyze_psoxide_gpu as gpu


class AnalyzePsoxideGpuTests(unittest.TestCase):
    def test_complete_present_intervals_and_cd_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            route_path = root / "route.csv"
            gpu_path = root / "gpu.csv"
            cd_path = root / "cd.csv"

            with route_path.open("w", newline="") as target:
                writer = csv.DictWriter(
                    target,
                    fieldnames=["route_tick", "bus_cycles", "display_start_changed"],
                )
                writer.writeheader()
                for tick, cycle, present in (
                    (1, 100, 1),
                    (2, 250, 1),
                    (3, 300, 0),
                    (4, 400, 1),
                    (5, 650, 1),
                ):
                    writer.writerow(
                        {
                            "route_tick": tick,
                            "bus_cycles": cycle,
                            "display_start_changed": present,
                        }
                    )
            with gpu_path.open("w", newline="") as target:
                writer = csv.DictWriter(
                    target,
                    fieldnames=["route_tick", "commands", "gpu_cycles"],
                )
                writer.writeheader()
                for tick, commands in enumerate((0, 10, 20, 30, 40), start=1):
                    writer.writerow(
                        {"route_tick": tick, "commands": commands, "gpu_cycles": commands * 2}
                    )
            cd_path.write_text("cycle,command\n200,0x06\n500,0x06\n700,0x06\n")

            route = gpu.read_rows(route_path)
            self.assertEqual(gpu.gameplay_bounds(route, cd_path), (2, 4))
            summary = gpu.summarize(gpu.read_rows(gpu_path), route, 2, 4)
            self.assertEqual(summary["complete_present_intervals"], 1)
            self.assertEqual(summary["per_present"]["commands"]["mean"], 50.0)
            self.assertEqual(summary["per_present"]["gpu_cycles"]["mean"], 100.0)


if __name__ == "__main__":
    unittest.main()
