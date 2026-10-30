#!/usr/bin/env python3
"""diag_meter.py —— 拍号（meter）检测可行性诊断

不依赖 numpy，纯 python。做两件事：

1. 合成带真实强弱模式的鼓组信号（3/4 waltz、4/4 rock、4/4 four-on-floor、6/8），
   写成 WAV，供 flac 编码后交给分析器验证 BPM 是否仍然正确。
2. 复刻 Rust 侧的 onset 包络，取「每拍的 onset 强度」序列，看自相关在
   lag=2/3/4 上的分布 —— 判断「用拍级强度周期性能不能把 3/4 和 4/4 分开」。

用法：
    python tools/diag_meter.py synth <输出目录>   # 生成 wav 样本
    python tools/diag_meter.py analyze            # 直接分析内存里的合成信号
"""

import math
import os
import random
import struct
import sys
import wave

SR = 22050
FRAME_LEN = 1024
HOP = 256
FPS = SR / HOP
LOG_COMPRESS = 1000.0
LOCAL_MEAN_SEC = 0.15
LONG_MEAN_SEC = 1.0

# ---------------------------------------------------------------------------
# 合成鼓组
# ---------------------------------------------------------------------------

# 每种「曲式」定义为小节内每拍要敲什么
PATTERNS = {
    # 3/4 圆舞曲：第 1 拍重 kick，2/3 拍弱 snare
    "waltz34": [["kick"], ["snare_soft"], ["snare_soft"]],
    # 4/4 摇滚：kick 1/3，snare 2/4 —— 注意这个模式的周期其实是 2 拍
    "rock44": [["kick"], ["snare"], ["kick"], ["snare"]],
    # 4/4 电子舞曲：每拍都有 kick，只有第 1 拍多一个 crash
    "dance44": [["kick", "crash", "hat"], ["kick", "hat"], ["kick", "hat"], ["kick", "hat"]],
    # 6/8：按八分音符 6 拍，重音在 1、4（每 3 个一组）
    "six8": [["kick"], ["hat"], ["hat"], ["snare"], ["hat"], ["hat"]],
}


def _kick(buf, t, gain=1.0):
    n = int(0.18 * SR)
    i0 = int(t * SR)
    for k in range(n):
        i = i0 + k
        if i >= len(buf):
            break
        tt = k / SR
        env = math.exp(-tt * 26.0)
        buf[i] += gain * env * math.sin(2 * math.pi * 58.0 * tt)


def _snare(buf, t, gain=0.6):
    n = int(0.13 * SR)
    i0 = int(t * SR)
    rnd = random.Random(int(t * 1000) + 7)
    for k in range(n):
        i = i0 + k
        if i >= len(buf):
            break
        tt = k / SR
        env = math.exp(-tt * 42.0)
        buf[i] += gain * env * (rnd.uniform(-1, 1) * 0.8 + 0.4 * math.sin(2 * math.pi * 190.0 * tt))


def _hat(buf, t, gain=0.18):
    n = int(0.05 * SR)
    i0 = int(t * SR)
    rnd = random.Random(int(t * 1000) + 13)
    prev = 0.0
    for k in range(n):
        i = i0 + k
        if i >= len(buf):
            break
        tt = k / SR
        env = math.exp(-tt * 90.0)
        x = rnd.uniform(-1, 1)
        # 粗糙高通
        hp = x - prev
        prev = x
        buf[i] += gain * env * hp


def _crash(buf, t, gain=0.32):
    n = int(0.9 * SR)
    i0 = int(t * SR)
    rnd = random.Random(int(t * 1000) + 29)
    for k in range(n):
        i = i0 + k
        if i >= len(buf):
            break
        tt = k / SR
        env = math.exp(-tt * 3.2)
        buf[i] += gain * env * rnd.uniform(-1, 1)


HITS = {"kick": _kick, "snare": _snare, "snare_soft": lambda b, t: _snare(b, t, 0.32),
        "hat": _hat, "crash": _crash}


def synth(name, bpm, seconds=14.0):
    """返回 (samples, beat_times, pattern)"""
    pat = PATTERNS[name]
    bar = len(pat)
    # 6/8 的「拍」按八分音符算，BPM 给的是八分音符速度
    period = 60.0 / bpm
    n = int(seconds * SR)
    buf = [0.0] * n

    beats = []
    b = 0
    while True:
        t = b * period
        if t >= seconds:
            break
        beats.append(t)
        for hit in pat[b % bar]:
            HITS[hit](buf, t)
        b += 1

    # 加一点点整体噪声底，避免纯静音段让归一化爆掉
    rnd = random.Random(1234)
    for i in range(0, n, 7):
        buf[i] += rnd.uniform(-0.002, 0.002)
    return buf, beats, pat


def write_wav(path, buf):
    peak = max(1e-9, max(abs(v) for v in buf))
    scale = 0.89 / peak
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        frames = bytearray()
        for v in buf:
            s = int(max(-1.0, min(1.0, v * scale)) * 32767)
            frames += struct.pack("<h", s)
        w.writeframes(bytes(frames))


# ---------------------------------------------------------------------------
# 复刻 Rust 的 onset 包络（只保留和分析相关的部分）
# ---------------------------------------------------------------------------

def onset_envelope(samples):
    n_frames = len(samples) // HOP + 1
    half = FRAME_LEN // 2
    energy = []
    for f in range(n_frames):
        center = f * HOP
        s = max(0, center - half)
        e = min(len(samples), center + half)
        if e <= s:
            energy.append(0.0)
            continue
        acc = 0.0
        for v in samples[s:e]:
            acc += v * v
        rms = math.sqrt(acc / (e - s))
        energy.append(math.log(1.0 + LOG_COMPRESS * rms))

    diff = [0.0]
    for i in range(1, len(energy)):
        diff.append(max(0.0, energy[i] - energy[i - 1]))

    win = max(1, round(LOCAL_MEAN_SEC * FPS))
    n = len(diff)
    prefix = [0.0]
    for v in diff:
        prefix.append(prefix[-1] + v)
    nov = []
    for i in range(n):
        lo = max(0, i - win // 2)
        hi = min(n, i + win // 2 + 1)
        mean = (prefix[hi] - prefix[lo]) / (hi - lo)
        nov.append(max(0.0, diff[i] - mean))

    # 3 点平滑
    sm = list(nov)
    for i in range(1, n - 1):
        sm[i] = 0.25 * nov[i - 1] + 0.5 * nov[i] + 0.25 * nov[i + 1]

    # 长窗高通
    win2 = max(1, round(FPS * LONG_MEAN_SEC))
    pref2 = [0.0]
    for v in sm:
        pref2.append(pref2[-1] + v)
    out = []
    for i in range(n):
        lo = max(0, i - win2 // 2)
        hi = min(n, i + win2 // 2 + 1)
        mean = (pref2[hi] - pref2[lo]) / (hi - lo)
        out.append(max(0.0, sm[i] - mean))

    mx = max(out) if out else 0.0
    if mx > 1e-9:
        out = [v / mx for v in out]
    return out


def beat_strength(nov, beats, pre=0.05, post=0.08):
    """每拍的 onset 强度：取 [t-pre, t+post] 内的最大值（对齐抖动的容忍）"""
    out = []
    for t in beats:
        i0 = max(0, int((t - pre) * FPS))
        i1 = min(len(nov), int((t + post) * FPS) + 1)
        out.append(max(nov[i0:i1]) if i1 > i0 else 0.0)
    return out


# ---------------------------------------------------------------------------
# 低频包络：判断 downbeat 该看底鼓，而不是宽带 onset
# ---------------------------------------------------------------------------

def lowpass(x, fc=110.0):
    """一阶 IIR 低通，保留 110 Hz 以下（底鼓基频）"""
    a = math.exp(-2.0 * math.pi * fc / SR)
    out = []
    prev = 0.0
    b = 1.0 - a
    for v in x:
        prev = b * v + a * prev
        out.append(prev)
    return out


def rms_envelope(x):
    """帧 RMS 包络（不打差分，保留「谁更响」的绝对信息）"""
    n_frames = len(x) // HOP + 1
    half = FRAME_LEN // 2
    out = []
    for f in range(n_frames):
        c = f * HOP
        s = max(0, c - half)
        e = min(len(x), c + half)
        if e <= s:
            out.append(0.0)
            continue
        acc = 0.0
        for v in x[s:e]:
            acc += v * v
        out.append(math.sqrt(acc / (e - s)))
    mx = max(out) if out else 0.0
    if mx > 1e-9:
        out = [v / mx for v in out]
    return out


def acf(seq, max_lag=8):
    n = len(seq)
    mean = sum(seq) / n
    x = [v - mean for v in seq]
    r = []
    for lag in range(max_lag + 1):
        cnt = n - lag
        if cnt <= 2:
            r.append(0.0)
            continue
        num = sum(x[i] * x[i + lag] for i in range(cnt))
        e1 = sum(x[i] * x[i] for i in range(cnt))
        e2 = sum(x[i + lag] * x[i + lag] for i in range(cnt))
        den = math.sqrt(e1 * e2)
        r.append(num / den if den > 1e-9 else 0.0)
    return r


def report(name, bpm, seconds=14.0, analysis_bpm=None):
    """`bpm` 是合成时的音符速度；`analysis_bpm` 是分析器实际会给出的拍速。

    6/8 就是典型例子：按八分音符 180 写谱，分析器（和人的听觉）会给出 90，
    拍网格因此变成「一拍 = 两个八分音符」，拍号结论也跟着变。
    """
    buf, beats, pat = synth(name, bpm, seconds)
    bpb = len(pat)
    if analysis_bpm and abs(analysis_bpm - bpm) > 1e-6:
        # 合成用的拍网格是按八分音符排的，分析拍速不同就重采样一条等间隔网格
        period = 60.0 / analysis_bpm
        beats = []
        t = 0.0
        while t < seconds:
            beats.append(t)
            t += period

    # 与 dsp.rs 的实现保持一致：拍号判据建立在**低频**强度上，不是宽带 onset
    low = rms_envelope(lowpass(buf))
    s = beat_strength(low, beats, pre=0.02, post=0.06)
    r = acf(s)

    tag = f"bpm={bpm}" + (f"（分析拍速 {analysis_bpm}）" if analysis_bpm and analysis_bpm != bpm else "")
    print(f"\n=== {name}  {tag}  小节 {bpb} 音符 ===")
    print("每拍低频强度:", " ".join(f"{v:.2f}" for v in s[:16]), "...")
    trio = r[3]
    even = r[2] if r[2] > r[4] else r[4]
    # 相对起伏：低于 15% 说明每拍一样重，ACF 峰全是采样伪影（同 dsp.rs::METER_MIN_VARIATION）
    mean = sum(s) / len(s)
    std = math.sqrt(sum((v - mean) ** 2 for v in s) / len(s))
    cv = std / mean if mean > 1e-6 else 0.0
    verdict = "起伏不足，不判" if cv < 0.15 else ("3 拍组" if trio >= 0.15 and trio >= even + 0.10 else "4 拍组")
    print(f"r(3)={trio:+.2f}  max(r2,r4)={even:+.2f}  起伏 cv={cv:.03f} → {verdict}")

    # 宽带 onset 的相位作对照：它会被 snare 带偏，所以实现里没用它
    s_broad = beat_strength(onset_envelope(buf), beats)
    avg_b = phase_avg(s_broad, bpb)
    print("宽带 onset 相位:", " ".join(f"{v:.2f}" for v in avg_b), f"→ 强拍 {max(range(bpb), key=lambda i: avg_b[i])}")

    avg = phase_avg(s, bpb)
    print("低频(≤110Hz) 相位:", " ".join(f"{v:.2f}" for v in avg), f"→ 强拍 {max(range(bpb), key=lambda i: avg[i])}")
    return r


def phase_avg(seq, bpb):
    """按小节内位置取模求均值"""
    sums = [0.0] * bpb
    counts = [0] * bpb
    for i, v in enumerate(seq):
        sums[i % bpb] += v
        counts[i % bpb] += 1
    return [sums[i] / counts[i] if counts[i] else 0.0 for i in range(bpb)]


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    if sys.argv[1] == "analyze":
        report("waltz34", 120)
        report("rock44", 120)
        report("dance44", 128)
        report("six8", 180, analysis_bpm=90)
        return 0
    if sys.argv[1] == "synth" and len(sys.argv) >= 3:
        outdir = sys.argv[2]
        os.makedirs(outdir, exist_ok=True)
        for name, bpm in [("waltz34", 120), ("rock44", 120), ("dance44", 128), ("six8", 180)]:
            buf, beats, pat = synth(name, bpm)
            p = os.path.join(outdir, f"{name}_{bpm}.wav")
            write_wav(p, buf)
            with open(p + ".txt", "w") as f:
                f.write(f"{name}\tbpm={bpm}\tbeats_per_bar={len(pat)}\n")
                f.write(" ".join(f"{t:.4f}" for t in beats))
            print("写出", p)
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
