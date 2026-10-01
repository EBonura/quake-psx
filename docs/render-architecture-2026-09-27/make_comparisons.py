#!/usr/bin/env python3
"""Lay out real fixed-camera captures; no filtering or generated scene pixels.

Requires Pillow. Input is the preserved local render-mesh experiment bundle.
Outputs lossless screenshots, integer-scaled comparison sheets and an offline viewer.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
VARIANTS = {
    "approximate": ("Small tolerance", "36.184 FPS / +2.83%", "1 UV unit / 8 light units"),
    "moderate": ("Wider tolerance", "36.398 FPS / +3.43%", "4 UV units / 16 light units"),
    "geometry": ("Aggressive merging", "37.448 FPS / +6.42%", "255 UV units / 255 light units"),
    "stitched": ("Junctions preserved", "35.203 FPS / +0.04%", "Boundary corners protected; negligible speed change"),
}

def font(size, bold=False):
    choices = [
        f"/System/Library/Fonts/Supplemental/Arial{' Bold' if bold else ''}.ttf",
        f"/usr/share/fonts/truetype/dejavu/DejaVuSans{'-Bold' if bold else ''}.ttf",
    ]
    for path in choices:
        if Path(path).exists():
            return ImageFont.truetype(path, size)
    return ImageFont.load_default(size=size)

def sheet(before, after, title, performance, destination, crop=None, scale=2):
    a = before.crop(crop) if crop else before
    b = after.crop(crop) if crop else after
    w, h = a.width * scale, a.height * scale
    width = max(1160, 2 * w + 72)
    panel = (width - 72) // 2
    canvas = Image.new("RGB", (width, h + 176), "#10151e")
    draw = ImageDraw.Draw(canvas)
    draw.text((24, 16), title, font=font(27, True), fill="#ffffff")
    note = f"Fixed camera / same lighting phase / nearest-neighbor {scale}x"
    if crop:
        note += f" / crop {crop}"
    draw.text((24, 53), note, font=font(17), fill="#aebaca")
    for x, picture, label, detail in [
        (24, a, "BEFORE / Original", "35.189 FPS"),
        (48 + panel, b, "AFTER / Rejected experiment", performance),
    ]:
        draw.text((x, 86), label, font=font(20, True), fill="#ffffff")
        draw.text((x, 112), detail, font=font(18), fill="#a8d5ff")
        canvas.paste(picture.resize((w, h), Image.Resampling.NEAREST), (x + (panel-w)//2, 144))
    draw.text((24, h + 152), "FPS is measured across the E1M1 route; these images show one fixed viewpoint.", font=font(16), fill="#aebaca")
    canvas.save(destination)

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("artifacts", type=Path)
    args = parser.parse_args()
    root = args.artifacts.resolve()
    sources = {}
    frames = {}
    for name in ["baseline", *VARIANTS]:
        source = root / f"visual-{name}" / "frame-180" / "frame.ppm"
        frame = Image.open(source).convert("RGB")
        assert frame.size == (320, 240)
        frame.save(HERE / f"{name}.png")
        frames[name] = frame
        sources[name] = {
            "source_relative_to_artifacts": str(source.relative_to(root)),
            "ppm_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "rgb_sha256": hashlib.sha256(frame.tobytes()).hexdigest(),
        }
        # Roundtrip proof: the exported PNG preserves every original RGB pixel.
        assert Image.open(HERE / f"{name}.png").tobytes() == frame.tobytes()
    before = frames["baseline"]
    measured = json.loads((root / "fixed-camera-results.json").read_text())
    view_data = {}
    for name, (title, performance, note) in VARIANTS.items():
        after = frames[name]
        difference = ImageChops.difference(before, after)
        pixels = difference.get_flattened_data() if hasattr(difference, "get_flattened_data") else difference.getdata()
        changed = sum(p != (0, 0, 0) for p in pixels)
        assert changed == measured[name]["changed_pixels"]
        sources[name]["changed_pixels"] = changed
        sheet(before, after, f"Quake E1M1 / {title}", performance, HERE / f"before-after-{name}.png")
        view_data[name] = {
            "title": title, "performance": performance, "note": note,
            "changed": changed,
            "image": "data:image/png;base64," + base64.b64encode((HERE / f"{name}.png").read_bytes()).decode(),
        }
    sheet(before, frames["geometry"], "Aggressive prototype / right-wall detail", VARIANTS["geometry"][1], HERE / "detail-wall.png", (192, 0, 320, 184), 3)
    sheet(before, frames["geometry"], "Aggressive prototype / floor detail", VARIANTS["geometry"][1], HERE / "detail-floor.png", (0, 145, 320, 184), 2)
    manifest = {
        "artifact_bundle": root.name,
        "camera": "owner-e1m1-2026-08-13",
        "origin_q12": [888798, 3824884, -728959], "angles": [43, 1088, 0],
        "guest_markers": 180, "render_observations": 176,
        "source_dimensions": [320, 240],
        "processing": "Lossless PNG conversion; labeled sheets use integer nearest-neighbor scaling. No color/contrast correction.",
        "sources": sources,
    }
    (HERE / "screenshot-provenance.json").write_text(json.dumps(manifest, indent=2) + "\n")
    baseline = "data:image/png;base64," + base64.b64encode((HERE / "baseline.png").read_bytes()).decode()
    html = (HERE / "viewer-template.html").read_text()
    html = html.replace("__BASELINE_IMAGE__", baseline).replace("__VARIANT_DATA__", json.dumps(view_data))
    (HERE / "compare.html").write_text(html)
    print(f"Wrote 5 lossless screenshots, 4 comparisons, 2 detail sheets, provenance and viewer to {HERE}")

if __name__ == "__main__":
    main()
