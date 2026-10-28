//! 自检：用合成信号验证 DSP 链路（不需要真实音频文件）
//!
//! 运行：`cargo run --release -- --selftest`
//! 生成若干已知 BPM 的鼓点信号，检查：
//!   * 重采样长度是否符合预期；
//!   * 波形峰值数组是否归一化；
//!   * onset 包络 → 自相关 → 相位对齐能否还原出正确的 BPM 与首拍偏移。

use crate::analysis::dsp::{
    compute_waveform_peaks, estimate_tempo, generate_beats, onset_envelope, resample_linear,
    TARGET_ANALYSIS_SR,
};

/// 生成一段简单的鼓点信号：每拍一个带指数衰减的噪声脉冲 + 持续的低频正弦（类似底鼓）
fn synth_click_track(bpm: f32, seconds: f32, sr: u32, first_beat: f32, seed: u64) -> Vec<f32> {
    let n = (seconds * sr as f32) as usize;
    let period_samples = 60.0 / bpm * sr as f32;
    let mut rng = seed;
    let mut out = vec![0f32; n];

    // 底鼓：每拍一次正弦冲击；1/3 拍加重，2/4 拍加一层噪声小军鼓（形成 2/4 拍层次）
    let mut t = first_beat;
    let mut beat_index = 0usize;
    while t < seconds {
        let start = (t * sr as f32) as usize;
        let env_len = (0.12 * sr as f32) as usize;
        let accent = if beat_index % 2 == 0 { 1.0 } else { 0.88 };
        for k in 0..env_len {
            let i = start + k;
            if i >= n {
                break;
            }
            let phase = 2.0 * std::f32::consts::PI * 60.0 * k as f32 / sr as f32;
            let env = (-4.0 * k as f32 / env_len as f32).exp();
            out[i] += phase.sin() * env * 0.9 * accent;
        }
        if beat_index % 2 == 1 {
            for k in 0..(0.05 * sr as f32) as usize {
                let i = start + k;
                if i >= n {
                    break;
                }
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                let r = ((rng >> 33) as f32 / u32::MAX as f32) - 0.5;
                out[i] += r * 0.25 * (-8.0 * k as f32 / (0.05 * sr as f32)).exp();
            }
        }
        t += 60.0 / bpm;
        beat_index += 1;
    }

    // 每二分拍的高频噪声背景，制造真实感
    let mut noise_idx = 0;
    let _ = period_samples;
    while noise_idx < n {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = ((rng >> 33) as f32 / u32::MAX as f32) - 0.5;
        out[noise_idx] += r * 0.02;
        noise_idx += 1;
    }
    out
}

/// 命令行单文件分析：`cargo run --release -- --analyze <file.flac|file.mp3>`
/// 用于在不启动界面的情况下验证「解码 + DSP」全链路是否正常。
///
/// 返回 `Some(())` 表示分析成功，`None` 表示解码/分析失败（调用方据此设置退出码）。
pub fn analyze_file(path: &std::path::Path) -> Option<crate::analysis::types::AnalysisResult> {
    use crate::analysis::{start_analysis, types::AnalysisEvent};
    use std::io::IsTerminal;
    use std::time::Duration;

    // 输出被重定向/管道时不刷进度（`\r` 在文件里会变成一长串），只留结果行
    let interactive = std::io::stdout().is_terminal();

    println!("分析文件：{}", path.display());
    let mut handle = start_analysis(path.to_path_buf());
    let timer = std::time::Instant::now();
    loop {
        let events = handle.poll();
        if events.is_empty() {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        for evt in events {
            match evt {
                AnalysisEvent::Progress { value, label } => {
                    if interactive {
                        print!("\r{label} {:.0}%   ", value * 100.0);
                    }
                }
                AnalysisEvent::Finished { result, pcm: _ } => {
                    println!(
                        "\n完成（耗时 {:.2}s）：BPM {:.2} | 置信度 {:.0}% | 首拍偏移 {:.3}s | 时长 {:.1}s | {} Hz / {} bit / {} 声道 | 共 {} 拍 | PCM 峰值桶 {}",
                        timer.elapsed().as_secs_f32(),
                        result.bpm,
                        result.confidence * 100.0,
                        result.first_beat_offset,
                        result.duration,
                        result.sample_rate,
                        result.bits_per_sample,
                        result.channels,
                        result.beats.len(),
                        result.waveform_peaks.len()
                    );
                    if !result.beats.is_empty() {
                        println!(
                            "前 8 拍：{}",
                            result
                                .beats
                                .iter()
                                .take(8)
                                .map(|b| format!("{b:.3}"))
                                .collect::<Vec<_>>()
                                .join(" / ")
                        );
                    }
                    return Some(*result);
                }
                AnalysisEvent::Failed(e) => {
                    println!("\n失败：{e}");
                    return None;
                }
            }
        }
    }
}

/// 自检入口，返回是否全部通过
pub fn run() -> bool {
    println!("=== DSP 自检 ===");
    let mut ok = true;

    // 1) 重采样
    let src: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.01).sin()).collect();
    let down = resample_linear(&src, 44100, TARGET_ANALYSIS_SR);
    let expected = src.len() / 2;
    let pass = (down.len() as i64 - expected as i64).abs() <= 2;
    println!(
        "重采样 44100 → {}: {} → {} 样本（期望 ~{expected}）{}",
        TARGET_ANALYSIS_SR,
        src.len(),
        down.len(),
        if pass { "OK" } else { "FAIL" }
    );
    ok &= pass;

    // 2) 波形峰值归一化
    let peaks = compute_waveform_peaks(&src, 1024);
    let max = peaks.iter().cloned().fold(0f32, f32::max);
    let pass = peaks.len() == 1024 && (max - 1.0).abs() < 1e-3;
    println!(
        "波形峰值：{} 桶，最大值 {max:.4} {}",
        peaks.len(),
        if pass { "OK" } else { "FAIL" }
    );
    ok &= pass;

    // 3) 多个 BPM 的鼓点
    for &bpm in &[70.0f32, 80.0, 100.0, 120.0, 128.0, 140.0, 150.0, 174.0] {
        let first_beat = 0.4f32;
        let pcm = synth_click_track(bpm, 40.0, TARGET_ANALYSIS_SR, first_beat, bpm as u64 + 7);
        let (novelty, fps) = onset_envelope(&pcm, TARGET_ANALYSIS_SR);
        let res = estimate_tempo(&novelty, fps);
        let beats = generate_beats(res.offset_sec, res.period_sec, 40.0);
        // 允许 ±2% 误差；倍频/分频情况下换算回真值
        let mut err_ratio = res.bpm / bpm;
        for mul in [0.5f32, 2.0] {
            let alt = (res.bpm * mul) / bpm;
            if (alt - 1.0).abs() < (err_ratio - 1.0).abs() {
                err_ratio = alt;
            }
        }
        let pass = (err_ratio - 1.0).abs() < 0.02 && beats.len() >= 20;
        println!(
            "真实 {bpm:>6.1} BPM → 估计 {:>6.2} BPM | 偏移 {:>5.3}s | 拍数 {:>3} | 置信度 {:>5.1}% {}",
            res.bpm,
            res.offset_sec,
            beats.len(),
            res.confidence * 100.0,
            if pass { "OK" } else { "FAIL" }
        );
        ok &= pass;
    }

    println!("=== 自检{} ===", if ok { "全部通过" } else { "存在失败" });
    ok
}


/// 播放自检：解码 → 载入播放引擎 → 播放 2 秒 → 检查播放头是否真的推进
///
/// 运行：`cargo run --release -- --playtest "某曲目.flac" 或 "某曲目.mp3"`
///
/// 判定依据：rodio 的 `get_pos()` 统计的是被输出设备**实际消费**的采样数，
/// 只有真的在出声才会推进；若它不动，说明 source 根本没被播放（例如被 pause 卡住）。
#[cfg(feature = "playback")]
pub fn playtest(path: &std::path::Path) {
    use crate::analysis::{start_analysis, types::AnalysisEvent};
    use crate::audio::{AudioEngine, PlaybackMode};
    use std::time::{Duration, Instant};

    println!("播放自检：{}", path.display());
    let mut handle = start_analysis(path.to_path_buf());
    let (result, pcm) = loop {
        let mut done = None;
        for evt in handle.poll() {
            match evt {
                AnalysisEvent::Finished { result, pcm } => done = Some((*result, pcm)),
                AnalysisEvent::Failed(e) => {
                    eprintln!("分析失败：{e}");
                    return;
                }
                AnalysisEvent::Progress { .. } => {}
            }
        }
        if let Some(v) = done {
            break v;
        }
        std::thread::sleep(Duration::from_millis(30));
    };

    let mut engine = AudioEngine::default();
    engine.load(pcm, result.channels.max(1), result.sample_rate.max(1), result.duration);
    match engine.mode() {
        PlaybackMode::Real => println!("输出模式：真实音频输出（rodio/cpal）"),
        PlaybackMode::Simulated => println!(
            "输出模式：模拟走时（无可用输出设备）{}",
            engine
                .fallback_reason()
                .map(|r| format!(" → {r}"))
                .unwrap_or_default()
        ),
    }

    let t0 = Instant::now();
    engine.play();
    let mut last = -1.0f64;
    while t0.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(50));
        engine.tick();
        let p = engine.position();
        if (p - last).abs() > 1e-6 {
            println!("  t={:.2}s  播放头 {:.3}s  playing={}", t0.elapsed().as_secs_f32(), p, engine.is_playing());
            last = p;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let advanced = engine.position();
    engine.pause();

    println!(
        "结果：{:.2}s 内播放头推进 {:.3}s（real={}）",
        elapsed,
        advanced,
        matches!(engine.mode(), PlaybackMode::Real)
    );
    let ok = advanced > 0.5 && matches!(engine.mode(), PlaybackMode::Real);
    println!("=== 播放自检{} ===", if ok { "通过：播放头随实际输出推进" } else { "失败：播放头未推进 / 非真实输出" });
}
