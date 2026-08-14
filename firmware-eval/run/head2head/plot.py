#!/usr/bin/env python3
"""Graph the head-to-head results - DO-NOT-MERGE.

    python3 plot.py          (inside `nix develop .#head2head`, which ships matplotlib)

Reads results/aggregate.json and writes results/head2head.png and
results/head2head-dark.png.

Three panels, one row per measured quantity, old (ctadl-souffle) beside new
(ctadl-rs) for each binary:

  1. on-disk footprint - a STACKED bar: import size with index size on top, so
     the total cost of analyzing one binary is the bar height and the split is
     visible inside it.
  2. summaries         - rows in the function-summary relation.
  3. SARIF paths       - `C0001` source-to-sink taint paths reported.

Colour carries engine identity and nothing else: old is blue, new is orange, in
every panel. Inside a stacked bar the two phases are separated by lightness
within that engine's own hue (a sequential step, not a second category) plus a
surface-coloured gap. The palette is the data-viz reference categorical slots 1
and 2, which pass every CVD and contrast check as a pair in both modes.

`results/TABLE.md` is the table view of the same numbers.
"""

import json
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.patches import Patch  # noqa: E402

HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results"

THEMES = {
    "light": {
        "surface": "#fcfcfb",
        "text": "#0b0b0b",
        "text_secondary": "#52514e",
        "grid": "#dcdbd6",
        "old": "#2a78d6",
        "new": "#eb6834",
        "out": "head2head.png",
    },
    "dark": {
        "surface": "#1a1a19",
        "text": "#ffffff",
        "text_secondary": "#c3c2b7",
        "grid": "#3a3a38",
        "old": "#3987e5",
        "new": "#d95926",
        "out": "head2head-dark.png",
    },
}

MB = 1024.0**2


def lighten(hex_color, surface, amount=0.55):
    """A lighter step of the same hue: blend toward the chart surface."""
    def rgb(h):
        h = h.lstrip("#")
        return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))
    c, s = rgb(hex_color), rgb(surface)
    m = tuple(round(ci + (si - ci) * amount) for ci, si in zip(c, s))
    return "#%02x%02x%02x" % m


def bar_label(ax, x, y, text, theme, dy=0.0):
    ax.text(
        x, y + dy, text,
        ha="center", va="bottom", fontsize=7.5,
        color=theme["text_secondary"], clip_on=False,
    )


def draw(theme_name):
    theme = THEMES[theme_name]
    agg = json.loads((RESULTS / "aggregate.json").read_text())
    rows = [r for r in agg["corpus"] if r.get("rs") or r.get("souffle")]
    labels = [r["label"] for r in rows]
    n = len(rows)

    def val(row, engine, key, default=0):
        r = row.get(engine)
        if not r:
            return default
        v = r.get(key)
        return default if v is None else v

    fig, axes = plt.subplots(3, 1, figsize=(12, 13.5), constrained_layout=True)
    fig.set_constrained_layout_pads(h_pad=0.22, hspace=0.10)
    fig.patch.set_facecolor(theme["surface"])

    width = 0.34
    gap = 0.02  # the 2px surface gap between the paired bars
    xs = range(n)
    left = [x - width / 2 - gap / 2 for x in xs]
    right = [x + width / 2 + gap / 2 for x in xs]

    for ax in axes:
        ax.set_facecolor(theme["surface"])
        ax.grid(axis="y", color=theme["grid"], linewidth=0.6, alpha=0.9)
        ax.set_axisbelow(True)
        for side in ("top", "right", "left"):
            ax.spines[side].set_visible(False)
        ax.spines["bottom"].set_color(theme["grid"])
        ax.tick_params(colors=theme["text_secondary"], labelsize=8.5, length=0)

    # ---- panel 1: on-disk footprint, stacked import + index ---------------
    ax = axes[0]
    for engine, pos, color in (("souffle", left, theme["old"]), ("rs", right, theme["new"])):
        imp = [val(r, engine, "import_bytes") / MB for r in rows]
        idx = [val(r, engine, "index_bytes") / MB for r in rows]
        light = lighten(color, theme["surface"])
        ax.bar(pos, imp, width, color=light, edgecolor=theme["surface"], linewidth=1.4)
        ax.bar(pos, idx, width, bottom=imp, color=color,
               edgecolor=theme["surface"], linewidth=1.4)
        for x, a, b in zip(pos, imp, idx):
            bar_label(ax, x, a + b, f"{a + b:.0f}", theme, dy=0.6)
    ax.set_ylabel("megabytes on disk", color=theme["text_secondary"], fontsize=9)
    ax.set_title(
        "Import and index size  ·  bar = import + index, total labelled",
        color=theme["text"], fontsize=11.5, loc="left", pad=26,
    )
    ax.legend(
        handles=[
            Patch(facecolor=lighten(theme["old"], theme["surface"]), label="old (souffle) import"),
            Patch(facecolor=theme["old"], label="old (souffle) index"),
            Patch(facecolor=lighten(theme["new"], theme["surface"]), label="new (rs) import"),
            Patch(facecolor=theme["new"], label="new (rs) index"),
        ],
        frameon=False, fontsize=8.5, ncol=4, loc="lower left",
        bbox_to_anchor=(0, 1.005), labelcolor=theme["text_secondary"],
    )
    ax.margins(y=0.12)

    # ---- panels 2 and 3: summaries, then paths ----------------------------
    for ax, key, title, ylabel in (
        (axes[1], "summaries", "Function summaries in the index", "summary rows"),
        (axes[2], "sarif_paths", "SARIF taint paths reported (C0001)", "paths"),
    ):
        for engine, pos, color, name in (
            ("souffle", left, theme["old"], "old (souffle)"),
            ("rs", right, theme["new"], "new (rs)"),
        ):
            vals = [val(r, engine, key) for r in rows]
            ax.bar(pos, vals, width, color=color, edgecolor=theme["surface"],
                   linewidth=1.4, label=name)
            top = max([max(vals) for vals in ([vals] or [[1]])] + [1])
            for x, v in zip(pos, vals):
                bar_label(ax, x, v, f"{v:g}", theme, dy=top * 0.015)
        ax.set_ylabel(ylabel, color=theme["text_secondary"], fontsize=9)
        ax.set_title(title, color=theme["text"], fontsize=11.5, loc="left", pad=26)
        ax.legend(frameon=False, fontsize=8.5, ncol=2, loc="lower left",
                  bbox_to_anchor=(0, 1.005), labelcolor=theme["text_secondary"])
        ax.margins(y=0.12)

    for ax in axes:
        ax.set_xticks(list(xs))
        ax.set_xticklabels(labels, color=theme["text_secondary"], fontsize=9.5)
        ax.set_xlim(-0.6, n - 0.4)

    fig.suptitle(
        "ctadl-souffle (old) vs ctadl-rs (new) on 5 firmware binaries\n"
        "same models, same Ghidra, engine defaults suppressed",
        color=theme["text"], fontsize=14,
    )

    out = RESULTS / theme["out"]
    fig.savefig(out, dpi=160, facecolor=theme["surface"], bbox_inches="tight")
    plt.close(fig)
    print(f"wrote {out}")


if __name__ == "__main__":
    for name in ("light", "dark"):
        draw(name)
