#!/usr/bin/env python3
"""Summarise the Remove Tool A/B runs of .github/workflows/remove-ab.yml.

usage: remove_ab_report.py <results dir> <base sha> <head sha>

Reads results/<kind>_t<threads>_<seq>_<rev>.json (kind: plain | phase, rev: base | head) written by
`bench_nonlocal --json`, prints a markdown report and exits 1 if any case's output hash differs
between the revisions or between thread counts within a revision.
"""
import glob
import json
import os
import re
import statistics
import sys

NAME = re.compile(r"^(plain|phase)_t(\w+)_(\d+)_(base|head)\.json$")


def load(directory):
    runs = []
    for path in sorted(glob.glob(os.path.join(directory, "*.json"))):
        m = NAME.match(os.path.basename(path))
        if not m:
            continue
        with open(path) as f:
            report = json.load(f)
        kind, threads, seq, rev = m.groups()
        time_file = path[:-5] + ".time"
        max_rss = None
        if os.path.exists(time_file):
            with open(time_file) as f:
                t = re.search(r"Maximum resident set size \(kbytes\): (\d+)", f.read())
                if t:
                    max_rss = int(t.group(1)) / 1024
        runs.append(
            dict(kind=kind, threads=threads, seq=int(seq), rev=rev, rows={r["name"]: r for r in report["rows"]}, max_rss=max_rss, context=report.get("context", {}))
        )
    return runs


def mb(v):
    return "-" if v is None else f"{v / 1e6:.0f}"


def main():
    directory, base_sha, head_sha = sys.argv[1], sys.argv[2], sys.argv[3]
    runs = load(directory)
    if not runs:
        print("No results: the runs failed before writing any.")
        return 1
    ctx = runs[0]["context"]
    out = [f"## Remove Tool A/B, {ctx.get('size')} , {ctx.get('reps')} reps per invocation", "", f"before `{base_sha[:10]}`, after `{head_sha[:10]}`", ""]
    names = []
    for r in runs:
        for n in r["rows"]:
            if n not in names:
                names.append(n)

    # Hash check: every run of every kind must agree on a case's output.
    bad = []
    for n in names:
        by = {}
        for r in runs:
            h = r["rows"].get(n, {}).get("hash")
            if h is not None:
                by.setdefault(h, []).append(f"{r['kind']}/t{r['threads']}/{r['rev']}")
        if len(by) > 1:
            bad.append((n, by))
        elif not by:
            bad.append((n, {"(no hash)": []}))

    plain = [r for r in runs if r["kind"] == "plain"]
    for threads in sorted({r["threads"] for r in plain}, key=lambda t: (not t.isdigit(), int(t) if t.isdigit() else 0)):
        out += [f"### RAYON_NUM_THREADS = {threads}", "", "| case | p50 before (ms) | p50 after (ms) | change | RSS before / after (MB) | hash |", "|---|--:|--:|--:|--:|:-:|"]
        sel = [r for r in plain if r["threads"] == threads]
        for n in names:
            side = {}
            for rev in ("base", "head"):
                rows = [r["rows"][n] for r in sel if r["rev"] == rev and n in r["rows"]]
                p50 = [x["p50_ms"] for x in rows if x.get("p50_ms") is not None]
                rss = [x["peak_rss_bytes"] for x in rows if x.get("peak_rss_bytes")]
                side[rev] = (statistics.mean(p50) if p50 else None, max(rss) if rss else None)
            (b, brss), (h, hrss) = side["base"], side["head"]
            change = f"{(h - b) / b * 100:+.1f} %" if b and h else "-"
            ok = "yes" if all(n != bn for bn, _ in bad) else "**NO**"
            out.append(f"| {n} | {b:.1f} | {h:.1f} | {change} | {mb(brss)} / {mb(hrss)} | {ok} |" if b and h else f"| {n} | - | - | - | - | {ok} |")
        out.append("")
    procs = [r for r in runs if r["max_rss"] is not None]
    if procs:
        out += ["### Process peak RSS (`/usr/bin/time -v`, MB)", "", "| run | max RSS |", "|---|--:|"]
        for r in procs:
            out.append(f"| {r['kind']} / threads {r['threads']} / {r['rev']} #{r['seq']} | {r['max_rss']:.0f} |")
        out.append("")

    phase = [r for r in runs if r["kind"] == "phase"]
    if phase:
        out += ["### Phases (ms per run, default threads)", "", "| case | side | onion_peel | patchmatch | refresh_costs | vote | final | exp vote | exp onion |", "|---|---|--:|--:|--:|--:|--:|--:|--:|"]
        keys = ["onion_peel", "patchmatch", "refresh_costs", "vote", "final"]
        for n in names:
            for rev in ("base", "head"):
                ph = [r["rows"][n]["phases"] for r in phase if r["rev"] == rev and n in r["rows"] and "phases" in r["rows"][n]]
                if not ph:
                    continue
                avg = lambda k: statistics.mean(p.get(k, 0) for p in ph)
                label = "before" if rev == "base" else "after"
                out.append(f"| {n} | {label} | " + " | ".join(f"{avg(k):.1f}" for k in keys) + f" | {avg('exp_vote'):.0f} | {avg('exp_onion'):.0f} |")
        out.append("")

    if bad:
        out += ["### Output hashes differ", ""]
        for n, by in bad:
            out.append(f"* `{n}`: " + "; ".join(f"`{h}` from {', '.join(w)}" for h, w in by.items()))
        out.append("")
        out.append("**FAILED**: the outputs are not bit-for-bit identical.")
    else:
        out.append(f"All {len(names)} cases hash identically across both revisions, every thread count and the phase-timing builds.")
    print("\n".join(out))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
