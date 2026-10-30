//! DSP：重采样、能量包络、onset 检测、自相关测速、相位对齐、节拍生成、置信度、拍号推断
//!
//! 流程（analysis/mod.rs 里串起来）：
//!   PCM(单声道) → 降采样 → 分帧 RMS → 对数压缩 → 半波整流差分 → 滑动均值归一化
//!     → 自相关求 BPM 候选（含倍频处理） → 抛物线插值细化 → 相位扫描 → 生成节拍表
//!     → 低频包络 → 每拍强度 → 拍号（3 拍组 / 4 拍组）与小节重拍相位
//!
//! 可改进点（未实现，见 README「已知限制」）：
//!   * 当前 onset 用「能量差分」，对鼓点弱、持续音强的音乐鲁棒性一般；
//!     换成 STFT 谱通量（spectral flux）+ rustfft 可明显提升精度；
//!   * 自相关用的是全局单点估计，未做 tempo 跟踪（长曲中的速度变化不处理）；
//!   * 拍号只到「3 拍组 / 4 拍组」，4/4 与 2/4、6/8 与 3/4 都不区分。

// ---------------------------------------------------------------------------
// 关键参数（集中在此便于调整）
// ---------------------------------------------------------------------------

/// 分析采样率：降低到 22.05 kHz 足够覆盖节拍相关频段并显著减少计算量
pub const TARGET_ANALYSIS_SR: u32 = 22050;
/// STFT / RMS 帧长
pub const FRAME_LEN: usize = 1024;
/// 帧移
pub const HOP: usize = 256;
/// BPM 搜索下限
pub const MIN_BPM: f32 = 60.0;
/// BPM 搜索上限
pub const MAX_BPM: f32 = 180.0;
/// 波形峰值聚合桶数（绘图用，与窗口宽度无关）
pub const WAVEFORM_BUCKETS: usize = 8192;
/// 能量对数压缩系数
pub const LOG_COMPRESS: f32 = 1000.0;
/// onset 包络滑动平均归一化窗口（秒）：突出相对于局部水平的突起
pub const LOCAL_MEAN_SEC: f32 = 0.15;
/// 长窗高通窗口（秒）：压掉慢变能量趋势，避免自相关偏向半速
pub const LONG_MEAN_SEC: f32 = 1.0;
/// 自相关梳状加权（对应 lag、2lag、3lag、4lag 的贡献）
pub const COMB_WEIGHTS: [f32; 4] = [1.0, 0.55, 0.38, 0.28];
/// 速度先验：以 CENTER 为中心的偏好，用于打破倍频/分频的平局（0.0 表示无偏好）
pub const TEMPO_PRIOR_CENTER: f32 = 125.0;
/// 先验强度
pub const TEMPO_PRIOR_STRENGTH: f32 = 0.45;
/// 先验宽度（八度尺度，越大越平坦）
pub const TEMPO_PRIOR_OCTAVE_WIDTH: f32 = 1.9;
/// 未知结果时的兜底 BPM
pub const FALLBACK_BPM: f32 = 120.0;
/// 半速陷阱修正：结果低于该 BPM 时，才检查是否该翻倍
pub const HALF_TIME_LIMIT_BPM: f32 = 70.0;
/// 半速陷阱修正：两倍速候选的梳状分达到当前分的这个比例，就判定为「强弱交替导致的半速」
///
/// 阈值依据（合成信号实测，见 tools/diag_octave.py）：
///   * 真正 62.5 / 70 / 80 / 90 BPM 的曲子，两倍速处几乎无能量，比值为 0.05 ~ 0.35
///   * 真 125 BPM 但相邻拍强弱交替（Creep.flac），比值为 0.75
/// 取 0.7 有充足裕量。
pub const HALF_TIME_RATIO: f32 = 0.7;

/// 线性插值重采样（整数倍 / 非整数倍都适用，简单且足够用于能量分析）
pub fn resample_linear(input: &[f32], src_sr: u32, dst_sr: u32) -> Vec<f32> {
    if input.is_empty() || src_sr == dst_sr || src_sr == 0 || dst_sr == 0 {
        return input.to_vec();
    }
    let ratio = dst_sr as f64 / src_sr as f64;
    let out_len = (input.len() as f64 * ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len.max(1));
    let max_idx = input.len() - 1;
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let i0 = pos.floor() as usize;
        if i0 >= max_idx {
            out.push(input[max_idx]);
            continue;
        }
        let frac = (pos - i0 as f64) as f32;
        let a = input[i0];
        let b = input[i0 + 1];
        out.push(a + (b - a) * frac);
    }
    out
}

/// 单次遍历聚合成定长峰值数组（0.0~1.0），专门用于波形绘制。
/// 每个桶取窗口内绝对值的最大值，保证瞬态不会被平均掉。
pub fn compute_waveform_peaks(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let buckets = buckets.min(samples.len());
    let step = samples.len() as f32 / buckets as f32;
    let mut peaks = Vec::with_capacity(buckets);
    let mut global_max = 0f32;

    for b in 0..buckets {
        let start = ((b as f32) * step) as usize;
        let end = ((((b + 1) as f32) * step) as usize).min(samples.len()).max(start + 1);
        let mut m = 0f32;
        for &s in &samples[start..end] {
            let abs = s.abs();
            if abs > m {
                m = abs;
            }
        }
        if m > global_max {
            global_max = m;
        }
        peaks.push(m);
    }

    if global_max > 1e-6 {
        for p in peaks.iter_mut() {
            *p /= global_max;
        }
    }
    peaks
}

/// 计算能量包络并转换为 onset 强度包络（novelty）。
/// 返回 (novelty, fps)，novelty 已归一化到 0~1。
pub fn onset_envelope(samples: &[f32], sr: u32) -> (Vec<f32>, f32) {
    let fps = sr as f32 / HOP as f32;
    if samples.len() <= FRAME_LEN {
        return (vec![0.0; samples.len().min(1)], fps);
    }

    let n_frames = samples.len() / HOP + 1;
    let half = FRAME_LEN / 2;
    let mut energy = Vec::with_capacity(n_frames);
    for f in 0..n_frames {
        // 居中窗：第 f 帧的时间 = f * HOP / sr，窗覆盖 [中心-半窗, 中心+半窗]
        let center = f * HOP;
        let start = center.saturating_sub(half);
        let end = (center + half).min(samples.len());
        if end <= start {
            energy.push(0f32);
            continue;
        }
        let slice = &samples[start..end];
        let mut sum = 0f32;
        for &s in slice {
            sum += s * s;
        }
        let rms = (sum / slice.len() as f32).sqrt();
        // 对数压缩，抑制动态范围
        energy.push((1.0 + LOG_COMPRESS * rms).ln());
    }
    if energy.len() < 2 {
        return (energy, fps);
    }

    // 半波整流的一阶差分
    let mut diff = Vec::with_capacity(energy.len());
    diff.push(0.0);
    for i in 1..energy.len() {
        diff.push((energy[i] - energy[i - 1]).max(0.0));
    }

    // 滑动平均归一化：减去局部均值后再整流一步，突出相对突起
    let win = ((LOCAL_MEAN_SEC * fps).round() as usize).max(1);
    let len = diff.len();
    let mut novelty = Vec::with_capacity(len);
    // 前缀和，O(n) 求局部均值
    let mut prefix = Vec::with_capacity(len + 1);
    prefix.push(0.0);
    for &v in &diff {
        let last = *prefix.last().unwrap();
        prefix.push(last + v);
    }
    let mut max_v = 0f32;
    for i in 0..len {
        let lo = i.saturating_sub(win / 2);
        let hi = (i + win / 2 + 1).min(len);
        let local_mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        let v = (diff[i] - local_mean).max(0.0);
        if v > max_v {
            max_v = v;
        }
        novelty.push(v);
    }
    if max_v > 1e-6 {
        for v in novelty.iter_mut() {
            *v /= max_v;
        }
    }

    // 轻度时域平滑：把宽度只有 1 帧的脉冲摊开到相邻帧，
    // 否则「非整数帧周期」会因采样抖动导致自相关在真实周期处偏低（偏向半速）
    novelty = smooth(&novelty);
    // 长窗高通：再减一次 1 秒级别的局部均值。慢变的能量趋势会让长 lag 的
    // 自相关虚高，从而系统性地偏向半速，这里把它压掉。
    novelty = high_pass(&novelty, (fps * LONG_MEAN_SEC).round() as usize);
    let max_v = novelty.iter().cloned().fold(0f32, f32::max);
    if max_v > 1e-6 {
        for v in novelty.iter_mut() {
            *v /= max_v;
        }
    }
    (novelty, fps)
}

/// 减去长窗均值后再做半波整流
fn high_pass(x: &[f32], win: usize) -> Vec<f32> {
    let win = win.max(1);
    let n = x.len();
    if n == 0 {
        return Vec::new();
    }
    let mut prefix = Vec::with_capacity(n + 1);
    prefix.push(0.0);
    for &v in x {
        let last = *prefix.last().unwrap();
        prefix.push(last + v);
    }
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(win / 2);
            let hi = (i + win / 2 + 1).min(n);
            let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
            (x[i] - mean).max(0.0)
        })
        .collect()
}

/// 3 点移动平均 [0.25, 0.5, 0.25]
fn smooth(x: &[f32]) -> Vec<f32> {
    let n = x.len();
    if n < 3 {
        return x.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let l = if i == 0 { x[0] } else { x[i - 1] };
        let r = if i + 1 >= n { x[n - 1] } else { x[i + 1] };
        out.push(0.25 * l + 0.5 * x[i] + 0.25 * r);
    }
    out
}

// ---------------------------------------------------------------------------
// 自相关 / BPM 估计
// ---------------------------------------------------------------------------

/// 去掉均值后的正中化序列
fn centered(x: &[f32]) -> Vec<f32> {
    let n = x.len().max(1);
    let mean = x.iter().copied().sum::<f32>() / n as f32;
    x.iter().map(|v| v - mean).collect()
}

/// 整数 lag 的归一化自相关胚函数，同时返回正中化序列，供分数 lag 复用
fn normalized_acf_core(x: &[f32], max_lag: usize) -> (Vec<f32>, Vec<f32>) {
    let n = x.len();
    let centered = centered(x);
    let mut r = Vec::with_capacity(max_lag + 1);
    for lag in 0..=max_lag {
        let count = n.saturating_sub(lag);
        if count == 0 {
            r.push(0.0);
            continue;
        }
        let mut num = 0f64;
        let mut e1 = 0f64;
        let mut e2 = 0f64;
        for i in 0..count {
            let a = centered[i] as f64;
            let b = centered[i + lag] as f64;
            num += a * b;
            e1 += a * a;
            e2 += b * b;
        }
        let denom = (e1 * e2).sqrt();
        r.push(if denom > 1e-9 {
            (num / denom) as f32
        } else {
            0.0
        });
    }
    (r, centered)
}

/// 「真正的」分数 lag 归一化自相关：对信号本身做线性插值延迟。
/// 直接插值整数 lag 的 ACF 会严重低估（脉冲只有 1~2 帧宽），
/// 从而让非整数帧周期的系统性地输给它的整数倍（偏向半速）。
fn acf_at_frac(c: &[f32], lag: f32) -> f32 {
    let n = c.len();
    if lag < 0.0 || n < 4 {
        return 0.0;
    }
    let floor = lag.floor() as usize;
    let frac = lag - floor as f32;
    let count = n.saturating_sub(floor + 2);
    if count < 4 {
        return 0.0;
    }
    let mut num = 0f64;
    let mut e1 = 0f64;
    let mut e2 = 0f64;
    for i in 0..count {
        let a = c[i] as f64;
        // 对偏移位置做线性插值
        let j0 = i + floor;
        let j1 = j0 + 1;
        let b = c[j0] as f64 + (c[j1] as f64 - c[j0] as f64) * frac as f64;
        num += a * b;
        e1 += a * a;
        e2 += b * b;
    }
    let denom = (e1 * e2).sqrt();
    if denom > 1e-9 {
        (num / denom) as f32
    } else {
        0.0
    }
}

pub struct TempoResult {
    pub bpm: f32,
    pub confidence: f32,
    /// 首拍偏移（秒）
    pub offset_sec: f32,
    /// 节拍周期（秒）
    pub period_sec: f32,
}

/// 对数速度轴上的高斯先验（类似 madmom / Essentia 的 preferred tempo）
fn tempo_prior(bpm: f32) -> f32 {
    let x = (bpm / TEMPO_PRIOR_CENTER).ln() / TEMPO_PRIOR_OCTAVE_WIDTH.ln();
    1.0 + TEMPO_PRIOR_STRENGTH * (-0.5 * x * x).exp()
}

/// 主入口：给定 onset 包络，估计速度、相位与置信度
pub fn estimate_tempo(novelty: &[f32], fps: f32) -> TempoResult {
    let n = novelty.len();
    let fallback = TempoResult {
        bpm: FALLBACK_BPM,
        confidence: 0.0,
        offset_sec: 0.0,
        period_sec: 60.0 / FALLBACK_BPM,
    };
    if n < 16 || !fps.is_finite() || fps <= 0.0 {
        return fallback;
    }

    // lag 搜索范围：由 BPM 下限决定最大 lag，上限决定最小 lag
    let lag_max = ((60.0 / MIN_BPM * fps).round() as usize).min(n / 2).max(4);
    let lag_min = ((60.0 / MAX_BPM * fps).round() as usize).max(2);
    if lag_max <= lag_min {
        return fallback;
    }

    // 梳状评分需要用到 4 倍 lag
    let acf_len = (lag_max * 4 + 4).min(n - 1);
    let (r, c) = normalized_acf_core(novelty, acf_len);

    // 原始最优 lag（限制在 [lag_min, lag_max]）
    let mut best_lag = lag_min;
    let mut best_val = f32::NEG_INFINITY;
    for lag in lag_min..=lag_max {
        if r[lag] > best_val {
            best_val = r[lag];
            best_lag = lag;
        }
    }

    // 收集候选：搜索区间内的所有局部极大值（最多取前 6 个），再补充其倍频/分频
    let mut candidates: Vec<usize> = Vec::new();
    let mut local_peaks: Vec<(usize, f32)> = Vec::new();
    for lag in (lag_min + 1)..lag_max {
        if r[lag] >= r[lag - 1] && r[lag] >= r[lag + 1] {
            local_peaks.push((lag, r[lag]));
        }
    }
    local_peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (lag, _) in local_peaks.iter().take(6) {
        push_candidate(&mut candidates, *lag, &r);
    }
    push_candidate(&mut candidates, best_lag, &r);

    // 逐个候选做梳状评分（处理倍频/分频问题）
    // 注意：评分与最终取值都使用「细化后的浮点周期」，否则真实的非整数帧周期会在
    // k 倍 lag 处产生漂移，导致系统性地偏向半速。
    let mut chosen_period = refine_lag(&r, best_lag);
    let mut best_score = f32::NEG_INFINITY;
    for &lag in candidates.iter() {
        if lag < lag_min || lag > lag_max {
            continue;
        }
        let period_f = refine_lag(&r, lag);
        let score = comb_score(&c, period_f, n);
        // 倍频/分频打成平手时，以 125 BPM 为中心的温和先验决定倾向。
        //
        // 这里**不能**再给「原始 ACF 最大 lag」额外加权：实测 Creep.flac（真 125 BPM，
        // 相邻拍强弱交替）的原始峰恰好落在半速 62.5 上，任何朝原始峰的奖励都会把半速坐实。
        let bpm_i = 60.0 * fps / period_f;
        let prior = tempo_prior(bpm_i);
        let score = score * prior;
        if score > best_score {
            best_score = score;
            chosen_period = period_f;
        }
    }

    // ---- 半速陷阱修正 ----
    // 翻倍时记下「原来的两拍周期」：信号最强的周期性证据在那一处，置信度要用它来评估。
    // 否则会拿半拍处的弱峰去算，得出一个误导性的低置信度。
    let mut evidence_lag = None;
    if let Some(fast) = fix_half_time(&c, chosen_period, fps, lag_min, lag_max, n) {
        evidence_lag = Some(chosen_period.round() as usize);
        chosen_period = fast;
    }

    let refined_lag = chosen_period;
    let bpm = 60.0 * fps / refined_lag.max(1.0);

    // 相位对齐
    let period_frames = refined_lag;
    let offset_frames = refine_phase(novelty, period_frames);
    // 差分 onset 天然会「提前」半个分析窗被检测到，这里做常数补偿；
    // HOP / analysis_sr = 1 / fps，故半窗换算成帧为 (FRAME_LEN/2)/HOP
    let half_window_frames = FRAME_LEN as f32 / (2.0 * HOP as f32);
    let offset_sec = ((offset_frames + half_window_frames) / fps).max(0.0);
    let period_sec = period_frames / fps;

    let conf_lag = evidence_lag
        .unwrap_or(refined_lag.round() as usize)
        .min(r.len().saturating_sub(1));
    let confidence = estimate_confidence(&r, conf_lag, lag_min, lag_max);

    TempoResult {
        bpm: bpm.clamp(MIN_BPM * 0.5, MAX_BPM * 2.0),
        confidence,
        offset_sec,
        period_sec,
    }
}

/// 半速陷阱修正：把「因为相邻拍强弱交替而被判成半速」的结果翻回两倍速。
///
/// 现象：4/4 流行/摇滚的鼓组是 kick / snare 交替，平移 1 拍后强峰对弱峰，1 拍处的
/// 自相关会腰斩，而 2 拍处才是完整循环。于是 ACF 主导周期变成 2 拍，BPM 被判成一半。
///
/// 判据：结果低于 `HALF_TIME_LIMIT_BPM` 时，看两倍速候选的梳状分能保住当前分的多少。
/// 真正的慢歌在半拍位置几乎没有 onset，这个比值很低（0.05~0.35）；
/// 而强弱交替的快歌在两倍速处仍有很强的周期性（0.75 左右）。阈值取 0.7。
///
/// 返回 `Some(两倍速周期)` 表示发生了翻倍，`None` 表示维持原判。
pub fn fix_half_time(
    c: &[f32],
    period: f32,
    fps: f32,
    lag_min: usize,
    lag_max: usize,
    n_frames: usize,
) -> Option<f32> {
    let bpm = 60.0 * fps / period.max(1e-6);
    if bpm >= HALF_TIME_LIMIT_BPM {
        return None;
    }
    // 两倍速的周期就是当前周期的一半；细化后的周期误差在这里也减半，通常更准
    let fast_period = period * 0.5;
    if (fast_period.round() as usize) < lag_min || (fast_period.round() as usize) > lag_max {
        return None;
    }
    let slow_score = comb_score(c, period, n_frames);
    let fast_score = comb_score(c, fast_period, n_frames);
    if should_double_for_half_time(bpm, slow_score, fast_score) {
        Some(fast_period)
    } else {
        None
    }
}

/// 半速陷阱的判定条件（纯逻辑，便于用实测数值做回归）
fn should_double_for_half_time(bpm: f32, slow_score: f32, fast_score: f32) -> bool {
    bpm < HALF_TIME_LIMIT_BPM && slow_score > 1e-6 && fast_score / slow_score >= HALF_TIME_RATIO
}

/// 把 lag 及其倍频/分频候选加入候选列表（若落在合法 lag 区间内）
fn push_candidate(list: &mut Vec<usize>, lag: usize, r: &[f32]) {
    let limit = r.len().saturating_sub(2);
    for l in [lag * 2, lag / 2, lag * 3, lag / 3] {
        if l >= 2 && l <= limit && !list.contains(&l) {
            list.push(l);
        }
    }
    if lag >= 2 && lag <= limit && !list.contains(&lag) {
        list.push(lag);
    }
}

/// 梳状评分：把候选周期各整数倍处的自相关值加权求和，用于判别真正的节拍周期。
/// `period_frames` 是浮点周期（来自抛物线细化），配合 `acf_at_frac` 可让
/// 非整数帧周期与其整数倍处在同等条件下比较。
fn comb_score(c: &[f32], period_frames: f32, n_frames: usize) -> f32 {
    let mut score = 0f32;
    let mut w_sum = 0f32;
    for (k, &w) in COMB_WEIGHTS.iter().enumerate() {
        let lag = period_frames * (k + 1) as f32;
        if lag > (n_frames - 3) as f32 {
            break;
        }
        score += w * acf_at_frac(c, lag);
        w_sum += w;
    }
    if w_sum > 0.0 {
        score / w_sum
    } else {
        0.0
    }
}

/// 用抛物线顶点修正整数 lag，得到亚帧精度
fn refine_lag(r: &[f32], lag: usize) -> f32 {
    if lag == 0 || lag + 1 >= r.len() {
        return lag as f32;
    }
    let y0 = r[lag - 1];
    let y1 = r[lag];
    let y2 = r[lag + 1];
    let denom = y0 - 2.0 * y1 + y2;
    if denom.abs() < 1e-9 {
        return lag as f32;
    }
    let delta = 0.5 * (y0 - y2) / denom;
    (lag as f32 + delta.clamp(-0.5, 0.5)).max(1.0)
}

/// 相位扫描：在 [0, period) 内以 1 帧为步长滑动，取 onset 能量和最大的位置。
/// 索引处用线性插值读取，使 sub-frame 周期也能正确累积。
fn refine_phase(novelty: &[f32], period_frames: f32) -> f32 {
    let n = novelty.len();
    let period = period_frames.max(1.0);
    if n < 4 {
        return 0.0;
    }

    /// 给定偏移（单位：帧，允许小数）累计该相位网格上的 onset 能量
    fn eval(novelty: &[f32], period: f32, offset: f32) -> f32 {
        let n = novelty.len();
        let mut score = 0f32;
        let mut idx = offset;
        let mut guard = 0usize;
        while idx < n as f32 - 1.0 && guard < n {
            let i0 = idx.floor() as usize;
            let frac = idx - i0 as f32;
            let v = novelty[i0] + (novelty[i0 + 1] - novelty[i0]) * frac;
            score += v * v;
            idx += period;
            guard += 1;
        }
        score
    }

    // 以 1/4 帧为步长扫描一个周期
    let step = 0.25f32;
    let count = (period / step).round() as usize;
    let mut best_offset = 0f32;
    let mut best_score = f32::NEG_INFINITY;
    for i in 0..count.max(1) {
        let o = i as f32 * step;
        let score = eval(novelty, period, o);
        if score > best_score {
            best_score = score;
            best_offset = o;
        }
    }

    // 相邻两点做二次插值修正
    let s0 = eval(novelty, period, (best_offset - step + period) % period);
    let s2 = eval(novelty, period, best_offset + step);
    let denom = s0 - 2.0 * best_score + s2;
    let delta = if denom.abs() > 1e-9 {
        0.5 * (s0 - s2) / denom
    } else {
        0.0
    };
    (best_offset + delta.clamp(-step, step)).max(0.0)
}

/// 置信度：综合「峰值相对高度」与「次峰比」，保守映射
fn estimate_confidence(r: &[f32], best_lag: usize, lag_min: usize, lag_max: usize) -> f32 {
    let peak = r[best_lag].max(0.0);
    // 区间均值
    let mut sum = 0f32;
    let mut count = 0usize;
    for lag in lag_min..=lag_max.min(r.len() - 1) {
        sum += r[lag].max(0.0);
        count += 1;
    }
    let mean = if count > 0 { sum / count as f32 } else { 0.0 };
    let rel = ((peak - mean) / (1.0 - mean + 1e-6)).clamp(0.0, 1.0);

    // 次峰（排除最佳峰附近 ±25% 周期）
    let guard = (best_lag as f32 * 0.25).max(2.0) as usize;
    let mut second = 0f32;
    for lag in lag_min..=lag_max.min(r.len() - 1) {
        if lag.abs_diff(best_lag) <= guard {
            continue;
        }
        let v = r[lag].max(0.0);
        if v > second {
            second = v;
        }
    }
    let sep = if peak > 1e-6 {
        (1.0 - second / peak).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // 两项加权后统一打 0.9 折，避免虚高
    ((0.5 * rel + 0.5 * sep) * 0.9).clamp(0.0, 1.0)
}

/// 由首拍偏移与周期生成整首曲子的节拍时间点（秒）
pub fn generate_beats(offset_sec: f32, period_sec: f32, duration_sec: f32) -> Vec<f32> {
    let mut beats = Vec::new();
    if !period_sec.is_finite() || period_sec <= 0.0 {
        return beats;
    }
    let mut t = offset_sec.max(0.0);
    // 若首拍偏移已超过时长（异常兜底），回退到 0
    if t >= duration_sec {
        t = 0.0;
    }
    while t < duration_sec {
        beats.push(t);
        t += period_sec;
    }
    beats
}

// ---------------------------------------------------------------------------
// 拍号（meter）与小节重拍（downbeat）
// ---------------------------------------------------------------------------
//
// 只用一路宽带 onset 判不出小节层次：snare 的频谱通量比底鼓大，实测 4/4 摇滚
// 会被判成「第 3 拍最强」。所以单独算一路低频包络（底鼓基频），在拍网格上取样，
// 得到「每拍有多重」的序列，再对它做自相关：
//   * r(3) 明显为正 → 每 3 拍重复 → 3/4
//   * r(3) 为负、r(2)/r(4) 为正 → 2 或 4 拍一组 → 按 4/4 处理
// 4/4 与 2/4 在信号上等价，6/8 与 3/4 同周期，都不做区分（见 README）。

/// 低频带截止频率（Hz）：底鼓基频基本落在 40~100 Hz，取 110 略有余量
pub const LOW_BAND_HZ: f32 = 110.0;
/// 每拍强度的取样窗：向前 / 向后（秒）
pub const BEAT_WIN_PRE: f32 = 0.02;
pub const BEAT_WIN_POST: f32 = 0.06;
/// 少于这么多拍就不猜拍号（≈2 个小节）
pub const METER_MIN_BEATS: usize = 9;
/// 判为三拍子的门槛：r(3) 至少这么高
pub const METER_R3_MIN: f32 = 0.15;
/// 且 r(3) 要比 r(2)、r(4) 中较大的那个还高出这么多
pub const METER_R3_MARGIN: f32 = 0.10;
/// 每拍强度的相对起伏（标准差 / 均值）低于此值就不做拍号判断
///
/// four-on-floor 这类每拍都有底鼓的曲子，强度序列几乎是常数，剩下那 1~2% 的
/// 细微差别全是「拍网格与帧网格」的采样相位伪影——自相关会把它放大成 0.4 以上的
/// 假峰（实测 dance44 就是这样被误判成 3/4）。真实强弱拍的差异远大于此。
pub const METER_MIN_VARIATION: f32 = 0.15;
/// 相位打平时（差值小于此值）取更靠前的那个，避免 4/4 摇滚在「第 1 拍 / 第 3 拍」
/// 两个等价候选之间来回跳
pub const PHASE_TIE_EPS: f32 = 0.02;

/// 低频（底鼓）RMS 包络，帧率与 onset 包络一致（sr / HOP）
///
/// 实现上边滤波边用环形缓冲维护最近一窗的平方和，不额外保存整段低通信号
/// （长曲目能省下几十 MB）。
pub fn low_band_envelope(samples: &[f32], sr: u32) -> Vec<f32> {
    if samples.is_empty() || sr == 0 {
        return Vec::new();
    }
    // 一阶 IIR 低通：z[n] = b*x[n] + a*z[n-1]
    let a = (-2.0 * std::f32::consts::PI * LOW_BAND_HZ / sr as f32).exp();
    let b = 1.0 - a;

    let n_frames = samples.len() / HOP + 1;
    let mut env = vec![0f32; n_frames];
    let half = FRAME_LEN / 2;
    let mut ring = vec![0f64; FRAME_LEN];
    let mut ring_sum = 0f64;
    let mut z = 0f32;

    for (i, &s) in samples.iter().enumerate() {
        z = b * s + a * z;
        let sq = (z as f64) * (z as f64);
        let slot = i % FRAME_LEN;
        ring_sum += sq - ring[slot];
        ring[slot] = sq;

        // 居中窗 [center-half, center+half] 的样本在读到 center+half 时刚好凑齐
        if i >= half && (i - half).is_multiple_of(HOP) {
            let f = (i - half) / HOP;
            if f < n_frames {
                env[f] = (ring_sum.max(0.0) / FRAME_LEN as f64).sqrt() as f32;
            }
        }
    }

    let max = env.iter().cloned().fold(0f32, f32::max);
    if max > 1e-9 {
        for v in env.iter_mut() {
            *v /= max;
        }
    }
    env
}

/// 在拍网格上取每拍的低频强度（窗口内取最大值，容忍几十毫秒的相位抖动）
pub fn beat_strengths(env: &[f32], fps: f32, beats: &[f32]) -> Vec<f32> {
    if env.is_empty() || !fps.is_finite() || fps <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(beats.len());
    for &t in beats {
        let i0 = ((t - BEAT_WIN_PRE) * fps).floor().max(0.0) as usize;
        let i1 = ((t + BEAT_WIN_POST) * fps).floor().max(0.0) as usize + 1;
        let i1 = i1.min(env.len());
        if i1 > i0 {
            out.push(env[i0..i1].iter().cloned().fold(0f32, f32::max));
        } else {
            out.push(0.0);
        }
    }
    out
}

/// 拍号推断结果
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeterGuess {
    /// 每小节拍数：3 = 3/4，4 = 4/4
    pub beats_per_bar: i32,
    /// 第一个小节重拍相对于 `beats[0]` 的偏移（0 ~ bpb-1）
    pub phase: usize,
    /// 推断把握 0.0 ~ 1.0（样本不足时为 0）
    pub confidence: f32,
}

impl Default for MeterGuess {
    fn default() -> Self {
        Self {
            beats_per_bar: 4,
            phase: 0,
            confidence: 0.0,
        }
    }
}

/// 由每拍的低频强度推断拍号与重拍相位
pub fn detect_meter(strengths: &[f32]) -> MeterGuess {
    let mut guess = MeterGuess::default();
    if strengths.len() < METER_MIN_BEATS {
        return guess;
    }

    // 首拍的窗内样本不足（包络刚起步），统计时跳过，避免把相位 0 的均值压低
    let s = &strengths[1..];
    if relative_spread(s) < METER_MIN_VARIATION {
        // 每拍一样重：没有任何小节内证据，按 4/4 且把握为 0
        return guess;
    }

    let r2 = normalized_autocorr(s, 2);
    let r3 = normalized_autocorr(s, 3);
    let r4 = normalized_autocorr(s, 4);
    let even = r2.max(r4);

    // 三拍子的证据必须够强，才敢推翻「默认 4/4」：误报 3/4 比漏报更让人不信任
    let is_triple = r3 >= METER_R3_MIN && r3 >= even + METER_R3_MARGIN;
    let margin = if is_triple { r3 - even } else { even - r3 };

    guess.beats_per_bar = if is_triple { 3 } else { 4 };
    guess.confidence = ((margin - METER_R3_MARGIN) / 0.4).clamp(0.0, 1.0);
    guess.phase = detect_downbeat_phase(strengths, guess.beats_per_bar as usize);
    guess
}

/// 相对起伏：标准差 / 均值（变异系数）。均值为 0 时返回 0
fn relative_spread(x: &[f32]) -> f32 {
    let n = x.len();
    if n < 2 {
        return 0.0;
    }
    let mean = x.iter().sum::<f32>() / n as f32;
    if mean <= 1e-6 {
        return 0.0;
    }
    let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n as f32;
    var.sqrt() / mean
}

/// 归一化自相关（去均值后按方差归一）
fn normalized_autocorr(x: &[f32], lag: usize) -> f32 {
    let n = x.len();
    if lag == 0 || n <= lag + 2 {
        return 0.0;
    }
    let count = n - lag;
    let mean = x.iter().sum::<f32>() / n as f32;
    let mut num = 0f64;
    let mut e1 = 0f64;
    let mut e2 = 0f64;
    for i in 0..count {
        let a = (x[i] - mean) as f64;
        let b = (x[i + lag] - mean) as f64;
        num += a * b;
        e1 += a * a;
        e2 += b * b;
    }
    let den = (e1 * e2).sqrt();
    if den > 1e-12 {
        (num / den) as f32
    } else {
        0.0
    }
}

/// 小节重拍相位：把拍强度按 bpb 取模分组，平均最强的那一组就是重拍位置
///
/// 平局时取最靠前的相位（曲子通常从小节头开始）。注意 4/4 摇滚的 kick 在第 1、3 拍，
/// 两者一样强、只能缩到两个候选；four-on-floor 舞曲每拍都有底鼓，这里没有任何信息。
pub fn detect_downbeat_phase(strengths: &[f32], bpb: usize) -> usize {
    let bpb = bpb.max(1);
    if strengths.is_empty() {
        return 0;
    }
    let mut best_phase = 0usize;
    let mut best_avg = f32::NEG_INFINITY;
    for ph in 0..bpb {
        let mut sum = 0f32;
        let mut count = 0usize;
        for (i, &v) in strengths.iter().enumerate().skip(1) {
            if (i + bpb - ph).is_multiple_of(bpb) {
                sum += v;
                count += 1;
            }
        }
        if count == 0 {
            continue;
        }
        let avg = sum / count as f32;
        // 加一点死区：接近并列时保留更靠前的相位（4/4 摇滚的 kick 在第 1、3 拍，
        // 两者一样强，来回跳会让小节线闪）
        if avg > best_avg + PHASE_TIE_EPS {
            best_avg = avg;
            best_phase = ph;
        }
    }
    best_phase
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    /// 每 bpb 拍一个重音的强度序列
    fn pattern(bpb: usize, n: usize, strong: f32, weak: f32) -> Vec<f32> {
        (0..n)
            .map(|i| if i % bpb == 0 { strong } else { weak })
            .collect()
    }

    #[test]
    fn 三拍子模式被识别为三拍() {
        let g = detect_meter(&pattern(3, 33, 1.0, 0.2));
        assert_eq!(g.beats_per_bar, 3, "每 3 拍一个重音应判 3 拍");
        assert_eq!(g.phase, 0);
        assert!(g.confidence > 0.5, "置信度应偏高，实际 {}", g.confidence);
    }

    #[test]
    fn 四拍子模式不会被误判为三拍() {
        let g = detect_meter(&pattern(4, 33, 1.0, 0.2));
        assert_eq!(g.beats_per_bar, 4);
    }

    #[test]
    fn 两拍交替不会被硬凑成三拍() {
        // 4/4 摇滚：kick 1/3、snare 2/4，实际的周期是 2 拍
        let g = detect_meter(&pattern(2, 33, 1.0, 0.2));
        assert_eq!(g.beats_per_bar, 4, "周期 2 拍应归到 4/4");
    }

    #[test]
    fn 强拍不在第一拍时能找回相位() {
        // 4 拍一组，但序列从第 3 拍开始：真正的小节头在相对索引 2
        let base = pattern(4, 40, 1.0, 0.2);
        let s: Vec<f32> = base.iter().cycle().skip(2).take(36).cloned().collect();
        let g = detect_meter(&s);
        assert_eq!(g.beats_per_bar, 4);
        assert_eq!(g.phase, 2, "应把重拍定位到相对首拍的第 2 个位置");
    }

    #[test]
    fn 三拍子也能找回相位() {
        let base = pattern(3, 36, 1.0, 0.2);
        let s: Vec<f32> = base.iter().cycle().skip(1).take(33).cloned().collect();
        let g = detect_meter(&s);
        assert_eq!(g.beats_per_bar, 3);
        assert_eq!(g.phase, 2, "重音落在 idx 2、5、8…，相位应为 2");
    }

    #[test]
    fn 样本太少时按四四拍兜底() {
        let g = detect_meter(&pattern(3, 6, 1.0, 0.2));
        assert_eq!(g.beats_per_bar, 4);
        assert_eq!(g.confidence, 0.0);
        assert_eq!(g.phase, 0);
    }

    #[test]
    fn 平坦序列没有可信证据() {
        // 每拍一样重（例如 four-on-floor）：不该有任何把握说它是三拍子
        let g = detect_meter(&vec![0.8f32; 40]);
        assert_eq!(g.beats_per_bar, 4);
        assert!(g.confidence < 0.5, "平坦序列的把握应当很低，实际 {}", g.confidence);
    }

    #[test]
    fn 采样伪影不会被当成三拍子() {
        // 复现实测：每拍强度 0.78~0.81，剩下那点差别是同 3 拍的采样相位抖动。
        // 不做起伏门槛时 r(3) 会被放大到 0.42，误判成 3/4
        let s: Vec<f32> = (0..40)
            .map(|i| 0.79 + if i % 3 == 0 { 0.015 } else { 0.0 })
            .collect();
        let g = detect_meter(&s);
        assert_eq!(g.beats_per_bar, 4, "2% 的起伏不足以判三拍子");
        assert!(g.confidence < 0.1, "把握应为 0，实际 {}", g.confidence);
    }

    #[test]
    fn 相位近乎并列时取更靠前的() {
        // kick 在第 0、2 拍，第 2 拍因叠了踩镲略强一点点
        let s: Vec<f32> = (0..40)
            .map(|i| match i % 4 {
                0 => 0.80,
                2 => 0.805,
                _ => 0.10,
            })
            .collect();
        assert_eq!(
            detect_downbeat_phase(&s, 4),
            0,
            "0.005 的差距不该把相位推到后一个候选"
        );
    }

    #[test]
    fn 相位差异明显时选更强的() {
        let s: Vec<f32> = (0..40)
            .map(|i| match i % 4 {
                0 => 0.20,
                1 => 0.90,
                _ => 0.10,
            })
            .collect();
        assert_eq!(detect_downbeat_phase(&s, 4), 1);
    }

    #[test]
    fn 低频包络只跟随底鼓() {
        // 8 kHz 下放两个 60 Hz 衰减脉冲（0.1s / 0.6s），中间留空
        let sr = 8000u32;
        let n = sr as usize;
        let mut buf = vec![0f32; n];
        for &t0 in &[0.1f32, 0.6] {
            let i0 = (t0 * sr as f32) as usize;
            for k in 0..(0.12 * sr as f32) as usize {
                if i0 + k < n {
                    let tt = k as f32 / sr as f32;
                    buf[i0 + k] += (-tt * 40.0).exp() * (2.0 * std::f32::consts::PI * 60.0 * tt).sin();
                }
            }
        }
        let env = low_band_envelope(&buf, sr);
        let fps = sr as f32 / HOP as f32;
        let at = |t: f32| env[((t * fps) as usize).min(env.len() - 1)];
        assert!(
            at(0.1) > at(0.35) * 3.0,
            "底鼓处应明显高于间隙：{} vs {}",
            at(0.1),
            at(0.35)
        );
    }
}

#[cfg(test)]
mod half_time_tests {
    use super::*;

    /// 半速陷阱判定：用实测过的数值做回归
    #[test]
    fn 强弱交替会被判定为半速陷阱() {
        // Creep.flac 实测：被判 62.5，comb(62.5)=0.5775（慢）、comb(125)=0.4342（快）
        assert!(
            should_double_for_half_time(62.5, 0.5775, 0.4342),
            "真 125 BPM 的强弱交替素材应翻倍"
        );
    }

    /// 真正 62.5 BPM 的慢曲：半拍处几乎没有 onset，比值极低，不能翻倍
    #[test]
    fn 真正的慢曲不会被翻倍() {
        // 合成 62.5 BPM 等强鼓点实测：comb(62.5)=0.9262、comb(125)=0.0682
        assert!(
            !should_double_for_half_time(62.5, 0.9262, 0.0682),
            "真 62.5 BPM 不应被翻成 125"
        );
    }

    /// 只在低于阈值时才受理，避免把正常速度的曲子推快
    #[test]
    fn 高于阈值不触发() {
        assert!(!should_double_for_half_time(70.0, 1.0, 1.0));
        assert!(!should_double_for_half_time(80.0, 0.9, 0.89));
        // 边界内侧仍受理
        assert!(should_double_for_half_time(69.9, 1.0, 0.7));
    }

    /// 慢周期分数异常（接近 0）时不做除法判断，直接维持原判
    #[test]
    fn 分数异常时不翻倍() {
        assert!(!should_double_for_half_time(62.5, 0.0, 1.0));
        assert!(!should_double_for_half_time(62.5, 1e-9, 1.0));
    }
}
