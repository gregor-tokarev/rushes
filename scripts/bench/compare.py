#!/usr/bin/env python3
"""Aggregate evaluate.py result JSONs into markdown tables: one table per task, one column per
harness, mean (min..max) over runs for the key metrics.

harness / task / run come from the result's run_meta; when missing they are parsed from the
file path with --path-regex (named groups harness, task, run).

Usage:
  compare.py results/**/*.json [--path-regex REGEX] [--out table.md]
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import os
import re
import sys

DEFAULT_REGEX = r'(?P<harness>[^/]+)/(?P<task>[^/]+)/(?P<run>[^/]+)/[^/]+\.json$'

# (label, getter path in the result, format, better: 'hi' | 'lo' | None)
METRICS = [
    ('valid output', 'summary.valid', 'bool', 'hi'),
    ('duration (s)', 'summary.duration_s', '.1f', None),
    ('segments / joins', 'summary.joins', '.0f', None),
    ('drop recall (all)', 'summary.drop_recall', 'pct', 'hi'),
    ('  false_start', 'summary.drop_recall_by_category.false_start', 'pct', 'hi'),
    ('  abandoned_take', 'summary.drop_recall_by_category.abandoned_take', 'pct', 'hi'),
    ('  instruction_to_editor', 'summary.drop_recall_by_category.instruction_to_editor', 'pct', 'hi'),
    ('  wrong_claim', 'summary.drop_recall_by_category.wrong_claim', 'pct', 'hi'),
    ('  fumble', 'summary.drop_recall_by_category.fumble', 'pct', 'hi'),
    ('  outtake', 'summary.drop_recall_by_category.outtake', 'pct', 'hi'),
    ('core keep recall', 'summary.core_keep_recall', 'pct', 'hi'),
    ('normal keep recall', 'summary.normal_keep_recall', 'pct', 'hi'),
    ('visual demo time kept', 'summary.visual_demo_time_fraction', 'pct', 'hi'),
    ('retakes: exactly one kept', 'summary.one_of_exactly_one_rate', 'pct', 'hi'),
    ('retakes: duplicated', 'summary.one_of_duplicates', '.1f', 'lo'),
    ('retakes: lost', 'summary.one_of_lost', '.1f', 'lo'),
    ('duplicated source (s)', 'summary.duplicated_source_s', '.1f', 'lo'),
    ('clipped words', 'summary.clipped_words', '.1f', 'lo'),
    ('cuts inside words', 'summary.edges_inside_words', '.1f', 'lo'),
    ('segments < 0.5 s', 'summary.segments_lt_0_5s', '.1f', 'lo'),
    ('clicks at joins', 'summary.clicks_over_threshold', '.1f', 'lo'),
    ('fillers kept', 'summary.filler_kept_pct', 'pct100', 'lo'),
    ('false-start tokens kept', 'summary.false_starts_kept', '.1f', 'lo'),
    ('words / min', 'summary.words_per_minute', '.0f', None),
    ('dead air (s, excl. demos)', 'summary.dead_air_s', '.1f', 'lo'),
    ('longest silence (s)', 'summary.longest_silence_s', '.1f', 'lo'),
    ('sync usable points', 'summary.sync_usable_points', '.0f', None),
    ('sync median (ms)', 'summary.sync_median_ms', '.0f', None),
    ('sync max abs (ms)', 'summary.sync_max_abs_ms', '.0f', 'lo'),
    ('sync drift (ms/join)', 'summary.sync_drift_ms_per_join', '.1f', None),
    ('sync drift (ms/min)', 'summary.sync_drift_ms_per_min', '.1f', None),
    ('loudness (LUFS)', 'summary.loudness_lufs', '.1f', None),
    ('true peak (dBTP)', 'summary.true_peak_dbtp', '.1f', None),
    ('cost (USD)', 'summary.cost_usd', '.2f', 'lo'),
    ('wall time (min)', 'summary.wall_seconds', 'min', 'lo'),
    ('turns', 'summary.num_turns', '.0f', None),
]


def get(d, path):
    for k in path.split('.'):
        if not isinstance(d, dict) or k not in d:
            return None
        d = d[k]
    return d


def num(v):
    if isinstance(v, bool):
        return 1.0 if v else 0.0
    if isinstance(v, (int, float)) and not (isinstance(v, float) and math.isnan(v)):
        return float(v)
    return None


def fmt(v, f):
    if v is None:
        return '-'
    if f == 'pct':
        return f'{100 * v:.0f}%'
    if f == 'pct100':
        return f'{v:.0f}%'
    if f == 'bool':
        return f'{100 * v:.0f}%'
    if f == 'min':
        return f'{v / 60:.1f}'
    return format(v, f)


def cell(vals, f):
    xs = [x for x in (num(v) for v in vals) if x is not None]
    if not xs:
        return '-'
    m = sum(xs) / len(xs)
    if len(xs) == 1 or max(xs) == min(xs):
        return fmt(m, f) + ('' if len(xs) == len(vals) else f' (n={len(xs)})')
    return f'{fmt(m, f)} ({fmt(min(xs), f)}..{fmt(max(xs), f)})' + ('' if len(xs) == len(vals) else f' n={len(xs)}')


def identify(res, path, rx):
    meta = res.get('run_meta') or {}
    out = {k: meta.get(k) for k in ('harness', 'task', 'run')}
    m = rx.search(os.path.abspath(path).replace(os.sep, '/'))
    for k in out:
        if out[k] is None and m and k in m.groupdict():
            out[k] = m.group(k)
    out['harness'] = out['harness'] or 'unknown'
    out['task'] = out['task'] or 'unknown'
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('results', nargs='+', help='result JSON files or globs')
    ap.add_argument('--path-regex', default=DEFAULT_REGEX)
    ap.add_argument('--out', help='write markdown here (default stdout)')
    args = ap.parse_args(argv)
    rx = re.compile(args.path_regex)
    files = []
    for p in args.results:
        files += sorted(glob.glob(p, recursive=True)) if any(c in p for c in '*?[') else [p]
    runs = []
    for f in files:
        try:
            res = json.load(open(f))
        except Exception as e:  # noqa
            print(f'skip {f}: {e}', file=sys.stderr)
            continue
        if 'summary' not in res:
            print(f'skip {f}: no summary', file=sys.stderr)
            continue
        runs.append((identify(res, f, rx), res, f))
    if not runs:
        sys.exit('no results')
    lines = ['# Benchmark comparison', '',
             'Cells: mean over runs (min..max). Recall/percent rows are per run then averaged.', '']
    for task in sorted({r[0]['task'] for r in runs}):
        tr = [r for r in runs if r[0]['task'] == task]
        harnesses = sorted({r[0]['harness'] for r in tr})
        by_h = {h: [r for r in tr if r[0]['harness'] == h] for h in harnesses}
        lines.append(f'## {task}')
        lines.append('')
        lines.append('| metric | ' + ' | '.join(f'{h} (n={len(by_h[h])})' for h in harnesses) + ' |')
        lines.append('|---|' + '---|' * len(harnesses))
        for label, path, f, _ in METRICS:
            vals = {h: [get(r[1], path) for r in by_h[h]] for h in harnesses}
            if all(v is None for vs in vals.values() for v in vs):
                continue
            lines.append(f'| {label} | ' + ' | '.join(cell(vals[h], f) for h in harnesses) + ' |')
        lines.append('')
        lines.append('Runs: ' + ', '.join(f"{r[0]['harness']}/{r[0].get('run') or os.path.basename(r[2])}" for r in tr))
        lines.append('')
    md = '\n'.join(lines) + '\n'
    if args.out:
        open(args.out, 'w').write(md)
    else:
        sys.stdout.write(md)


if __name__ == '__main__':
    main()
