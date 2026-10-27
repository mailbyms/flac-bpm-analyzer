//! egui 应用：状态机（Empty / Loading / Analyzing / Ready / Error）与全部界面布局

use std::path::PathBuf;

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
    jump_to_beat, outline_button, primary_button, show_waveform_card, WaveViewState,
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

/// 当前文件信息（无论是否分析完成都要展示文件名 / 大小）
struct LoadedFile {
    name: String,
    path: PathBuf,
    size_bytes: u64,
}

pub struct FlacBpmApp {
    stage: Stage,
    file: Option<LoadedFile>,
    result: Option<AnalysisResult>,
    handle: Option<AnalysisHandle>,
    progress: f32,
    progress_label: String,
    error: Option<String>,
    audio: AudioEngine,
    view: WaveViewState,
    /// 状态栏右侧的临时提示（例如点击节拍后的信息）
    status_extra: Option<String>,
    /// 齿轮设置面板开关
    settings_open: bool,
}

impl FlacBpmApp {
    pub fn new() -> Self {
        Self {
            stage: Stage::Empty,
            file: None,
            result: None,
            handle: None,
            progress: 0.0,
            progress_label: String::new(),
            error: None,
            audio: AudioEngine::default(),
            view: WaveViewState::default(),
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
        if ext != "flac" {
            self.fail(format!("暂不支持的文件类型 .{ext}，本工具只处理 .flac"));
            return;
        }

        // 停掉旧播放、丢弃旧结果
        self.audio.stop();
        self.audio = AudioEngine::default();
        self.result = None;
        self.handle = None;
        self.progress = 0.0;
        self.error = None;
        self.status_extra = None;
        self.view = WaveViewState::default();
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
            self.progress_label = "准备中…".to_string();
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
        self.handle = Some(crate::analysis::start_analysis(path));
        self.progress = 0.0;
        self.progress_label = "准备中…".to_string();
        self.stage = Stage::Analyzing;
    }

    fn pick_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("FLAC 音频", &["flac"])
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

impl FlacBpmApp {
    /// 每帧逻辑（与 eframe 解耦，方便离屏冒烟测试）
    pub fn frame_update(&mut self, ctx: &egui::Context) {
        // Loading 状态下启动后台分析
        if matches!(self.stage, Stage::Loading) {
            self.start_analysis();
        }
        self.poll();
        self.audio.tick();
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
                    ui.label(RichText::new("FLAC 节拍分析器").size(15.0).color(TEXT));

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
                            RichText::new("导入 FLAC").size(13.0).color(Color32::WHITE),
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
            Stage::Empty => "就绪 · 等待导入 FLAC 文件".to_string(),
            Stage::Loading => "正在打开文件…".to_string(),
            Stage::Analyzing => format!(
                "分析中 · {} {:.0}%",
                self.progress_label,
                self.progress * 100.0
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
                let taken = self.result.take();
                match &taken {
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
                        RichText::new("拖入一个 FLAC 文件开始分析")
                            .size(17.0)
                            .color(TEXT),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("支持 .flac · 单次分析一个文件")
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
        let (p, label) = (self.progress, self.progress_label.clone());
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
                    let bar = egui::ProgressBar::new(p.clamp(0.0, 1.0))
                        .desired_width((ui.available_width() * 0.6).clamp(220.0, 560.0))
                        .desired_height(8.0)
                        .fill(PRIMARY)
                        .corner_radius(4.0);
                    ui.add(bar);
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("正在解码 → 降采样 → onset 包络 → 自相关测速")
                            .size(12.0)
                            .color(TEXT_SUB),
                    );
                });
            });
        });
    }

    /// 结果视图：文件卡 + 指标行 + 波形卡
    fn render_result(&mut self, ui: &mut Ui, content: Rect, result: &AnalysisResult) {
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
                    0 => metric_bpm_card(ui, result),
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

        // 借用冲突处理：临时取出 audio / view，绘制完再放回
        let mut audio = std::mem::take(&mut self.audio);
        let mut view = self.view;
        let mut info = self.status_extra.clone();
        ui.scope_builder(UiBuilder::new().max_rect(wave_rect), |ui| {
            show_waveform_card(ui, wave_rect.size(), result, &mut audio, &mut view, &mut info);
        });
        self.audio = audio;
        self.view = view;
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
                        let meta = match result {
                            Some(r) => format!(
                                "{} · {} Hz / {} bit · FLAC · {} · {} 声道",
                                format_timecode(r.duration),
                                r.sample_rate,
                                r.bits_per_sample,
                                human_size(size),
                                r.channels
                            ),
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
                ui.label(RichText::new("FLAC 节拍分析器 v0.1.0").size(12.0).color(TEXT_SUB));
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
fn metric_bpm_card(ui: &mut Ui, result: &AnalysisResult) {
    ui.vertical(|ui| {
        ui.label(RichText::new("每分钟节拍数").size(12.5).color(TEXT_SUB));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{:.1}", result.bpm)).size(34.0).color(PRIMARY));
            ui.add_space(8.0);
            let pct = (result.confidence * 100.0).round() as i32;
            egui::Frame::NONE
                .fill(BADGE_GREEN_BG)
                .corner_radius(10.0)
                .inner_margin(Margin::symmetric(8, 3))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("置信度 {pct}%"))
                            .size(11.5)
                            .color(BADGE_GREEN_FG),
                    );
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
