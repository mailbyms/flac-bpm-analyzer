//! 分析模块的数据结构定义

/// 音频文件的基础元信息（解码时读取）
#[derive(Clone, Debug)]
pub struct FileMeta {
    /// 时长（秒）
    pub duration: f32,
    /// 原始采样率
    pub sample_rate: u32,
    /// 位深（FLAC 一般为 16 / 24）
    pub bits_per_sample: u32,
    /// 声道数
    pub channels: u16,
}

/// 分析结果（后台线程产出，通过 channel 回传给 UI）
#[derive(Clone, Debug)]
pub struct AnalysisResult {
    /// 估计 BPM
    pub bpm: f32,
    /// 置信度 0.0 ~ 1.0
    pub confidence: f32,
    /// 首拍偏移（秒）
    pub first_beat_offset: f32,
    /// 全部节拍时间点（秒，递增）
    pub beats: Vec<f32>,
    /// 音频时长（秒）
    pub duration: f32,
    /// 文件原始采样率
    pub sample_rate: u32,
    /// 位深
    pub bits_per_sample: u32,
    /// 声道数
    pub channels: u16,
    /// 用于绘制波形的归一化峰值数组（0.0 ~ 1.0，长度固定）
    pub waveform_peaks: Vec<f32>,
    /// 每小节拍数（当前固定 4/4）
    // 只被 CSV 导出（gui feature）读取；纯命令行构建里没人用，别为它报警告
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub beats_per_bar: i32,
}

/// 后台线程向 UI 发送的事件
pub enum AnalysisEvent {
    /// 进度更新：0.0 ~ 1.0 与阶段文案
    Progress { value: f32, label: String },
    /// 分析完成：结果 + 用于播放的原始交错 PCM（共享所有权，零拷贝交给音频引擎）
    Finished {
        result: Box<AnalysisResult>,
        // 只有播放链路会消费 PCM；关掉 playback 后命令行分析不需要它
        #[cfg_attr(not(feature = "playback"), allow(dead_code))]
        pcm: std::sync::Arc<Vec<f32>>,
    },
    /// 失败原因
    Failed(String),
}

/// 统一错误类型
#[derive(Debug)]
pub enum AnalysisError {
    Io(String),
    Decode(String),
    Other(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::Io(s) => write!(f, "文件读取失败：{s}"),
            AnalysisError::Decode(s) => write!(f, "解码失败：{s}"),
            AnalysisError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl From<std::io::Error> for AnalysisError {
    fn from(e: std::io::Error) -> Self {
        AnalysisError::Io(e.to_string())
    }
}

impl From<symphonia::core::errors::Error> for AnalysisError {
    fn from(e: symphonia::core::errors::Error) -> Self {
        AnalysisError::Decode(e.to_string())
    }
}
