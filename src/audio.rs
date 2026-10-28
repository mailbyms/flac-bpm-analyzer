//! 播放控制：基于 cpal 的真实输出（rodio），失败时自动降级为「模拟走时」
//!
//! 之所以用一个自定义 Source（SliceSource）而不是 SamplesBuffer：
//! 直接从 Arc<Vec<f32>> 切片播放可以做到零拷贝定位（拖动播放头 / 跳转节拍时不会复制几十 MB）。
//!
//! ## 无播放后端（`--no-default-features`）
//!
//! rodio 会经 cpal 链接系统音频库（Linux 是 ALSA），纯命令行/服务器环境往往没有开发包。
//! 关掉 `playback` feature 后本模块仍然编译，只是 `real::Real::open()` 恒失败，
//! 引擎固定走「模拟走时」——分析、波形、导出都不受影响，只是不出声。

use std::sync::Arc;
use std::time::Instant;

/// 真实输出后端。只有开启 `playback` feature 时才把 rodio 拉进来。
#[cfg(feature = "playback")]
mod real {
    use super::*;
    use rodio::{ChannelCount, DeviceSinkBuilder, MixerDeviceSink, Player, SampleRate, Source};
    use std::time::Duration;

    /// 从共享 PCM 的任意采样点开始播放的零拷贝 Source
    struct SliceSource {
        data: Arc<Vec<f32>>,
        pos: usize,
        channels: ChannelCount,
        sample_rate: SampleRate,
    }

    impl Iterator for SliceSource {
        type Item = f32;

        fn next(&mut self) -> Option<f32> {
            if self.pos >= self.data.len() {
                return None;
            }
            let v = self.data[self.pos];
            self.pos += 1;
            Some(v)
        }
    }

    impl Source for SliceSource {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> ChannelCount {
            self.channels
        }
        fn sample_rate(&self) -> SampleRate {
            self.sample_rate
        }
        fn total_duration(&self) -> Option<Duration> {
            let frames = (self.data.len() - self.pos) as f64 / self.channels.get() as f64;
            Some(Duration::from_secs_f64(frames / self.sample_rate.get() as f64))
        }
    }

    /// 真实音频输出（rodio / cpal）
    pub struct Real {
        /// rodio 输出设备句柄，必须长期持有否则会被释放
        _sink: MixerDeviceSink,
        player: Player,
    }

    impl Real {
        pub fn open() -> Result<Self, String> {
            match DeviceSinkBuilder::open_default_sink() {
                Ok(mut sink) => {
                    // 程序退出时 rodio 会打一行 "Dropping DeviceSink" 提示，此处属于正常释放
                    sink.log_on_drop(false);
                    let player = Player::connect_new(sink.mixer());
                    Ok(Self {
                        _sink: sink,
                        player,
                    })
                }
                Err(e) => Err(e.to_string()),
            }
        }

        /// 从指定帧开始播放（零拷贝）
        pub fn start_from(
            &self,
            data: Arc<Vec<f32>>,
            start_frame: usize,
            channels: u16,
            sample_rate: u32,
        ) {
            let chan = ChannelCount::new(channels).unwrap_or(ChannelCount::new(1).unwrap());
            let sr = SampleRate::new(sample_rate).unwrap_or(SampleRate::new(44100).unwrap());

            self.player.clear();
            self.player.append(SliceSource {
                data,
                pos: start_frame,
                channels: chan,
                sample_rate: sr,
            });
            // 关键：rodio 的 `Player::clear()` 内部最后会调用 `self.pause()`（见 player.rs），
            // 而 `append()` 不会自动恢复播放。若不显式 play()，source 只是入队却一直处于
            // 暂停状态 —— 表现就是「点了播放、界面显示 playing，但既没声音播放头也不走」。
            self.player.play();
        }

        pub fn clear(&self) {
            self.player.clear();
            self.player.pause();
        }

        pub fn pause(&self) {
            self.player.pause();
        }

        /// 相对当前 source 起点的已播放秒数
        pub fn position_secs(&self) -> f64 {
            self.player.get_pos().as_secs_f64()
        }

        pub fn is_empty(&self) -> bool {
            self.player.len() == 0
        }
    }
}

/// 未编译音频输出时的空后端：`open()` 恒失败，引擎固定走「模拟走时」。
#[cfg(not(feature = "playback"))]
mod real {
    use super::*;

    pub const NOT_COMPILED: &str = "本构建未包含音频输出（playback feature 已关闭），仅模拟走时";

    pub struct Real;

    // 签名与真实后端保持一致，但关闭 playback 时不会被调用（open 恒失败）
    #[allow(dead_code)]
    impl Real {
        pub fn open() -> Result<Self, String> {
            Err(NOT_COMPILED.to_string())
        }
        pub fn start_from(
            &self,
            _data: Arc<Vec<f32>>,
            _start_frame: usize,
            _channels: u16,
            _sample_rate: u32,
        ) {
        }
        pub fn clear(&self) {}
        pub fn pause(&self) {}
        pub fn position_secs(&self) -> f64 {
            0.0
        }
        pub fn is_empty(&self) -> bool {
            true
        }
    }
}

/// 本构建是否编入了音频输出后端（用于界面文案）
pub fn playback_available() -> bool {
    cfg!(feature = "playback")
}

/// 播放模式
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackMode {
    /// 真实音频输出（rodio / cpal）
    Real,
    /// 降级：仅按秒推进播放头
    Simulated,
}

pub struct AudioEngine {
    /// 真实输出后端；`None` = 尚未创建或创建失败（走模拟走时）
    real: Option<real::Real>,
    pcm: Option<Arc<Vec<f32>>>,
    sample_rate: u32,
    channels: u16,
    duration: f32,
    playing: bool,
    /// 当前位置（秒）
    position: f64,
    /// 当前正在播放的 source 对应的起始时间（真实模式下 get_pos 是相对该起点的）
    base_offset: f64,
    /// source 是否需要重建
    dirty: bool,
    mode: PlaybackMode,
    /// 降级原因（用于状态栏提示）
    fallback_reason: Option<String>,
    sim_anchor: Option<(Instant, f64)>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self {
            real: None,
            pcm: None,
            sample_rate: 44100,
            channels: 2,
            duration: 0.0,
            playing: false,
            position: 0.0,
            base_offset: 0.0,
            dirty: true,
            mode: PlaybackMode::Simulated,
            fallback_reason: None,
            sim_anchor: None,
        }
    }
}

impl AudioEngine {
    /// 载入新的 PCM（替换当前曲目）
    pub fn load(&mut self, pcm: Arc<Vec<f32>>, channels: u16, sample_rate: u32, duration: f32) {
        self.stop();
        self.pcm = Some(pcm);
        self.channels = channels.max(1);
        self.sample_rate = sample_rate.max(1);
        self.duration = duration.max(0.0);
        self.position = 0.0;
        self.base_offset = 0.0;
        self.dirty = true;
        self.sim_anchor = None;
        // 每次换曲时都尝试重新初始化设备（可能上次是降级状态）
        self.fallback_reason = None;
        self.mode = if self.ensure_device() {
            PlaybackMode::Real
        } else {
            PlaybackMode::Simulated
        };
    }

    /// 惰性创建输出设备；失败则记录降级原因
    fn ensure_device(&mut self) -> bool {
        if self.real.is_some() {
            return true;
        }
        match real::Real::open() {
            Ok(backend) => {
                self.real = Some(backend);
                true
            }
            Err(e) => {
                self.fallback_reason = Some(format!("音频输出初始化失败，已降级为模拟走时：{e}"));
                false
            }
        }
    }

    pub fn mode(&self) -> PlaybackMode {
        self.mode
    }

    pub fn fallback_reason(&self) -> Option<&str> {
        self.fallback_reason.as_deref()
    }

    pub fn has_content(&self) -> bool {
        self.pcm.is_some() && self.duration > 0.0
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn position(&self) -> f64 {
        self.position
    }

    /// 从给定时间重建播放源（零拷贝）
    fn rebuild_source(&mut self) -> bool {
        let (Some(pcm), Some(backend)) = (self.pcm.as_ref(), self.real.as_ref()) else {
            return false;
        };
        let channels = self.channels;
        let sr = self.sample_rate.max(1) as usize;
        let mut start = (self.position * sr as f64) as usize * channels as usize;
        // 对齐到整帧
        start -= start % channels as usize;
        start = start.min(pcm.len());

        backend.start_from(Arc::clone(pcm), start, self.channels, self.sample_rate);
        self.base_offset = self.position;
        self.dirty = false;
        true
    }

    pub fn play(&mut self) {
        if !self.has_content() {
            return;
        }
        if self.position >= self.duration as f64 {
            self.position = 0.0;
            self.dirty = true;
        }
        if self.mode == PlaybackMode::Simulated || !self.ensure_device() {
            self.mode = PlaybackMode::Simulated;
            self.playing = true;
            self.sim_anchor = Some((Instant::now(), self.position));
            return;
        }
        if self.rebuild_source() {
            self.playing = true;
        }
    }

    pub fn pause(&mut self) {
        self.playing = false;
        if let Some(backend) = &self.real {
            backend.pause();
        }
        if self.mode == PlaybackMode::Simulated {
            if let Some((t0, base)) = self.sim_anchor.take() {
                self.position = base + t0.elapsed().as_secs_f64();
            }
        }
    }

    pub fn toggle(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn stop(&mut self) {
        self.playing = false;
        self.position = 0.0;
        self.dirty = true;
        if let Some(backend) = &self.real {
            backend.clear();
        }
        self.sim_anchor = None;
    }

    /// 定位到指定秒（保持播放/暂停状态）
    pub fn set_position(&mut self, t: f64) {
        let t = t.clamp(0.0, self.duration as f64);
        self.position = t;
        self.dirty = true;
        if self.mode == PlaybackMode::Simulated {
            if self.playing {
                self.sim_anchor = Some((Instant::now(), t));
            }
        } else if self.playing {
            self.rebuild_source();
        } else if let Some(backend) = &self.real {
            // 暂停状态先清空队列，等下次 play 时按新位置重建
            backend.clear();
        }
    }

    /// 每帧调用：推进播放头、检测播放结束
    pub fn tick(&mut self) {
        if !self.playing {
            return;
        }
        let end = self.duration as f64;
        if self.mode == PlaybackMode::Simulated {
            if let Some((t0, base)) = self.sim_anchor {
                self.position = base + t0.elapsed().as_secs_f64();
            }
        } else if let Some(backend) = &self.real {
            self.position = self.base_offset + backend.position_secs();
            if backend.is_empty() {
                self.playing = false;
                self.dirty = true;
            }
        }
        if self.position >= end {
            self.position = end;
            self.playing = false;
            self.dirty = true;
            if let Some(backend) = &self.real {
                backend.pause();
            }
        }
    }
}

#[cfg(all(test, not(feature = "playback")))]
mod no_playback_tests {
    use super::*;

    /// 关掉 playback 后引擎必须稳定落到「模拟走时」，并给出可读的降级原因
    #[test]
    fn 无播放后端时固定模拟走时并说明原因() {
        assert!(!playback_available());
        let mut engine = AudioEngine::default();
        engine.load(Arc::new(vec![0.0; 4410]), 1, 44100, 0.1);
        assert_eq!(engine.mode(), PlaybackMode::Simulated);
        let reason = engine.fallback_reason().unwrap_or_default();
        assert!(
            reason.contains("playback"),
            "降级原因应说明是构建缺少播放后端，实际：{reason}"
        );
        // 模拟走时仍要能推进播放头，否则无界面构建连自检都跑不了
        engine.play();
        std::thread::sleep(std::time::Duration::from_millis(30));
        engine.tick();
        assert!(engine.position() > 0.0, "模拟模式下播放头应前进");
    }
}
