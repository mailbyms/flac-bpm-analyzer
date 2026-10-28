"""合成对照：区分「真正的半速曲」与「强弱交替导致的半速误判」。

生成两段已知真值的信号，走同一条 onset/ACF 链路：

  S1 真 62.5 BPM，每拍等强的 kick
  S2 真 125 BPM，kick / snare 交替（模拟 Creep 那种 backbeat）

观察两项：
  r(1拍) / r(2拍)                         —— 原始自相关，看半速偏好有多强
  alt_ratio = r_norm(1拍) / r_raw(1拍)     —— 峰归一化后 1 拍相关性的提升比例，
                                             用来判断「是不是强弱交替造成的」

用法：python tools/diag_octave.py
"""

import math

import diag_tempo as dt
from diag_tempo import acf_at_frac, acf_at_lags, centered, refine_lag

SR = 22050


def synth(seconds, period, alternating):
    """period 秒一拍；alternating=True 时奇数拍 kick、偶数拍 snare"""
    n = int(seconds * SR)
    out = [0.0] * n
    beat = 0
    t = 0.0
    while t < seconds:
        start = int(t * SR)
        if alternating and beat % 2 == 1:
            # snare：短促高频噪声（用固定序列保证可复现）
            length = int(0.06 * SR)
            seed = 12345
            for k in range(length):
                i = start + k
                if i >= n:
                    break
                seed = (seed * 1103515245 + 12345) & 0x7FFFFFFF
                noise = (seed / 0x7FFFFFFF) * 2.0 - 1.0
                env = math.exp(-6.0 * k / length)
                out[i] += 0.45 * noise * env
        else:
            # kick：60Hz 正弦 + 指数衰减
            length = int(0.12 * SR)
            for k in range(length):
                i = start + k
                if i >= n:
                    break
                env = math.exp(-5.0 * k / length)
                out[i] += 0.8 * math.sin(2 * math.pi * 60.0 * k / SR) * env
        t += period
        beat += 1
    return out


def peak_normalize(x, win):
    n = len(x)
    half = max(1, win // 2)
    env = [0.0] * n
    for i in range(n):
        env[i] = max(x[max(0, i - half) : min(n, i + half + 1)])
    return [x[i] / env[i] if env[i] > 1e-6 else 0.0 for i in range(n)]


def analyze(name, samples, true_bpm):
    novelty, fps = dt.onset_envelope(samples)
    n = len(novelty)
    lag_min = max(2, round(60.0 / 180.0 * fps))
    lag_max = max(4, min(round(60.0 / 60.0 * fps), n // 2))
    rr = [0.0] * (lag_max + 1)
    for k, v in acf_at_lags(centered(novelty), list(range(lag_min, lag_max + 1))).items():
        rr[k] = v

    p_fast = refine_lag(rr, round(60.0 * fps / true_bpm))
    p_slow = p_fast * 2.0
    fast_lag_ok = lag_min <= round(p_fast) <= lag_max
    slow_lag_ok = round(p_slow) <= lag_max

    c_raw = centered(novelty)
    c_norm = centered(peak_normalize(novelty, round(0.12 * fps)))

    r_raw_fast = acf_at_frac(c_raw, p_fast)
    r_norm_fast = acf_at_frac(c_norm, p_fast)
    r_raw_slow = acf_at_frac(c_raw, p_slow) if slow_lag_ok else float("nan")
    r_norm_slow = acf_at_frac(c_norm, p_slow) if slow_lag_ok else float("nan")

    alt = r_norm_fast / r_raw_fast if abs(r_raw_fast) > 1e-6 else float("nan")
    print(f"\n{name}（真 {true_bpm} BPM，周期 {p_fast:.2f} 帧）")
    print(f"  r(1拍) 原始 {r_raw_fast:.4f} → 归一化 {r_norm_fast:.4f}   alt_ratio = {alt:.3f}")
    if slow_lag_ok:
        print(
            f"  r(2拍) 原始 {r_raw_slow:.4f} → 归一化 {r_norm_slow:.4f}"
            f"   （原始 2拍/1拍 = {r_raw_slow / r_raw_fast:.2f} 倍）"
        )
    else:
        print("  2 拍 lag 超出搜索范围（不受半速问题影响）")
    return alt


def main():
    print("合成对照：真 62.5 BPM 等强 vs 真 125 BPM 强弱交替")
    s1 = synth(60.0, 0.96, alternating=False)
    s2 = synth(60.0, 0.48, alternating=True)
    a1 = analyze("S1 每拍等强的 62.5 BPM", s1, 62.5)
    a2 = analyze("S2 kick/snare 交替的 125 BPM", s2, 125.0)

    print("\n=== 判据可用性 ===")
    print(f"等强素材 alt_ratio = {a1:.3f}（归一化几乎不改变）")
    print(f"交替素材 alt_ratio = {a2:.3f}（归一化明显抬高 1 拍相关性）")
    if a2 > a1 * 1.08:
        print("→ 两者可区分：alt_ratio 明显偏高时可判定为「强弱交替导致的半速误判」")
    else:
        print("→ 区分度不足，不能只靠 alt_ratio")


if __name__ == "__main__":
    main()
