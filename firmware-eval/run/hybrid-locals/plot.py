#!/usr/bin/env python3
"""Deck figures for the hybrid-locals experiment. Light and dark, PNG + SVG.

Reads `runs/aggregate.json`; writes `runs/figs/`.

Figure choices (form follows the data's job):
  fig1 trade-off scatter -- the two ratios are one *relationship*, and the question
      "what does the memory win cost in time?" is answered by where the cloud sits
      relative to the (1,1) crosshair. One dot per binary, area = the control's peak
      footprint, so the eye lands on the benchmarks that actually cost something.
  fig2 ratio vs scale -- two stacked panels on a shared log x (control peak). This is
      the regime story: does the structure pay off everywhere, or only when `locals`
      gets big? Two panels rather than two y-axes: never a dual-axis chart.
  fig3 corpus totals -- what the whole suite costs, as paired bars. Two separate
      panels because seconds and megabytes do not share a scale.
  fig4 per binary -- the picket fence, sorted, kept for the appendix.

Palette: categorical slots 1-2 of the reference palette (blue/orange), validated
for both modes with `validate_palette.js` (all checks PASS, worst adjacent CVD
delta-E 24.7 light / 26.8 dark).

Colour means ONE thing across the whole deck: **blue is the hybrid build, orange is
the control**. So fig3, which draws both, uses both; every ratio chart is
hybrid-relative and therefore blue alone, with the metric carried by the panel title
and axis label rather than by hue. Grey is not a series -- it de-emphasises the
benchmarks whose index phase is too short to measure.
"""
import json
import math
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
from matplotlib.ticker import FuncFormatter, LogLocator, NullFormatter  # noqa: E402

HERE = Path(__file__).resolve().parent
RUNS = HERE / "runs"
FIGS = RUNS / "figs"

THEMES = {
    "light": dict(
        surface="#fcfcfb",
        text="#0b0b0b",
        text2="#52514e",
        grid="#dedcd6",
        s1="#2a78d6",  # hybrid
        s2="#eb6834",  # control
        muted="#9b9a94",
        good="#0ca30c",
        crit="#d03b3b",
    ),
    "dark": dict(
        surface="#1a1a19",
        text="#ffffff",
        text2="#c3c2b7",
        grid="#3a3a37",
        s1="#3987e5",
        s2="#d95926",
        muted="#77766f",
        good="#0ca30c",
        crit="#d03b3b",
    ),
}

SUBSTANTIVE_S = 1.0  # control index wall below this is wakeup noise, not a measurement


def apply_theme(T):
    """Ink colours go through rcParams, not per-artist.

    `ax.set_title(...)` re-applies `axes.titlecolor` every time it is called, so a
    colour set on `ax.title` beforehand is silently reverted -- which is how a dark
    figure ends up with a black title on a black surface.
    """
    matplotlib.rcParams.update(
        {
            "text.color": T["text"],
            "axes.titlecolor": T["text"],
            "axes.labelcolor": T["text2"],
            "xtick.color": T["text2"],
            "ytick.color": T["text2"],
        }
    )


def style(fig, axes, T):
    fig.patch.set_facecolor(T["surface"])
    for ax in axes:
        ax.set_facecolor(T["surface"])
        for s in ("top", "right"):
            ax.spines[s].set_visible(False)
        for s in ("left", "bottom"):
            ax.spines[s].set_color(T["grid"])
        ax.tick_params(colors=T["text2"], labelsize=9)
        ax.xaxis.label.set_color(T["text2"])
        ax.yaxis.label.set_color(T["text2"])
        ax.title.set_color(T["text"])
        ax.grid(True, color=T["grid"], lw=0.6, alpha=0.7)
        ax.set_axisbelow(True)


def save(fig, name, theme):
    FIGS.mkdir(parents=True, exist_ok=True)
    suffix = "" if theme == "light" else "-dark"
    for ext in ("png", "svg"):
        fig.savefig(
            FIGS / f"{name}{suffix}.{ext}",
            dpi=200,
            bbox_inches="tight",
            facecolor=fig.get_facecolor(),
        )
    plt.close(fig)


def xfmt(v, _=None):
    if v <= 0:
        return ""
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return f"{s}x"


def ratio_axis(ax, which="both"):
    """A log ratio axis whose ticks read `0.5x`, `1x`, `2x` -- majors AND minors.

    On a log scale spanning under a decade, matplotlib puts everything on the minor
    ticks and prints them as `1.1 x 10^0`. Label both levels with the plain-ratio
    formatter instead.
    """
    axes = []
    if which in ("x", "both"):
        axes.append(ax.xaxis)
    if which in ("y", "both"):
        axes.append(ax.yaxis)
    for a in axes:
        a.set_major_locator(LogLocator(base=10, subs=(1.0,)))
        a.set_major_formatter(FuncFormatter(xfmt))
        a.set_minor_locator(LogLocator(base=10, subs=(0.2, 0.3, 0.5, 0.7, 1.5, 2.0, 3.0, 5.0, 7.0)))
        a.set_minor_formatter(FuncFormatter(xfmt))
        a.set_tick_params(which="minor", labelsize=8)


def load():
    agg = json.loads((RUNS / "aggregate.json").read_text())
    pairs = [p for p in agg["pairs"] if p.get("time_ratio") and p.get("mem_ratio")]
    return agg, pairs


def fig1(agg, pairs, theme):
    T = THEMES[theme]
    big = [p for p in pairs if p["control"]["wall_s"] >= SUBSTANTIVE_S]
    small = [p for p in pairs if p["control"]["wall_s"] < SUBSTANTIVE_S]
    fig, ax = plt.subplots(figsize=(9.5, 5.6))
    style(fig, [ax], T)

    ax.axhline(1.0, color=T["text2"], lw=1.2, ls="--", zorder=1)
    ax.axvline(1.0, color=T["text2"], lw=1.2, ls="--", zorder=1)

    def area(p):
        return 18 + 260 * (math.log10(max(p["control"]["peak_fp_mb"], 10)) - 1) / 3.5

    ax.scatter(
        [p["time_ratio"] for p in small],
        [p["mem_ratio"] for p in small],
        s=[area(p) for p in small],
        c=T["muted"],
        alpha=0.55,
        lw=0.8,
        edgecolors=T["surface"],
        zorder=2,
        label=f"index phase < {SUBSTANTIVE_S:g}s (n={len(small)})",
    )
    ax.scatter(
        [p["time_ratio"] for p in big],
        [p["mem_ratio"] for p in big],
        s=[area(p) for p in big],
        c=T["s1"],
        alpha=0.85,
        lw=0.8,
        edgecolors=T["surface"],
        zorder=3,
        label=f"index phase >= {SUBSTANTIVE_S:g}s (n={len(big)})",
    )

    # name the extremes -- the benchmarks a reader will ask about
    for p in sorted(big, key=lambda r: r["mem_ratio"])[:3] + sorted(
        big, key=lambda r: -r["time_ratio"]
    )[:2]:
        ax.annotate(
            p["name"],
            (p["time_ratio"], p["mem_ratio"]),
            textcoords="offset points",
            xytext=(7, 5),
            fontsize=8,
            color=T["text2"],
        )

    ax.set_xscale("log")
    ax.set_yscale("log")
    ratio_axis(ax)
    ax.set_xlabel("index wall time,  hybrid / control  (right = hybrid slower)")
    ax.set_ylabel("peak footprint,  hybrid / control  (down = hybrid smaller)")
    s = agg["summary"].get("substantive_only") or {}
    sub = (
        f"{s['n']} benchmarks with a real index phase: "
        f"memory {s['mem_ratio_geomean']:.2f}x, time {s['time_ratio_geomean']:.2f}x (geomean). "
        f"Bubble area = control peak footprint."
        if s
        else ""
    )
    ax.set_title(
        "The trade: hybrid `locals` buys memory with time",
        fontsize=13,
        loc="left",
        pad=24,
    )
    if sub:
        ax.text(0.0, 1.015, sub, transform=ax.transAxes, fontsize=9, color=T["text2"])
    leg = ax.legend(frameon=False, fontsize=9, loc="upper left")
    for t in leg.get_texts():
        t.set_color(T["text2"])
    fig.tight_layout()
    save(fig, "fig1-tradeoff", theme)


def binned_median(xs, ys, nbins=6):
    pts = sorted(zip(xs, ys))
    if len(pts) < nbins * 2:
        return [], []
    per = len(pts) // nbins
    bx, by = [], []
    for i in range(nbins):
        chunk = pts[i * per : (i + 1) * per if i < nbins - 1 else len(pts)]
        if not chunk:
            continue
        bx.append(sum(c[0] for c in chunk) / len(chunk))
        srt = sorted(c[1] for c in chunk)
        by.append(srt[len(srt) // 2])
    return bx, by


def fig2(agg, pairs, theme):
    """Ratios against `locals` size -- the relation the data structure actually stores.

    x is rows in `locals`, not the control's peak footprint: it is a property of the
    benchmark rather than an outcome of one of the two conditions being compared, so
    it can carry a causal reading. Only the benchmarks with a measured row count
    appear (the sub-second ones were not re-run for stats).
    """
    T = THEMES[theme]
    pts = [p for p in pairs if (p.get("workload") or {}).get("locals_rows")]
    fig, axes = plt.subplots(2, 1, figsize=(9.5, 6.4), sharex=True)
    style(fig, axes, T)
    x = [p["workload"]["locals_rows"] for p in pts]

    for ax, key, color, ylab, title in (
        (axes[0], "mem_ratio", T["s1"], "peak footprint ratio", "Memory: the win grows with the relation"),
        (axes[1], "time_ratio", T["s1"], "wall time ratio", "Time: the cost does not"),
    ):
        y = [p[key] for p in pts]
        ax.axhline(1.0, color=T["text2"], lw=1.2, ls="--")
        ax.scatter(x, y, s=26, c=color, alpha=0.8, lw=0.8, edgecolors=T["surface"], zorder=3)
        bx, by = binned_median(x, y, 6)
        if bx:
            ax.plot(bx, by, color=color, lw=2.0, zorder=4)
            ax.scatter(bx, by, s=42, c=color, lw=1.6, edgecolors=T["surface"], zorder=5)
        ax.set_xscale("log")
        ax.set_yscale("log")
        ratio_axis(ax, "y")
        ax.set_ylabel(ylab)
        ax.set_title(title, fontsize=11, loc="left")
    axes[1].set_xlabel("rows in `locals` at fixpoint (log)")
    fig.suptitle(
        f"Hybrid / control against the size of `locals`  (n={len(pts)})",
        fontsize=13,
        x=0.005,
        ha="left",
        color=T["text"],
    )
    fig.tight_layout()
    save(fig, "fig2-by-scale", theme)


def fig5(agg, pairs, theme):
    """The mechanism, in one number per run: bytes of peak footprint per `locals` row.

    Two conditions on one axis, so this is where the condition colours are used.
    """
    T = THEMES[theme]
    pts = [p for p in pairs if (p.get("workload") or {}).get("locals_rows")]
    pts.sort(key=lambda p: p["workload"]["locals_rows"])
    x = [p["workload"]["locals_rows"] for p in pts]
    fig, ax = plt.subplots(figsize=(9.5, 5.2))
    style(fig, [ax], T)
    for cond, color in (("control", T["s2"]), ("hybrid", T["s1"])):
        y = [p[cond]["peak_fp_mb"] * 1024 * 1024 / p["workload"]["locals_rows"] for p in pts]
        ax.scatter(x, y, s=30, c=color, alpha=0.85, lw=0.8, edgecolors=T["surface"], label=cond, zorder=3)
        bx, by = binned_median(x, y, 6)
        if bx:
            ax.plot(bx, by, color=color, lw=2.0, zorder=4)
    ax.set_xscale("log")
    ax.set_yscale("log")
    ax.set_xlabel("rows in `locals` at fixpoint (log)")
    ax.set_ylabel("peak footprint per `locals` row (bytes, log)")
    ax.set_title(
        "Where the memory goes: bytes of peak footprint per row of `locals`",
        fontsize=13,
        loc="left",
        pad=10,
    )
    leg = ax.legend(frameon=False, fontsize=10, loc="upper right")
    for t in leg.get_texts():
        t.set_color(T["text2"])
    fig.tight_layout()
    save(fig, "fig5-bytes-per-row", theme)


def fig3(agg, pairs, theme):
    T = THEMES[theme]
    tot = agg["summary"]["totals"]
    fig, axes = plt.subplots(1, 2, figsize=(8.6, 4.2))
    style(fig, axes, T)
    for ax, vals, unit, title, ratio in (
        (
            axes[0],
            [tot["hybrid_wall_s"], tot["control_wall_s"]],
            "s",
            "Total index wall time",
            tot["wall_ratio"],
        ),
        (
            axes[1],
            [tot["hybrid_peak_sum_mb"] / 1024, tot["control_peak_sum_mb"] / 1024],
            "GB",
            "Sum of per-binary peak footprint",
            tot["peak_sum_ratio"],
        ),
    ):
        bars = ax.bar(
            ["hybrid", "control"],
            vals,
            color=[T["s1"], T["s2"]],
            width=0.55,
        )
        for b, v in zip(bars, vals):
            ax.annotate(
                f"{v:,.0f} {unit}",
                (b.get_x() + b.get_width() / 2, v),
                textcoords="offset points",
                xytext=(0, 4),
                ha="center",
                fontsize=10,
                color=T["text"],
            )
        ax.set_title(f"{title}\nhybrid = {ratio:.2f}x control", fontsize=11, loc="left")
        ax.grid(axis="x", visible=False)
        ax.set_ylim(0, max(vals) * 1.18)
    fig.suptitle(
        f"Corpus totals over {agg['summary']['n_paired_ok']} paired binaries",
        fontsize=13,
        x=0.005,
        ha="left",
        color=T["text"],
    )
    fig.tight_layout()
    save(fig, "fig3-totals", theme)


def fig4(agg, pairs, theme):
    T = THEMES[theme]
    ps = sorted(pairs, key=lambda p: p["mem_ratio"])
    n = len(ps)
    fig, axes = plt.subplots(1, 2, figsize=(10, max(6.0, n * 0.13)), sharey=True)
    style(fig, axes, T)
    ys = range(n)
    for ax, key, color, title in (
        (axes[0], "mem_ratio", T["s1"], "peak footprint ratio"),
        (axes[1], "time_ratio", T["s1"], "wall time ratio"),
    ):
        # Bars anchored at 1x, not at the axis edge: on a log ratio axis the left edge
        # is arbitrary, so a bar drawn from it encodes nothing. From 1x, length is the
        # deviation and direction is the sign.
        vals = [p[key] for p in ps]
        ax.barh(list(ys), [v - 1.0 for v in vals], left=1.0, color=color, height=0.72)
        ax.axvline(1.0, color=T["text2"], lw=1.2, ls="--")
        ax.set_xscale("log")
        ratio_axis(ax, "x")
        ax.set_title(title, fontsize=11, loc="left")
        ax.grid(axis="y", visible=False)
    axes[0].set_yticks(list(ys))
    axes[0].set_yticklabels(
        [f"{p['name']} ({p['control']['wall_s']:.1f}s)" for p in ps], fontsize=6.5
    )
    axes[0].set_ylim(-1, n)
    fig.suptitle(
        "Every binary, sorted by memory ratio (appendix)",
        fontsize=13,
        x=0.005,
        ha="left",
        color=T["text"],
    )
    fig.tight_layout()
    save(fig, "fig4-per-binary", theme)


def main():
    agg, pairs = load()
    for theme in ("light", "dark"):
        apply_theme(THEMES[theme])
        fig1(agg, pairs, theme)
        fig2(agg, pairs, theme)
        fig3(agg, pairs, theme)
        fig4(agg, pairs, theme)
        fig5(agg, pairs, theme)
    print(f"wrote {len(list(FIGS.glob('*')))} files to {FIGS}")


if __name__ == "__main__":
    main()
