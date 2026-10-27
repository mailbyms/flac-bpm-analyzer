//! FLAC 解码：symphonia → 单声道 f32 分析流 + 原始交错 PCM（用于播放）
//!
//! 关键点：
//! 1. 只注册 FLAC 解码器/格式，避免无关依赖；
//! 2. 解码循环中同时做「多声道混音」与「原始交错 PCM 保留（播放用）」；
//! 3. 用一个带字节计数的 MediaSource 包装器汇报真实进度。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use symphonia::core::audio::{Channels, SampleBuffer};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::analysis::dsp::{resample_linear, TARGET_ANALYSIS_SR};
use crate::analysis::types::{AnalysisError, FileMeta};

/// 用于播放的原始 PCM（保持原始采样率与声道布局，交错存放）
pub struct PlaybackBuffer {
    pub interleaved: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// 解码产物
pub struct DecodedAudio {
    pub meta: FileMeta,
    /// 单声道、已降到分析采样率的分析样本
    pub mono: Vec<f32>,
    /// mono 对应的采样率
    pub analysis_sr: u32,
    /// 播放用原始 PCM
    pub playback: PlaybackBuffer,
}

/// 会统计已读取字节数的 MediaSource 包装（用于进度条）
struct ProgressSource {
    inner: File,
    len: Option<u64>,
    read_bytes: Arc<AtomicU64>,
    pos: u64,
}

impl Read for ProgressSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        self.read_bytes.store(self.pos, Ordering::Relaxed);
        Ok(n)
    }
}

impl Seek for ProgressSource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let p = self.inner.seek(pos)?;
        self.pos = p;
        self.read_bytes.store(p, Ordering::Relaxed);
        Ok(p)
    }
}

impl MediaSource for ProgressSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.len
    }
}

/// 打开并解码 FLAC 文件。
/// `progress` 回调接收 0.0~1.0 的进度值。
pub fn decode_flac(path: &Path, mut progress: impl FnMut(f32)) -> Result<DecodedAudio, AnalysisError> {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let file = File::open(path)?;

    let read_bytes = Arc::new(AtomicU64::new(0));
    let source = ProgressSource {
        inner: file,
        len: Some(file_len),
        read_bytes: Arc::clone(&read_bytes),
        pos: 0,
    };
    let mss = MediaSourceStream::new(Box::new(source), Default::default());

    // 提示扩展名，帮助 probe 快速识别
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions {
            enable_gapless: true,
            ..Default::default()
        },
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;

    // 先取出需要的元信息（克隆 CodecParameters，避免后续借用冲突）
    let (track_id, codec_params) = {
        let track = format
            .default_track()
            .ok_or_else(|| AnalysisError::Decode("找不到可用的音轨".into()))?;
        (track.id, track.codec_params.clone())
    };

    let params = &codec_params;
    let src_sr = params.sample_rate.unwrap_or(44_100);
    let channels = params
        .channels
        .map(|c: Channels| c.count() as u16)
        .unwrap_or(2);
    let bits = params
        .bits_per_sample
        .or(params.bits_per_coded_sample)
        .unwrap_or(16) as u32;
    let n_frames = params.n_frames.unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs().make(params, &DecoderOptions::default())?;

    // 估算总样本数，用于「解码结束后」的重采样/包络阶段划分进度
    let estimated_frames = if n_frames > 0 {
        Some(n_frames)
    } else if src_sr > 0 && file_len > 0 {
        // 兜底：按 FLAC 平均压缩率猜测（约 60%）
        Some((file_len as u64 * 60 / 100) / (channels as u64 * (bits as u64 / 8)))
    } else {
        None
    };

    let mut interleaved: Vec<f32> = Vec::new();
    let mut mono_at_src: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut spec_channels = 0usize;
    let mut decoded_frames = 0u64;

    loop {
        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }
                let decoded = decoder.decode(&packet);
                let decoded = match decoded {
                    Ok(d) => d,
                    // 单帧损坏时跳过，避免整曲失败
                    Err(SymphoniaError::DecodeError(_)) => continue,
                    Err(e) => return Err(AnalysisError::Decode(e.to_string())),
                };

                let spec = *decoded.spec();
                let ch = spec.channels.count();
                if sample_buf.is_none() || ch != spec_channels {
                    spec_channels = ch;
                    sample_buf = Some(SampleBuffer::<f32>::new(
                        decoded.capacity() as u64,
                        spec,
                    ));
                }
                let buf = sample_buf.as_mut().unwrap();
                buf.copy_interleaved_ref(decoded);
                let samples = buf.samples();

                // 交错 PCM 原样保留（播放用，保持原始分辨率）
                interleaved.extend_from_slice(samples);
                // 同时混音成单声道
                let frames = samples.len() / ch.max(1);
                let inv = 1.0f32 / ch.max(1) as f32;
                for f in 0..frames {
                    let base = f * ch;
                    let mut sum = 0f32;
                    for c in 0..ch {
                        sum += samples[base + c];
                    }
                    mono_at_src.push(sum * inv);
                }
                decoded_frames += frames as u64;

                progress(progress_value(decoded_frames, estimated_frames, file_len, &read_bytes));
            }
            // EOF
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => continue,
            Err(_) => break,
        }
    }

    if mono_at_src.is_empty() {
        return Err(AnalysisError::Other("没有解码出任何音频数据".into()));
    }

    // ---- 降采样到分析采样率（线性插值） ----
    progress(0.88);
    let mono = if src_sr != TARGET_ANALYSIS_SR {
        resample_linear(&mono_at_src, src_sr, TARGET_ANALYSIS_SR)
    } else {
        mono_at_src
    };

    let duration = mono.len() as f32 / TARGET_ANALYSIS_SR as f32;

    Ok(DecodedAudio {
        meta: FileMeta {
            duration,
            sample_rate: src_sr,
            bits_per_sample: bits,
            channels,
        },
        mono,
        analysis_sr: TARGET_ANALYSIS_SR,
        playback: PlaybackBuffer {
            interleaved,
            sample_rate: src_sr,
            channels,
        },
    })
}

/// 综合两种进度来源：优先用已知总帧数，否则用已读字节数 / 文件大小。
fn progress_value(
    decoded_frames: u64,
    estimated_frames: Option<u64>,
    file_len: u64,
    read_bytes: &Arc<AtomicU64>,
) -> f32 {
    if let Some(total) = estimated_frames {
        if total > 0 {
            return (decoded_frames as f32 / total as f32).clamp(0.0, 0.85);
        }
    }
    if file_len > 0 {
        let bytes = read_bytes.load(Ordering::Relaxed) as f32;
        return (bytes / file_len as f32).clamp(0.0, 0.85);
    }
    0.0
}
