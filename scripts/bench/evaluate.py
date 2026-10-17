#!/usr/bin/env python3
"""Measure one edited video against its source footage, a word-level reference
transcript and a gold annotation.

Everything is measured from the output file itself. An EDL, when given, is only a hint:
its ranges seed the timeline search and are reported next to what was measured.

Usage:
  evaluate.py --source SRC.mp4 --reference SCRIBE.json --gold GOLD.json \
              --output final.mp4 [--edl edl.json] [--run-meta run.json] --out result.json

See README.md next to this file for what each metric means.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

import numpy as np

VERSION = '1.1'
SR = 16000                 # analysis sample rate (mono)
HOP = 160                  # 10 ms envelope frames
ENV_RATE = SR // HOP       # 100 envelope frames per second
CLICK_SR = 48000           # click analysis sample rate
TW, TH = 64, 36            # thumbnail size used for video matching
GRID = (4, 4)              # blocks (rows, cols) for block-normalised video matching
NN_W, NN_H = 32, 18        # global nearest-neighbour search feature size
# 'pts' audio clock (used for the source): silence-fill timestamp gaps and trim overlaps > 1 ms
# so sample n sits at t=n/sr of the container clock - the clock cutting tools (trim, -ss) use.
# 'player' audio clock (used for the output): every decoded sample played back to back from the
# first audio frame's timestamp - what players do with an MP4 whose audio timestamps were
# squeezed (e.g. concat -c copy of AAC segments), and what a viewer hears.
TS_HONOUR = 'min_comp=0.001:min_hard_comp=0.001:first_pts=0'

DEFAULT_FILLERS = ','.join([
    r'э+(-э+)*', r'а(-а)+', r'а{2,}', r'м(-м)+', r'м{2,}', r'эм+', r'ам', r'хм+',
    r'uh+', r'um+', r'uhm+', r'hm+', r'mm+', r'e(-e)+', r'a(-a)+', r'm(-m)+', r'eh+', r'ah+', r'er+m+',
])


def log(*a):
    print('[evaluate]', *a, file=sys.stderr, flush=True)


# ----------------------------------------------------------------------------- probing

def ffprobe(path):
    r = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-show_format', '-of', 'json', path],
                       capture_output=True, text=True, timeout=120)
    if r.returncode:
        raise RuntimeError(f'ffprobe failed on {path}: {r.stderr.strip()[:500]}')
    return json.loads(r.stdout)


def fnum(x, default=None):
    try:
        v = float(x)
        return v if math.isfinite(v) else default
    except (TypeError, ValueError):
        return default


def frac(s):
    if not s or s == '0/0':
        return None
    if '/' in s:
        a, b = s.split('/')
        return float(a) / float(b) if float(b) else None
    return fnum(s)


def first_stream(info, kind):
    for s in info.get('streams', []):
        if s.get('codec_type') == kind:
            return s
    return None


def stream_duration(s, fmt):
    d = fnum(s.get('duration'))
    if d is None and s.get('duration_ts') and s.get('time_base'):
        d = float(s['duration_ts']) * frac(s['time_base'])
    if d is None:
        d = fnum((s.get('tags') or {}).get('DURATION'))
    return d


def packet_continuity(path, sel, timeout=300):
    """Timestamp continuity of a stream's packets (gaps/overlaps > 1 ms)."""
    try:
        r = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', sel, '-show_entries',
                            'packet=pts_time,duration_time', '-of', 'csv=p=0', path],
                           capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {'checked': False, 'reason': 'timeout'}
    rows = []
    for line in r.stdout.splitlines():
        p = line.split(',')
        if len(p) >= 2 and p[0] not in ('', 'N/A') and p[1] not in ('', 'N/A'):
            rows.append((float(p[0]), float(p[1])))
    if len(rows) < 2:
        return {'checked': False, 'reason': 'no packets'}
    a = np.array(sorted(rows))
    gap = a[1:, 0] - (a[:-1, 0] + a[:-1, 1])
    bad = np.abs(gap) > 0.001
    return {'checked': True, 'packets': int(len(a)), 'discontinuities': int(bad.sum()),
            'gap_total_ms': round(float(gap[gap > 0.001].sum() * 1000), 2),
            'overlap_total_ms': round(float(-gap[gap < -0.001].sum() * 1000), 2),
            'max_abs_ms': round(float(np.abs(gap).max() * 1000), 2)}


# ----------------------------------------------------------------------------- decoding

def decode(path, info, tmpdir, want_video=True, want48=False, timeout=900, audio_clock='pts', band_aspect=None):
    """One ffmpeg pass: every video frame as a 64x36 gray thumbnail with its pts, and the
    first audio stream as mono float at 16 kHz (and 48 kHz if asked). Doubles as the
    end-to-end decode check. Audio is placed on the container clock (timestamp gaps filled
    with silence, overlaps trimmed, first_pts=0), so sample n is t=n/sr of the same clock as
    the video pts."""
    has_v = first_stream(info, 'video') is not None
    has_a = first_stream(info, 'audio') is not None
    pts_file = os.path.join(tmpdir, 'pts.txt')
    v_file = os.path.join(tmpdir, 'v.raw')
    a16_file = os.path.join(tmpdir, 'a16.raw')
    a48_file = os.path.join(tmpdir, 'a48.raw')
    cmd = ['ffmpeg', '-nostdin', '-hide_banner', '-v', 'error', '-i', path]
    if want_video and has_v:
        vf = (f'scale={TW}:{TH}:flags=area,format=gray,entropy,'
              f'metadata=mode=print:file={pts_file}')
        cmd += ['-map', '0:v:0', '-vf', vf, '-fps_mode', 'passthrough', '-f', 'rawvideo', '-y', v_file]
        if band_aspect:
            # the middle band holding a letterboxed source of aspect band_aspect ('fit' framing)
            vs_ = first_stream(info, 'video')
            oa = (vs_.get('width') or 1) / max(1, vs_.get('height') or 1)
            crop = (f'crop=iw:trunc(iw/{band_aspect:.6f}/2)*2' if oa < band_aspect
                    else f'crop=trunc(ih*{band_aspect:.6f}/2)*2:ih')
            cmd += ['-map', '0:v:0', '-vf', f'{crop},scale={TW}:{TH}:flags=area,format=gray',
                    '-fps_mode', 'passthrough', '-f', 'rawvideo', '-y', os.path.join(tmpdir, 'band.raw')]
    if has_a:
        # player clock: restamp frames back to back (N/SR) so nothing downstream reorders or drops
        pre, opt = ('', ':' + TS_HONOUR) if audio_clock == 'pts' else ('asetpts=N/SR/TB,', '')
        cmd += ['-map', '0:a:0', '-af', f'{pre}aresample={SR}{opt},aformat=channel_layouts=mono',
                '-f', 'f32le', '-y', a16_file]
        if want48:
            cmd += ['-map', '0:a:0', '-af', f'{pre}aresample={CLICK_SR}{opt},aformat=channel_layouts=mono',
                    '-f', 'f32le', '-y', a48_file]
    t0 = time.time()
    timed_out = False
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        rc, stderr = r.returncode, r.stderr
    except subprocess.TimeoutExpired as e:
        timed_out, rc = True, None
        stderr = (e.stderr or b'').decode('utf8', 'replace') if isinstance(e.stderr, bytes) else (e.stderr or '')
    out = {'wall_s': round(time.time() - t0, 2), 'returncode': rc, 'timed_out': timed_out,
           'error_lines': [l for l in stderr.splitlines() if l.strip()]}
    if want_video and has_v and os.path.exists(v_file):
        raw = np.fromfile(v_file, dtype=np.uint8)
        n = raw.size // (TW * TH)
        thumbs = raw[:n * TW * TH].reshape(n, TH, TW)
        pts, pts_time = [], []
        with open(pts_file) as f:
            for line in f:
                m = re.match(r'frame:\s*\d+\s+pts:\s*(-?\d+)\s+pts_time:\s*(\S+)', line)
                if m:
                    pts.append(int(m.group(1)))
                    pts_time.append(float(m.group(2)))
        pts = np.array(pts, dtype=np.float64)
        pts_time = np.array(pts_time, dtype=np.float64)
        vs = first_stream(info, 'video')
        tb = frac(vs.get('time_base') or '') or 0.0
        t = pts * tb if tb else pts_time
        # metadata prints pts_time with 6 significant digits: use pts * time_base when consistent
        if tb and len(t) and np.max(np.abs(t - pts_time) / np.maximum(1e-3, np.abs(pts_time))) > 2e-5:
            k = np.argmax(np.abs(pts))
            tb2 = pts_time[k] / pts[k] if pts[k] else 0
            t = pts * tb2 if tb2 else pts_time
        m = min(len(t), len(thumbs))
        out['thumbs'] = thumbs[:m]
        out['pts'] = t[:m]
        out['frames_decoded'] = int(n)
        bf = os.path.join(tmpdir, 'band.raw')
        if band_aspect and os.path.exists(bf):
            rb = np.fromfile(bf, dtype=np.uint8)
            nb = rb.size // (TW * TH)
            if nb >= m:
                out['band'] = rb[:m * TW * TH].reshape(m, TH, TW)
    shift = 0.0
    if has_a and audio_clock == 'player':
        shift = first_audio_time(path, info)
        out['audio_start_s'] = shift
    if has_a and os.path.exists(a16_file):
        out['a16'] = align_start(np.fromfile(a16_file, dtype=np.float32), shift, SR)
    if want48 and has_a and os.path.exists(a48_file):
        out['a48'] = align_start(np.fromfile(a48_file, dtype=np.float32), shift, CLICK_SR)
    return out


def first_audio_time(path, info):
    """Timestamp of the first decoded audio frame relative to the container start (the
    clock ffmpeg uses for the video pts)."""
    try:
        r = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a:0', '-read_intervals', '%+#1',
                            '-show_entries', 'frame=pts_time', '-of', 'csv=p=0', path],
                           capture_output=True, text=True, timeout=60)
        t = fnum(r.stdout.strip().split('\n')[0].strip(',')) if r.stdout.strip() else None
    except subprocess.TimeoutExpired:
        t = None
    if t is None:
        return 0.0
    return t - (fnum(info.get('format', {}).get('start_time')) or 0.0)


def align_start(x, shift, sr):
    n = int(round(shift * sr))
    if n > 0:
        return np.concatenate([np.zeros(n, dtype=x.dtype), x])
    if n < 0:
        return x[-n:]
    return x


def audio_clock_breakpoints(path, fmt_start=0.0, sample_rate=48000, timeout=600):
    """(container time, container - plain-decode time) breakpoints for the first audio stream.
    A plain decode (what most transcription pipelines feed the ASR) concatenates decoded
    samples and ignores timestamp gaps; the container clock does not."""
    try:
        r = subprocess.run(['ffprobe', '-v', 'error', '-select_streams', 'a:0', '-show_entries',
                            'frame=pts_time,nb_samples', '-of', 'compact=p=0', path],
                           capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return [[0.0, 0.0]]
    pts, dur = [], []
    for line in r.stdout.splitlines():
        kv = dict(x.split('=', 1) for x in line.split('|') if '=' in x)
        t, nb = fnum(kv.get('pts_time')), fnum(kv.get('nb_samples'))
        if t is None or not nb:
            continue
        pts.append(t)
        dur.append(nb / sample_rate)
    if not pts:
        return [[0.0, 0.0]]
    pts = np.array(pts) - fmt_start
    plain = np.concatenate([[0.0], np.cumsum(dur)[:-1]])
    off = pts - plain
    bp = [[float(pts[0]), float(off[0])]]
    for i in range(1, len(off)):
        if abs(off[i] - bp[-1][1]) > 0.0005:
            bp.append([float(pts[i]), float(off[i])])
    return bp


class Clock:
    """Map container-clock source times to the reference transcript's timeline."""

    def __init__(self, breakpoints, mode='plain'):
        bp = np.array(breakpoints if (breakpoints and mode == 'plain') else [[0.0, 0.0]], dtype=float)
        self.t = bp[:, 0]
        self.o = bp[:, 1]

    def to_ref(self, x):
        x = np.asarray(x, dtype=float)
        i = np.clip(np.searchsorted(self.t, x, side='right') - 1, 0, len(self.t) - 1)
        return x - self.o[i]


CLICK_VERSION = 2


def decode_audio_only(path, sr, timeout=900):
    r = subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', path, '-map', '0:a:0', '-af',
                        f'aresample={sr}:{TS_HONOUR},aformat=channel_layouts=mono', '-f', 'f32le', '-'],
                       capture_output=True, timeout=timeout)
    return np.frombuffer(r.stdout, dtype=np.float32)


def file_key(path):
    st = os.stat(path)
    h = hashlib.sha1(f'{os.path.realpath(path)}|{st.st_size}|{int(st.st_mtime)}|{VERSION}'.encode()).hexdigest()
    return h[:16]


def load_source(path, info, cache_dir, timeout):
    """Decode the source once and cache thumbnails, pts, 16 kHz audio and the click baseline."""
    d = os.path.join(cache_dir, 'src_' + file_key(path))
    files = {k: os.path.join(d, k + '.npy') for k in ('thumbs', 'pts', 'a16')}
    meta_f = os.path.join(d, 'meta.json')
    if all(os.path.exists(f) for f in files.values()) and os.path.exists(meta_f):
        log('source: cache hit', d)
        src = {k: np.load(f, mmap_mode='r' if k == 'thumbs' else None) for k, f in files.items()}
        src['thumbs'] = np.asarray(src['thumbs'])
        meta = json.load(open(meta_f))
        if meta.get('click_version') != CLICK_VERSION:
            meta['click_baseline'] = click_baseline(decode_audio_only(path, CLICK_SR, timeout))
            meta['click_version'] = CLICK_VERSION
            json.dump(meta, open(meta_f, 'w'))
        src.update(meta)
        src['cache'] = d
        return src
    log('source: decoding (first run, cached afterwards) ...')
    tmp = tempfile.mkdtemp(prefix='rbe_src_')
    try:
        dec = decode(path, info, tmp, want_video=True, want48=True, timeout=timeout)
        if dec['timed_out'] or 'a16' not in dec:
            raise RuntimeError('source decode failed: ' + ' | '.join(dec['error_lines'][:5]))
        base = click_baseline(dec['a48'])
        fs = fnum(info.get('format', {}).get('start_time')) or 0.0
        asr = int((first_stream(info, 'audio') or {}).get('sample_rate') or 48000)
        bps = audio_clock_breakpoints(path, fs, asr)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(d, exist_ok=True)
    for k in ('thumbs', 'pts', 'a16'):
        np.save(files[k], dec[k])
    meta = {'click_baseline': base, 'click_version': CLICK_VERSION, 'path': os.path.realpath(path),
            'audio_clock_breakpoints': bps}
    json.dump(meta, open(meta_f, 'w'))
    src = {k: dec[k] for k in ('thumbs', 'pts', 'a16')}
    src.update(meta)
    src['cache'] = d
    return src


def load_source_crop(path, cache_dir, aspect, n_frames, timeout):
    """Source thumbnails of the centre crop with the given aspect ('fill' framing of an output
    whose aspect differs from the source), cached next to the main source cache."""
    d = os.path.join(cache_dir, 'src_' + file_key(path))
    os.makedirs(d, exist_ok=True)
    f = os.path.join(d, f'thumbs_crop_{aspect:.4f}.npy')
    if os.path.exists(f):
        return np.load(f)
    info = ffprobe(path)
    vs_ = first_stream(info, 'video')
    sa = (vs_.get('width') or 1) / max(1, vs_.get('height') or 1)
    crop = (f'crop=trunc(ih*{aspect:.6f}/2)*2:ih' if aspect < sa else f'crop=iw:trunc(iw/{aspect:.6f}/2)*2')
    log(f'source: decoding centre crop for aspect {aspect:.3f} (cached afterwards) ...')
    r = subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-i', path, '-map', '0:v:0', '-vf',
                        f'{crop},scale={TW}:{TH}:flags=area,format=gray', '-fps_mode', 'passthrough',
                        '-f', 'rawvideo', '-'], capture_output=True, timeout=timeout)
    raw = np.frombuffer(r.stdout, dtype=np.uint8)
    n = raw.size // (TW * TH)
    th = raw[:n * TW * TH].reshape(n, TH, TW)
    if n < n_frames:
        th = np.concatenate([th, np.repeat(th[-1:], n_frames - n, 0)]) if n else np.zeros((n_frames, TH, TW), np.uint8)
    th = th[:n_frames]
    np.save(f, th)
    return th


# ----------------------------------------------------------------------------- envelopes

def env_db(x):
    n = len(x) // HOP
    e = (x[:n * HOP].astype(np.float64).reshape(n, HOP) ** 2).mean(1)
    return 10 * np.log10(e + 1e-12)


def speech_level(db):
    return float(np.percentile(db, 95)) if len(db) else -20.0


def moving_mean(a, r):
    """Centered moving mean over 2r+1 samples along the last axis (edge-truncated)."""
    if r <= 0:
        return a
    c = np.cumsum(np.pad(a, [(0, 0)] * (a.ndim - 1) + [(1, 0)]), axis=-1)
    n = a.shape[-1]
    i = np.arange(n)
    lo = np.clip(i - r, 0, n)
    hi = np.clip(i + r + 1, 0, n)
    return (c[..., hi] - c[..., lo]) / (hi - lo)


# ----------------------------------------------------------------------------- audio mapping

class AudioMapper:
    """Recover the output->source mapping from audio.

    1. log-energy envelopes (10 ms), floored 45 dB under each file's speech level, gain
       offset removed;
    2. candidate lags: EDL hint + global search (3 s windows of the output matched against
       the whole source by FFT normalised cross-correlation on a 20 ms envelope), plus a
       second pass with 1 s windows over output speech the first pass did not explain;
    3. Viterbi over candidate lags (+ an 'unmatched' state) with a switch penalty gives a
       10 ms segmentation; silence costs nothing so it inherits the neighbouring lag;
    4. per segment, sample-level cross-correlation (16 kHz) refines the lag (overall, at
       the start and at the end); boundaries are refined at sample level by minimising
       the prediction residual of 'segment A' vs 'segment B' around the cut.
    """

    SWITCH_PENALTY = 40.0   # dB*frames, between two source lags
    UNMATCHED_SWITCH = 20.0  # dB*frames, into / out of the 'unmatched' state (2x >= lag switch)
    UNMATCHED_COST = 6.0    # dB, mean abs log-energy difference above which audio is unexplained
    UNMATCHED_SILENT = 0.5  # cost of 'unmatched' for output frames without speech
    LOCAL_R = 7             # +-70 ms smoothing of the local cost
    CLIP = 20.0

    def __init__(self, out16, src16, verbose=True):
        self.out16 = out16
        self.src16 = src16
        self.o_db = env_db(out16)
        self.s_db = env_db(src16)
        self.o_lvl = speech_level(self.o_db)
        self.s_lvl = speech_level(self.s_db)
        self.gain = self.o_lvl - self.s_lvl
        self.floor = self.s_lvl - 45.0
        self.cfloor = self.s_lvl - 35.0       # local cost ignores near-floor noise (codecs gate it)
        self.s_f = np.maximum(self.s_db, self.floor)
        self.s_c = np.maximum(self.s_db, self.cfloor)
        self._set_out()
        loud = self.o_db > self.o_lvl - 30.0
        # sustained sound only: isolated clicks / bumps (< ~100 ms in a 300 ms window) are not speech
        self.o_speech = loud & (moving_mean(loud.astype(float), 15) >= 0.3)

    def _set_out(self):
        self.o_f = np.maximum(self.o_db - self.gain, self.floor)
        self.o_c = np.maximum(self.o_db - self.gain, self.cfloor)

    # -- global search
    def coarse(self, win_s=3.0, hop_s=0.5, min_ncc=0.6, regions=None):
        """Return {lag_frames(10ms): best ncc}. regions: list of (f0,f1) output frames to scan."""
        o20 = self.o_f[:len(self.o_f) // 2 * 2].reshape(-1, 2).mean(1)
        s20 = self.s_f[:len(self.s_f) // 2 * 2].reshape(-1, 2).mean(1)
        W = int(round(win_s * 50))
        H = max(1, int(round(hop_s * 50)))
        sp20 = self.o_speech[:len(o20) * 2].reshape(-1, 2).any(1)
        starts = []
        if regions is None:
            regions = [(0, len(self.o_f))]
        for f0, f1 in regions:
            a, b = max(0, f0 // 2), min(len(o20), f1 // 2 + 1)
            if b - a < W:
                a = max(0, min(a, b - W))
                b = min(len(o20), a + W)
            for w0 in range(a, max(a, b - W) + 1, H):
                seg = o20[w0:w0 + W]
                if len(seg) < W or seg.std() < 4.0 or sp20[w0:w0 + W].mean() < 0.3:
                    continue
                starts.append(w0)
        starts = sorted(set(starts))
        Ns = len(s20)
        if not starts or Ns < W:
            return {}
        n = 1 << int(math.ceil(math.log2(Ns + W)))
        S = np.fft.rfft(s20, n)
        c1 = np.concatenate([[0], np.cumsum(s20)])
        c2 = np.concatenate([[0], np.cumsum(s20 * s20)])
        L = Ns - W + 1
        ssum = c1[W:W + L] - c1[:L]
        ssq = c2[W:W + L] - c2[:L]
        svar = np.maximum(ssq - ssum * ssum / W, 1e-6)
        found = {}
        B = 32
        for i in range(0, len(starts), B):
            batch = starts[i:i + B]
            M = np.stack([o20[w0:w0 + W] - o20[w0:w0 + W].mean() for w0 in batch])
            norms = np.sqrt((M * M).sum(1)) + 1e-9
            X = np.fft.rfft(M, n, axis=1)
            corr = np.fft.irfft(S[None, :] * np.conj(X), n, axis=1)[:, :L]
            ncc = corr / (norms[:, None] * np.sqrt(svar)[None, :])
            for k, w0 in enumerate(batch):
                c = ncc[k]
                j1 = int(np.argmax(c))
                peaks = [(j1, c[j1])]
                c2_ = c.copy()
                c2_[max(0, j1 - 25):j1 + 26] = -1
                j2 = int(np.argmax(c2_))
                if c2_[j2] >= min_ncc and c2_[j2] >= c[j1] - 0.1:
                    peaks.append((j2, c2_[j2]))
                for j, v in peaks:
                    if v < min_ncc:
                        continue
                    lag10 = self._refine10(w0 * 2, W * 2, (j - w0) * 2)
                    if lag10 is not None:
                        found[lag10] = max(found.get(lag10, 0), float(v))
        return found

    def _refine10(self, o0, n, lag, r=3):
        best, bl = None, None
        for l in range(lag - r, lag + r + 1):
            s0 = o0 + l
            if s0 < 0 or s0 + n > len(self.s_f):
                continue
            d = np.abs(self.o_f[o0:o0 + n] - self.s_f[s0:s0 + n]).mean()
            if best is None or d < best:
                best, bl = d, l
        return bl

    def local_cost(self, lags):
        T = len(self.o_f)
        Ls = len(self.s_f)
        cost = np.empty((len(lags) + 1, T), dtype=np.float32)
        t = np.arange(T)
        for i, l in enumerate(lags):
            j = t + l
            ok = (j >= 0) & (j < Ls)
            d = np.full(T, self.CLIP, dtype=np.float64)
            d[ok] = np.minimum(np.abs(self.o_c[ok] - self.s_c[j[ok]]), self.CLIP)
            cost[i] = moving_mean(d, self.LOCAL_R)
        quiet = moving_mean(self.o_speech.astype(float), self.LOCAL_R) < 0.05
        cost[-1] = np.where(quiet, self.UNMATCHED_SILENT, self.UNMATCHED_COST)
        return cost

    def estimate_gain(self, lags_by_frame):
        t = np.nonzero((lags_by_frame != np.iinfo(np.int64).min) & self.o_speech)[0]
        if len(t) < 50:
            return
        j = t + lags_by_frame[t]
        ok = (j >= 0) & (j < len(self.s_db))
        t, j = t[ok], j[ok]
        sp = self.s_db[j] > self.s_lvl - 25
        if sp.sum() < 50:
            return
        self.gain = float(np.median(self.o_db[t[sp]] - self.s_db[j[sp]]))
        self._set_out()

    def viterbi(self, lags):
        cost = self.local_cost(lags)
        C, T = cost.shape
        U = C - 1
        P, PU = self.SWITCH_PENALTY, self.UNMATCHED_SWITCH
        acc = cost[:, 0].astype(np.float64).copy()
        choice = np.zeros((T, C), dtype=np.int8)        # 0 stay, 1 from best lag, 2 from unmatched
        bestprev = np.zeros(T, dtype=np.int32)
        for t in range(1, T):
            bi = int(np.argmin(acc[:U])) if U else 0
            from_lag = acc[bi] + P
            from_u = acc[U] + PU
            new = acc.copy()
            ch = np.zeros(C, dtype=np.int8)
            m1 = from_lag < new
            new[m1] = from_lag
            ch[m1] = 1
            m2 = from_u < new
            new[m2] = from_u
            ch[m2] = 2
            # the unmatched state itself: stay, or enter from the best lag
            if U and acc[bi] + PU < acc[U]:
                new[U] = acc[bi] + PU
                ch[U] = 1
            else:
                new[U] = acc[U]
                ch[U] = 0
            choice[t] = ch
            bestprev[t] = bi
            acc = new + cost[:, t]
        state = np.empty(T, dtype=np.int32)
        s = int(np.argmin(acc))
        for t in range(T - 1, -1, -1):
            state[t] = s
            if t > 0:
                c = choice[t, s]
                if c == 1:
                    s = int(bestprev[t])
                elif c == 2:
                    s = U
        return state, cost

    def prune(self, lags, keep_always=()):
        """Keep lags that are the (near-)best explanation for >= 0.3 s of output speech."""
        if not lags:
            return []
        cost = self.local_cost(lags)[:-1]
        sp = self.o_speech
        if sp.sum() == 0:
            return list(lags)
        cs = cost[:, sp]
        best = cs.min(0)
        good = (cs <= best[None, :] + 1.0) & (best[None, :] < self.UNMATCHED_COST)
        cnt = good.sum(1)
        keep = [l for l, c in zip(lags, cnt) if c >= 30 or l in keep_always]
        return keep

    def run(self, hint_lags=()):
        cand = self.coarse()
        hint = sorted(set(int(h) for h in hint_lags))
        lags = sorted(set(cand) | set(hint))
        lags = self.prune(lags, keep_always=set())
        if not lags:
            lags = hint[:1] or [0]
        state, cost = self.viterbi(lags)
        lag_f = self._lag_frames(state, lags)
        self.estimate_gain(lag_f)
        # second pass over output speech that is unexplained or poorly explained
        T = len(self.o_f)
        best_cost = cost[state, np.arange(T)]
        bad = self.o_speech & ((state == len(lags)) | (best_cost > 3.0))
        regions = runs_of(bad, min_len=30)
        if regions:
            regions = [(max(0, a - 50), min(T, b + 50)) for a, b in regions]
            extra = self.coarse(win_s=1.0, hop_s=0.25, min_ncc=0.75, regions=regions)
            lags2 = sorted(set(lags) | set(extra))
            lags2 = self.prune(lags2)
            if lags2:
                lags = lags2
        state, cost = self.viterbi(lags)
        state = self._bridge_quiet_gaps(state, len(lags))
        self.lags = lags
        self.state = state
        self.cost = cost
        return self._lag_frames(state, lags)

    def _bridge_quiet_gaps(self, state, U, max_len=200):
        """An 'unmatched' run of <= 2 s without speech between two runs of the same lag is a
        pause inside one continuous take (the codec gated the room tone): give it that lag."""
        runs = value_runs(state)
        st = state.copy()
        for i in range(1, len(runs) - 1):
            a, b, v = runs[i]
            if v != U or b - a > max_len or self.o_speech[a:b].any():
                continue
            if runs[i - 1][2] == runs[i + 1][2] != U:
                st[a:b] = runs[i - 1][2]
        return st

    def _lag_frames(self, state, lags):
        NONE = np.iinfo(np.int64).min
        la = np.array(list(lags) + [NONE], dtype=np.int64)
        return la[state]


NONE_LAG = np.iinfo(np.int64).min


def runs_of(mask, min_len=1):
    m = np.concatenate([[False], np.asarray(mask, bool), [False]])
    d = np.diff(m.astype(np.int8))
    a = np.nonzero(d == 1)[0]
    b = np.nonzero(d == -1)[0]
    return [(int(x), int(y)) for x, y in zip(a, b) if y - x >= min_len]


def value_runs(arr):
    """Runs of equal values: list of (start, end, value)."""
    out = []
    n = len(arr)
    if n == 0:
        return out
    ch = np.nonzero(arr[1:] != arr[:-1])[0] + 1
    st = np.concatenate([[0], ch])
    en = np.concatenate([ch, [n]])
    for a, b in zip(st, en):
        out.append((int(a), int(b), arr[a]))
    return out


# ----------------------------------------------------------------------------- sample-level refinement

def xcorr_lag(out16, src16, o0, o1, lag_s, search_s=0.02):
    """Refine lag (src = out + lag, seconds) with waveform cross-correlation on out[o0:o1]."""
    a = int(round(o0 * SR))
    b = int(round(o1 * SR))
    if b - a < SR // 10:
        return None, 0.0
    L0 = int(round(lag_s * SR))
    M = int(round(search_s * SR))
    s0 = a + L0 - M
    s1 = b + L0 + M
    if s0 < 0 or s1 > len(src16) or b > len(out16):
        lo = max(0, -s0)
        hi = max(0, s1 - len(src16))
        a2, b2 = a + lo, b - hi - max(0, b - len(out16))
        if b2 - a2 < SR // 10:
            return None, 0.0
        a, b = a2, b2
        s0, s1 = a + L0 - M, b + L0 + M
    x = out16[a:b].astype(np.float64)
    y = src16[s0:s1].astype(np.float64)
    n = 1 << int(math.ceil(math.log2(len(y) + len(x))))
    c = np.fft.irfft(np.fft.rfft(y, n) * np.conj(np.fft.rfft(x, n)), n)[:2 * M + 1]
    ex = float((x * x).sum())
    cy = np.concatenate([[0], np.cumsum(y * y)])
    ey = cy[len(x):len(x) + 2 * M + 1] - cy[:2 * M + 1]
    ncc = c / np.sqrt(np.maximum(ex * ey, 1e-20))
    k = int(np.argmax(ncc))
    peak = float(ncc[k])
    frac_ = 0.0
    if 0 < k < len(ncc) - 1:
        y0, y1, y2 = ncc[k - 1], ncc[k], ncc[k + 1]
        den = y0 - 2 * y1 + y2
        if den < 0:
            frac_ = 0.5 * (y0 - y2) / den
    lag = (L0 - M + k + frac_) / SR
    return lag, peak


def speech_window(mask, f0, f1, need=40, span=200, from_end=False):
    """Pick a window (frames) of up to `span` frames inside [f0,f1) holding >= need speech frames,
    as close to the start (or end) as possible."""
    if f1 - f0 <= span:
        return (f0, f1) if mask[f0:f1].sum() >= min(need, max(5, (f1 - f0) // 4)) else None
    rng = range(f1 - span, f0 - 1, -25) if from_end else range(f0, f1 - span + 1, 25)
    for a in rng:
        if mask[a:a + span].sum() >= need:
            return (a, a + span)
    return None


# ----------------------------------------------------------------------------- video features

def block_feats(thumbs, crop_bottom=0.0):
    """Per-block zero-mean unit-norm features: (N, B, d) float32."""
    t = np.asarray(thumbs, dtype=np.float32)
    if crop_bottom > 0:
        t = t[:, :int(round(TH * (1 - crop_bottom))), :]
    n, h, w = t.shape
    gr, gc = GRID
    bh, bw = h // gr, w // gc
    t = t[:, :bh * gr, :bw * gc].reshape(n, gr, bh, gc, bw).transpose(0, 1, 3, 2, 4).reshape(n, gr * gc, bh * bw)
    t = t - t.mean(2, keepdims=True)
    nrm = np.sqrt((t * t).sum(2, keepdims=True))
    return t / np.maximum(nrm, 1.0)


def nn_feats(thumbs):
    t = np.asarray(thumbs, dtype=np.float32)
    n = t.shape[0]
    t = t.reshape(n, NN_H, TH // NN_H, NN_W, TW // NN_W).mean((2, 4)).reshape(n, -1)
    t = t - t.mean(1, keepdims=True)
    nrm = np.sqrt((t * t).sum(1, keepdims=True))
    return t / np.maximum(nrm, 1.0)


def video_curve(o_feat, o_pts, s_feat_fn, s_pts, center, search=1.5, step=0.002):
    """D(sigma): mean distance between output frames (pts o) and source frames at o+sigma,
    linearly interpolated between neighbouring source frames, per-block, with blocks that
    never match (overlays, captions) excluded. Returns (sigmas, curve, n_blocks_used)."""
    K = len(o_pts)
    sig = center + np.arange(-search, search + step / 2, step)
    lo = o_pts[0] + sig[0] - 0.1
    hi = o_pts[-1] + sig[-1] + 0.1
    j0 = max(0, int(np.searchsorted(s_pts, lo)) - 1)
    j1 = min(len(s_pts), int(np.searchsorted(s_pts, hi)) + 2)
    if j1 - j0 < 3:
        return None
    F = s_feat_fn(j0, j1)                       # (J,B,d)
    p = s_pts[j0:j1]
    G = np.einsum('kbd,jbd->kjb', o_feat, F)    # (K,J,B)
    FF = np.einsum('jbd,jbd->jb', F, F)
    FN = np.einsum('jbd,jbd->jb', F[:-1], F[1:])
    OO = np.einsum('kbd,kbd->kb', o_feat, o_feat)
    tau = o_pts[:, None] + sig[None, :]
    J = len(p)
    j = np.clip(np.searchsorted(p, tau, side='right') - 1, 0, J - 2)
    dt = p[j + 1] - p[j]
    a = np.clip((tau - p[j]) / np.where(dt > 0, dt, 1), 0, 1)[..., None]
    kk = np.arange(K)[:, None]
    dot = (1 - a) * G[kk, j] + a * G[kk, j + 1]
    ss = (1 - a) ** 2 * FF[j] + 2 * a * (1 - a) * FN[j] + a ** 2 * FF[j + 1]
    D = OO[:, None, :] + ss - 2 * dot           # (K,S,B)
    outside = (tau < p[0]) | (tau > p[-1])
    D[outside] = 4.0
    Db = D.mean(0)                               # (S,B)
    bmin = Db.min(0)
    keep = bmin <= max(2 * np.median(bmin), 0.05)
    if keep.sum() < max(3, Db.shape[1] // 3):
        keep = np.argsort(bmin)[:max(3, Db.shape[1] // 3)]
    curve = Db[:, keep].mean(1)
    return sig, curve, int(np.size(np.nonzero(keep)[0]) if keep.dtype == bool else len(keep))


def curve_min(sig, curve, excl=0.1):
    """Minimum of a distance curve with sub-step refinement and confidence measures:
    contrast  = (median - best) / median
    uniqueness= (best other local minimum > excl away - best) / (median - best)  (1 if none)
    width     = width (s) of the valley where the curve stays under best + 25% of (median - best)"""
    k = int(np.argmin(curve))
    best = float(curve[k])
    x = float(sig[k])
    if 0 < k < len(curve) - 1:
        y0, y1, y2 = curve[k - 1], curve[k], curve[k + 1]
        den = y0 - 2 * y1 + y2
        if den > 0:
            x += 0.5 * (y0 - y2) / den * (sig[1] - sig[0])
    med = float(np.median(curve))
    sm = moving_mean(curve, 2)
    lm = np.nonzero((sm[1:-1] <= sm[:-2]) & (sm[1:-1] <= sm[2:]))[0] + 1
    lm = lm[np.abs(sig[lm] - sig[k]) > excl]
    second = float(curve[lm].min()) if len(lm) else med
    contrast = (med - best) / med if med > 0 else 0.0
    uniq = min(1.0, (second - best) / (med - best)) if med > best else 0.0
    lvl = best + 0.25 * (med - best)
    i0 = k
    while i0 > 0 and curve[i0 - 1] <= lvl:
        i0 -= 1
    i1 = k
    while i1 < len(curve) - 1 and curve[i1 + 1] <= lvl:
        i1 += 1
    width = float(sig[i1] - sig[i0])
    return x, best, contrast, uniq, width


# ----------------------------------------------------------------------------- framing views

ZOOMS = (1.0, 1.06, 1.12, 1.2)   # centred punch-in factors tried for the output picture


def zoom_thumbs(th, z):
    """Centre zoom of thumbnails by factor z (crop 1/z around the centre, scale back), bilinear."""
    th = np.asarray(th)
    if abs(z - 1.0) < 1e-6:
        return th
    H, W = th.shape[1:]

    def axis(n):
        c = np.clip((np.arange(n) + 0.5 - n / 2) / z + n / 2 - 0.5, 0, n - 1)
        i0 = np.floor(c).astype(np.int64)
        i1 = np.minimum(i0 + 1, n - 1)
        return i0, i1, (c - i0).astype(np.float32)
    y0, y1, wy = axis(H)
    x0, x1, wx = axis(W)
    out = np.empty(th.shape, dtype=np.uint8)
    for c0 in range(0, len(th), 4096):
        t = th[c0:c0 + 4096].astype(np.float32)
        ty0, ty1 = t[:, y0], t[:, y1]
        top = ty0[:, :, x0] * (1 - wx) + ty0[:, :, x1] * wx
        bot = ty1[:, :, x0] * (1 - wx) + ty1[:, :, x1] * wx
        out[c0:c0 + 4096] = np.clip(top * (1 - wy)[:, None] + bot * wy[:, None] + 0.5, 0, 255).astype(np.uint8)
    return out


class Views:
    """Ways the output picture can relate to the source picture.

    A view pairs an output thumbnail set with a source thumbnail set:
      z1.00 .. z1.20 : same framing, output = centred punch-in of the source by that factor;
      fill           : output aspect differs, output = centre crop of the source scaled to cover;
      fit            : output aspect differs, the source sits letterboxed in the middle band.
    A census on audio-located frames decides which views the output uses."""

    def __init__(self, o_thumbs, o_pts, s_thumbs, s_pts, o_band=None, s_crop=None, crop_bottom=0.0):
        self.o = {'full': o_thumbs}
        if o_band is not None:
            self.o['band'] = o_band
        self.s_base = s_thumbs
        self.s_crop = s_crop
        self.o_pts = o_pts
        self.s_pts = s_pts
        self.crop_bottom = crop_bottom
        self.all = []
        if o_band is None and s_crop is None:
            self.all = [(f'z{z:.2f}', 'full', ('zoom', z)) for z in ZOOMS]
        else:
            if s_crop is not None:
                self.all += [(f'fill{"" if z == 1.0 else f"_z{z:.2f}"}', 'full', ('crop', z)) for z in ZOOMS[:3]]
            if o_band is not None:
                self.all += [('fit', 'band', ('zoom', 1.0))]
        self.used = list(self.all)
        self._snn = {}
        self._sone = {}
        self._onn = {}
        self.census = None

    def view(self, name):
        return next(v for v in self.all if v[0] == name)

    def s_range(self, skey, j0, j1):
        kind, z = skey
        base = self.s_crop if kind == 'crop' else self.s_base
        return zoom_thumbs(base[j0:j1], z)

    def s_nn_t(self, skey):
        """All source features of a view, transposed and contiguous (d, N) for fast products.
        Built only for views the search actually uses."""
        if skey not in self._snn:
            kind, z = skey
            base = self.s_crop if kind == 'crop' else self.s_base
            parts = [nn_feats(zoom_thumbs(base[c0:c0 + 8192], z)) for c0 in range(0, len(base), 8192)]
            m = np.concatenate(parts) if parts else np.zeros((0, NN_W * NN_H), np.float32)
            self._snn[skey] = np.ascontiguousarray(m.T)
        return self._snn[skey]

    def s_one(self, skey, j):
        if skey in self._snn:
            return self._snn[skey][:, j]
        key = (skey, j)
        f = self._sone.get(key)
        if f is None:
            kind, z = skey
            base = self.s_crop if kind == 'crop' else self.s_base
            f = nn_feats(zoom_thumbs(base[j:j + 1], z))[0]
            if len(self._sone) > 50000:
                self._sone.clear()
            self._sone[key] = f
        return f

    def o_nn(self, oname):
        if oname not in self._onn:
            self._onn[oname] = nn_feats(self.o[oname])
        return self._onn[oname]

    def src_index(self, src_t):
        sp = self.s_pts
        j = int(np.clip(np.searchsorted(sp, src_t), 0, len(sp) - 1))
        j2 = max(0, j - 1)
        jj = j if abs(sp[j] - src_t) < abs(sp[j2] - src_t) else j2
        return jj if abs(sp[jj] - src_t) <= 0.1 else None

    def dist(self, k, src_t, names=None):
        """Smallest whole-frame distance (0 identical .. 4 inverted) over views; (d, view)."""
        j = self.src_index(src_t)
        if j is None:
            return 9.0, None
        best = (9.0, None)
        for name, oname, skey in (self.used if names is None else [self.view(n) for n in names]):
            d = float(2 - 2 * (self.o_nn(oname)[k] @ self.s_one(skey, j)))
            if d < best[0]:
                best = (d, name)
        return best

    def nn(self, ks):
        """Global nearest source frame for output frames ks over the used views:
        (best_j, d1, d2 = best outside +-1 s of best_j, view name)."""
        ks = np.asarray(ks)
        n = len(ks)
        best_j = np.zeros(n, dtype=np.int64)
        d1 = np.full(n, 9.0)
        d2 = np.full(n, 9.0)
        vname = [None] * n
        sp = self.s_pts
        for name, oname, skey in self.used:
            Q = self.o_nn(oname)[ks]
            ST = self.s_nn_t(skey)
            for c0 in range(0, n, 64):
                sim = Q[c0:c0 + 64] @ ST
                j = np.argmax(sim, 1)
                for r, jj in enumerate(j):
                    dd = float(2 - 2 * sim[r, jj])
                    i = c0 + r
                    if dd < d1[i]:
                        lo = np.searchsorted(sp, sp[jj] - 1.0)
                        hi = np.searchsorted(sp, sp[jj] + 1.0)
                        row = sim[r].copy()
                        row[lo:hi] = -2
                        best_j[i], d1[i], d2[i], vname[i] = jj, dd, float(2 - 2 * row.max()), name
        return best_j, d1, d2, vname

    def run_census(self, am, lag_f, n=150):
        """Which views explain the picture at audio-located positions."""
        conf = np.nonzero((lag_f != NONE_LAG) & am.o_speech)[0]
        res = {'samples': 0, 'views': {}, 'used': [v[0] for v in self.all], 'status': 'no audio-located frames'}
        if not len(conf):
            self.census = res
            return res
        pick = conf[np.unique(np.linspace(0, len(conf) - 1, min(n, len(conf))).astype(int))]
        counts = {v[0]: 0 for v in self.all}
        good = 0
        dists = []
        for f in pick:
            t = f / ENV_RATE
            k = int(np.clip(np.searchsorted(self.o_pts, t), 0, len(self.o_pts) - 1))
            d, name = self.dist(k, self.o_pts[k] + lag_f[f] / ENV_RATE, names=[v[0] for v in self.all])
            dists.append(d)
            if name is not None and d < 0.3:
                counts[name] += 1
                good += 1
        used = [v for v in self.all if counts[v[0]] >= max(2, 0.03 * good)]
        res = {'samples': int(len(pick)), 'matched': good, 'views': counts,
               'median_best_distance': r3(np.median(dists)) if dists else None}
        if good >= max(5, 0.2 * len(pick)) and used:
            self.used = used
            res['status'] = 'ok'
        else:
            self.used = list(self.all)
            res['status'] = 'picture does not match the source in any known framing'
        res['used'] = [v[0] for v in self.used]
        self.census = res
        return res


def verify_by_picture(am, lag_f, views, step=0.25):
    """Audio runs can stretch over near-silent output (a silent screen demo before speech,
    faint game audio) where the lag is only inherited. Sample the picture in the low-speech
    parts of every audio run: where a unique source match elsewhere explains the picture much
    better than the run's lag for >= 1 s, take those frames out of the audio mapping so the
    video placement decides them."""
    lag_f = lag_f.copy()
    log_ = []
    dens = moving_mean(am.o_speech.astype(float), 25)
    o_pts = views.o_pts
    for a, b, v in value_runs(lag_f):
        if v == NONE_LAG or b - a < ENV_RATE:
            continue
        ts = np.arange(a / ENV_RATE + step / 2, b / ENV_RATE, step)
        fr = np.clip((ts * ENV_RATE).astype(int), 0, len(dens) - 1)
        low = dens[fr] < 0.2
        if low.sum() < 4:
            continue
        ks = np.clip(np.searchsorted(o_pts, ts), 0, len(o_pts) - 1)
        dl = np.zeros(len(ts))
        for i in np.nonzero(low)[0]:
            dl[i] = views.dist(ks[i], o_pts[ks[i]] + v / ENV_RATE)[0]
        # a global search is only needed where the run's own lag does not explain the picture
        lk = np.nonzero(low & (dl > 0.15))[0]
        strong = np.zeros(len(ts), dtype=bool)
        weak = np.zeros(len(ts), dtype=bool)
        agree = ~low | (dl <= 0.15)                    # speech, or the run's lag fits the picture
        if len(lk) < 4:
            continue
        bj, d1, d2, _ = views.nn(ks[lk])
        for q, i in enumerate(lk):
            nn_lag = views.s_pts[bj[q]] - o_pts[ks[i]]
            if d1[q] < 0.5 * dl[i]:
                weak[i] = True
                if d1[q] < 0.25 and d1[q] < 0.6 * d2[q] and abs(nn_lag - v / ENV_RATE) > 1.0:
                    strong[i] = True
            else:
                agree[i] = dl[i] <= max(0.15, 1.25 * d1[q] + 0.02)
        bad = np.zeros(len(ts), dtype=bool)
        for i0, i1 in runs_of(weak):
            if strong[i0:i1].sum() >= 2 and i1 - i0 >= 4:
                bad[i0:i1] = True
        # close gaps between bad runs that hold no positive agreement
        br = runs_of(bad)
        for (a0, a1), (b0, b1) in zip(br, br[1:]):
            if not agree[a1:b0].any():
                bad[a1:b0] = True
        for i0, i1 in runs_of(bad):
            f0 = max(a, int(round((ts[i0] - step / 2) * ENV_RATE)))
            f1 = min(b, int(round((ts[i1 - 1] + step / 2) * ENV_RATE)))
            lag_f[f0:f1] = NONE_LAG
            log_.append({'out_start': r3(f0 / ENV_RATE), 'out_end': r3(f1 / ENV_RATE),
                         'audio_lag_s': r3(v / ENV_RATE)})
    return lag_f, log_


# ----------------------------------------------------------------------------- reference & gold

def load_reference(path, src_duration):
    ref = json.load(open(path))
    words = []
    for w in ref.get('words', []):
        if w.get('type') != 'word':
            continue
        s, e = fnum(w.get('start')), fnum(w.get('end'))
        if s is None or e is None or s >= src_duration:
            continue
        words.append({'text': w.get('text', ''), 'start': s, 'end': min(e, src_duration)})
    return words


def norm_token(t):
    t = t.lower().strip()
    t = re.sub(r'^[^\w]+|[^\w-]+$', '', t)
    return t


class Intervals:
    """Union of source intervals with fast coverage queries."""

    def __init__(self, spans):
        sp = sorted((a, b) for a, b in spans if b > a)
        merged = []
        for a, b in sp:
            if merged and a <= merged[-1][1]:
                merged[-1][1] = max(merged[-1][1], b)
            else:
                merged.append([a, b])
        self.a = np.array([m[0] for m in merged])
        self.b = np.array([m[1] for m in merged])

    def covered(self, s, e):
        if e <= s or not len(self.a):
            return 0.0
        i0 = max(0, int(np.searchsorted(self.b, s)))
        tot = 0.0
        for i in range(i0, len(self.a)):
            if self.a[i] >= e:
                break
            tot += max(0.0, min(e, self.b[i]) - max(s, self.a[i]))
        return tot

    def total(self):
        return float((self.b - self.a).sum()) if len(self.a) else 0.0


def word_kept(words, kept):
    return [kept.covered(w['start'], w['end']) >= 0.5 * max(w['end'] - w['start'], 1e-3)
            if w['end'] > w['start'] else kept.covered(w['start'] - 0.005, w['start'] + 0.005) > 0
            for w in words]


def words_in(words, s, e):
    out = []
    for i, w in enumerate(words):
        d = w['end'] - w['start']
        if d <= 0:
            if s <= w['start'] <= e:
                out.append(i)
        elif min(w['end'], e) - max(w['start'], s) >= 0.5 * d:
            out.append(i)
    return out


def presence(span, words, kept_flags, kept, spoken=True):
    """Word-duration-weighted presence for spoken spans, time coverage otherwise."""
    s, e = span['start'], span['end']
    idx = words_in(words, s, e) if spoken else []
    if idx:
        dur = np.array([max(words[i]['end'] - words[i]['start'], 0.02) for i in idx])
        k = np.array([kept_flags[i] for i in idx], dtype=float)
        return float((dur * k).sum() / dur.sum()), 'words'
    return (kept.covered(s, e) / (e - s) if e > s else 0.0), 'time'


def score_gold(gold, segments, words, kept_flags, kept):
    res = {'units': [], 'drop': {}, 'keep': {}, 'visual_demo': {}, 'one_of': {}}
    drop_by_cat = {}
    keep_by_imp = {}
    demo = []
    groups = []
    for u in gold.get('units', []):
        kind, cat = u.get('kind'), u.get('category')
        rec = {'id': u.get('id'), 'kind': kind, 'category': cat}
        if kind == 'drop':
            d = u['end'] - u['start']
            present = kept.covered(u['start'], u['end']) / d if d > 0 else 0.0
            rec.update(start=u['start'], end=u['end'], present=round(present, 3), removed=present <= 0.2)
            drop_by_cat.setdefault(cat, []).append((rec['removed'], d))
        elif kind == 'keep' and cat == 'visual_demo':
            d = u['end'] - u['start']
            present = kept.covered(u['start'], u['end']) / d if d > 0 else 0.0
            rec.update(start=u['start'], end=u['end'], importance=u.get('importance'),
                       present=round(present, 3), kept=present >= 0.5)
            demo.append((rec['kept'], present, d))
        elif kind == 'keep':
            p, basis = presence(u, words, kept_flags, kept)
            rec.update(start=u['start'], end=u['end'], importance=u.get('importance', 'normal'),
                       present=round(p, 3), basis=basis, kept=p >= 0.8)
            keep_by_imp.setdefault(u.get('importance', 'normal'), []).append(rec['kept'])
        elif kind == 'one_of':
            ps = [presence(a, words, kept_flags, kept)[0] for a in u.get('alternatives', [])]
            n = sum(p >= 0.5 for p in ps)
            outcome = 'exactly_one' if n == 1 else ('duplicate' if n > 1 else 'none')
            rec.update(alternatives=[round(p, 3) for p in ps], kept_alternatives=n, outcome=outcome,
                       importance=u.get('importance'))
            groups.append(outcome)
        res['units'].append(rec)
    all_drop = [x for v in drop_by_cat.values() for x in v]
    for cat, v in sorted(drop_by_cat.items()):
        res['drop'][cat] = {'n': len(v), 'removed': sum(r for r, _ in v),
                            'recall': round(sum(r for r, _ in v) / len(v), 3)}
    if all_drop:
        tot = sum(d for _, d in all_drop)
        res['drop']['overall'] = {'n': len(all_drop), 'removed': sum(r for r, _ in all_drop),
                                  'recall': round(sum(r for r, _ in all_drop) / len(all_drop), 3),
                                  'recall_by_duration': round(sum(d for r, d in all_drop if r) / tot, 3) if tot else None}
    for imp, v in sorted(keep_by_imp.items()):
        res['keep'][imp] = {'n': len(v), 'kept': int(sum(v)), 'recall': round(sum(v) / len(v), 3)}
    if demo:
        tot = sum(d for _, _, d in demo)
        res['visual_demo'] = {'n': len(demo), 'kept': sum(k for k, _, _ in demo),
                              'time_fraction_present': round(sum(p * d for _, p, d in demo) / tot, 3) if tot else None}
    if groups:
        res['one_of'] = {'n': len(groups), 'exactly_one': groups.count('exactly_one'),
                         'duplicate': groups.count('duplicate'), 'none': groups.count('none'),
                         'exactly_one_rate': round(groups.count('exactly_one') / len(groups), 3)}
    return res


# ----------------------------------------------------------------------------- clicks

def click_stat(x, centers, sr=CLICK_SR, win_ms=2.0, search_ms=10.0, ctx_ms=100.0, floor=None):
    """Max first-difference energy in 2 ms windows within +-search of each center, over the
    median of the same quantity in the surrounding +-100 ms (search zone excluded)."""
    d = np.diff(x.astype(np.float64), prepend=x[0] if len(x) else 0)
    e = d * d
    w = int(sr * win_ms / 1000)
    cs = np.concatenate([[0], np.cumsum(e)])
    out = []
    for c, srch in centers:
        c = int(round(c * sr))
        s = int(sr * max(search_ms, srch) / 1000)
        ctx = int(sr * ctx_ms / 1000)
        a, b = c - s, c + s
        if a - ctx < 0 or b + ctx + w >= len(e):
            out.append(None)
            continue
        idx = np.arange(a, b, max(1, w // 2))
        E = (cs[idx + w] - cs[idx]) / w
        idx2 = np.concatenate([np.arange(a - ctx, a - w, w), np.arange(b, b + ctx, w)])
        E2 = (cs[idx2 + w] - cs[idx2]) / w
        base = float(np.median(E2))
        if floor is not None:
            base = max(base, floor)
        out.append(float(E.max() / max(base, 1e-12)))
    return out


def click_floor(x):
    """5% of the typical first-difference energy of the louder half of the file: in silence
    the ratio is taken against this floor, so a join is only flagged for a transient that is
    loud relative to the programme, not relative to digital silence."""
    d = np.diff(x.astype(np.float64))
    n = len(d) // 96
    if n == 0:
        return 1e-12
    E = (d[:n * 96].reshape(n, 96) ** 2).mean(1)
    loud = E[E >= np.percentile(E, 50)]
    return float(0.05 * np.median(loud)) if len(loud) else 1e-12


def click_baseline(a48, n=4000, seed=0):
    """Distribution of the click statistic at random points of the (uncut) source."""
    if a48 is None or len(a48) < CLICK_SR:
        return None
    rng = np.random.default_rng(seed)
    centers = rng.uniform(0.2, len(a48) / CLICK_SR - 0.2, n)
    fl = click_floor(a48)
    st = [v for v in click_stat(a48, [(c, 10.0) for c in centers], floor=fl) if v is not None]
    st = np.array(st)
    return {'n': int(len(st)), 'p50': float(np.percentile(st, 50)), 'p95': float(np.percentile(st, 95)),
            'p99': float(np.percentile(st, 99)), 'p99_5': float(np.percentile(st, 99.5)),
            'max': float(st.max())}


# ----------------------------------------------------------------------------- loudness & silence

def loudness(path, timeout=600):
    r = subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-nostats', '-i', path, '-map', '0:a:0',
                        '-filter:a', 'ebur128=peak=true', '-f', 'null', '-'],
                       capture_output=True, text=True, timeout=timeout)
    txt = r.stderr
    i = txt.rfind('Summary:')
    s = txt[i:] if i >= 0 else ''

    def grab(pat):
        m = re.search(pat, s)
        return float(m.group(1)) if m else None
    return {'integrated_lufs': grab(r'I:\s+(-?[\d.]+|-inf) LUFS'), 'lra_lu': grab(r'LRA:\s+(-?[\d.]+) LU'),
            'true_peak_dbtp': grab(r'True peak:\s+Peak:\s+(-?[\d.]+|-inf) dBFS')}


def silences(path, noise_db, min_d=1.0, timeout=600, duration=None):
    r = subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-nostats', '-i', path, '-map', '0:a:0',
                        '-af', f'silencedetect=n={noise_db:.1f}dB:d={min_d}', '-f', 'null', '-'],
                       capture_output=True, text=True, timeout=timeout)
    out = []
    st = None
    for line in r.stderr.splitlines():
        m = re.search(r'silence_start:\s*(-?[\d.]+)', line)
        if m:
            st = float(m.group(1))
        m = re.search(r'silence_end:\s*(-?[\d.]+)', line)
        if m and st is not None:
            out.append((max(0.0, st), float(m.group(1))))
            st = None
    if st is not None and duration:
        out.append((st, duration))
    return out


# ----------------------------------------------------------------------------- helpers

def pct(a, q):
    return float(np.percentile(a, q)) if len(a) else None


def r3(x):
    return None if x is None else round(float(x), 3)


def r1(x):
    return None if x is None else round(float(x), 1)


def theil_sen(x, y):
    x, y = np.asarray(x, float), np.asarray(y, float)
    if len(x) < 2:
        return None
    i, j = np.triu_indices(len(x), 1)
    dx = x[j] - x[i]
    ok = np.abs(dx) > 1e-9
    if not ok.any():
        return None
    return float(np.median((y[j] - y[i])[ok] / dx[ok]))


def ols(x, y):
    x, y = np.asarray(x, float), np.asarray(y, float)
    if len(x) < 2 or np.ptp(x) == 0:
        return None, None, None
    A = np.vstack([x, np.ones_like(x)]).T
    (m, c), *_ = np.linalg.lstsq(A, y, rcond=None)
    pred = A @ np.array([m, c])
    ss = ((y - y.mean()) ** 2).sum()
    r2 = 1 - ((y - pred) ** 2).sum() / ss if ss > 0 else None
    return float(m), float(c), (float(r2) if r2 is not None else None)


def load_edl(path, source_path, out_rate_hint=None):
    edl = json.load(open(path))
    if not isinstance(edl, dict):
        # Another harness's own cut-list format: the EDL is only a hint, so go without it.
        return {'ranges': [], 'declared_duration': 0.0, 'note': 'EDL not in video-use format; ignored'}
    srcs = edl.get('sources') or {}
    real_src = os.path.realpath(source_path)
    ranges = []
    t = 0.0
    for i, r in enumerate(edl.get('ranges') or []):
        s, e = fnum(r.get('start')), fnum(r.get('end'))
        if s is None or e is None or e <= s:
            continue
        name = r.get('source')
        p = srcs.get(name) if isinstance(srcs, dict) else None
        resolves = bool(p) and os.path.exists(p)
        same = resolves and os.path.realpath(p) == real_src
        ranges.append({'index': i, 'source': name, 'source_resolves': resolves, 'is_eval_source': same,
                       'src_start': s, 'src_end': e, 'out_start': t, 'out_end': t + (e - s)})
        t += e - s
    return {'ranges': ranges, 'declared_duration': t}


# ----------------------------------------------------------------------------- main evaluation

def evaluate(args):
    T0 = time.time()
    timing = {}
    res = {'tool': {'name': 'rushes-bench evaluate', 'version': VERSION},
           'inputs': {k: (os.path.abspath(v) if isinstance(v, str) and os.path.exists(v) else v)
                      for k, v in vars(args).items() if k in ('source', 'reference', 'gold', 'output', 'edl', 'run_meta')}}

    # ---------------- 1. validity
    t = time.time()
    oinfo = ffprobe(args.output)
    sinfo = ffprobe(args.source)
    fmt = oinfo.get('format', {})
    vs, as_ = first_stream(oinfo, 'video'), first_stream(oinfo, 'audio')
    vdur = stream_duration(vs, fmt) if vs else None
    adur = stream_duration(as_, fmt) if as_ else None
    out_dur = fnum(fmt.get('duration')) or max(vdur or 0, adur or 0)
    validity = {
        'container': fmt.get('format_name'), 'size_bytes': int(fmt.get('size', 0) or 0),
        'duration_s': r3(out_dur),
        'video': None if not vs else {
            'codec': vs.get('codec_name'), 'profile': vs.get('profile'), 'pix_fmt': vs.get('pix_fmt'),
            'width': vs.get('width'), 'height': vs.get('height'),
            'r_frame_rate': vs.get('r_frame_rate'), 'avg_frame_rate': vs.get('avg_frame_rate'),
            'avg_fps': r3(frac(vs.get('avg_frame_rate'))), 'nb_frames': int(vs['nb_frames']) if vs.get('nb_frames', '').isdigit() else None,
            'start_time_s': fnum(vs.get('start_time')), 'duration_s': r3(vdur)},
        'audio': None if not as_ else {
            'codec': as_.get('codec_name'), 'sample_rate': int(as_.get('sample_rate', 0) or 0),
            'channels': as_.get('channels'), 'start_time_s': fnum(as_.get('start_time')), 'duration_s': r3(adur)},
        'audio_minus_video_duration_ms': r1((adur - vdur) * 1000) if (adur is not None and vdur is not None) else None,
        'audio_packet_timestamps': packet_continuity(args.output, 'a:0') if as_ else None,
    }
    timing['probe'] = time.time() - t

    # ---------------- decode output (also the end-to-end decode check)
    t = time.time()
    tmp = tempfile.mkdtemp(prefix='rbe_out_')
    try:
        ov, sv = first_stream(oinfo, 'video'), first_stream(sinfo, 'video')
        o_aspect = s_aspect = None
        if ov and sv and ov.get('width') and sv.get('width'):
            o_aspect = ov['width'] / max(1, ov['height'])
            s_aspect = sv['width'] / max(1, sv['height'])
        aspect_differs = bool(o_aspect and s_aspect and abs(math.log(o_aspect / s_aspect)) > 0.02)
        dec = decode(args.output, oinfo, tmp, want_video=True, want48=True, timeout=args.decode_timeout,
                     audio_clock=args.output_audio_clock, band_aspect=s_aspect if aspect_differs else None)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    exp_frames = validity['video']['nb_frames'] if validity['video'] else None
    validity['decode_check'] = {
        'method': 'full decode of every video frame (scaled to 64x36 gray) and of the first audio stream; '
                  'ffmpeg -v error output counted as errors',
        'returncode': dec['returncode'], 'timed_out': dec['timed_out'], 'wall_s': dec['wall_s'],
        'error_count': len(dec['error_lines']), 'errors_sample': dec['error_lines'][:5],
        'video_frames_decoded': dec.get('frames_decoded'), 'video_frames_in_header': exp_frames,
        'audio_seconds_decoded': r3(len(dec['a16']) / SR) if 'a16' in dec else None,
    }
    if 'a16' in dec and adur:
        validity['audio_samples_minus_timestamps_ms'] = r1((len(dec['a16']) / SR - (dec.get('audio_start_s') or 0)
                                                            - adur) * 1000)
    validity['output_audio_clock'] = args.output_audio_clock
    validity['note_audio_samples_minus_timestamps'] = (
        'decoded audio length minus the container audio duration; far from 0 means the audio packet '
        'timestamps disagree with the samples they hold (players then drift)')
    validity['decodes_end_to_end'] = (not dec['timed_out'] and dec['returncode'] == 0 and not dec['error_lines']
                                      and 'a16' in dec and (dec.get('frames_decoded') or 0) > 0)
    res['validity'] = validity
    timing['decode_output'] = time.time() - t
    if 'a16' not in dec:
        res['error'] = 'output has no decodable audio; nothing else can be measured'
        res['summary'] = {'valid': False}
        return res

    # ---------------- source
    t = time.time()
    os.makedirs(args.cache_dir, exist_ok=True)
    src = load_source(args.source, sinfo, args.cache_dir, timeout=max(args.decode_timeout, 1800))
    src_dur = len(src['a16']) / SR
    timing['source'] = time.time() - t

    out16 = dec['a16']
    o_pts = dec.get('pts')
    o_thumbs = dec.get('thumbs')
    out_audio_dur = len(out16) / SR

    # ---------------- EDL hint
    edl = None
    hint = []
    if args.edl and os.path.exists(args.edl):
        edl = load_edl(args.edl, args.source)
        for r in edl['ranges']:
            hint.append(int(round((r['src_start'] - r['out_start']) * ENV_RATE)))

    # ---------------- 2. audio mapping
    t = time.time()
    am = AudioMapper(out16, src['a16'])
    lag_f = am.run(hint_lags=hint)
    timing['audio_dp'] = time.time() - t
    t = time.time()
    s_pts = np.asarray(src['pts'])
    s_thumbs = src['thumbs']
    use_video = bool(o_thumbs is not None and len(o_thumbs) and len(s_thumbs) and not args.no_video)
    views = None
    verify_log = []
    if use_video:
        s_crop = (load_source_crop(args.source, args.cache_dir, o_aspect, len(s_thumbs), max(args.decode_timeout, 1800))
                  if aspect_differs else None)
        views = Views(o_thumbs, o_pts, s_thumbs, s_pts, o_band=dec.get('band'), s_crop=s_crop,
                      crop_bottom=args.sync_crop_bottom)
        views.run_census(am, lag_f)
        if views.census.get('status') != 'ok':
            use_video = False      # unknown framing: the picture must not steer the audio mapping
    if use_video:
        lag_f = demote_unconfirmed(am, lag_f, views=views)
        lag_f, verify_log = verify_by_picture(am, lag_f, views)
        lag_f = demote_unconfirmed(am, lag_f, views=views)     # remnants left by the verification
    else:
        lag_f = demote_unconfirmed(am, lag_f)
    timing['picture_checks'] = time.time() - t

    # video fill of long silent stretches
    t = time.time()
    basis = np.where(lag_f == NONE_LAG, 0, 1).astype(np.int8)   # 0 none, 1 audio, 2 video
    video_fill_log = []
    if use_video:
        lag_f, basis, video_fill_log = video_fill(am, lag_f, basis, views)
    timing['video_fill'] = time.time() - t

    # segments from the per-frame mapping
    t = time.time()
    segments = build_segments(am, lag_f, basis, out_audio_dur, o_thumbs, o_pts)
    timing['refine'] = time.time() - t

    # per-segment EDL comparison
    edl_report = None
    if edl:
        edl_report = compare_edl(edl, segments)
    mapped = sum(s['out_end'] - s['out_start'] for s in segments)
    unexplained = runs_of((lag_f == NONE_LAG) & am.o_speech, min_len=10)
    res['mapping'] = {
        'method': 'audio log-energy envelope search + Viterbi over candidate lags, sample-level '
                  'cross-correlation refinement; silent stretches located by video frame matching',
        'gain_db': r3(am.gain),
        'candidate_lags': len(am.lags),
        'segments': segments,
        'output_mapped_s': r3(mapped),
        'output_unexplained_speech_s': r3(sum(b - a for a, b in unexplained) / ENV_RATE),
        'video_fill': video_fill_log,
        'picture_verification': verify_log,
        'edl': edl_report,
    }
    res['video_framing'] = dict(views.census or {}, output_aspect=r3(o_aspect), source_aspect=r3(s_aspect)) \
        if views is not None else {'status': 'video not used'}

    # ---------------- 3. A/V sync
    t = time.time()
    if views is not None:
        res['sync'] = measure_sync(segments, views, n_points=args.sync_points)
        if (views.census or {}).get('status') != 'ok':
            res['sync']['note'] = ('the output picture matched no known framing (same framing with centred '
                                   'zoom 1.0-1.2, or centre-crop / letterbox for another aspect); sync points '
                                   'are likely unusable, the audio mapping is unaffected')
    else:
        res['sync'] = {'usable_points': 0, 'note': 'no video'}
    timing['sync'] = time.time() - t

    # ---------------- 4. gold (reference/gold times live on the reference clock)
    t = time.time()
    clock = Clock(src.get('audio_clock_breakpoints'), args.reference_timeline)
    seg_ref = []
    for sgm in segments:
        x = dict(sgm)
        x['src_start'], x['src_end'] = (float(v) for v in clock.to_ref([sgm['src_start'], sgm['src_end']]))
        seg_ref.append(x)
    res['mapping']['reference_clock'] = {
        'reference_timeline': args.reference_timeline,
        'source_audio_clock_breakpoints': src.get('audio_clock_breakpoints'),
        'note': 'segment src times are on the source container clock; reference words and gold units are '
                'compared after subtracting the breakpoint offset (plain-decode timeline)'}
    words = load_reference(args.reference, src_dur)
    spans = [(x['src_start'], x['src_end']) for x in seg_ref]
    kept = Intervals(spans)
    kept_flags = word_kept(words, kept)
    gold = json.load(open(args.gold))
    res['gold'] = score_gold(gold, seg_ref, words, kept_flags, kept)
    res['gold']['duplicated_source_s'] = r3(duplicated_time(spans))
    res['gold']['source_time_used_s'] = r3(kept.total())
    timing['gold'] = time.time() - t

    # ---------------- 5. cut craft
    t = time.time()
    res['cut_craft'] = cut_craft(seg_ref, words, dec.get('a48'), src.get('click_baseline'), am)
    timing['cut_craft'] = time.time() - t

    # ---------------- 6. fillers
    pats = [p.strip() for p in args.fillers.split(',') if p.strip()]
    rx = re.compile(r'^(?:' + '|'.join(pats) + r')$', re.IGNORECASE)
    if seg_ref:
        lo = min(x['src_start'] for x in seg_ref)
        hi = max(x['src_end'] for x in seg_ref)
    else:
        lo = hi = 0.0
    fill_tot = fill_kept = fs_tot = fs_kept = 0
    examples = []
    for w, k in zip(words, kept_flags):
        if w['end'] < lo or w['start'] > hi:
            continue
        tok = norm_token(w['text'])
        if tok and rx.match(tok):
            fill_tot += 1
            fill_kept += int(k)
            if k and len(examples) < 10:
                examples.append({'text': w['text'], 'src': round(w['start'], 2)})
        raw = w['text'].strip()
        if re.search(r'\w-{1,2}[^\w]*$', raw):
            fs_tot += 1
            fs_kept += int(k)
    res['fillers'] = {'patterns': pats, 'source_span': [r3(lo), r3(hi)], 'filler_tokens_in_span': fill_tot,
                      'filler_tokens_kept': fill_kept,
                      'filler_kept_pct': r1(100 * fill_kept / fill_tot) if fill_tot else None,
                      'kept_examples': examples,
                      'false_start_tokens_in_span': fs_tot, 'false_start_tokens_kept': fs_kept}

    # ---------------- 8. loudness (needed for the silence threshold)
    t = time.time()
    try:
        res['loudness'] = loudness(args.output)
    except Exception as e:  # noqa
        res['loudness'] = {'error': str(e)}
    timing['loudness'] = time.time() - t

    # ---------------- 7. pacing
    t = time.time()
    I = res['loudness'].get('integrated_lufs')
    thr = args.silence_db if args.silence_db is not None else (min(-30.0, max(-60.0, I - 25.0)) if I is not None else -50.0)
    sil = silences(args.output, thr, 1.0, duration=out_dur)
    demo_spans = [(u['start'], u['end']) for u in gold.get('units', []) if u.get('category') == 'visual_demo']
    sil_ex = []
    for a, b in sil:
        demo_t = time_mapping_into(seg_ref, a, b, demo_spans)
        sil_ex.append(max(0.0, (b - a) - demo_t))
    n_words = sum(1 for w, k in zip(words, kept_flags) if k and not rx.match(norm_token(w['text']) or '#'))
    res['pacing'] = {
        'kept_words': n_words,
        'words_per_minute': r1(n_words / (out_dur / 60)) if out_dur else None,
        'silence_threshold_db': round(thr, 1), 'silence_min_s': 1.0,
        'silences': {'count': len(sil), 'total_s': r3(sum(b - a for a, b in sil)),
                     'longest_s': r3(max((b - a for a, b in sil), default=0.0))},
        'silences_excluding_visual_demo': {
            'count': sum(1 for x in sil_ex if x >= 1.0), 'total_s': r3(sum(sil_ex)),
            'longest_s': r3(max(sil_ex, default=0.0))},
        'silence_list': [[r3(a), r3(b)] for a, b in sil][:200],
    }
    timing['pacing'] = time.time() - t

    # ---------------- 9. run meta
    if args.run_meta and os.path.exists(args.run_meta):
        try:
            res['run_meta'] = json.load(open(args.run_meta))
        except Exception as e:  # noqa
            res['run_meta'] = {'error': f'unreadable: {e}'}
    timing['total'] = time.time() - T0
    res['timing_s'] = {k: round(v, 2) for k, v in timing.items()}
    res['summary'] = summarize(res)
    return res


def demote_unconfirmed(am, lag_f, max_len_s=2.0, min_peak=0.5, views=None):
    """Short audio runs whose lag the waveform does not confirm (clicks, room noise matched to
    some random place), or whose picture is clearly from elsewhere in the source, are dropped
    from the audio mapping; video fill may locate them."""
    lag_f = lag_f.copy()
    for a, b, v in value_runs(lag_f):
        if v == NONE_LAG or b - a > max_len_s * ENV_RATE:
            continue
        w = speech_window(am.o_speech, a, b, need=10, span=b - a)
        pk = 0.0
        if w:
            _, pk = xcorr_lag(am.out16, am.src16, w[0] / ENV_RATE, w[1] / ENV_RATE, v / ENV_RATE, 0.02)
        bad = pk < min_peak
        if not bad and views is not None:
            o_pts = views.o_pts
            ts = np.linspace(a / ENV_RATE, b / ENV_RATE, 5)[1:-1]
            ks = np.clip(np.searchsorted(o_pts, ts), 0, len(o_pts) - 1)
            da = [views.dist(k, o_pts[k] + v / ENV_RATE)[0] for k in ks]
            if np.median(da) > 0.3:
                bj, dn, _, _ = views.nn(ks)
                ln = views.s_pts[bj] - o_pts[ks]
                if np.median(dn) < 0.5 * np.median(da) and abs(np.median(ln) - v / ENV_RATE) > 1.5:
                    bad = True
        if bad:
            lag_f[a:b] = NONE_LAG
    return lag_f


# ----------------------------------------------------------------------------- video fill

def video_fill(am, lag_f, basis, views, min_len_s=1.0, step_s=0.2):
    """Output stretches without speech (>= 1 s) cannot be located by audio. Sample the picture
    every 0.2 s: keep the neighbouring audio segment's lag when the picture agrees with it
    (pause inside a continuous take, static screens), otherwise use a unique global
    nearest-neighbour match in the source (e.g. a silent screen demo cut in on its own)."""
    T = len(lag_f)
    # sustained speech only (>= 30% of a +-0.25 s window): faint game audio or a click inside a
    # silent stretch must not anchor it to some audio lag
    strong = am.o_speech & (moving_mean(am.o_speech.astype(float), 25) >= 0.3)
    confirmed = (lag_f != NONE_LAG) & strong
    stretches = runs_of(~confirmed, min_len=int(min_len_s * ENV_RATE))
    if not stretches:
        return lag_f, basis, []
    o_pts, s_pts = views.o_pts, views.s_pts
    log_ = []
    lag_f = lag_f.copy()
    basis = basis.copy()
    audio_ok = confirmed

    def neighbour(f, step):
        g = f
        for _ in range(300):
            g += step
            if g < 0 or g >= T:
                return None
            if audio_ok[g]:
                return int(lag_f[g])
        return None

    def dist_at(k, lag_s):
        return views.dist(k, o_pts[k] + lag_s)[0]

    for a, b in stretches:
        prev_l, next_l = neighbour(a, -1), neighbour(b - 1, +1)
        cands = [l for l in dict.fromkeys([prev_l, next_l]) if l is not None]
        ts = np.arange(a / ENV_RATE + step_s / 2, b / ENV_RATE, step_s)
        ks = np.clip(np.searchsorted(o_pts, ts), 0, len(o_pts) - 1)
        if not len(ks):
            continue
        dnb = np.array([min([dist_at(k, l / ENV_RATE) for l in cands] or [9.0]) for k in ks])
        need = np.nonzero(dnb > 0.15)[0]            # neighbours' lags do not explain these frames
        best_j = np.zeros(len(ks), dtype=np.int64)
        d1 = dnb.copy()
        d2 = np.full(len(ks), 9.0)
        if len(need):
            bj_, d1_, d2_, _ = views.nn(ks[need])
            best_j[need], d1[need], d2[need] = bj_, d1_, d2_
        nn_l = (s_pts[best_j] - o_pts[ks]) * ENV_RATE
        uniq = np.zeros(len(ks), dtype=bool)
        uniq[need] = (d1[need] < 0.25) & (d1[need] < 0.6 * d2[need])
        if prev_l is not None and prev_l == next_l:
            # same lag on both sides: normally the source's own pause (nothing removed or
            # inserted). Keep it unless the picture is uniquely from elsewhere (> 1.5 s away;
            # a smaller offset is just the A/V offset of the take). Else the picture decides.
            agree = [(dist_at(k, prev_l / ENV_RATE) <= max(0.15, 1.25 * d1[i] + 0.02)) or not uniq[i]
                     or abs(nn_l[i] - prev_l) <= 150 for i, k in enumerate(ks)]
            if np.mean(agree) >= 0.8:
                lag_f[a:b] = prev_l
                basis[a:b] = 1
                continue
        # video candidates: unique nearest-neighbour lags, clustered (+-50 ms), >= 2 samples
        vids = []
        for x in sorted(nn_l[uniq]):
            if vids and x - vids[-1][-1] <= 5:
                vids[-1].append(x)
            else:
                vids.append([x])
        for grp in vids:
            if len(grp) >= 3:
                l = int(round(float(np.median(grp))))
                # a picture within 1.5 s of a neighbour's audio lag is that take (A/V offset)
                if all(abs(l - c) > (150 if c in (prev_l, next_l) else 5) for c in cands):
                    cands.append(l)
        if not cands:
            continue
        if len(cands) == 1:
            lag_f[a:b] = cands[0]
            basis[a:b] = 1 if cands[0] in (prev_l, next_l) else 2
            continue
        D = np.array([[min(dist_at(k, l / ENV_RATE), 2.0) for l in cands] for k in ks])
        # Viterbi over samples, switch penalty 1.0 (distance units, 0 = identical, 4 = opposite)
        acc = D[0].copy()
        back = np.zeros(D.shape, dtype=np.int32)
        for i in range(1, len(ks)):
            bi = int(np.argmin(acc))
            stay = acc
            sw = acc[bi] + 1.0
            back[i] = np.where(sw < stay, bi, np.arange(len(cands)))
            acc = np.minimum(stay, sw) + D[i]
        st = np.empty(len(ks), dtype=np.int32)
        st[-1] = int(np.argmin(acc))
        for i in range(len(ks) - 1, 0, -1):
            st[i - 1] = back[i, st[i]]
        cur = None
        for i, ci in enumerate(st):
            l = cands[ci]
            f0 = a if i == 0 else int(round((ts[i] - step_s / 2) * ENV_RATE))
            f1 = b if i == len(st) - 1 else int(round((ts[i] + step_s / 2) * ENV_RATE))
            lag_f[f0:f1] = l
            is_audio = l in (prev_l, next_l)
            basis[f0:f1] = 1 if is_audio else 2
            if is_audio:
                cur = None
            elif cur is None or cur['lag_f'] != l:
                cur = {'lag_f': l, 'out_start': r3(f0 / ENV_RATE), 'out_end': r3(f1 / ENV_RATE), 'samples': 1}
                log_.append(cur)
            else:
                cur['out_end'] = r3(f1 / ENV_RATE)
                cur['samples'] += 1
    for x in log_:
        x['lag_s'] = r3(x.pop('lag_f') / ENV_RATE)
    return lag_f, basis, log_


# ----------------------------------------------------------------------------- segments

def build_segments(am, lag_f, basis, out_dur, o_thumbs=None, o_pts=None):
    T = len(lag_f)
    none = lag_f == NONE_LAG
    key = np.where(none, 0, lag_f) * 4 + basis
    key[none] = NONE_LAG
    # tiny runs (< 50 ms) are absorbed by an adjacent run (previous preferred)
    for _ in range(3):
        runs_all = value_runs(key)
        changed = False
        for i, (a, b, v) in enumerate(runs_all):
            if v == NONE_LAG or b - a >= 5:
                continue
            prev = runs_all[i - 1] if i > 0 else None
            nxt = runs_all[i + 1] if i + 1 < len(runs_all) else None
            if prev is not None and prev[2] != NONE_LAG:
                key[a:b] = prev[2]
            elif nxt is not None and nxt[2] != NONE_LAG:
                key[a:b] = nxt[2]
            else:
                key[a:b] = NONE_LAG
            changed = True
        if not changed:
            break
    runs = [(a, b, v) for a, b, v in value_runs(key) if v != NONE_LAG]
    segs = []
    for a, b, v in runs:
        lag = int(v // 4)
        bas = int(v % 4)
        segs.append({'f0': a, 'f1': b, 'lag': lag / ENV_RATE, 'basis': 'audio' if bas == 1 else 'video'})
    # sample-level lag refinement for audio segments
    for s in segs:
        s['lag_start'] = s['lag_end'] = s['lag']
        s['xcorr_peak'] = None
        if s['basis'] != 'audio':
            continue
        f0, f1 = s['f0'], s['f1']
        w = speech_window(am.o_speech, f0, f1, need=40, span=min(1000, f1 - f0))
        if w:
            lag, pk = xcorr_lag(am.out16, am.src16, w[0] / ENV_RATE, w[1] / ENV_RATE, s['lag'], 0.02)
            if lag is not None and pk > 0.3:
                s['lag'] = lag
                s['xcorr_peak'] = round(pk, 3)
        s['lag_start'] = s['lag_end'] = s['lag']
        if w:
            a0, a1 = w[0] * HOP, w[1] * HOP
            L = int(round(s['lag'] * SR))
            if a0 + L >= 0 and a1 + L <= len(am.src16):
                x_ = am.out16[a0:a1].astype(np.float64)
                y_ = am.src16[a0 + L:a1 + L].astype(np.float64)
                den = (y_ * y_).sum()
                if den > 0:
                    g = (x_ * y_).sum() / den
                    if 0.01 < g < 100:
                        s['g'] = float(g)
        ws = speech_window(am.o_speech, f0, f1, need=40, span=200)
        we = speech_window(am.o_speech, f0, f1, need=40, span=200, from_end=True)
        if ws:
            l, pk = xcorr_lag(am.out16, am.src16, ws[0] / ENV_RATE, ws[1] / ENV_RATE, s['lag'], 0.01)
            if l is not None and pk > 0.3:
                s['lag_start'] = l
        if we:
            l, pk = xcorr_lag(am.out16, am.src16, we[0] / ENV_RATE, we[1] / ENV_RATE, s['lag'], 0.01)
            if l is not None and pk > 0.3:
                s['lag_end'] = l
    # merge output-contiguous audio segments whose refined lags agree within 1.5 ms
    merged = []
    for s in segs:
        gap_ok = merged and (s['f0'] - merged[-1]['f1'] <= 3 or (
            0 < s['f0'] - merged[-1]['f1'] <= 50 and not am.o_speech[merged[-1]['f1']:s['f0']].any()))
        if (merged and gap_ok and s['basis'] == merged[-1]['basis'] == 'audio'
                and abs(s['lag'] - merged[-1]['lag']) < 0.0015):
            m = merged[-1]
            n0, n1 = m['f1'] - m['f0'], s['f1'] - s['f0']
            m['lag'] = (m['lag'] * n0 + s['lag'] * n1) / (n0 + n1)
            m['lag_end'] = s['lag_end']
            m['f1'] = s['f1']
        else:
            merged.append(dict(s))
    segs = merged
    # boundaries: sample-level between output-adjacent segments, else frame edges
    bounds = []
    for i, s in enumerate(segs):
        s['out_start'] = s['f0'] / ENV_RATE
        s['out_end'] = s['f1'] / ENV_RATE
        s['start_unc'] = s['end_unc'] = 0.005
    for i in range(len(segs) - 1):
        A, B = segs[i], segs[i + 1]
        gap = B['f0'] - A['f1']
        if 0 < gap <= 50 and not am.o_speech[A['f1']:B['f0']].any():
            # short quiet 'unmatched' gap: still a direct join between A and B
            b, unc = refine_edge(am, A, B, (A['f1'] + B['f0']) // 2, search_s=gap / 2 / ENV_RATE + 0.3)
            A['out_end'] = B['out_start'] = b
            A['end_unc'] = B['start_unc'] = unc
            continue
        if A['f1'] != B['f0']:
            # gap (unmatched) between them: refine each edge against 'unexplained', without overlap
            A['out_end'], A['end_unc'] = refine_edge(am, A, None, A['f1'], limit=(0, B['f0'] / ENV_RATE))
            B['out_start'], B['start_unc'] = refine_edge(am, None, B, B['f0'], limit=(A['out_end'], 1e9))
            continue
        b, unc = refine_edge(am, A, B, A['f1'])
        A['out_end'] = B['out_start'] = b
        A['end_unc'] = B['start_unc'] = unc
    if segs:
        segs[0]['out_start'], segs[0]['start_unc'] = refine_edge(am, None, segs[0], segs[0]['f0'])
        segs[-1]['out_end'], segs[-1]['end_unc'] = refine_edge(am, segs[-1], None, segs[-1]['f1'])
    # an edge in silence at the very start / end of the output belongs to the file boundary
    if segs:
        if segs[0]['out_start'] - segs[0]['start_unc'] <= 0.011 and not am.o_speech[:segs[0]['f0']].any():
            segs[0]['out_start'] = 0.0
        last_t = len(am.out16) / SR
        if segs[-1]['out_end'] + segs[-1]['end_unc'] >= last_t - 0.011 and not am.o_speech[segs[-1]['f1']:].any():
            segs[-1]['out_end'] = last_t
    # joins inside silence are ambiguous in audio: use the picture cut when there is one
    for s in segs:
        s['start_basis'] = s['end_basis'] = 'audio'
    if o_thumbs is not None and len(o_thumbs) > 2:
        fd = np.concatenate([[0.0], np.abs(np.diff(np.asarray(o_thumbs, dtype=np.int16), axis=0)).mean((1, 2))])
        for i in range(len(segs) - 1):
            A, B = segs[i], segs[i + 1]
            gap = B['out_start'] - A['out_end']
            if gap < -1e-6 or gap > 0.5 or max(A['end_unc'], B['start_unc']) <= 0.02 and gap < 0.01:
                continue
            b, unc = (A['out_end'] + B['out_start']) / 2, max(A['end_unc'], B['start_unc']) + gap / 2
            k0 = max(1, int(np.searchsorted(o_pts, b - unc - 0.02)))
            k1 = int(np.searchsorted(o_pts, b + unc + 0.02))
            c0 = int(np.searchsorted(o_pts, b - 0.5))
            c1 = int(np.searchsorted(o_pts, b + 0.5))
            if k1 - k0 < 1 or c1 - c0 < 5:
                continue
            k = k0 + int(np.argmax(fd[k0:k1]))
            med = float(np.median(fd[c0:c1]))
            if fd[k] > max(4 * med, 2.0):
                dt = float(o_pts[k] - o_pts[k - 1]) if k > 0 else 1 / 60
                A['out_end'] = B['out_start'] = float(o_pts[k])
                A['end_unc'] = B['start_unc'] = dt / 2
                A['end_basis'] = B['start_basis'] = 'video_cut'
    out = []
    for i, s in enumerate(segs):
        if s['out_end'] - s['out_start'] < 0.02:
            continue
        lag_s = s['lag_start']
        lag_e = s['lag_end']
        out.append({
            'index': len(out),
            'out_start': r3(s['out_start']), 'out_end': r3(s['out_end']),
            'src_start': r3(s['out_start'] + lag_s), 'src_end': r3(s['out_end'] + lag_e),
            'duration_s': r3(s['out_end'] - s['out_start']),
            'basis': s['basis'],
            'lag_ms': r1(s['lag'] * 1000), 'lag_start_ms': r1(lag_s * 1000), 'lag_end_ms': r1(lag_e * 1000),
            'lag_drift_ms': r1((lag_e - lag_s) * 1000),
            'xcorr_peak': s['xcorr_peak'],
            'start_uncertainty_ms': r1(s['start_unc'] * 1000), 'end_uncertainty_ms': r1(s['end_unc'] * 1000),
            'start_basis': s['start_basis'], 'end_basis': s['end_basis'],
        })
    return merge_continuous(out)


def merge_continuous(segs, tol=0.010):
    """Neighbours that continue the same source span at the same lag (within 10 ms; output gap
    up to 1 s, e.g. a short stretch the audio could not confirm) are one segment: not a join,
    no cut edges."""
    merged = []
    for x in segs:
        m = merged[-1] if merged else None
        gap = x['out_start'] - m['out_end'] if m is not None else None
        if (m is not None and -tol <= gap <= 1.0
                and abs((x['src_start'] - x['out_start']) - (m['src_end'] - m['out_end'])) <= tol):
            d0, d1 = m['duration_s'], x['duration_s']
            m['out_end'], m['src_end'] = x['out_end'], x['src_end']
            m['duration_s'] = r3(m['out_end'] - m['out_start'])
            m['lag_ms'] = r1((m['lag_ms'] * d0 + x['lag_ms'] * d1) / max(d0 + d1, 1e-6))
            m['lag_end_ms'] = x['lag_end_ms']
            m['lag_drift_ms'] = r1(m['lag_end_ms'] - m['lag_start_ms'])
            m['end_uncertainty_ms'], m['end_basis'] = x['end_uncertainty_ms'], x['end_basis']
            if x['basis'] == 'audio':
                m['basis'] = 'audio'
            pk = max(m['xcorr_peak'] or 0, x['xcorr_peak'] or 0)
            m['xcorr_peak'] = pk or None
            m['merged_parts'] = m.get('merged_parts', 1) + 1
        else:
            merged.append(dict(x))
    for i, x in enumerate(merged):
        x['index'] = i
    return merged


def refine_edge(am, A, B, f_edge, search_s=0.3, limit=None):
    """Boundary between A (before) and B (after); either may be None (= nothing / silence).
    Minimises sum_{n<k} resA + sum_{n>=k} resB over a +-search window (within `limit`
    output seconds when given)."""
    x = am.out16
    c = f_edge * HOP
    lo = c - int(search_s * SR)
    hi = c + int(search_s * SR)
    if limit is not None:
        lo = max(lo, int(limit[0] * SR))
        hi = min(hi, int(limit[1] * SR))
    if A is not None:
        lo = max(lo, A['f0'] * HOP + SR // 50)
    if B is not None:
        hi = min(hi, B['f1'] * HOP - SR // 50)
    lo = max(lo, 0)
    hi = min(hi, len(x))
    if hi - lo < 32:
        return f_edge / ENV_RATE, search_s

    def resid(S):
        seg = x[lo:hi].astype(np.float64)
        if S is None:
            return seg * seg
        L = int(round(S['lag'] * SR))
        a, b = lo + L, hi + L
        y = np.zeros(hi - lo)
        aa, bb = max(a, 0), min(b, len(am.src16))
        if bb > aa:
            y[aa - a:bb - a] = am.src16[aa:bb]
        g = S.get('g') or 10 ** (am.gain / 20)
        return (seg - g * y) ** 2
    ra, rb = resid(A), resid(B)
    ca = np.concatenate([[0], np.cumsum(ra)])
    cb = np.concatenate([[0], np.cumsum(rb)])
    cost = ca + (cb[-1] - cb)
    k = int(np.argmin(cost))
    floor_e = 10 ** ((am.floor + am.gain) / 10)
    tol = max(floor_e * SR * 0.01, 1e-3 * (cost.max() - cost.min()))
    near = np.nonzero(cost <= cost[k] + tol)[0]
    a_, b_ = near.min(), near.max()
    if b_ - a_ > SR * 0.02:
        k = (a_ + b_) // 2
    return (lo + k) / SR, max(0.001, (b_ - a_) / 2 / SR)


def compare_edl(edl, segments):
    rows = []
    for r in edl['ranges']:
        decl_lag = r['src_start'] - r['out_start']
        # measured segment covering most of the declared source span
        best, bo = None, 0.0
        for s in segments:
            o = min(s['src_end'], r['src_end']) - max(s['src_start'], r['src_start'])
            if o > bo:
                best, bo = s, o
        row = {'range': r['index'], 'src_start': r3(r['src_start']), 'src_end': r3(r['src_end']),
               'declared_out_start': r3(r['out_start']), 'declared_out_end': r3(r['out_end']),
               'source_resolves': r['source_resolves'], 'is_eval_source': r['is_eval_source']}
        if best is None or bo < 0.5 * min(0.5, r['src_end'] - r['src_start']):
            row['found'] = False
        else:
            ls = best['lag_start_ms'] / 1000
            le = best['lag_end_ms'] / 1000
            ms = r['src_start'] - ls
            me = r['src_end'] - le
            row.update(found=True, segment=best['index'], covered_s=r3(bo),
                       measured_out_start=r3(ms), measured_out_end=r3(me),
                       residual_start_ms=r1((ms - r['out_start']) * 1000),
                       residual_end_ms=r1((me - r['out_end']) * 1000),
                       lag_start_ms=best['lag_start_ms'], lag_end_ms=best['lag_end_ms'],
                       declared_lag_ms=r1(decl_lag * 1000))
        rows.append(row)
    found = [x for x in rows if x.get('found')]
    res_ = [abs(x['residual_start_ms']) for x in found] + [abs(x['residual_end_ms']) for x in found]
    return {'ranges': rows, 'declared_duration_s': r3(edl['declared_duration']),
            'found': len(found), 'n': len(rows),
            'residual_abs_ms': {'median': r1(pct(res_, 50)), 'p95': r1(pct(res_, 95)), 'max': r1(max(res_) if res_ else None)}}


def duplicated_time(spans):
    ev = []
    for a, b in spans:
        if b > a:
            ev.append((a, 1))
            ev.append((b, -1))
    ev.sort()
    tot, depth, last = 0.0, 0, None
    for x, d in ev:
        if last is not None and depth >= 2:
            tot += (x - last) * (depth - 1)
        depth += d
        last = x
    return tot


def time_mapping_into(segments, a, b, src_spans):
    """Output time within [a,b] whose mapped source time falls inside any of src_spans."""
    tot = 0.0
    for s in segments:
        o0, o1 = max(a, s['out_start']), min(b, s['out_end'])
        if o1 <= o0:
            continue
        lag = s['src_start'] - s['out_start']
        for x, y in src_spans:
            tot += max(0.0, min(o1 + lag, y) - max(o0 + lag, x))
    return tot


# ----------------------------------------------------------------------------- A/V sync

def join_index(segments, tol=0.05):
    """Number of real joins (source discontinuity or output gap) before each segment."""
    out, j = [], 0
    for i, s in enumerate(segments):
        if i > 0:
            p = segments[i - 1]
            if abs(s['src_start'] - p['src_end']) > tol or abs(s['out_start'] - p['out_end']) > tol:
                j += 1
        out.append(j)
    return out


def measure_sync(segments, views, n_points=48, win=0.5, margin=0.3, search=1.5):
    # only segments whose lag was measured on the waveform (speech present)
    elig = [s for s in segments if s['basis'] == 'audio' and (s.get('xcorr_peak') or 0) >= 0.5
            and s['duration_s'] >= 2 * margin + win + 0.05]
    if not elig:
        return {'usable_points': 0, 'points_tried': 0, 'note': 'no audio segment long enough'}
    # one point per segment (evenly chosen if too many), extra points spread over long ones
    pts = []
    if len(elig) >= n_points:
        idx = np.unique(np.round(np.linspace(0, len(elig) - 1, n_points)).astype(int))
        for i in idx:
            s = elig[i]
            pts.append((s, (s['out_start'] + s['out_end'] - win) / 2))
    else:
        for s in elig:
            pts.append((s, (s['out_start'] + s['out_end'] - win) / 2))
        extra = n_points - len(elig)
        tot = sum(s['duration_s'] - 2 * margin - win for s in elig)
        if extra > 0 and tot > 0:
            grid = (np.arange(extra) + 0.5) * tot / extra
            acc = 0.0
            for s in elig:
                usable = s['duration_s'] - 2 * margin - win
                for g in grid[(grid >= acc) & (grid < acc + usable)]:
                    pts.append((s, s['out_start'] + margin + (g - acc)))
                acc += usable
    pts.sort(key=lambda p: p[1])
    jidx = join_index(segments)
    pred = 0.0
    o_pts, s_pts = views.o_pts, views.s_pts
    cb = views.crop_bottom
    s_cache = {}

    def feat_fn(skey):
        def fn(j0, j1):
            key = (skey, j0, j1)
            if key not in s_cache:
                if len(s_cache) > 8:
                    s_cache.clear()
                s_cache[key] = block_feats(views.s_range(skey, j0, j1), cb)
            return s_cache[key]
        return fn

    def match(ks, lag, center, srch, step):
        """Best view for this window: (ok, x, best, contrast, uniq, width, nb, view, motion)."""
        out = None
        for name, oname, skey in views.used:
            of = block_feats(views.o[oname][ks], cb)
            vc = video_curve(of, o_pts[ks], feat_fn(skey), s_pts, center, search=srch, step=step)
            if vc is None:
                continue
            sig, curve, nb = vc
            x, best, contrast, uniq, width = curve_min(sig, curve)
            ok = (contrast >= 0.3 and uniq >= 0.25 and width <= 0.5 and best < 0.6
                  and abs(x - center) < srch - 0.05)
            motion = float(np.mean(np.sum((of[1:] - of[:-1]) ** 2, axis=2))) if len(ks) > 1 else 0.0
            cand = (ok, x, best, contrast, uniq, width, nb, name, motion)
            if out is None or (ok, -best) > (out[0], -out[2]):
                out = cand
        return out
    rows = []
    for s, t0 in pts:
        ks = np.nonzero((o_pts >= t0) & (o_pts < t0 + win))[0]
        row = {'out_t': r3(t0), 'segment': s['index'], 'join_index': jidx[s['index']]}
        if len(ks) < 4:
            row['usable'] = False
            row['reason'] = 'too few output frames'
            rows.append(row)
            continue
        # audio-located source position (lag interpolated between segment start and end)
        f = (t0 + win / 2 - s['out_start']) / max(s['duration_s'], 1e-6)
        lag = (s['lag_start_ms'] + f * (s['lag_end_ms'] - s['lag_start_ms'])) / 1000
        # search around the audio-located position shifted by the offset seen at the previous
        # usable point (offsets accumulate join by join), then a wide fallback
        m = match(ks, lag, lag + pred, search, 0.002)
        if m is not None and not m[0]:
            m2 = match(ks, lag, lag, 4.0, 0.004)
            if m2 is not None and m2[0]:
                m = m2
        if m is None:
            row['usable'] = False
            row['reason'] = 'source frames out of range'
            rows.append(row)
            continue
        ok, x, best, contrast, uniq, width, nb, vname, motion = m
        err = (lag - x) * 1000
        if ok:
            pred = x - lag
        row.update(audio_src_s=r3(t0 + win / 2 + lag), video_src_s=r3(t0 + win / 2 + x), error_ms=r1(err),
                   view=vname, contrast=r3(contrast), uniqueness=r3(uniq), valley_width_s=r3(width),
                   best_dist=r3(best), motion=r3(motion), blocks=nb, usable=bool(ok))
        if not ok:
            row['reason'] = 'low confidence (static or ambiguous picture, or unknown framing)'
        rows.append(row)
    use = [r for r in rows if r.get('usable')]
    vc_ = {}
    for r in use:
        vc_[r['view']] = vc_.get(r['view'], 0) + 1
    out = {'points_tried': len(rows), 'usable_points': len(use), 'window_s': win, 'search_s': search,
           'views_of_usable_points': vc_, 'framing': views.census,
           'definition': 'error = audio source time - video source time at the same output instant; '
                         'negative = sound late relative to picture'}
    if use:
        e = np.array([r['error_ms'] for r in use])
        tt = np.array([r['out_t'] for r in use])
        si = np.array([r['join_index'] for r in use])
        m_t, c_t, r2_t = ols(tt / 60, e)
        m_j, c_j, r2_j = ols(si, e)
        out.update({
            'median_ms': r1(np.median(e)), 'mean_ms': r1(e.mean()),
            'p95_abs_ms': r1(np.percentile(np.abs(e), 95)), 'max_abs_ms': r1(np.abs(e).max()),
            'first_ms': r1(e[0]), 'last_ms': r1(e[-1]),
            'drift_ms_per_min': r1(m_t), 'drift_ms_per_min_theil_sen': r1(theil_sen(tt / 60, e)), 'drift_r2_time': r3(r2_t),
            'drift_ms_per_join': r1(m_j), 'drift_ms_per_join_theil_sen': r1(theil_sen(si, e)), 'drift_r2_join': r3(r2_j),
            'intercept_ms_join_fit': r1(c_j),
        })
    out['points'] = rows
    return out


# ----------------------------------------------------------------------------- cut craft

def cut_craft(segments, words, a48, baseline, am):
    res = {}
    n = len(segments)
    joins = [(segments[i], segments[i + 1]) for i in range(n - 1)]
    res['segments'] = n
    res['joins'] = len(joins)
    durs = np.array([s['duration_s'] for s in segments]) if segments else np.array([])
    res['segment_length_s'] = {'min': r3(durs.min()) if len(durs) else None,
                               'median': r3(np.median(durs)) if len(durs) else None,
                               'mean': r3(durs.mean()) if len(durs) else None,
                               'count_lt_0_5s': int((durs < 0.5).sum())}
    # edges vs words
    clipped, inside = [], []
    wa = np.array([w['start'] for w in words])
    for s in segments:
        for edge, t in (('in', s['src_start']), ('out', s['src_end'])):
            i0 = max(0, int(np.searchsorted(wa, t - 5)))
            for w in words[i0:]:
                if w['start'] > t + 0.001:
                    break
                if not (w['start'] < t < w['end']):
                    continue
                d = w['end'] - w['start']
                kept_ms = (w['end'] - t) * 1000 if edge == 'in' else (t - w['start']) * 1000
                cut_ms = d * 1000 - kept_ms
                frac_in = kept_ms / (d * 1000)
                rec = {'segment': s['index'], 'edge': edge, 'src_t': r3(t), 'word': w['text'],
                       'word_start': r3(w['start']), 'word_end': r3(w['end']),
                       'kept_ms': r1(kept_ms), 'cut_ms': r1(cut_ms), 'fraction_kept': r3(frac_in)}
                inside.append(rec)
                if 0.1 <= frac_in <= 0.9:
                    clipped.append(rec)
    res['clipped_words'] = len(clipped)
    res['edges_inside_words'] = len(inside)
    res['clipped_word_list'] = clipped[:100]
    res['edges_inside_word_list'] = inside[:100]
    # clicks at joins
    if a48 is not None and len(a48) and joins:
        fl = click_floor(a48)
        centers = [(A['out_end'], min(50.0, max(10.0, A['end_uncertainty_ms'] or 0))) for A, B in joins]
        st = click_stat(a48, centers, floor=fl)
        thr = baseline.get('p99_5') if baseline else None
        vals = [v for v in st if v is not None]
        res['clicks'] = {
            'statistic': 'max 2 ms first-difference energy within +-10 ms of the join / median of the same in the '
                         'surrounding +-100 ms',
            'threshold': r3(thr), 'threshold_basis': 'p99.5 of the statistic at 4000 random points of the uncut source',
            'source_baseline': baseline,
            'joins_measured': len(vals),
            'over_threshold': int(sum(v > thr for v in vals)) if thr else None,
            'median_ratio': r3(np.median(vals)) if vals else None, 'max_ratio': r3(max(vals)) if vals else None,
            'per_join': [{'out_t': r3(c[0]), 'ratio': r3(v)} for c, v in zip(centers, st)][:300],
        }
    return res


# ----------------------------------------------------------------------------- summary

def summarize(res):
    v = res.get('validity', {})
    g = res.get('gold', {})
    s = res.get('sync', {})
    c = res.get('cut_craft', {})
    p = res.get('pacing', {})
    lo = res.get('loudness', {})
    f = res.get('fillers', {})
    m = res.get('mapping', {})
    meta = res.get('run_meta') or {}
    usage = meta.get('usage') or {}
    out = {
        'valid': bool(v.get('decodes_end_to_end')),
        'duration_s': v.get('duration_s'),
        'video': f"{(v.get('video') or {}).get('codec')} {(v.get('video') or {}).get('width')}x{(v.get('video') or {}).get('height')} "
                 f"@{(v.get('video') or {}).get('avg_fps')}",
        'audio_minus_video_ms': v.get('audio_minus_video_duration_ms'),
        'segments': c.get('segments'), 'joins': c.get('joins'),
        'mapped_s': m.get('output_mapped_s'), 'unexplained_speech_s': m.get('output_unexplained_speech_s'),
        'edl_residual_median_ms': ((m.get('edl') or {}).get('residual_abs_ms') or {}).get('median'),
        'edl_residual_max_ms': ((m.get('edl') or {}).get('residual_abs_ms') or {}).get('max'),
        'drop_recall': (g.get('drop', {}).get('overall') or {}).get('recall'),
        'drop_recall_by_category': {k: x['recall'] for k, x in g.get('drop', {}).items() if k != 'overall'},
        'core_keep_recall': (g.get('keep', {}).get('core') or {}).get('recall'),
        'normal_keep_recall': (g.get('keep', {}).get('normal') or {}).get('recall'),
        'visual_demo_kept': f"{g['visual_demo']['kept']}/{g['visual_demo']['n']}" if g.get('visual_demo') else None,
        'visual_demo_time_fraction': (g.get('visual_demo') or {}).get('time_fraction_present'),
        'one_of_exactly_one_rate': (g.get('one_of') or {}).get('exactly_one_rate'),
        'one_of_duplicates': (g.get('one_of') or {}).get('duplicate'),
        'one_of_lost': (g.get('one_of') or {}).get('none'),
        'duplicated_source_s': g.get('duplicated_source_s'),
        'clipped_words': c.get('clipped_words'), 'edges_inside_words': c.get('edges_inside_words'),
        'segments_lt_0_5s': (c.get('segment_length_s') or {}).get('count_lt_0_5s'),
        'clicks_over_threshold': (c.get('clicks') or {}).get('over_threshold'),
        'filler_kept_pct': f.get('filler_kept_pct'), 'false_starts_kept': f.get('false_start_tokens_kept'),
        'words_per_minute': p.get('words_per_minute'),
        'dead_air_s': (p.get('silences_excluding_visual_demo') or {}).get('total_s'),
        'longest_silence_s': (p.get('silences') or {}).get('longest_s'),
        'sync_usable_points': s.get('usable_points'), 'sync_median_ms': s.get('median_ms'),
        'sync_p95_abs_ms': s.get('p95_abs_ms'), 'sync_max_abs_ms': s.get('max_abs_ms'),
        'sync_first_ms': s.get('first_ms'), 'sync_last_ms': s.get('last_ms'),
        'sync_drift_ms_per_join': s.get('drift_ms_per_join'), 'sync_drift_ms_per_min': s.get('drift_ms_per_min'),
        'loudness_lufs': lo.get('integrated_lufs'), 'true_peak_dbtp': lo.get('true_peak_dbtp'), 'lra_lu': lo.get('lra_lu'),
        'harness': meta.get('harness'), 'task': meta.get('task'), 'run': meta.get('run'),
        'wall_seconds': meta.get('wall_seconds'), 'cost_usd': meta.get('cost_usd'), 'num_turns': meta.get('num_turns'),
        'output_tokens': usage.get('output_tokens'),
        'eval_seconds': (res.get('timing_s') or {}).get('total'),
    }
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--source', required=True, help='raw footage the edit was made from')
    ap.add_argument('--reference', required=True, help='word-level reference transcript (Scribe JSON)')
    ap.add_argument('--gold', required=True, help='gold annotation JSON')
    ap.add_argument('--output', required=True, help='edited video to measure')
    ap.add_argument('--edl', help='EDL written by the harness (hint only)')
    ap.add_argument('--run-meta', help='run metadata JSON written by the run harness')
    ap.add_argument('--out', required=True, help='where to write result JSON')
    ap.add_argument('--cache-dir', default=os.environ.get('RUSHES_BENCH_CACHE',
                                                          os.path.expanduser('~/.cache/rushes-bench-eval')))
    ap.add_argument('--fillers', default=DEFAULT_FILLERS,
                    help='comma-separated regexes matched against lower-cased tokens (punctuation stripped)')
    ap.add_argument('--sync-points', type=int, default=48)
    ap.add_argument('--sync-crop-bottom', type=float, default=0.0,
                    help='ignore this fraction of the frame bottom in video matching (captions)')
    ap.add_argument('--silence-db', type=float, default=None,
                    help='silencedetect threshold; default integrated loudness - 25 dB, clamped to [-60,-30]')
    ap.add_argument('--reference-timeline', choices=['plain', 'pts'], default='plain',
                    help="clock of the reference/gold times: 'plain' = a plain decode of the source audio that "
                         "ignores timestamp gaps (what ASR pipelines usually see), 'pts' = container clock")
    ap.add_argument('--output-audio-clock', choices=['player', 'pts'], default='player',
                    help="'player' (default): decoded output samples played back to back from the first audio "
                         "timestamp, as players do; 'pts': follow the audio packet timestamps")
    ap.add_argument('--decode-timeout', type=int, default=900)
    ap.add_argument('--no-video', action='store_true', help='skip all video matching')
    args = ap.parse_args(argv)
    res = evaluate(args)
    os.makedirs(os.path.dirname(os.path.abspath(args.out)) or '.', exist_ok=True)
    with open(args.out, 'w') as f:
        json.dump(res, f, ensure_ascii=False, indent=1, default=lambda o: o.item() if hasattr(o, 'item') else str(o))
    print(json.dumps(res.get('summary', {}), ensure_ascii=False, indent=1,
                     default=lambda o: o.item() if hasattr(o, 'item') else str(o)))


if __name__ == '__main__':
    main()
