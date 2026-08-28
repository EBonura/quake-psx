import csv
import tempfile
import unittest
from pathlib import Path

from symbolize_psoxide_pc import aggregate, enclosing_symbol, load_symbols


class SymbolizePsoxidePcTests(unittest.TestCase):
    def test_named_map_rows_win_over_duplicate_input_sections(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "guest.map"
            path.write_text(
                " VMA LMA Size Align Out In Symbol\n"
                "80010000 80010000 20 4 /tmp/a.o:(.text.foo)\n"
                "80010000 80010000 20 1 crate::foo\n"
                "80010020 80010020 10 1 crate::bar\n"
            )
            symbols = load_symbols(path)
            self.assertEqual([symbol.name for symbol in symbols], ["crate::foo", "crate::bar"])
            starts = [symbol.start for symbol in symbols]
            self.assertEqual(enclosing_symbol(symbols, starts, 0x8001001C).name, "crate::foo")
            self.assertIsNone(enclosing_symbol(symbols, starts, 0x80010040))

    def test_window_filter_and_unmapped_samples(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "samples.csv"
            with path.open("w", newline="") as output:
                writer = csv.writer(output)
                writer.writerow(["window_start_tick", "pc", "samples", "percent_window"])
                writer.writerow([0, "0x80010004", 3, 0])
                writer.writerow([300, "0x80010004", 5, 0])
                writer.writerow([300, "0x70000000", 2, 0])
                writer.writerow([600, "0x80010004", 11, 0])
            symbols = load_symbols_from_text(
                "80010000 80010000 20 1 crate::foo\n", Path(directory)
            )
            grand, totals, _ = aggregate(symbols, path, 300, 300)
            self.assertEqual(grand, 7)
            self.assertEqual(totals, {"crate::foo": 5, "<unmapped>": 2})


def load_symbols_from_text(text: str, directory: Path):
    path = directory / "guest.map"
    path.write_text(text)
    return load_symbols(path)


if __name__ == "__main__":
    unittest.main()
