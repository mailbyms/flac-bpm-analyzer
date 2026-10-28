"""复现 flac-bpm-analyzer 的 onset/ACF 测速链路，定位 Creep.flac 被判成 62.5 BPM 的原因。

不改 Rust 代码，只用外部脚本按 dsp.rs 的同一套参数重算，对比 62.5 vs 125 的得分。

参数全部对齐 src/analysis/dsp.rs：
  FRAME_LEN=1024  HOP=256  LOG_COMPRESS=1000  LOCAL_MEAN_SEC=0.15  LONG_MEAN_SEC=1.0
  MIN_BPM=60  MAX_BPM=180  COMB_WEIGHTS=[1.0,0.55,0.38,0.28]
  TEMPO_PRIOR_CENTER=125  TEMPO_PRIOR_STRENGTH=0.45  TEMPO_PRIOR_OCTAVE_WIDTH=1.9
  bonus：原始 ACF 最优点 ×1.02

用法：python tools/diag_tempo.py <22050-s16le-mono.raw>  （先用 ffmpeg 解码）
"""

import array
import math
import sys

FRAME_LEN = 1024
HOP = 256
SR = 22050
LOG_COMPRESS = 1000.0
LOCAL_MEAN_SEC = 0.15
LONG_MEAN_SEC = 1.0
MIN_BPM = 60.0
MAX_BPM = 180.0
COMB_WEIGHTS = [1.0, 0.55, 0.38, 0.28]
PRIOR_CENTER = 125.0
PRIOR_STRENGTH = 0.45
PRIOR_OCTAVE_WIDTH = 1.9


def read_pcm(path):
    a = array.array("h")
    with open(path, "rb") as f:
        a.frombytes(f.read())
    # s16 → f32 [-1,1)
    return [v / 32768.0 for v in a]


def onset_envelope(samples, use_highpass=True):
    fps = SR / HOP
    half = FRAME_LEN // 2
    n_frames = len(samples) // HOP + 1

    # 平方前缀和 → O(1) 求每帧 RMS
    sq = [0.0] * (len(samples) + 1)
    acc = 0.0
    for i, s in enumerate(samples):
        acc += s * s
        sq[i + 1] = acc

    energy = []
    for f in range(n_frames):
        center = f * HOP
        start = max(0, center - half)
        end = min(center + half, len(samples))
        if end <= start:
            energy.append(0.0)
            continue
        rms = math.sqrt((sq[end] - sq[start]) / (end - start))
        energy.append(math.log(1.0 + LOG_COMPRESS * rms))

    diff = [0.0]
    for i in range(1, len(energy)):
        diff.append(max(0.0, energy[i] - energy[i - 1]))

    win = max(1, round(LOCAL_MEAN_SEC * fps))
    n = len(diff)
    prefix = [0.0] * (n + 1)
    for i, v in enumerate(diff):
        prefix[i + 1] = prefix[i] + v
    novelty = []
    max_v = 0.0
    for i in range(n):
        lo = max(0, i - win // 2)
        hi = min(n, i + win // 2 + 1)
        mean = (prefix[hi] - prefix[lo]) / (hi - lo)
        v = max(0.0, diff[i] - mean)
        max_v = max(max_v, v)
        novelty.append(v)
    if max_v > 1e-6:
        novelty = [v / max_v for v in novelty]

    # 3 点平滑 [0.25, 0.5, 0.25]
    sm = []
    m = len(novelty)
    for i in range(m):
        l = novelty[0] if i == 0 else novelty[i - 1]
        r = novelty[m - 1] if i + 1 >= m else novelty[i + 1]
        sm.append(0.25 * l + 0.5 * novelty[i] + 0.25 * r)
    novelty = sm

    if not use_highpass:
        mx = max(novelty) if novelty else 0.0
        return ([v / mx for v in novelty] if mx > 1e-6 else novelty), fps

    # 长窗高通（1 秒）
    w = max(1, round(fps * LONG_MEAN_SEC))
    m = len(novelty)
    prefix = [0.0] * (m + 1)
    for i, v in enumerate(novelty):
        prefix[i + 1] = prefix[i] + v
    out = []
    for i in range(m):
        lo = max(0, i - w // 2)
        hi = min(m, i + w // 2 + 1)
        mean = (prefix[hi] - prefix[lo]) / (hi - lo)
        out.append(max(0.0, novelty[i] - mean))
    mx = max(out) if out else 0.0
    if mx > 1e-6:
        out = [v / mx for v in out]
    return out, fps


def centered(x):
    mean = sum(x) / len(x)
    return [v - mean for v in x]


def acf_at_lags(c, lags):
    """对给定整数 lag 计算归一化自相关（与 normalized_acf_core 一致）"""
    n = len(c)
    res = {}
    for lag in lags:
        count = n - lag
        if count <= 0:
            res[lag] = 0.0
            continue
        num = e1 = e2 = 0.0
        for i in range(count):
            a = c[i]
            b = c[i + lag]
            num += a * b
            e1 += a * a
            e2 += b * b
        denom = math.sqrt(e1 * e2)
        res[lag] = num / denom if denom > 1e-9 else 0.0
    return res


def acf_at_frac(c, lag):
    """分数 lag 的归一化自相关（线性插值延迟），对应 acf_at_frac"""
    if lag < 0:
        return 0.0
    fl = int(math.floor(lag))
    frac = lag - fl
    n = len(c)
    count = n - (fl + 2)
    if count < 4:
        return 0.0
    num = e1 = e2 = 0.0
    for i in range(count):
        a = c[i]
        b = c[i + fl] * (1.0 - frac) + c[i + fl + 1] * frac
        num += a * b
        e1 += a * a
        e2 += b * b
    denom = math.sqrt(e1 * e2)
    return num / denom if denom > 1e-9 else 0.0


def refine_lag(r, lag):
    """抛物线顶点修正"""
    if lag == 0 or lag + 1 >= len(r):
        return float(lag)
    a, b, cc = r[lag - 1], r[lag], r[lag + 1]
    denom = a - 2.0 * b + cc
    if abs(denom) < 1e-9:
        return float(lag)
    delta = 0.5 * (a - cc) / denom
    return lag + max(-0.5, min(0.5, delta))


def comb_score(c, period, n_frames):
    score = 0.0
    w_sum = 0.0
    for k, w in enumerate(COMB_WEIGHTS):
        lag = period * (k + 1)
        if lag > n_frames - 3:
            break
        score += w * acf_at_frac(c, lag)
        w_sum += w
    return score / w_sum if w_sum > 0 else 0.0


def tempo_prior(bpm):
    x = math.log(bpm / PRIOR_CENTER) / math.log(PRIOR_OCTAVE_WIDTH)
    return 1.0 + PRIOR_STRENGTH * math.exp(-0.5 * x * x)


def main():
    path = sys.argv[1]
    samples = read_pcm(path)
    novelty, fps = onset_envelope(samples)
    print(f"帧数 {len(novelty)} | fps {fps:.3f} | 时长 {len(samples)/SR:.1f}s")

    lag_min = max(2, round(60.0 / MAX_BPM * fps))
    lag_max = max(4, min(round(60.0 / MIN_BPM * fps), len(novelty) // 2))
    print(f"lag 搜索区间 [{lag_min}, {lag_max}]  (对应 {60.0*fps/lag_max:.1f} ~ {60.0*fps/lag_min:.1f} BPM)")

    # 只算搜索区间内的整数 lag（够找峰）
    r = acf_at_lags(centered(novelty), list(range(lag_min, lag_max + 1)))
    rr = [0.0] * (lag_max + 1)
    for k, v in r.items():
        rr[k] = v

    # 局部极大值
    peaks = []
    for lag in range(lag_min + 1, lag_max):
        if rr[lag] >= rr[lag - 1] and rr[lag] >= rr[lag + 1]:
            peaks.append((lag, rr[lag]))
    peaks.sort(key=lambda t: -t[1])
    print("\nACF 局部峰（前 6）:")
    for lag, v in peaks[:6]:
        print(f"  lag {lag:>3} → {60.0*fps/lag:>7.2f} BPM   r={v:.4f}")

    best_lag = max(range(lag_min, lag_max + 1), key=lambda l: rr[l])
    print(f"\n原始最优 lag {best_lag} → {60.0*fps/best_lag:.2f} BPM (r={rr[best_lag]:.4f})")

    # 关键对比：125 BPM 与半速 62.5 BPM
    print("\n=== 125 vs 62.5 逐项对比 ===")
    c = centered(novelty)
    rows = []
    for target_bpm, label in ((125.0, "125"), (62.5, "62.5")):
        ideal = 60.0 * fps / target_bpm
        lag_i = round(ideal)
        period = refine_lag(rr, lag_i)
        bpm_refined = 60.0 * fps / period
        cs = comb_score(c, period, len(novelty))
        prior = tempo_prior(bpm_refined)
        bonus = 1.02 if lag_i == best_lag else 1.0
        total = cs * prior * bonus
        rows.append((label, ideal, lag_i, period, bpm_refined, cs, prior, bonus, total))
        print(
            f"{label:>5} BPM: 理想 lag {ideal:.2f} → 峰 lag {lag_i}, 细化 {period:.3f}"
            f" ({bpm_refined:.2f} BPM)\n"
            f"        梳状分 {cs:.4f} × 先验 {prior:.4f} × bonus {bonus} = {total:.4f}"
        )

    print("\n=== 结论 ===")
    a, b = rows[0], rows[1]
    winner = "125（正确）" if a[8] > b[8] else "62.5（半速）"
    print(f"按当前评分 {winner} 胜出，总分比 125/62.5 = {a[8]/b[8]:.3f}")
    print(
        f"其中梳状分比 = {a[5]/b[5]:.3f}（1 倍 lag 的 ACF：125→{acf_at_frac(c, a[3]):.4f}, 62.5→{acf_at_frac(c, b[3]):.4f}）"
    )
    for k in range(4):
        la = a[3] * (k + 1)
        lb = b[3] * (k + 1)
        if la <= len(novelty) - 3 and lb <= len(novelty) - 3:
            print(
                f"  k={k+1}: {la:>6.2f} 帧 r={acf_at_frac(c, la):>7.4f}   "
                f"{lb:>6.2f} 帧 r={acf_at_frac(c, lb):>7.4f}"
            )


if __name__ == "__main__":
    main()
