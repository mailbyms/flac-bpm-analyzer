//! 波形与节拍卡：工具栏 / 时间刻度 / 舞台（波形层 + 节拍层 + 播放头）/ 播放控制
//!
//! 分层顺序（严格按要求）：
//!   ① 波形层（已播放橙 / 未播放米色 / 中轴线）
//!   ② 节拍（拍：靛蓝虚线；小节重拍：深靛实线 + 顶部手柄条）
//!   ③ 播放头（橙色竖线 + 圆点手柄）

use egui::{
    Align2, Color32, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke, Ui, UiBuilder,
    Vec2,
};

use crate::analysis::types::AnalysisResult;
use crate::audio::AudioEngine;
use crate::export::format_timecode;
use crate::theme::*;

/// 节拍最小可视间距（像素）：低于阈值时抽稀，避免标记糊成一片
const MIN_BEAT_SPACING: f32 = 6.0;
/// 播放/分页相关控件尺寸
const TRANSPORT_H: f32 = 56.0;
const TOOLBAR_H: f32 = 26.0;
const RULER_H: f32 = 24.0;

/// 波形视图状态（缩放 + 起始时间）
#[derive(Clone, Copy)]
pub struct WaveViewState {
    /// 缩放倍数：1.0 = 整曲铺满
    pub zoom: f32,
    /// 可视窗口起始时间（秒）
    pub start_sec: f32,
}

impl Default for WaveViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            start_sec: 0.0,
        }
    }
}

impl WaveViewState {
    /// 可视时长
    pub fn visible_duration(&self, duration: f32) -> f32 {
        (duration / self.zoom).max(0.05)
    }

    /// 归一化的可视窗口 [t0, t1]
    pub fn window(&self, duration: f32) -> (f32, f32) {
        let vis = self.visible_duration(duration);
        let max_start = (duration - vis).max(0.0);
        let t0 = self.start_sec.clamp(0.0, max_start);
        (t0, t0 + vis)
    }

    /// 时间 → x 坐标
    pub fn time_to_x(&self, t: f32, rect: Rect, duration: f32) -> f32 {
        let (t0, t1) = self.window(duration);
        let ratio = (t - t0) / (t1 - t0).max(1e-6);
        rect.left() + ratio * rect.width()
    }

    /// x 坐标 → 时间
    pub fn x_to_time(&self, x: f32, rect: Rect, duration: f32) -> f32 {
        let (t0, t1) = self.window(duration);
        let ratio = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        t0 + ratio * (t1 - t0)
    }

    pub fn zoom_in(&mut self, duration: f32) {
        self.set_zoom(self.zoom * 1.5, 0.5, duration);
    }

    pub fn zoom_out(&mut self, duration: f32) {
        self.set_zoom(self.zoom / 1.5, 0.5, duration);
    }

    /// 以 `anchor_ratio`（0~1，可视窗口内的相对位置）为锚点缩放
    pub fn set_zoom(&mut self, new_zoom: f32, anchor_ratio: f32, duration: f32) {
        let (t0, t1) = self.window(duration);
        let anchor_t = t0 + (t1 - t0) * anchor_ratio;
        let zoom = new_zoom.clamp(1.0, 60.0);
        let vis = duration / zoom;
        self.zoom = zoom;
        // 保持锚点处的时间不动
        let new_start = anchor_t - vis * anchor_ratio;
        let max_start = (duration - vis).max(0.0);
        self.start_sec = new_start.clamp(0.0, max_start);
    }

    /// 平移可视窗口
    pub fn pan(&mut self, delta_sec: f32, duration: f32) {
        let vis = self.visible_duration(duration);
        let max_start = (duration - vis).max(0.0);
        self.start_sec = (self.start_sec + delta_sec).clamp(0.0, max_start);
    }
}

/// 绘制整张「波形与节拍」卡片
pub fn show_waveform_card(
    ui: &mut Ui,
    card_size: Vec2,
    result: &AnalysisResult,
    audio: &mut AudioEngine,
    view: &mut WaveViewState,
    info_text: &mut Option<String>,
) {
    let duration = result.duration;
    let (rect, _) = ui.allocate_exact_size(card_size, Sense::hover());
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        egui::Frame::NONE
            .fill(CARD_BG)
            .corner_radius(CARD_RADIUS)
            .shadow(card_shadow())
            .stroke(card_stroke())
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                let inner = ui.max_rect();
                let toolbar_r = Rect::from_min_size(inner.left_top(), Vec2::new(inner.width(), TOOLBAR_H));
                let ruler_r = Rect::from_min_size(
                    toolbar_r.left_bottom() + Vec2::new(0.0, 8.0),
                    Vec2::new(inner.width(), RULER_H),
                );
                let transport_r = Rect::from_min_size(
                    Pos2::new(inner.left(), inner.bottom() - TRANSPORT_H),
                    Vec2::new(inner.width(), TRANSPORT_H),
                );
                let stage_r = Rect::from_min_max(
                    ruler_r.left_bottom() + Vec2::new(0.0, 6.0),
                    Pos2::new(inner.right(), transport_r.top() - 8.0),
                );

                draw_toolbar(ui, toolbar_r, view, duration);
                draw_ruler(ui, ruler_r, result, view);
                draw_stage(ui, stage_r, result, audio, view, info_text);
                draw_transport(ui, transport_r, result, audio, view);
            });
    });
}

/// 工具行：标题 + 图例 + 缩放控件
fn draw_toolbar(ui: &mut Ui, rect: Rect, view: &mut WaveViewState, duration: f32) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new("波形与节拍").size(15.0).color(TEXT));
            ui.add_space(18.0);

            // 图例
            legend_dot(ui, PRIMARY, "波形");
            legend_dot(ui, PRIMARY, "已播放");
            legend_dot(ui, BEAT, "节拍");
            legend_dot(ui, DOWNBEAT, "小节重拍");

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.style_mut().spacing.item_spacing.x = 4.0;
                let zoom_txt = format!("{}%", (view.zoom * 100.0).round() as i32);
                ui.label(RichText::new(zoom_txt).size(12.0).color(TEXT_SUB).monospace());
                if round_icon_button(ui, "+", 22.0) {
                    view.zoom_in(duration);
                }
                ui.add(egui::Separator::default().vertical());
                if round_icon_button(ui, "−", 22.0) {
                    view.zoom_out(duration);
                }
            });
        });
    });
}

fn legend_dot(ui: &mut Ui, color: Color32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(6.0, 6.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
    ui.add_space(2.0);
    ui.label(RichText::new(text).size(12.0).color(TEXT_SUB));
    ui.add_space(10.0);
}

fn round_icon_button(ui: &mut Ui, symbol: &str, size: f32) -> bool {
    let btn = egui::Button::new(RichText::new(symbol).size(14.0).color(TEXT))
        .fill(Color32::from_rgb(0xF5, 0xF0, 0xE7))
        .stroke(outline_stroke())
        .corner_radius(size / 2.0)
        .min_size(Vec2::new(size, size));
    ui.add(btn).clicked()
}

/// 时间刻度尺（自适应步长）
fn draw_ruler(ui: &mut Ui, rect: Rect, result: &AnalysisResult, view: &WaveViewState) {
    let duration = result.duration;
    let (t0, t1) = view.window(duration);
    let visible = t1 - t0;

    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            6.0,
            Color32::from_rgb(0xFB, 0xF8, 0xF2),
        );

        let step = nice_step(visible);
        let minor_step = step / 5.0;
        let start = (t0 / minor_step).floor() * minor_step;

        let painter = painter.with_clip_rect(rect);
        let mut t = start;
        let mut index = 0i64;
        while t <= t1 {
            let x = view.time_to_x(t, rect, duration);
            if x >= rect.left() - 1.0 && x <= rect.right() + 1.0 {
                let is_major = ((t / step).round() * step - t).abs() < minor_step * 0.5;
                if is_major {
                    painter.line_segment(
                        [Pos2::new(x, rect.bottom() - 10.0), Pos2::new(x, rect.bottom() - 2.0)],
                        Stroke::new(1.0_f32, TEXT_SUB),
                    );
                    painter.text(
                        Pos2::new(x + 3.0, rect.top() + 4.0),
                        Align2::LEFT_TOP,
                        format_timecode(t.max(0.0)),
                        FontId::proportional(10.0),
                        TEXT_SUB,
                    );
                } else {
                    painter.line_segment(
                        [Pos2::new(x, rect.bottom() - 6.0), Pos2::new(x, rect.bottom() - 2.0)],
                        Stroke::new(1.0_f32, BEIGE_LIGHT),
                    );
                }
            }
            index += 1;
            t = start + index as f32 * minor_step;
            if index > 100_000 {
                break;
            }
        }
    });
}

/// 选取一个「好看」的主刻度步长
fn nice_step(visible: f32) -> f32 {
    const CANDIDATES: [f32; 12] = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0];
    for c in CANDIDATES {
        if visible / c <= 10.0 {
            return c;
        }
    }
    600.0
}

/// 波形舞台：交互 + 三层绘制
fn draw_stage(
    ui: &mut Ui,
    rect: Rect,
    result: &AnalysisResult,
    audio: &mut AudioEngine,
    view: &mut WaveViewState,
    info_text: &mut Option<String>,
) {
    let duration = result.duration;

    // ---- 交互 ----
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let (ctrl_or_cmd, shift, wheel_delta, zoom_delta) = ui.input(|i| {
        (
            i.modifiers.ctrl || i.modifiers.command,
            i.modifiers.shift,
            i.smooth_scroll_delta.y + i.smooth_scroll_delta.x,
            i.zoom_delta(),
        )
    });

    if resp.hovered() {
        if ctrl_or_cmd || zoom_delta != 1.0 {
            // Ctrl/⌘ + 滚轮 或 触控板捏合 → 以鼠标位置为锚点缩放
            let pointer_x = ui
                .input(|i| i.pointer.hover_pos().map(|p| p.x))
                .unwrap_or(rect.center().x);
            let anchor = ((pointer_x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            let factor = if zoom_delta != 1.0 {
                zoom_delta
            } else {
                1.15_f32.powf(wheel_delta.signum())
            };
            view.set_zoom(view.zoom * factor, anchor, duration);
        } else if shift && wheel_delta != 0.0 {
            // Shift + 滚轮 → 横向平移
            view.pan(-wheel_delta * 0.02, duration);
        } else if wheel_delta != 0.0 {
            // 普通滚轮 → 缩放，保持中心不动
            let factor = 1.15_f32.powf(wheel_delta.signum());
            view.set_zoom(view.zoom * factor, 0.5, duration);
        }
    }

    if resp.clicked() || resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = view.x_to_time(pos.x, rect, duration);
            let period = 60.0 / result.bpm.max(1e-3);
            // 距离 6px 内则吸附到该节拍
            let nearest = if result.beats.is_empty() || !period.is_finite() || period <= 0.0 {
                None
            } else {
                let idx = ((t - result.first_beat_offset) / period).round() as i64;
                let bt = result.first_beat_offset + idx as f32 * period;
                if bt >= 0.0
                    && bt < duration
                    && (view.time_to_x(bt, rect, duration) - pos.x).abs() < 6.0
                {
                    Some((idx.max(0) as usize, bt))
                } else {
                    None
                }
            };
            let playhead = nearest.map(|(_, bt)| bt).unwrap_or(t);
            audio.set_position(playhead as f64);
            if let Some((idx, bt)) = nearest {
                *info_text = Some(format!(
                    "节拍 #{} · {} · 第 {} 小节第 {} 拍",
                    idx + 1,
                    format_timecode(bt),
                    idx / result.beats_per_bar.max(1) as usize + 1,
                    idx % result.beats_per_bar.max(1) as usize + 1
                ));
            } else {
                *info_text = Some(format!("播放头定位于 {}", format_timecode(t)));
            }
        }
    }

    // ---- 绘制 ----
    let painter = ui.painter();
    let painter = painter.with_clip_rect(rect);
    // 舞台底色
    painter.rect_filled(rect, 10.0, Color32::from_rgb(0xFD, 0xFB, 0xF7));

    let cy = rect.center().y;
    let half_h = (rect.height() / 2.0 - 8.0).max(4.0);
    let playhead = audio.position() as f32;

    draw_waveform_layer(&painter, rect, cy, half_h, result, view, playhead);
    draw_beat_layer(&painter, rect, result, view);
    draw_playhead(&painter, rect, cy, result, view, playhead);
}

/// ① 波形层
fn draw_waveform_layer(
    painter: &Painter,
    rect: Rect,
    cy: f32,
    half_h: f32,
    result: &AnalysisResult,
    view: &WaveViewState,
    playhead: f32,
) {
    // 中轴线
    painter.line_segment(
        [Pos2::new(rect.left(), cy), Pos2::new(rect.right(), cy)],
        Stroke::new(1.0_f32, BEIGE_LIGHT),
    );

    let peaks = &result.waveform_peaks;
    if peaks.is_empty() || result.duration <= 0.0 {
        return;
    }
    let duration = result.duration;
    let (t0, t1) = view.window(duration);
    let visible = (t1 - t0).max(1e-6);
    let cols = (rect.width().ceil() as usize).max(1);
    let scale = peaks.len() as f32 / duration;

    let mut played_shapes: Vec<Shape> = Vec::with_capacity(cols);
    let mut rest_shapes: Vec<Shape> = Vec::with_capacity(cols);

    for i in 0..cols {
        let x = rect.left() + i as f32;
        let ta = t0 + visible * (i as f32 / cols as f32);
        let tb = t0 + visible * ((i + 1) as f32 / cols as f32);
        let mut b0 = (ta * scale).floor() as i64;
        let mut b1 = (tb * scale).ceil() as i64;
        b0 = b0.clamp(0, peaks.len() as i64 - 1);
        b1 = b1.clamp(b0 + 1, peaks.len() as i64);

        let amp = if (b1 - b0) <= 1 {
            // 高倍放大：桶比像素还密，用线性插值避免台阶感
            let p = ta * scale;
            let i0 = p.floor() as usize;
            let frac = p - i0 as f32;
            let v0 = peaks[i0.min(peaks.len() - 1)];
            let v1 = peaks[(i0 + 1).min(peaks.len() - 1)];
            v0 + (v1 - v0) * frac
        } else {
            let mut m = 0f32;
            for k in b0..b1 {
                let v = peaks[k as usize];
                if v > m {
                    m = v;
                }
            }
            m
        };
        if amp <= 0.0005 {
            continue;
        }
        let h = (amp * half_h).max(0.5);
        let seg = Shape::LineSegment {
            points: [Pos2::new(x, cy - h), Pos2::new(x, cy + h)],
            stroke: Stroke::new(1.0_f32, Color32::TRANSPARENT),
        };
        if ta <= playhead {
            played_shapes.push(seg);
        } else {
            rest_shapes.push(seg);
        }
    }

    // 先画未播放（米色），再画已播放（橙色）；颜色统一在此替换
    let make = |shapes: Vec<Shape>, color: Color32| -> Vec<Shape> {
        shapes
            .into_iter()
            .map(|s| {
                if let Shape::LineSegment { points, .. } = s {
                    Shape::LineSegment {
                        points,
                        stroke: Stroke::new(1.0_f32, color),
                    }
                } else {
                    s
                }
            })
            .collect()
    };
    painter.extend(make(rest_shapes, BEIGE));
    painter.extend(make(played_shapes, PRIMARY));
}

/// ② 节拍层（随缩放抽稀）
fn draw_beat_layer(
    painter: &Painter,
    rect: Rect,
    result: &AnalysisResult,
    view: &WaveViewState,
) {
    let duration = result.duration;
    let (t0, t1) = view.window(duration);
    let period = 60.0 / result.bpm.max(1e-3);
    if !period.is_finite() || period <= 0.0 || result.beats.is_empty() {
        return;
    }

    let px_per_sec = rect.width() / (duration / view.zoom).max(1e-6);
    let px_per_beat = px_per_sec * period;

    // 抽稀策略：间距不足时只画 n 的倍数拍
    let stride: usize = if px_per_beat >= MIN_BEAT_SPACING {
        1
    } else if px_per_beat >= MIN_BEAT_SPACING / 4.0 {
        4
    } else if px_per_beat >= MIN_BEAT_SPACING / 16.0 {
        16
    } else {
        64
    };

    let first_idx = ((t0 - result.first_beat_offset) / period).floor() as i64 - 1;
    let mut idx = first_idx;
    while idx < (result.beats.len() as i64 + 1) {
        if idx >= 0 {
            let t = result.first_beat_offset + idx as f32 * period;
            if t > t1 {
                break;
            }
            if t >= t0 {
                let x = view.time_to_x(t, rect, duration);
                if (idx as usize) % stride == 0 {
                    let is_downbeat = (idx as usize) % result.beats_per_bar.max(1) as usize == 0;
                    if is_downbeat {
                        painter.line_segment(
                            [Pos2::new(x, rect.top() + 6.0), Pos2::new(x, rect.bottom())],
                            Stroke::new(1.5_f32, DOWNBEAT.linear_multiply(0.75)),
                        );
                        // 顶部手柄条
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(x - 6.0, rect.top() + 1.0),
                                Vec2::new(12.0, 4.0),
                            ),
                            2.0,
                            DOWNBEAT,
                        );
                    } else {
                        // 虚线（手动分段模拟）
                        let mut y = rect.top() + 6.0;
                        let mut on = true;
                        while y < rect.bottom() {
                            let len = if on { 6.0 } else { 4.0 };
                            let y_end = (y + len).min(rect.bottom());
                            if on {
                                painter.line_segment(
                                    [Pos2::new(x, y), Pos2::new(x, y_end)],
                                    Stroke::new(1.0_f32, BEAT),
                                );
                            }
                            y = y_end;
                            on = !on;
                        }
                    }
                }
            }
        }
        idx += 1;
    }
}

/// ③ 播放头
fn draw_playhead(
    painter: &Painter,
    rect: Rect,
    cy: f32,
    result: &AnalysisResult,
    view: &WaveViewState,
    playhead: f32,
) {
    let (t0, t1) = view.window(result.duration);
    if playhead < t0 || playhead > t1 {
        return;
    }
    let x = view.time_to_x(playhead, rect, result.duration);
    painter.line_segment(
        [Pos2::new(x, rect.top() - 6.0), Pos2::new(x, rect.bottom())],
        Stroke::new(1.5_f32, PRIMARY),
    );
    painter.circle_filled(Pos2::new(x, rect.top() - 6.0), 4.5, PRIMARY);
    painter.circle_filled(Pos2::new(x, cy), 3.0, PRIMARY.linear_multiply(0.6));
}

/// 播放控制行
fn draw_transport(
    ui: &mut Ui,
    rect: Rect,
    result: &AnalysisResult,
    audio: &mut AudioEngine,
    view: &mut WaveViewState,
) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            let size = 44.0;
            let symbol = if audio.is_playing() { "❚❚" } else { "▶" };
            let btn = egui::Button::new(RichText::new(symbol).size(16.0).color(Color32::WHITE))
                .fill(PRIMARY)
                .corner_radius(size / 2.0)
                .min_size(Vec2::new(size, size));
            if ui.add(btn).clicked() {
                audio.toggle();
            }

            ui.add_space(12.0);
            let pos = audio.position() as f32;
            ui.label(
                RichText::new(format_timecode(pos))
                    .size(18.0)
                    .monospace()
                    .color(TEXT),
            );
            ui.label(RichText::new(format!("/ {}", format_timecode(result.duration))).size(13.0).color(TEXT_SUB));

            ui.add_space(12.0);
            if outline_button(ui, "上一拍") {
                jump_to_beat(audio, result, -1);
            }
            if outline_button(ui, "下一拍") {
                jump_to_beat(audio, result, 1);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if outline_button(ui, "全曲视图") {
                    view.zoom = 1.0;
                    view.start_sec = 0.0;
                }
                ui.label(
                    RichText::new(format!(
                        "共 {} 拍 · {:.1} BPM",
                        result.beats.len(),
                        result.bpm
                    ))
                    .size(12.0)
                    .color(TEXT_SUB),
                );
            });
        });
    });
}

/// 描边按钮（次要操作）
pub fn outline_button(ui: &mut Ui, text: &str) -> bool {
    let btn = egui::Button::new(RichText::new(text).size(13.0).color(TEXT))
        .fill(Color32::TRANSPARENT)
        .stroke(outline_stroke())
        .corner_radius(CONTROL_RADIUS)
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(btn).clicked()
}

/// 主按钮（实心橙）
pub fn primary_button(ui: &mut Ui, text: &str) -> bool {
    let btn = egui::Button::new(RichText::new(text).size(13.0).color(Color32::WHITE))
        .fill(PRIMARY)
        .corner_radius(CONTROL_RADIUS)
        .min_size(Vec2::new(0.0, 32.0));
    ui.add(btn).clicked()
}

/// 跳转到上一拍 / 下一拍
pub fn jump_to_beat(audio: &mut AudioEngine, result: &AnalysisResult, dir: i32) {
    if result.beats.is_empty() {
        return;
    }
    let cur = audio.position() as f32;
    if dir < 0 {
        let t = result
            .beats
            .iter()
            .rev()
            .find(|&&b| b < cur - 0.02)
            .copied()
            .unwrap_or(0.0);
        audio.set_position(t as f64);
    } else {
        let t = result
            .beats
            .iter()
            .find(|&&b| b > cur + 0.02)
            .copied()
            .unwrap_or(result.duration);
        audio.set_position(t as f64);
    }
}
