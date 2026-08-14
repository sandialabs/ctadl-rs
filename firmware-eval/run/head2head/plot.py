#!/usr/bin/env python3
"""Graph the head-to-head results - DO-NOT-MERGE.

    python3 plot.py          (inside `nix develop .#head2head`, which ships matplotlib)

Reads results/aggregate.json and writes, in a light and a dark version each:

    head2head.png            the headline: corpus totals, old vs new. Slide-sized.
    head2head-spread.png     the per-binary ratio distribution - what n=50 buys
                             that n=5 could not: is the headline one binary or
                             all of them?
    head2head-per-binary.png every binary, tall. The appendix figure.

Why three figures and not one grid of 50. At 5 binaries a grouped bar per binary
is readable; at 50 it is a picket fence, and the reader cannot get a total or a
typical case out of it. So the three questions are split: totals answer "what
does this corpus cost and yield", the spread answers "does that hold binary by
binary", and the per-binary figure is the receipt.

Panel 1 of the headline keeps the stacked bar the experiment asked for - import
size with index size on top, so the total on-disk cost of analyzing the corpus
is the bar height and the split is visible inside it.

Colour carries engine identity and nothing else: old is blue, new is orange, in
every panel and every figure. Inside a stacked bar the two phases are separated
by lightness within that engine's own hue (a sequential step, not a second
category) plus a surface-coloured gap. The pair passes every CVD and contrast
check against both surfaces (validate_palette.js, categorical, light and dark).

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
        "suffix": "",
    },
    "dark": {
        "surface": "#1a1a19",
        "text": "#ffffff",
        "text_secondary": "#c3c2b7",
        "grid": "#3a3a38",
        "old": "#3987e5",
        "new": "#d95926",
        "suffix": "-dark",
    },
}

MB = 1024.0**2


def lighten(hex_color, surface, amount=0.55):
    """A lighter step of the same hue: blend toward the chart surface."""

    def rgb(h):
        h = h.lstrip("#")
        return tuple(int(h[i:i + 2], 16) for i in (0, 2, 4))

    c, s = rgb(hex_color), rgb(surface)
    return "#%02x%02x%02x" % tuple(
        round(ci + (si - ci) * amount) for ci, si in zip(c, s)
    )


def style(ax, theme, axis="y"):
    ax.set_facecolor(theme["surface"])
    ax.grid(axis=axis, color=theme["grid"], linewidth=0.6, alpha=0.9)
    ax.set_axisbelow(True)
    for side in ("top", "right", "left" if axis == "y" else "bottom"):
        ax.spines[side].set_visible(False)
    keep = "bottom" if axis == "y" else "left"
    ax.spines[keep].set_color(theme["grid"])
    ax.tick_params(colors=theme["text_secondary"], labelsize=8.5, length=0)


def title(ax, text, theme, pad=24, size=11.5):
    ax.set_title(text, color=theme["text"], fontsize=size, loc="left", pad=pad)


def done(row):
    return all(
        row.get(e) and row[e].get("status") == "ok" for e in ("rs", "souffle")
    )


def val(row, engine, key, default=0):
    r = row.get(engine)
    v = r.get(key) if r else None
    return default if v is None else v


# ---------------------------------------------------------------------------
# figure 1: the headline. Corpus totals, three panels, two bars each.
# ---------------------------------------------------------------------------
def draw_totals(agg, theme, out):
    s = agg["summary"]
    to, tn = s["totals"]["souffle"], s["totals"]["rs"]
    n = s["binaries_both_engines_ok"]

    fig, axes = plt.subplots(1, 3, figsize=(12, 5.0), constrained_layout=True)
    fig.patch.set_facecolor(theme["surface"])
    for ax in axes:
        style(ax, theme)
        ax.set_xlim(-0.65, 1.65)
        ax.set_xticks([0, 1])
        ax.set_xticklabels(
            ["old\n(souffle)", "new\n(rs)"], color=theme["text_secondary"], fontsize=10
        )
        ax.margins(y=0.20)

    # -- panel 1: stacked import + index -------------------------------------
    ax = axes[0]
    for x, eng, color in ((0, to, theme["old"]), (1, tn, theme["new"])):
        imp, idx = eng["import_bytes"] / MB, eng["index_bytes"] / MB
        ax.bar(x, imp, 0.52, color=lighten(color, theme["surface"]),
               edgecolor=theme["surface"], linewidth=1.6)
        ax.bar(x, idx, 0.52, bottom=imp, color=color,
               edgecolor=theme["surface"], linewidth=1.6)
        total = imp + idx
        ax.text(x, total,
                f"  {total / 1024:.1f} GB" if total >= 1024 else f"  {total:,.0f} MB",
                ha="center", va="bottom", fontsize=10.5, color=theme["text"],
                fontweight="bold")
    ax.set_ylabel("megabytes on disk", color=theme["text_secondary"], fontsize=9)
    title(ax, "On-disk footprint", theme, pad=30)
    # Two entries, one row, so the legend and the ratio note share the strip
    # under the title without colliding. The swatches are the old engine's hue
    # because they have to be some hue; what they encode is the phase, and the
    # x tick labels already say which bar is which engine.
    ax.legend(
        handles=[
            Patch(facecolor=lighten(theme["old"], theme["surface"]),
                  label="import (lighter)"),
            Patch(facecolor=theme["old"], label="index (solid)"),
        ],
        frameon=False, fontsize=7.5, ncol=2, loc="lower left",
        bbox_to_anchor=(0, 1.005), labelcolor=theme["text_secondary"],
        handlelength=1.1, columnspacing=1.0, handletextpad=0.5,
    )
    ratio_note(
        ax, f"{to['import_bytes'] / max(tn['import_bytes'], 1):.1f}x import  ·  "
            f"{to['index_bytes'] / max(tn['index_bytes'], 1):.0f}x index", theme
    )

    # -- panels 2 and 3: summaries, paths -------------------------------------
    for ax, key, head, ylabel, note in (
        (axes[1], "summaries", "Function summaries", "summary rows",
         lambda: f"{tn['summaries'] / max(to['summaries'], 1):.1f}x more"),
        (axes[2], "sarif_paths", "SARIF taint paths (C0001)", "paths",
         lambda: f"{tn['sarif_paths'] / max(to['sarif_paths'], 1):.0f}x more"),
    ):
        vals = [to[key], tn[key]]
        ax.bar([0, 1], vals, 0.52, color=[theme["old"], theme["new"]],
               edgecolor=theme["surface"], linewidth=1.6)
        for x, v in zip((0, 1), vals):
            ax.text(x, v, f"  {v:,}", ha="center", va="bottom", fontsize=10.5,
                    color=theme["text"], fontweight="bold")
        ax.set_ylabel(ylabel, color=theme["text_secondary"], fontsize=9)
        title(ax, head, theme, pad=30)
        ratio_note(ax, note(), theme)

    fig.suptitle(
        f"ctadl-souffle (old) vs ctadl-rs (new), {n} firmware binaries\n"
        "same models, same Ghidra, engine defaults suppressed  ·  corpus totals",
        color=theme["text"], fontsize=14,
    )
    save(fig, out, theme)


def ratio_note(ax, text, theme):
    """The comparison itself, stated in words on the panel it belongs to."""
    ax.text(1.0, 1.005, text, transform=ax.transAxes, ha="right", va="bottom",
            fontsize=8.5, color=theme["text_secondary"])


# ---------------------------------------------------------------------------
# figure 2: the per-binary spread. One dot per binary on a log ratio axis.
# ---------------------------------------------------------------------------
def draw_spread(agg, theme, out):
    """Does the headline hold binary by binary, or is it one big binary?

    A strip plot rather than a box plot: at n=50 every binary fits as its own
    mark, and showing them is strictly more informative than showing a summary
    of them. The box is drawn behind as the quartile context.
    """
    rows = [r for r in agg["corpus"] if done(r)]
    series = [
        ("import size", [r["souffle"]["import_bytes"] / r["rs"]["import_bytes"]
                         for r in rows if r["rs"].get("import_bytes")]),
        ("index size", [r["souffle"]["index_bytes"] / r["rs"]["index_bytes"]
                        for r in rows if r["rs"].get("index_bytes")]),
        ("summaries", [r["rs"]["summaries"] / r["souffle"]["summaries"]
                       for r in rows if r["souffle"].get("summaries")]),
    ]

    fig, ax = plt.subplots(figsize=(11, 4.4), constrained_layout=True)
    fig.patch.set_facecolor(theme["surface"])
    style(ax, theme, axis="x")
    ax.set_facecolor(theme["surface"])

    def q(s, p):
        i = p * (len(s) - 1)
        lo = int(i)
        hi = min(lo + 1, len(s) - 1)
        return s[lo] + (s[hi] - s[lo]) * (i - lo)

    for i, (name, vals) in enumerate(series):
        y = len(series) - 1 - i
        s = sorted(vals)
        q1, med, q3 = q(s, 0.25), q(s, 0.5), q(s, 0.75)
        ax.plot([q1, q3], [y, y], color=theme["grid"], linewidth=9,
                solid_capstyle="round", zorder=1)
        # Every binary as its own mark, jittered off the spine so ties are
        # visible. Colour still means engine, as everywhere else: the dot wears
        # the hue of whichever engine that binary's ratio favours, so a binary
        # that went the other way is blue and cannot hide in the crowd.
        for j, v in enumerate(s):
            ax.plot(v, y + ((j % 5) - 2) * 0.035, "o", markersize=6.5,
                    color=theme["new"] if v >= 1 else theme["old"],
                    markeredgecolor=theme["surface"],
                    markeredgewidth=1.2, alpha=0.9, zorder=2)
        ax.plot([med], [y], "|", markersize=26, markeredgewidth=2.6,
                color=theme["text"], zorder=3)
        ax.text(med, y + 0.30, f"median {med:.1f}x", ha="center", va="bottom",
                fontsize=9.5, color=theme["text"], fontweight="bold")

    ax.axvline(1.0, color=theme["text_secondary"], linewidth=1.0, linestyle=":")
    ax.text(1.0, len(series) - 0.42, " parity", fontsize=8.5, va="top",
            color=theme["text_secondary"])
    ax.set_xscale("log")
    # "10x", not "10^1" - the reader is comparing engines, not reading exponents
    ax.xaxis.set_major_formatter(
        matplotlib.ticker.FuncFormatter(lambda v, _: f"{v:g}x")
    )
    ax.xaxis.set_minor_formatter(matplotlib.ticker.NullFormatter())
    ax.set_yticks(range(len(series)))
    ax.set_yticklabels(
        ["summaries\nnew / old", "index size\nold / new", "import size\nold / new"],
        color=theme["text_secondary"], fontsize=10,
    )
    ax.set_ylim(-0.55, len(series) - 0.45)
    ax.set_xlabel("ratio, log scale — right of parity favours the new engine",
                  color=theme["text_secondary"], fontsize=9)
    title(
        ax,
        f"Per-binary spread — one dot per binary ({len(series[0][1])} binaries), "
        "bar = interquartile range",
        theme, pad=14,
    )
    save(fig, out, theme)


# ---------------------------------------------------------------------------
# figure 3: every binary. The appendix.
# ---------------------------------------------------------------------------
def draw_per_binary(agg, theme, out):
    rows = sorted(
        [r for r in agg["corpus"] if done(r)],
        key=lambda r: r["rs"]["binary_bytes"],
    )
    n = len(rows)
    labels = [f"{r['label']}  ({r['rs']['binary_bytes'] // 1024}K)" for r in rows]

    fig, axes = plt.subplots(
        1, 3, figsize=(15, max(8.0, 0.30 * n + 2.2)),
        constrained_layout=True, sharey=True,
    )
    fig.patch.set_facecolor(theme["surface"])

    h = 0.36
    gap = 0.03
    ys = range(n)
    up = [y + h / 2 + gap / 2 for y in ys]  # old, above
    dn = [y - h / 2 - gap / 2 for y in ys]  # new, below

    for ax in axes:
        style(ax, theme, axis="x")
        ax.set_ylim(-0.7, n - 0.3)

    # -- footprint, stacked ---------------------------------------------------
    ax = axes[0]
    for engine, pos, color in (("souffle", up, theme["old"]), ("rs", dn, theme["new"])):
        imp = [val(r, engine, "import_bytes") / MB for r in rows]
        idx = [val(r, engine, "index_bytes") / MB for r in rows]
        ax.barh(pos, imp, h, color=lighten(color, theme["surface"]),
                edgecolor=theme["surface"], linewidth=1.0)
        ax.barh(pos, idx, h, left=imp, color=color,
                edgecolor=theme["surface"], linewidth=1.0)
    ax.set_xlabel("megabytes on disk", color=theme["text_secondary"], fontsize=9)
    title(ax, "On-disk footprint (import + index)", theme, pad=46, size=10.5)
    ax.legend(
        handles=[
            Patch(facecolor=lighten(theme["old"], theme["surface"]), label="old import"),
            Patch(facecolor=theme["old"], label="old index"),
            Patch(facecolor=lighten(theme["new"], theme["surface"]), label="new import"),
            Patch(facecolor=theme["new"], label="new index"),
        ],
        frameon=False, fontsize=8, ncol=2, loc="lower left",
        bbox_to_anchor=(0, 1.005), labelcolor=theme["text_secondary"],
    )

    # -- summaries and paths --------------------------------------------------
    for ax, key, head, xlabel in (
        (axes[1], "summaries", "Function summaries", "summary rows"),
        (axes[2], "sarif_paths", "SARIF taint paths (C0001)", "paths"),
    ):
        for engine, pos, color, name in (
            ("souffle", up, theme["old"], "old (souffle)"),
            ("rs", dn, theme["new"], "new (rs)"),
        ):
            ax.barh(pos, [val(r, engine, key) for r in rows], h, color=color,
                    edgecolor=theme["surface"], linewidth=1.0, label=name)
        ax.set_xlabel(xlabel, color=theme["text_secondary"], fontsize=9)
        title(ax, head, theme, pad=46, size=10.5)
        ax.legend(frameon=False, fontsize=8, ncol=2, loc="lower left",
                  bbox_to_anchor=(0, 1.005), labelcolor=theme["text_secondary"])
        if key == "summaries":
            # Summaries span 10 to 54,000 across the corpus; on a linear axis two
            # binaries own the panel and the other 45 are a hairline. Every count
            # here is >= 10, so a log axis costs nothing and shows all 47.
            ax.set_xscale("log")
            ax.set_xlim(left=8)
            ax.xaxis.set_major_formatter(
                matplotlib.ticker.FuncFormatter(lambda v, _: f"{v:,.0f}")
            )
            ax.xaxis.set_minor_formatter(matplotlib.ticker.NullFormatter())
        # The footprint panel stays linear: a stacked bar on a log axis is a lie,
        # its segments stop being additive. The paths panel stays linear because
        # a log axis would drop the zeros, which are most of the reason to look.

    axes[0].set_yticks(list(ys))
    axes[0].set_yticklabels(labels, color=theme["text_secondary"], fontsize=8)

    fig.suptitle(
        f"ctadl-souffle (old, upper bar) vs ctadl-rs (new, lower bar)  ·  "
        f"all {n} binaries, smallest at the bottom",
        color=theme["text"], fontsize=13,
    )
    save(fig, out, theme)


def save(fig, out, theme):
    fig.savefig(out, dpi=160, facecolor=theme["surface"], bbox_inches="tight")
    plt.close(fig)
    print(f"wrote {out}")


def main():
    agg = json.loads((RESULTS / "aggregate.json").read_text())
    for name, theme in THEMES.items():
        sfx = theme["suffix"]
        draw_totals(agg, theme, RESULTS / f"head2head{sfx}.png")
        draw_spread(agg, theme, RESULTS / f"head2head-spread{sfx}.png")
        draw_per_binary(agg, theme, RESULTS / f"head2head-per-binary{sfx}.png")


if __name__ == "__main__":
    main()
