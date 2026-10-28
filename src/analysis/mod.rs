//! 分析管线总控：把解码 + DSP 放到后台线程，结果通过 std channel 回传 UI

pub mod decode;
pub mod dsp;
pub mod types;

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::thread::{self, JoinHandle};

use types::{AnalysisError, AnalysisEvent, AnalysisResult};

use self::decode::{decode_flac, DecodedAudio};
use self::dsp::{
    compute_waveform_peaks, estimate_tempo, generate_beats, onset_envelope, WAVEFORM_BUCKETS,
};

/// 后台分析任务的句柄：UI 只需不断 `poll()` 收事件
pub struct AnalysisHandle {
    rx: Receiver<AnalysisEvent>,
    _worker: JoinHandle<()>,
    finished: bool,
}

impl AnalysisHandle {
    /// 取出当前所有待处理事件（非阻塞）
    pub fn poll(&mut self) -> Vec<AnalysisEvent> {
        let mut events = Vec::new();
        while let Ok(evt) = self.rx.try_recv() {
            match evt {
                AnalysisEvent::Progress { .. } => {
                    // 同一帧里只保留最后一次进度更新
                    if let Some(AnalysisEvent::Progress { .. }) = events.last() {
                        events.pop();
                    }
                    events.push(evt);
                }
                AnalysisEvent::Finished { .. } | AnalysisEvent::Failed(_) => {
                    self.finished = true;
                    events.push(evt);
                }
            }
        }
        events
    }

}

/// 启动后台分析（立刻返回句柄）
pub fn start_analysis(path: PathBuf) -> AnalysisHandle {
    let (tx, rx) = channel::<AnalysisEvent>();

    let worker = thread::spawn(move || {
        let result = run_pipeline(&path, &tx);
        match result {
            Ok(out) => {
                let _ = tx.send(AnalysisEvent::Finished {
                    result: Box::new(out.result),
                    pcm: out.pcm,
                });
            }
            Err(e) => {
                let _ = tx.send(AnalysisEvent::Failed(e.to_string()));
            }
        }
    });

    AnalysisHandle {
        rx,
        _worker: worker,
        finished: false,
    }
}

fn send(tx: &std::sync::mpsc::Sender<AnalysisEvent>, value: f32, label: &str) {
    let _ = tx.send(AnalysisEvent::Progress {
        value,
        label: label.to_string(),
    });
}

/// 分析管线的返回值：结果 + 用于播放的原始 PCM
struct PipelineOutput {
    result: AnalysisResult,
    pcm: std::sync::Arc<Vec<f32>>,
}

fn run_pipeline(
    path: &PathBuf,
    tx: &std::sync::mpsc::Sender<AnalysisEvent>,
) -> Result<PipelineOutput, AnalysisError> {
    send(tx, 0.02, "正在打开 FLAC 文件…");

    // ---- 1) 解码 + 混音 + 降采样 ----
    // 阶段权重按真实耗时分配：解码+降采样是最重的一段，给它 3%~58%
    let mut last_reported = -1.0f32;
    let DecodedAudio {
        meta,
        mono,
        analysis_sr,
        playback,
    } = decode_flac(path, |p| {
        // decode_flac 内部约定：解码循环 0~0.85，降采样 0.85~1.0
        let mapped = 0.03 + p * 0.55;
        let label = if p < 0.86 {
            "正在解码音频…"
        } else {
            "正在降采样…"
        };
        if (mapped - last_reported).abs() > 0.01 {
            last_reported = mapped;
            send(tx, mapped, label);
        }
    })?;

    let duration = meta.duration.max(mono.len() as f32 / analysis_sr as f32);

    // ---- 2) 波形峰值（一次遍历聚合，后续绘图不再触碰原始 PCM） ----
    send(tx, 0.62, "正在生成波形…");
    let waveform_peaks = compute_waveform_peaks(&mono, WAVEFORM_BUCKETS);

    // ---- 3) onset 包络 ----
    send(tx, 0.74, "正在计算能量包络…");
    let (novelty, fps) = onset_envelope(&mono, analysis_sr);
    if novelty.len() < 32 {
        return Err(AnalysisError::Other("音频太短，无法进行节拍分析".into()));
    }

    // ---- 4) 自相关测速 + 相位对齐 + 置信度 ----
    send(tx, 0.86, "正在估计 BPM…");
    let tempo = estimate_tempo(&novelty, fps);

    // ---- 5) 生成节拍序列 ----
    send(tx, 0.95, "正在生成节拍表…");
    let beats = generate_beats(tempo.offset_sec, tempo.period_sec, duration);

    send(tx, 1.0, "完成");
    Ok(PipelineOutput {
        result: AnalysisResult {
            bpm: tempo.bpm,
            confidence: tempo.confidence,
            first_beat_offset: tempo.offset_sec,
            beats,
            duration,
            sample_rate: meta.sample_rate.max(playback.sample_rate),
            bits_per_sample: meta.bits_per_sample,
            channels: meta.channels.max(playback.channels),
            waveform_peaks,
            beats_per_bar: 4,
        },
        pcm: std::sync::Arc::new(playback.interleaved),
    })
}
