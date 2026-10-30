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
    /// 每小节拍数：3 = 3/4，4 = 4/4，2 = 2/4
    pub beats_per_bar: i32,
    /// 第一个小节重拍的位置：`beats[downbeat_phase]` 是第一个强拍，之后每 `beats_per_bar` 拍一个
    pub downbeat_phase: usize,
    /// 拍号推断的把握 0.0 ~ 1.0（0 表示样本太少、没做推断）
    pub meter_confidence: f32,
}

impl AnalysisResult {
    /// 每小节拍数（至少 1，防止脏数据导致除零）
    pub fn bpb(&self) -> usize {
        self.beats_per_bar.max(1) as usize
    }

    /// 第 `idx` 拍是否是小节重拍
    // 只有界面（波形小节线 / CSV 导出）读这两个方法，纯命令行构建里没人调
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn is_downbeat(&self, idx: usize) -> bool {
        let bpb = self.bpb();
        (idx + bpb - self.downbeat_phase % bpb).is_multiple_of(bpb)
    }

    /// 第 `idx` 拍的小节位置：返回 (小节号, 小节内第几拍)，均为 1 起。
    ///
    /// 第一个重拍之前不足一小节的部分算「第 0 小节」，调用方可以显示成「前导」。
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub fn bar_position(&self, idx: usize) -> (i64, usize) {
        bar_position_of(idx, self.bpb(), self.downbeat_phase)
    }

    /// 拍号的显示文本，例如 `4 / 4`
    pub fn meter_label(&self) -> String {
        format!("{} / 4", self.bpb())
    }
}

/// 纯函数版的小节位置计算（便于直接测边界）
///
/// `phase` 是第一个小节重拍的索引。注意不能用 `idx % bpb` 硬算：
/// 重拍不在第 0 拍时，小节头会整体错位。
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
pub fn bar_position_of(idx: usize, bpb: usize, phase: usize) -> (i64, usize) {
    let bpb = bpb.max(1);
    let phase = (phase % bpb) as i64;
    let rel = idx as i64 - phase;
    let bar = rel.div_euclid(bpb as i64);
    let beat = rel.rem_euclid(bpb as i64) as usize;
    (bar + 1, beat + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 重拍在第一拍时按四拍分小节() {
        assert_eq!(bar_position_of(0, 4, 0), (1, 1));
        assert_eq!(bar_position_of(3, 4, 0), (1, 4));
        assert_eq!(bar_position_of(4, 4, 0), (2, 1));
        assert_eq!(bar_position_of(7, 4, 0), (2, 4));
    }

    #[test]
    fn 重拍错开时小节头跟着后移() {
        // 首个重拍在第 3 个位置：idx 0、1 属于「第 0 小节」的尾巴
        assert_eq!(bar_position_of(0, 4, 2), (0, 3));
        assert_eq!(bar_position_of(1, 4, 2), (0, 4));
        assert_eq!(bar_position_of(2, 4, 2), (1, 1));
        assert_eq!(bar_position_of(5, 4, 2), (1, 4));
        assert_eq!(bar_position_of(6, 4, 2), (2, 1));
    }

    #[test]
    fn 三拍子的小节划分() {
        assert_eq!(bar_position_of(1, 3, 1), (1, 1));
        assert_eq!(bar_position_of(3, 3, 1), (1, 3));
        assert_eq!(bar_position_of(4, 3, 1), (2, 1));
        assert_eq!(bar_position_of(0, 3, 1), (0, 3));
    }

    #[test]
    fn 脏数据不会除零() {
        // bpb = 0 应被夹到 1，phase 取模后落在合法范围
        assert_eq!(bar_position_of(5, 0, 7), (6, 1));
    }
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
