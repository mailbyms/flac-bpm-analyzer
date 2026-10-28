//! egui 应用：状态机（Empty / Loading / Analyzing / Ready / Error）与全部界面布局

use std::path::PathBuf;
use std::time::Instant;

use egui::{
    Align, Align2, Color32, Layout, Margin, Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder,
    Vec2, Window,
};

use crate::analysis::types::{AnalysisEvent, AnalysisResult};
use crate::analysis::AnalysisHandle;
use crate::audio::{AudioEngine, PlaybackMode};
use crate::export::{export_beats_csv, format_timecode};
use crate::theme::*;
use crate::ui::waveform::{
    jump_to_beat, outline_button, primary_button, show_waveform_card, TapTempo, WaveViewState,
};

/// 文件卡高度
const FILE_CARD_H: f32 = 76.0;
/// 波形卡最小高度
const MIN_WAVE_H: f32 = 300.0;
/// 空状态卡建议高度
const EMPTY_CARD_MIN_H: f32 = 320.0;

/// 应用状态机
enum Stage {
    /// 未导入任何文件
    Empty,
    /// 已选择文件，等待启动后台线程
    Loading,
    /// 正在分析（带进度）
    Analyzing,
    /// 分析完成
    Ready,
    /// 出错
    Error,
}

/// 支持导入的文件扩展名（小写）
pub const SUPPORTED_EXTENSIONS: [&str; 2] = ["flac", "mp3"];

/// 当前文件信息（无论是否分析完成都要展示文件名 / 大小）
struct LoadedFile {
    name: String,
    path: PathBuf,
    size_bytes: u64,
}

impl LoadedFile {
    /// 展示用的容器格式标签：`.flac` → FLAC，`.mp3` → MP3
    fn format_label(&self) -> String {
        self.path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_uppercase())
            .unwrap_or_else(|| "音频".to_string())
    }
}

/// 扩展名是否受支持（大小写不敏感）
fn is_supported_extension(ext: &str) -> bool {
    let lower = ext.to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.contains(&lower.as_str())
}

pub struct FlacBpmApp {
    stage: Stage,
    file: Option<LoadedFile>,
    result: Option<AnalysisResult>,
    handle: Option<AnalysisHandle>,
    /// 后台线程上报的目标进度（0~1）
    progress: f32,
    /// 界面实际绘制的进度（向 progress 平滑逼近，避免数字长时间不动/突然跳变）
    progress_shown: f32,
    progress_label: String,
    /// 本轮分析的起始时刻，用于在阶段之间给出"仍在工作"的反馈
    analysis_started: Option<Instant>,
    /// 上一帧时刻，用于自行计算帧间隔（不依赖 egui 的 dt API）
    last_frame: Option<Instant>,
    error: Option<String>,
    audio: AudioEngine,
    view: WaveViewState,
    /// 手工测速（右键点击波形打点，两点间隔 = 一个节拍）
    tap: TapTempo,
    /// 状态栏右侧的临时提示（例如点击节拍后的信息）
    status_extra: Option<String>,
    /// 齿轮设置面板开关
    settings_open: bool,
}

impl FlacBpmApp {
    /// 输入语义配置：应在 `ctx.run` 之前调用一次（eframe 的 creator 回调里）。
    ///
    /// egui 默认把 `Ctrl/⌘ + 滚轮` 当成缩放手势（`InputOptions::zoom_modifier` 默认
    /// 为 `Modifiers::COMMAND`），框架层会把滚轮增量经**平滑**后转成 Zoom 事件。
    /// 那个平滑余量会在滚轮停下后继续释放若干帧，于是「滚完再按住 Ctrl」会变成
    /// 持续缩放——波形自己放大。
    ///
    /// 因此这里把它置为 `NONE`：不让框架插手，改由 `ui::waveform` 用**未平滑**的
    /// `raw_scroll_delta` 判定「本帧是否真的滚了」，Ctrl 缩放、平移语义全部自控。
    /// （触控板捏合走独立的 `Event::Zoom`，不受此项影响。）
    pub fn configure(ctx: &egui::Context) {
        ctx.options_mut(|o| o.input_options.zoom_modifier = egui::Modifiers::NONE);
    }

    pub fn new() -> Self {
        Self {
            stage: Stage::Empty,
            file: None,
            result: None,
            handle: None,
            progress: 0.0,
            progress_shown: 0.0,
            progress_label: String::new(),
            analysis_started: None,
            last_frame: None,
            error: None,
            audio: AudioEngine::default(),
            view: WaveViewState::default(),
            tap: TapTempo::default(),
            status_extra: None,
            settings_open: false,
        }
    }

    // ------------------------------------------------------------------
    // 业务动作
    // ------------------------------------------------------------------

    /// 导入文件 —— 单文件模式：直接替换当前结果
    fn open_file(&mut self, path: PathBuf) {
        let size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();

        if !path.exists() {
            self.fail(format!("文件不存在：{name}"));
            return;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !is_supported_extension(&ext) {
            self.fail(format!(
                "暂不支持的文件类型 .{ext}，本工具只处理 .flac / .mp3"
            ));
            return;
        }

        // 停掉旧播放、丢弃旧结果
        self.audio.stop();
        self.audio = AudioEngine::default();
        self.result = None;
        self.handle = None;
        self.progress = 0.0;
        self.progress_shown = 0.0;
        self.analysis_started = None;
        self.error = None;
        self.status_extra = None;
        self.view = WaveViewState::default();
        self.tap.clear();
        self.file = Some(LoadedFile {
            name,
            path,
            size_bytes,
        });
        self.stage = Stage::Loading;
    }

    fn fail(&mut self, msg: String) {
        self.error = Some(msg);
        self.stage = Stage::Error;
        self.handle = None;
        self.result = None;
    }

    /// 由 Loading → Analyzing：启动后台线程
    fn start_analysis(&mut self) {
        if let Some(f) = &self.file {
            self.handle = Some(crate::analysis::start_analysis(f.path.clone()));
            self.progress = 0.0;
            self.progress_shown = 0.0;
            self.progress_label = "准备中…".to_string();
            self.analysis_started = Some(Instant::now());
            self.stage = Stage::Analyzing;
        }
    }

    /// 重新分析（同一文件）
    fn reanalyze(&mut self) {
        let Some(f) = self.file.as_ref() else { return };
        let path = f.path.clone();
        self.result = None;
        self.status_extra = None;
        self.audio.stop();
        self.view = WaveViewState::default();
        self.tap.clear();
        self.handle = Some(crate::analysis::start_analysis(path));
        self.progress = 0.0;
        self.progress_shown = 0.0;
        self.progress_label = "准备中…".to_string();
        self.analysis_started = Some(Instant::now());
        self.stage = Stage::Analyzing;
    }

    fn pick_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("音频文件 (FLAC / MP3)", &SUPPORTED_EXTENSIONS)
            .add_filter("FLAC 无损音频", &["flac"])
            .add_filter("MP3 音频", &["mp3"])
            .pick_file()
        {
            self.open_file(path);
        }
    }

    fn try_export(&mut self, result: &AnalysisResult) {
        let stem = self
            .file
            .as_ref()
            .map(|f| {
                f.name
                    .rsplit_once('.')
                    .map(|(s, _)| s.to_string())
                    .unwrap_or_else(|| f.name.clone())
            })
            .unwrap_or_else(|| "beats".to_string());
        match export_beats_csv(result, &stem) {
            Ok(Some(p)) => {
                self.status_extra = Some(format!("已导出节拍表：{}", p.display()));
            }
            Ok(None) => {}
            Err(e) => {
                self.status_extra = Some(e);
            }
        }
    }

    /// 处理后台线程回传的事件
    fn poll(&mut self) {
        let Some(handle) = self.handle.as_mut() else {
            return;
        };
        for evt in handle.poll() {
            match evt {
                AnalysisEvent::Progress { value, label } => {
                    self.progress = value.clamp(0.0, 1.0);
                    self.progress_label = label;
                }
                AnalysisEvent::Finished { result, pcm } => {
                    let channels = result.channels.max(1);
                    let sample_rate = result.sample_rate.max(1);
                    let duration = result.duration;
                    self.audio.load(pcm, channels, sample_rate, duration);
                    self.result = Some(*result);
                    self.stage = Stage::Ready;
                    self.handle = None;
                    self.progress = 1.0;
                }
                AnalysisEvent::Failed(e) => {
                    self.error = Some(e);
                    self.stage = Stage::Error;
                    self.handle = None;
                }
            }
        }
    }

    /// 进度补间：把后台上报的离散进度平滑成连续推进，
    /// 避免"长时间停在某个数字、然后一下子跳到 100%"。
    fn tick_progress(&mut self) {
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|t| (now - t).as_secs_f32())
            .unwrap_or(1.0 / 60.0)
            .clamp(1.0 / 240.0, 0.25);
        self.last_frame = Some(now);

        if !matches!(self.stage, Stage::Loading | Stage::Analyzing) {
            // 非分析态直接对齐，避免残留
            self.progress_shown = self.progress;
            return;
        }

        // 目标值只增不减，阶段切换时进度条不回退
        let target = self.progress.max(self.progress_shown);
        // 时间常数 0.18s：既平滑掉跳变，又不至于明显滞后于真实进度
        let k = 1.0 - (-dt / 0.18f32).exp();
        self.progress_shown += (target - self.progress_shown) * k;
        if target >= 1.0 || (target - self.progress_shown).abs() < 0.002 {
            self.progress_shown = target;
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (space, left, right, o, ctrl) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::O),
                i.modifiers.ctrl || i.modifiers.command,
            )
        });

        if ctrl && o {
            self.pick_file();
            return;
        }
        if space && self.audio.has_content() {
            self.audio.toggle();
        }
        // 左右方向键：上一拍 / 下一拍（借用 result，避免每帧克隆）
        if left || right {
            let taken = self.result.take();
            if let Some(r) = &taken {
                jump_to_beat(&mut self.audio, r, if left { -1 } else { 1 });
            }
            self.result = taken;
        }
    }
}

impl eframe::App for FlacBpmApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_update(ctx);
    }
}

/// 是否应该执行自动跟随：开关打开、正在播放、且不在手动平移冷却期内
fn follow_allowed(follow_enabled: bool, playing: bool, now: f64, hold_until: f64) -> bool {
    follow_enabled && playing && now >= hold_until
}

impl FlacBpmApp {
    /// 播放中按需把视图滚动到播放头。
    ///
    /// 三个前提：开关打开、正在播放、不在「手动平移冷却期」内。
    /// 冷却机制是为了避免用户滚开去看别的段落时，视图每帧都被拽回播放头。
    fn auto_follow(&mut self, now: f64) {
        if !follow_allowed(
            self.view.follow_playhead,
            self.audio.is_playing(),
            now,
            self.view.follow_hold_until,
        ) {
            return;
        }
        let Some(result) = self.result.as_ref() else {
            return;
        };
        let duration = result.duration;
        let pos = self.audio.position() as f32;
        self.view.follow(duration, pos);
    }

    /// 每帧逻辑（与 eframe 解耦，方便离屏冒烟测试）
    pub fn frame_update(&mut self, ctx: &egui::Context) {
        Self::configure(ctx);

        // Loading 状态下启动后台分析
        if matches!(self.stage, Stage::Loading) {
            self.start_analysis();
        }
        self.poll();
        self.tick_progress();
        self.audio.tick();
        self.auto_follow(ctx.input(|i| i.time));
        self.handle_shortcuts(ctx);

        // 文件拖放（单文件模式：只取第一个文件）
        let dropped = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped {
            self.open_file(path);
        }

        // 播放中持续重绘，保证播放头平滑推进
        if self.audio.is_playing() {
            ctx.request_repaint();
        }

        // 分析期间必须持续重绘：egui 是反应式渲染，不主动请求就不会刷新，
        // 否则进度条会一直停在某个数字，直到完成时才一次性跳到 100%。
        if matches!(self.stage, Stage::Loading | Stage::Analyzing) {
            ctx.request_repaint();
        }

        self.render(ctx);
    }
}

// ----------------------------------------------------------------------
// 渲染
// ----------------------------------------------------------------------

impl FlacBpmApp {
    fn render(&mut self, ctx: &egui::Context) {
        // 顶栏
        egui::TopBottomPanel::top("top_bar")
            .exact_height(TOP_BAR_H)
            .frame(
                egui::Frame::NONE
                    .fill(CARD_BG)
                    .stroke(Stroke::new(1.0_f32, CARD_BORDER)),
            )
            .show(ctx, |ui| {
                self.render_top_bar(ui);
            });

        // 底部状态栏（米色）
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(STATUS_BAR_H)
            .frame(egui::Frame::NONE.fill(BEIGE))
            .show(ctx, |ui| {
                self.render_status_bar(ui);
            });

        // 主内容区
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(APP_BG))
            .show(ctx, |ui| {
                let avail = ui.available_rect_before_wrap();
                let content = Rect::from_min_size(
                    avail.left_top() + Vec2::new(PAGE_MARGIN, PAGE_MARGIN),
                    Vec2::new(
                        (avail.width() - PAGE_MARGIN * 2.0).max(200.0),
                        (avail.height() - PAGE_MARGIN * 2.0).max(200.0),
                    ),
                );
                ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
                    self.render_content(ui, content);
                });
            });

        if self.settings_open {
            self.render_settings(ctx);
        }
    }

    // ---------------- 顶栏 ----------------
    fn render_top_bar(&mut self, ui: &mut Ui) {
        let r = ui.max_rect().shrink2(Vec2::new(PAGE_MARGIN, 0.0));
        ui.scope_builder(UiBuilder::new().max_rect(r), |ui| {
            ui.horizontal_centered(|ui| {
                    draw_app_icon(ui, 24.0);
                    ui.add_space(10.0);
                    ui.label(RichText::new("音频节拍分析器").size(15.0).color(TEXT));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let gear = egui::Button::new(RichText::new("⚙").size(15.0).color(TEXT_SUB))
                            .fill(Color32::TRANSPARENT)
                            .stroke(outline_stroke())
                            .corner_radius(CONTROL_RADIUS)
                            .min_size(Vec2::new(32.0, 32.0));
                        if ui.add(gear).clicked() {
                            self.settings_open = !self.settings_open;
                        }
                        ui.add_space(8.0);
                        let import = egui::Button::new(
                            RichText::new("导入音频").size(13.0).color(Color32::WHITE),
                        )
                        .fill(PRIMARY)
                        .corner_radius(CONTROL_RADIUS)
                        .min_size(Vec2::new(0.0, 32.0));
                        if ui.add(import).clicked() {
                            self.pick_file();
                        }
                    });
                });
        });
    }

    // ---------------- 状态栏 ----------------
    fn render_status_bar(&mut self, ui: &mut Ui) {
        let mut text = match self.stage {
            Stage::Empty => "就绪 · 等待导入音频文件（FLAC / MP3）".to_string(),
            Stage::Loading => "正在打开文件…".to_string(),
            Stage::Analyzing => format!(
                "分析中 · {} {:.0}%",
                self.progress_label,
                self.progress_shown.clamp(0.0, 1.0) * 100.0
            ),
            Stage::Ready => self.status_extra.clone().unwrap_or_else(|| {
                let bpm = self.result.as_ref().map(|r| format!("{:.1}", r.bpm)).unwrap_or_default();
                let len = self.result.as_ref().map(|r| r.beats.len()).unwrap_or(0);
                format!(
                    "分析完成 · {bpm} BPM · 共 {len} 拍 · 播放头 {}",
                    format_timecode(self.audio.position() as f32)
                )
            }),
            Stage::Error => self.error.clone().unwrap_or_default(),
        };

        if let Some(reason) = self.audio.fallback_reason() {
            if !matches!(self.stage, Stage::Empty) {
                text = reason.to_string();
            }
        }

        ui.horizontal_centered(|ui| {
            ui.add_space(PAGE_MARGIN);
            ui.label(RichText::new(text).size(12.5).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(PAGE_MARGIN);
                ui.label(
                    RichText::new("空格 播放/暂停 · ← → 上一拍/下一拍 · Ctrl+O 打开文件")
                        .size(11.5)
                        .color(TEXT),
                );
            });
        });
    }

    // ---------------- 内容分发 ----------------
    fn render_content(&mut self, ui: &mut Ui, content: Rect) {
        match self.stage {
            Stage::Empty => self.render_empty_state(ui, content),
            Stage::Error => self.render_error_state(ui, content),
            Stage::Loading | Stage::Analyzing => self.render_busy(ui, content),
            Stage::Ready => {
                // 取出 result 以避免与 &mut self 的借用冲突（手工采用 BPM 时要就地改写它）
                let mut taken = self.result.take();
                match taken.as_mut() {
                    Some(r) => self.render_result(ui, content, r),
                    None => self.render_empty_state(ui, content),
                }
                self.result = taken;
            }
        }
    }

    /// 空状态
    fn render_empty_state(&mut self, ui: &mut Ui, content: Rect) {
        let card_h = (content.height() * 0.62).clamp(EMPTY_CARD_MIN_H, 460.0);
        let card_w = content.width().min(620.0).max(360.0);
        let card_rect = Rect::from_center_size(content.center(), Vec2::new(card_w, card_h));

        ui.scope_builder(UiBuilder::new().max_rect(card_rect), |ui| {
            card_frame().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(36.0);
                    draw_wave_badge(ui, 72.0);
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new("拖入一个音频文件开始分析")
                            .size(17.0)
                            .color(TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("支持 .flac / .mp3 · 单次分析一个文件")
                            .size(12.5)
                            .color(TEXT_SUB),
                    );
                    ui.add_space(24.0);
                    ui.scope(|ui| {
                        ui.style_mut().spacing.button_padding = Vec2::new(24.0, 9.0);
                        if primary_button(ui, "选择文件") {
                            self.pick_file();
                        }
                    });
                });
            });
        });
    }

    /// 错误状态
    fn render_error_state(&mut self, ui: &mut Ui, content: Rect) {
        let card_w = content.width().min(620.0).max(360.0);
        let card_rect = Rect::from_center_size(content.center(), Vec2::new(card_w, 260.0));
        ui.scope_builder(UiBuilder::new().max_rect(card_rect), |ui| {
            card_frame().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(44.0);
                    ui.label(
                        RichText::new("分析失败")
                            .size(17.0)
                            .color(Color32::from_rgb(0xC0, 0x39, 0x2B)),
                    );
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(self.error.clone().unwrap_or_default())
                            .size(13.0)
                            .color(TEXT_SUB),
                    );
                    ui.add_space(24.0);
                    if primary_button(ui, "重新选择文件") {
                        self.pick_file();
                    }
                });
            });
        });
    }

    /// 分析中（Loading / Analyzing）：文件卡 + 进度
    fn render_busy(&mut self, ui: &mut Ui, content: Rect) {
        let file_rect =
            Rect::from_min_size(content.left_top(), Vec2::new(content.width(), FILE_CARD_H));
        ui.scope_builder(UiBuilder::new().max_rect(file_rect), |ui| {
            self.render_file_card(ui, None);
        });

        let bar_rect = Rect::from_min_max(
            Pos2::new(content.left(), file_rect.bottom() + CARD_GAP),
            Pos2::new(content.right(), content.bottom()),
        );
        // 用补间后的值绘制，保证数字与进度条是连续推进的
        let p = self.progress_shown.clamp(0.0, 1.0);
        let label = self.progress_label.clone();
        let elapsed = self
            .analysis_started
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        ui.scope_builder(UiBuilder::new().max_rect(bar_rect), |ui| {
            card_frame().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.label(
                        RichText::new(format!("{label} {:.0}%", p * 100.0))
                            .size(15.0)
                            .color(TEXT),
                    );
                    ui.add_space(14.0);
                    let bar = egui::ProgressBar::new(p)
                        .desired_width((ui.available_width() * 0.6).clamp(220.0, 560.0))
                        .desired_height(8.0)
                        .fill(PRIMARY)
                        .corner_radius(4.0);
                    ui.add(bar);
                    ui.add_space(10.0);
                    // 已用时间持续走动，即使某一阶段暂无细分进度也能看出程序在工作
                    ui.label(
                        RichText::new(format!(
                            "已用 {:.1} s · 解码 → 降采样 → 波形 → onset 包络 → 自相关测速",
                            elapsed
                        ))
                        .size(12.0)
                        .color(TEXT_SUB),
                    );
                });
            });
        });
    }

    /// 结果视图：文件卡 + 指标行 + 波形卡
    fn render_result(&mut self, ui: &mut Ui, content: Rect, result: &mut AnalysisResult) {
        let w = content.width();

        // ---- 1. 文件信息卡 ----
        let file_rect = Rect::from_min_size(content.left_top(), Vec2::new(w, FILE_CARD_H));
        ui.scope_builder(UiBuilder::new().max_rect(file_rect), |ui| {
            self.render_file_card(ui, Some(result));
        });

        // ---- 2. 指标行（四张等高卡片） ----
        let metrics_top = file_rect.bottom() + CARD_GAP;
        let col_w = (w - CARD_GAP * 3.0) / 4.0;
        for i in 0..4 {
            let rect = Rect::from_min_size(
                Pos2::new(content.left() + i as f32 * (col_w + CARD_GAP), metrics_top),
                Vec2::new(col_w, METRIC_H),
            );
            ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                card_frame().show(ui, |ui| match i {
                    0 => metric_bpm_card(ui, result, self.tap.applied.is_some()),
                    1 => metric_simple_card(ui, "拍号", "4 / 4"),
                    2 => metric_simple_card(
                        ui,
                        "首拍偏移",
                        &format!("{:.3} s", result.first_beat_offset),
                    ),
                    _ => metric_simple_card(ui, "节拍总数", &format!("{}", result.beats.len())),
                });
            });
        }

        // ---- 3. 波形与节拍卡 ----
        let wave_top = metrics_top + METRIC_H + CARD_GAP;
        let wave_h = (content.bottom() - wave_top).max(MIN_WAVE_H);
        let wave_rect = Rect::from_min_size(Pos2::new(content.left(), wave_top), Vec2::new(w, wave_h));

        // 借用冲突处理：临时取出 audio / view / tap，绘制完再放回
        let mut audio = std::mem::take(&mut self.audio);
        let mut view = self.view;
        let mut tap = std::mem::take(&mut self.tap);
        let mut info = self.status_extra.clone();
        ui.scope_builder(UiBuilder::new().max_rect(wave_rect), |ui| {
            show_waveform_card(
                ui,
                wave_rect.size(),
                result,
                &mut audio,
                &mut view,
                &mut tap,
                &mut info,
            );
        });
        self.audio = audio;
        self.view = view;
        self.tap = tap;
        self.status_extra = info;
    }

    /// 文件信息卡（文件名 + 元数据 + 重新分析 / 导出按钮）
    fn render_file_card(&mut self, ui: &mut Ui, result: Option<&AnalysisResult>) {
        let card = ui.max_rect();
        ui.scope_builder(UiBuilder::new().max_rect(card), |ui| {
            card_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左：文件名与元数据
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        let name = self
                            .file
                            .as_ref()
                            .map(|f| f.name.clone())
                            .unwrap_or_default();
                        let size = self.file.as_ref().map(|f| f.size_bytes).unwrap_or(0);
                        ui.label(RichText::new(name).size(17.0).color(TEXT).strong());
                        ui.add_space(5.0);
                        // 容器格式按实际扩展名显示（FLAC / MP3），不要写死
                        let fmt = self
                            .file
                            .as_ref()
                            .map(|f| f.format_label())
                            .unwrap_or_else(|| "音频".to_string());
                        let meta = match result {
                            Some(r) => {
                                // 有损格式（MP3）没有「位深」这个概念，显示 16 bit 会误导
                                let quality = if fmt == "MP3" {
                                    format!("{} Hz", r.sample_rate)
                                } else {
                                    format!("{} Hz / {} bit", r.sample_rate, r.bits_per_sample)
                                };
                                format!(
                                    "{} · {} · {} · {} · {} 声道",
                                    format_timecode(r.duration),
                                    quality,
                                    fmt,
                                    human_size(size),
                                    r.channels
                                )
                            }
                            None => format!("正在读取文件信息 · {}", human_size(size)),
                        };
                        ui.label(RichText::new(meta).size(12.5).color(TEXT_SUB));
                    });

                    // 右：操作按钮
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if result.is_some() {
                            if outline_button(ui, "导出节拍表") {
                                if let Some(r) = self.result.take() {
                                    self.try_export(&r);
                                    self.result = Some(r);
                                }
                            }
                            ui.add_space(8.0);
                            if outline_button(ui, "重新分析") {
                                self.reanalyze();
                            }
                        }
                    });
                });
            });
        });
    }

    /// 齿轮：设置面板（纯展示，无任何节拍编辑功能）
    fn render_settings(&mut self, ctx: &egui::Context) {
        Window::new("设置")
            .open(&mut self.settings_open)
            .anchor(Align2::RIGHT_TOP, egui::vec2(-16.0, 60.0))
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.set_min_width(280.0);
                ui.label(RichText::new("播放模式").size(12.5).color(TEXT_SUB));
                let mode = match self.audio.mode() {
                    PlaybackMode::Real => "真实音频输出（cpal / WASAPI）",
                    PlaybackMode::Simulated => "模拟走时（未找到可用音频设备）",
                };
                ui.label(RichText::new(mode).size(13.0).color(TEXT));
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);
                ui.label(
                    RichText::new(
                        "本工具为只读分析器：节拍结果由算法直接给出，不提供手动增删节拍、微调偏移等功能。",
                    )
                    .size(12.0)
                    .color(TEXT_SUB),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!(
                        "音频节拍分析器 v{}",
                        env!("CARGO_PKG_VERSION")
                    ))
                    .size(12.0)
                    .color(TEXT_SUB),
                );
            });
    }
}

// ----------------------------------------------------------------------
// 复用的小组件
// ----------------------------------------------------------------------

fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(CARD_BG)
        .corner_radius(CARD_RADIUS)
        .shadow(card_shadow())
        .stroke(card_stroke())
        .inner_margin(Margin::symmetric(16, 12))
        .outer_margin(Margin::ZERO)
}

/// 应用图标：橙色圆角方块 + 白色波形条
fn draw_app_icon(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size, size), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 7.0, PRIMARY);
    let cy = rect.center().y;
    let left = rect.left() + 4.0;
    let step = (size - 8.0) / 4.0;
    for i in 0..4 {
        let x = left + i as f32 * step + step * 0.2;
        let h = [7.0f32, 12.0, 9.0, 5.0][i];
        p.rect_filled(
            Rect::from_center_size(Pos2::new(x, cy), Vec2::new(2.0, h)),
            1.0,
            Color32::WHITE,
        );
    }
}

/// 空状态图标：橙色圆形 + 白色波形
fn draw_wave_badge(ui: &mut Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size, size), Sense::hover());
    let p = ui.painter();
    p.circle_filled(rect.center(), size / 2.0, PRIMARY);
    let cy = rect.center().y;
    let heights = [10.0f32, 22.0, 14.0, 28.0, 16.0, 8.0];
    let step = size / (heights.len() as f32 + 1.0);
    for (i, &h) in heights.iter().enumerate() {
        let x = rect.left() + step * (i as f32 + 1.0);
        p.rect_filled(
            Rect::from_center_size(
                Pos2::new(x, cy),
                Vec2::new(3.0, h.clamp(6.0, size * 0.45)),
            ),
            1.5,
            Color32::WHITE,
        );
    }
}

/// ① BPM 指标卡：大号橙色数字 + 置信度徽章
fn metric_bpm_card(ui: &mut Ui, result: &AnalysisResult, manual: bool) {
    ui.vertical(|ui| {
        ui.label(RichText::new("每分钟节拍数").size(12.5).color(TEXT_SUB));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{:.1}", result.bpm)).size(34.0).color(PRIMARY));
            ui.add_space(8.0);
            // 手工值没有「置信度」可言，改标注来源，避免把人工测量伪装成算法置信度
            let (bg, fg, text) = if manual {
                (
                    Color32::from_rgb(0xFD, 0xF1, 0xEA),
                    Color32::from_rgb(0xC0, 0x56, 0x2F),
                    "手工测量".to_string(),
                )
            } else {
                let pct = (result.confidence * 100.0).round() as i32;
                (
                    BADGE_GREEN_BG,
                    BADGE_GREEN_FG,
                    format!("置信度 {pct}%"),
                )
            };
            egui::Frame::NONE
                .fill(bg)
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).size(11.5).color(fg));
                });
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("周期 {:.0} ms", 60000.0 / result.bpm.max(1e-3)))
                .size(11.5)
                .color(TEXT_SUB),
        );
    });
}

/// ②③④ 普通指标卡
fn metric_simple_card(ui: &mut Ui, title: &str, value: &str) {
    ui.vertical(|ui| {
        ui.label(RichText::new(title).size(12.5).color(TEXT_SUB));
        ui.add_space(8.0);
        ui.label(RichText::new(value).size(28.0).color(TEXT));
    });
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.2} GB", b / KB / KB / KB)
    }
}

#[cfg(test)]
mod smoke_tests {
    use super::*;
    use crate::analysis::types::AnalysisResult;

    /// 造一份可直接进入 Ready 态的假结果
    fn fake_result() -> AnalysisResult {
        let period = 0.5;
        AnalysisResult {
            bpm: 120.0,
            confidence: 0.92,
            first_beat_offset: 0.12,
            beats: (0..480).map(|i| 0.12 + i as f32 * period).collect(),
            duration: 240.0,
            sample_rate: 44100,
            bits_per_sample: 16,
            channels: 2,
            waveform_peaks: vec![0.45; 8192],
            beats_per_bar: 4,
        }
    }

    fn ready_app(result: AnalysisResult, zoom: f32, start: f32) -> FlacBpmApp {
        let mut app = FlacBpmApp::new();
        app.stage = Stage::Ready;
        app.file = Some(LoadedFile {
            name: "test.flac".into(),
            path: std::path::PathBuf::from("test.flac"),
            size_bytes: 28_000_000,
        });
        app.result = Some(result);
        app.view = crate::ui::waveform::WaveViewState { zoom, start_sec: start, ..Default::default() };
        app
    }

    /// 跑一帧并返回本帧产生的所有绘制指令（离屏、无需 GPU/窗口）
    fn frame(
        app: &mut FlacBpmApp,
        ctx: &egui::Context,
        pointer: Option<egui::Pos2>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let events = pointer
            .map(egui::Event::PointerMoved)
            .into_iter()
            .collect();
        frame_raw(app, ctx, egui::Modifiers::NONE, events)
    }

    /// 同上，但可指定修饰键与任意输入事件（用于验证滚轮/捏合语义）
    fn frame_raw(
        app: &mut FlacBpmApp,
        ctx: &egui::Context,
        modifiers: egui::Modifiers,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        FlacBpmApp::configure(ctx);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(1440.0, 920.0),
            )),
            modifiers,
            events,
            ..Default::default()
        };
        ctx.run(input, |ctx| app.frame_update(ctx)).shapes
    }

    /// 在本帧绘制指令里找包含指定文字的文本图元
    fn has_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
        shapes.iter().any(|cs| match &cs.shape {
            egui::Shape::Text(ts) => ts.galley.text().contains(needle),
            _ => false,
        })
    }

    /// 取出本帧「指针附近」的文本图元。
    ///
    /// 必须限定在指针附近：播放控制行本身就有时间码文本，不加位置过滤会得到假阳性。
    fn texts_near(shapes: &[egui::epaint::ClippedShape], pointer: egui::Pos2, max_dy: f32) -> Vec<String> {
        shapes
            .iter()
            .filter_map(|cs| match &cs.shape {
                egui::Shape::Text(ts) if (ts.pos.y - pointer.y).abs() < max_dy => {
                    Some(ts.galley.text().to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// 是否形如时间码 `m:ss.mmm` / `mm:ss.mmm`
    fn is_timecode(s: &str) -> bool {
        let Some((m, rest)) = s.split_once(':') else {
            return false;
        };
        let Some((sec, ms)) = rest.split_once('.') else {
            return false;
        };
        !m.is_empty()
            && m.chars().all(|c| c.is_ascii_digit())
            && sec.len() == 2
            && sec.chars().all(|c| c.is_ascii_digit())
            && ms.len() == 3
            && ms.chars().all(|c| c.is_ascii_digit())
    }

    /// 在指定位置右键点击一次（press + release 两帧）
    fn right_click_at(app: &mut FlacBpmApp, ctx: &egui::Context, pos: egui::Pos2) {
        let m = egui::Modifiers::NONE;
        let ev = |pressed: bool| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: m,
                },
            ]
        };
        frame_raw(app, ctx, m, ev(true));
        frame_raw(app, ctx, m, ev(false));
    }

    /// 右键点击波形两次 = 手工测速：应记录打点、算出 BPM，并给出「采用」按钮
    #[test]
    fn 右键点击可手工测速() {
        let ctx = egui::Context::default();
        // 20 倍放大 → 可视 12s / 约 1368px，即约 114px/s
        let mut app = ready_app(fake_result(), 20.0, 0.0);
        let y = 500.0;
        frame(&mut app, &ctx, None); // 先建一帧，让舞台 widget 完成首次布局

        right_click_at(&mut app, &ctx, egui::Pos2::new(600.0, y));
        assert_eq!(app.tap.taps().len(), 1, "第一次右键应记录一个打点");
        assert_eq!(app.tap.bpm(), None, "只点一次还算不出 BPM");

        // 右移 56px ≈ 0.49s ≈ 122 BPM
        right_click_at(&mut app, &ctx, egui::Pos2::new(656.0, y));
        assert_eq!(app.tap.taps().len(), 2, "第二次右键应记录第二个打点");
        let bpm = app.tap.bpm().expect("两次打点后应能算出 BPM");
        assert!(
            (bpm - 120.0).abs() < 10.0,
            "两次点击相隔约 0.5s，应约 120 BPM，实际 {bpm:.1}"
        );

        let shapes = frame(&mut app, &ctx, Some(egui::Pos2::new(700.0, y)));
        assert!(
            has_text(&shapes, "采用此 BPM"),
            "测出 BPM 后应出现「采用此 BPM」按钮"
        );
        assert!(has_text(&shapes, "手工"), "应显示手工测速结果文本");
    }

    /// 左键点击仍然是定位播放头，不会被打点逻辑吃掉
    #[test]
    fn 左键点击仍然定位播放头() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 20.0, 0.0);
        let pos = egui::Pos2::new(600.0, 500.0);
        frame(&mut app, &ctx, None);
        let m = egui::Modifiers::NONE;
        for pressed in [true, false] {
            frame_raw(
                &mut app,
                &ctx,
                m,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: m,
                    },
                ],
            );
        }
        assert!(app.tap.taps().is_empty(), "左键点击不应产生打点");
        // 离屏环境没有载入 PCM，audio.position() 会被 duration=0 钳住，
        // 因此改为断言状态栏提示（它由同一条定位分支写入）
        let msg = app.status_extra.clone().unwrap_or_default();
        assert!(
            msg.contains("播放头定位于"),
            "左键点击应定位播放头，实际状态栏：{msg:?}"
        );
    }

    /// 自动跟随的三个前提（开关 / 正在播放 / 不在手动冷却期）
    #[test]
    fn 自动跟随的三个前提() {
        assert!(follow_allowed(true, true, 10.0, 9.0), "条件齐备时应跟随");
        assert!(!follow_allowed(false, true, 10.0, 9.0), "开关关闭不应跟随");
        assert!(!follow_allowed(true, false, 10.0, 9.0), "未播放不应跟随");
        assert!(
            !follow_allowed(true, true, 10.0, 12.0),
            "手动平移冷却期内不应跟随"
        );
        assert!(
            follow_allowed(true, true, 12.0, 12.0),
            "冷却结束后应恢复跟随"
        );
    }

    /// 播放控制行应有「跟随播放头」开关，默认开启
    #[test]
    fn 跟随播放头开关存在且默认开启() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 0.0);
        let shapes = frame(&mut app, &ctx, None);
        assert!(
            has_text(&shapes, "跟随播放头"),
            "播放控制行应有「跟随播放头」开关"
        );
        assert!(
            has_text(&shapes, "跟随播放头 · 开"),
            "默认应为开启状态"
        );
    }

    /// 光标在波形区任意位置移动都应实时显示时间码（不只在节拍线上）
    #[test]
    fn 光标移动实时显示时间码() {
        let ctx = egui::Context::default();
        // 放大到 20 倍：节拍线间距足够大，能明确区分「命中」与「未命中」
        let mut app = ready_app(fake_result(), 20.0, 0.0);
        let mut plain_hits = 0;
        let mut x = 40.0;
        while x <= 1400.0 {
            let p = egui::Pos2::new(x, 500.0);
            let shapes = frame(&mut app, &ctx, Some(p));
            let ts = texts_near(&shapes, p, 120.0);
            let on_beat = ts.iter().any(|t| t.contains("节拍 #"));
            let has_time = ts.iter().any(|t| is_timecode(t));
            if !on_beat && has_time {
                plain_hits += 1;
            }
            x += 7.0;
        }
        assert!(
            plain_hits > 0,
            "光标未落在节拍线上时也应显示时间码提示（实际未出现）"
        );
    }

    /// 离屏渲染若干帧，覆盖全曲视图 / 放大视图 / 滚到曲末 / 指针扫过舞台
    #[test]
    fn 界面渲染不panic且悬停路径可执行() {
        let ctx = egui::Context::default();
        for (zoom, start) in [(1.0, 0.0), (6.0, 30.0), (20.0, 200.0), (60.0, 239.0)] {
            let mut app = ready_app(fake_result(), zoom, start);
            frame(&mut app, &ctx, None);
            // 指针扫过舞台区域，触发节拍悬停命中与提示绘制
            let mut y = 380.0;
            while y <= 700.0 {
                let mut x = 40.0;
                while x <= 1400.0 {
                    frame(&mut app, &ctx, Some(egui::Pos2::new(x, y)));
                    x += 40.0;
                }
                y += 40.0;
            }
        }
    }

    /// 扩展名白名单：接受 flac / mp3（大小写不敏感），拒绝其他格式
    #[test]
    fn 扩展名白名单包含mp3() {
        for ext in ["flac", "FLAC", "mp3", "MP3", "Mp3"] {
            assert!(is_supported_extension(ext), "{ext} 应被接受");
        }
        for ext in ["wav", "m4a", "ogg", "flac.part", ""] {
            assert!(!is_supported_extension(ext), "{ext} 不应被接受");
        }
    }

    /// 元数据行按实际扩展名显示容器格式，不能写死 FLAC
    #[test]
    fn 格式标签随扩展名变化() {
        let mk = |name: &str| LoadedFile {
            name: name.to_string(),
            path: std::path::PathBuf::from(name),
            size_bytes: 1024,
        };
        assert_eq!(mk("a.flac").format_label(), "FLAC");
        assert_eq!(mk("b.mp3").format_label(), "MP3");
        assert_eq!(mk("c.MP3").format_label(), "MP3");
        assert_eq!(mk("no_ext").format_label(), "音频");
    }

    /// 空状态同样要能渲染
    #[test]
    fn 空状态渲染不panic() {
        let ctx = egui::Context::default();
        let mut app = FlacBpmApp::new();
        frame(&mut app, &ctx, None);
        frame(&mut app, &ctx, Some(egui::Pos2::new(720.0, 460.0)));
    }

    /// 指针扫过舞台时，命中节拍线应真的画出提示气泡（而不是只跑通代码）
    #[test]
    fn 悬停节拍线会画出提示气泡() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 0.0);
        let mut hits = 0;
        let mut x = 40.0;
        while x <= 1400.0 {
            let shapes = frame(&mut app, &ctx, Some(egui::Pos2::new(x, 500.0)));
            if has_text(&shapes, "节拍 #") {
                hits += 1;
            }
            x += 3.0;
        }
        assert!(hits > 0, "扫过舞台时应至少命中一条节拍线并绘制提示气泡");
    }

    /// 只按住 Ctrl（滚轮完全没动）→ 波形必须纹丝不动，既不缩放也不平移。
    ///
    /// 回归测试：egui 的 smooth_scroll_delta 在滚轮停下后仍会释放几帧余量，
    /// 若用平滑值判定缩放，就会出现「只按 Ctrl 波形自己放大」。
    #[test]
    fn 只按ctrl波形不动() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 30.0);
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let pt = egui::Pos2::new(700.0, 500.0);

        // 先滚一下滚轮（不按 Ctrl），制造平滑余量
        for _ in 0..3 {
            frame_raw(
                &mut app,
                &ctx,
                egui::Modifiers::NONE,
                vec![
                    egui::Event::PointerMoved(pt),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::Vec2::new(0.0, -1.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        // 然后只按住 Ctrl，滚轮不再动，连跑 10 帧
        let zoom0 = app.view.zoom;
        let start0 = app.view.start_sec;
        for _ in 0..10 {
            frame_raw(&mut app, &ctx, ctrl, vec![egui::Event::PointerMoved(pt)]);
        }
        assert!(
            (app.view.zoom - zoom0).abs() < 1e-3,
            "只按 Ctrl 不应缩放：{zoom0} → {}",
            app.view.zoom
        );
        assert!(
            (app.view.start_sec - start0).abs() < 1e-3,
            "只按 Ctrl 也不应平移：{start0} → {}",
            app.view.start_sec
        );
    }

    /// Ctrl + 滚轮 = 缩放（且不产生平移）
    #[test]
    fn ctrl加滚轮缩放() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 60.0);
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let pt = egui::Pos2::new(700.0, 500.0);
        let zoom0 = app.view.zoom;
        for _ in 0..3 {
            frame_raw(
                &mut app,
                &ctx,
                ctrl,
                vec![
                    egui::Event::PointerMoved(pt),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::Vec2::new(0.0, -1.0),
                        modifiers: ctrl,
                    },
                ],
            );
        }
        assert!(
            app.view.zoom < zoom0 * 0.9,
            "Ctrl+向下滚应缩小视图：{zoom0} → {}",
            app.view.zoom
        );
    }

    /// 单滚轮（不按 Ctrl）= 横向平移，不缩放
    #[test]
    fn 单滚轮只平移不缩放() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 30.0);
        let pt = egui::Pos2::new(700.0, 500.0);
        let zoom0 = app.view.zoom;
        let start0 = app.view.start_sec;
        for _ in 0..6 {
            frame_raw(
                &mut app,
                &ctx,
                egui::Modifiers::NONE,
                vec![
                    egui::Event::PointerMoved(pt),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::Vec2::new(0.0, -1.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert!(
            (app.view.zoom - zoom0).abs() < 1e-3,
            "单滚轮不应改变缩放：{zoom0} → {}",
            app.view.zoom
        );
        assert!(
            app.view.start_sec > start0 + 0.05,
            "单滚轮应横向平移：start {start0} → {}",
            app.view.start_sec
        );
    }

    /// 触控板双指捏合仍然缩放（唯一保留的手势缩放入口）
    #[test]
    fn 触控板捏合仍然缩放() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 4.0, 60.0);
        let zoom0 = app.view.zoom;
        for _ in 0..3 {
            frame_raw(
                &mut app,
                &ctx,
                egui::Modifiers::NONE,
                vec![
                    egui::Event::PointerMoved(egui::Pos2::new(700.0, 500.0)),
                    egui::Event::Zoom(1.6),
                ],
            );
        }
        assert!(
            app.view.zoom > zoom0 * 1.2,
            "捏合应放大视图：{zoom0} → {}",
            app.view.zoom
        );
    }

    /// 滚动条：放大后应出现滑块图元（全曲视图下不该有可拖动的窄滑块）
    #[test]
    fn 放大后出现滚动条滑块() {
        let ctx = egui::Context::default();
        let mut app = ready_app(fake_result(), 8.0, 60.0);
        let shapes = frame(&mut app, &ctx, None);
        // 滑块宽 = 轨道宽 × 可视比例，8 倍放大下应远窄于舞台宽度
        let narrow = shapes.iter().any(|cs| match &cs.shape {
            egui::Shape::Rect(rs) => {
                let w = rs.rect.width();
                let h = rs.rect.height();
                (h - 8.0).abs() < 0.6 && w > 30.0 && w < 300.0
            }
            _ => false,
        });
        assert!(narrow, "8 倍放大时应绘制出窄滑块");
    }
}
