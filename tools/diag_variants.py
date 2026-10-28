"""对照实验：定位半速偏好的来源。

对同一份 onset 包络比较三个变体，在「1 拍（125 BPM）」与「2 拍（62.5 BPM）」处的自相关：

  A baseline        —— 当前实现（局部均值归一化 + 3 点平滑 + 1 秒长窗高通）
  B no_highpass     —— 去掉 1 秒长窗高通（长窗宽度设成 1 帧）
  C peak_normalized —— baseline 之后按「滑动峰值包络」把每个 onset 峰归一化到 1，
                       用来验证「kick / snare 强弱交替导致 2 拍周期主导」这个假设

用法：python tools/diag_variants.py <22050-s16le-mono.raw>
"""

import sys

import diag_tempo as dt
from diag_tempo import acf_at_frac, acf_at_lags, centered, comb_score, refine_lag, tempo_prior


def peak_normalize(x, win):
    """按滑动最大值包络归一化：把每个 onset 峰拉到 1，抹平强弱差异。"""
    n = len(x)
    half = max(1, win // 2)
    out = []
    env = [0.0] * n
    for i in range(n):
        lo = max(0, i - half)
        hi = min(n, i + half + 1)
        env[i] = max(x[lo:hi])
    for i in range(n):
        out.append(x[i] / env[i] if env[i] > 1e-6 else 0.0)
    return out


def onset_without_highpass(samples):
    v, _ = dt.onset_envelope(samples, use_highpass=False)
    return v


def main():
    samples = dt.read_pcm(sys.argv[1])
    novelty, fps = dt.onset_envelope(samples)
    n = len(novelty)

    lag_min = max(2, round(60.0 / 180.0 * fps))
    lag_max = max(4, min(round(60.0 / 60.0 * fps), n // 2))
    rr = [0.0] * (lag_max + 1)
    for k, v in acf_at_lags(centered(novelty), list(range(lag_min, lag_max + 1))).items():
        rr[k] = v
    p125 = refine_lag(rr, round(60.0 * fps / 125.0))
    p625 = refine_lag(rr, round(60.0 * fps / 62.5))
    print(f"帧数 {n} | fps {fps:.3f}")
    print(f"周期：125 BPM → {p125:.3f} 帧 | 62.5 BPM → {p625:.3f} 帧\n")

    variants = [
        ("A baseline", novelty),
        ("B no_highpass", onset_without_highpass(samples)),
        ("C peak_norm", peak_normalize(novelty, round(0.12 * fps))),
    ]

    header = (
        f"{'变体':<16}{'r(1拍)':>8}{'r(2拍)':>8}"
        f"{'梳状125':>9}{'梳状62.5':>10}{'最终125':>9}{'最终62.5':>10}{'判定':>10}"
    )
    print(header)
    print("-" * 78)
    for name, x in variants:
        c = centered(x)
        r1 = acf_at_frac(c, p125)
        r2 = acf_at_frac(c, p625)
        s1 = comb_score(c, p125, n)
        s2 = comb_score(c, p625, n)
        t1 = s1 * tempo_prior(60.0 * fps / p125)
        t2 = s2 * tempo_prior(60.0 * fps / p625)
        verdict = "125 正确" if t1 > t2 else "62.5 半速"
        print(
            f"{name:<16}{r1:>8.4f}{r2:>8.4f}{s1:>9.4f}{s2:>10.4f}"
            f"{t1:>9.4f}{t2:>10.4f}{verdict:>12}"
        )


if __name__ == "__main__":
    main()
