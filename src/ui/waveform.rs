//! 波形与节拍卡：工具栏 / 时间刻度 / 舞台（波形层 + 节拍层 + 播放头）/ 播放控制
//!
//! 分层顺序（严格按要求）：
//!   ① 波形层（已播放橙 / 未播放米色 / 中轴线）
//!   ② 节拍（拍：靛蓝虚线；小节重拍：深靛实线 + 顶部手柄条）
//!   ③ 播放头（橙色竖线 + 圆点手柄）

use egui::{
    Align2, Color32, CursorIcon, FontId, Painter, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
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
/// 水平滚动条高度（放大后用于横向浏览整轨）
const SCROLL_H: f32 = 14.0;
/// 滚动条滑块的最小宽度（像素），保证任何缩放下都抓得住
const SCROLL_THUMB_MIN_W: f32 = 32.0;
/// 悬停命中节拍线的容差（像素）
const BEAT_HIT_TOLERANCE: f32 = 5.0;
/// 判定「本帧真的滚了滚轮」的阈值。egui 的行距倍率为 40，一档滚轮的原始增量远大于此；
/// 设这个阈值是为了过滤触控板的微小抖动，同时保证只按 Ctrl 时（原始增量为 0）绝不触发缩放。
const REAL_SCROLL_THRESHOLD: f32 = 4.0;
/// Ctrl + 滚轮每滚一档的缩放倍数
const ZOOM_PER_NOTCH: f32 = 1.15;

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
                // 水平滚动条：紧贴舞台下方、播放控制行上方
                let scroll_r = Rect::from_min_size(
                    Pos2::new(inner.left(), transport_r.top() - 10.0 - SCROLL_H),
                    Vec2::new(inner.width(), SCROLL_H),
                );
                let stage_r = Rect::from_min_max(
                    ruler_r.left_bottom() + Vec2::new(0.0, 6.0),
                    Pos2::new(inner.right(), scroll_r.top() - 8.0),
                );

                draw_toolbar(ui, toolbar_r, view, duration);
                draw_ruler(ui, ruler_r, result, view);
                draw_stage(ui, stage_r, result, audio, view, info_text);
                draw_scrollbar(ui, scroll_r, view, duration);
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

/// 水平滚动条：拖动滑块平移可视窗口，点击轨道跳转到该处
///
/// 映射规则：`Δt = Δpx * duration / (track_w - thumb_w)`，
/// 等价于滑块位移 = 鼠标位移，即拖动时滑块严格跟手。
fn draw_scrollbar(ui: &mut Ui, rect: Rect, view: &mut WaveViewState, duration: f32) {
    if duration <= 0.0 {
        return;
    }
    let (t0, t1) = view.window(duration);
    let vis = t1 - t0;

    // 轨道（含上下 6px 的命中区，方便抓取）
    let track_h = 8.0;
    let track = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), track_h));
    let hit = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), rect.height() + 6.0));

    let ratio = (vis / duration).clamp(0.0, 1.0);
    let thumb_w = (track.width() * ratio).max(SCROLL_THUMB_MIN_W).min(track.width());
    let scrollable = (duration - vis).max(0.0);
    let thumb_x = track.left() + (t0 / duration).clamp(0.0, 1.0) * (track.width() - thumb_w).max(0.0);
    let thumb = Rect::from_min_size(Pos2::new(thumb_x, track.top()), Vec2::new(thumb_w, track_h));

    let resp = ui
        .allocate_rect(hit, Sense::click_and_drag())
        .on_hover_cursor(CursorIcon::Grab);
    let active = resp.dragged();
    let hovered = resp.hovered() || thumb.contains(
        ui.input(|i| i.pointer.hover_pos()).unwrap_or(Pos2::new(-999.0, -999.0)),
    );

    let painter = ui.painter();
    // 轨道底色
    painter.rect_filled(track, track_h / 2.0, Color32::from_rgb(0xF3, 0xED, 0xE2));
    // 滑块
    let thumb_color = if active {
        PRIMARY
    } else if hovered {
        PRIMARY.linear_multiply(0.72)
    } else {
        BEIGE
    };
    painter.rect_filled(thumb, track_h / 2.0, thumb_color);
    if ratio >= 1.0 {
        // 全曲视图：滑块铺满，无内容可滚动
        painter.rect_filled(thumb, track_h / 2.0, BEIGE_LIGHT);
    }

    // ---- 交互 ----
    let drag_x = resp.drag_delta().x;
    if drag_x != 0.0 {
        let max_off = (track.width() - thumb_w).max(1.0);
        view.pan(drag_x / max_off * duration, duration);
    } else if resp.clicked() {
        // 点击轨道空白处 → 把点击位置移到窗口中心（点在滑块上则不动）
        if let Some(p) = resp.interact_pointer_pos() {
            if !thumb.contains(p) {
                let r = ((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
                view.start_sec = (r * duration - vis * 0.5).clamp(0.0, scrollable);
            }
        }
    }
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
    let (ctrl_or_cmd, shift, raw_scroll, wheel_delta, zoom_delta) = ui.input(|i| {
        (
            i.modifiers.ctrl || i.modifiers.command,
            i.modifiers.shift,
            // 本帧「真的发生了」的滚轮增量：每帧重新累加、不做平滑。
            // 必须用它来判定用户是否在滚，因为 smooth_scroll_delta 会在滚轮停下后
            // 继续释放若干帧余量——若拿平滑值判定，滚完再按住 Ctrl 就会持续缩放。
            i.raw_scroll_delta.y + i.raw_scroll_delta.x,
            i.smooth_scroll_delta.y + i.smooth_scroll_delta.x,
            i.zoom_delta(),
        )
    });

    if resp.hovered() {
        if ctrl_or_cmd {
            // ① Ctrl/⌘ + 滚轮 → 以指针为锚点缩放
            //    只按着 Ctrl 而滚轮没动时（raw_scroll == 0）什么都不做，波形保持不动
            if raw_scroll.abs() > REAL_SCROLL_THRESHOLD {
                let anchor = anchor_ratio(ui, rect);
                let factor = ZOOM_PER_NOTCH.powf(raw_scroll.signum());
                view.set_zoom(view.zoom * factor, anchor, duration);
            }
        } else if zoom_delta != 1.0 {
            // ② 触控板双指捏合 → 以指针为锚点缩放
            view.set_zoom(view.zoom * zoom_delta, anchor_ratio(ui, rect), duration);
        } else if wheel_delta != 0.0 {
            // ③ 滚轮 → 横向平移（Shift 步长按 3 倍，用于快速浏览）
            //    步长按可视时长比例给，缩放到任意级别手感一致
            let vis = view.visible_duration(duration);
            let speed = if shift { 0.012 } else { 0.004 };
            view.pan(-wheel_delta * speed * vis, duration);
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

    // 悬停命中检测：必须在绘制前算出，以便高亮该节拍线
    let hover_beat = if resp.hovered() {
        resp.hover_pos().and_then(|p| hovered_beat(p, rect, result, view))
    } else {
        None
    };

    draw_waveform_layer(&painter, rect, cy, half_h, result, view, playhead);
    draw_beat_layer(&painter, rect, result, view, hover_beat.map(|(i, _)| i));
    draw_playhead(&painter, rect, cy, result, view, playhead);

    // 悬浮提示画在最上层
    if let (Some((idx, t)), Some(p)) = (hover_beat, resp.hover_pos()) {
        draw_beat_tooltip(&painter, rect, p, idx, t, result);
    }
}

/// 命中测试：找出鼠标 x 附近、且当前确实被绘制出来的那条节拍线
fn hovered_beat(
    pos: Pos2,
    rect: Rect,
    result: &AnalysisResult,
    view: &WaveViewState,
) -> Option<(usize, f32)> {
    let duration = result.duration;
    let period = 60.0 / result.bpm.max(1e-3);
    if !period.is_finite() || period <= 0.0 || result.beats.is_empty() || duration <= 0.0 {
        return None;
    }
    let (t0, t1) = view.window(duration);
    let px_per_beat = (rect.width() / (duration / view.zoom).max(1e-6)) * period;
    let stride = beat_stride(px_per_beat);

    let center = ((view.x_to_time(pos.x, rect, duration) - result.first_beat_offset) / period).round();
    let mut best: Option<(f32, usize, f32)> = None; // (像素距离, 拍号索引, 时间)
    for d in -2i64..=2 {
        let idx = center as i64 + d;
        if idx < 0 || (idx as usize) >= result.beats.len() {
            continue;
        }
        if (idx as usize) % stride != 0 {
            continue; // 被抽稀掉的拍不参与命中
        }
        let t = result.first_beat_offset + idx as f32 * period;
        if t < t0 || t > t1 {
            continue;
        }
        let dist = (view.time_to_x(t, rect, duration) - pos.x).abs();
        if dist <= BEAT_HIT_TOLERANCE && best.map_or(true, |b| dist < b.0) {
            best = Some((dist, idx as usize, t));
        }
    }
    best.map(|(_, i, t)| (i, t))
}

/// 节拍抽稀步长：间距不足时只画 n 的倍数拍（1 / 4 / 16 / 64）
fn beat_stride(px_per_beat: f32) -> usize {
    if px_per_beat >= MIN_BEAT_SPACING {
        1
    } else if px_per_beat >= MIN_BEAT_SPACING / 4.0 {
        4
    } else if px_per_beat >= MIN_BEAT_SPACING / 16.0 {
        16
    } else {
        64
    }
}

/// 节拍悬浮提示：拍号 + 时间码 + 小节内位置
fn draw_beat_tooltip(
    painter: &Painter,
    rect: Rect,
    pointer: Pos2,
    idx: usize,
    t: f32,
    result: &AnalysisResult,
) {
    let bpb = result.beats_per_bar.max(1) as usize;
    let g_num = painter.layout_no_wrap(format!("节拍 #{}", idx + 1), FontId::proportional(12.0), TEXT_SUB);
    let g_time = painter.layout_no_wrap(format_timecode(t), FontId::monospace(16.0), BEAT);
    let g_bar = painter.layout_no_wrap(
        format!("第 {} 小节 · 第 {} 拍", idx / bpb + 1, idx % bpb + 1),
        FontId::proportional(11.0),
        TEXT_SUB,
    );

    let pad_x = 11.0;
    let pad_y = 9.0;
    let line_gap = 3.0;
    let w = [g_num.size().x, g_time.size().x, g_bar.size().x]
        .iter()
        .copied()
        .fold(0.0_f32, f32::max)
        + pad_x * 2.0;
    let h = g_num.size().y + g_time.size().y + g_bar.size().y + line_gap * 2.0 + pad_y * 2.0;

    // 跟随指针，越界则翻转，最终收敛在舞台内
    let mut x = pointer.x + 16.0;
    if x + w > rect.right() - 4.0 {
        x = pointer.x - 16.0 - w;
    }
    x = x.clamp(rect.left() + 4.0, (rect.right() - 4.0 - w).max(rect.left() + 4.0));
    let mut y = pointer.y + 18.0;
    if y + h > rect.bottom() - 4.0 {
        y = pointer.y - 18.0 - h;
    }
    y = y.clamp(rect.top() + 4.0, (rect.bottom() - 4.0 - h).max(rect.top() + 4.0));

    let box_r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
    // 阴影 + 白底 + 细描边
    painter.rect_filled(
        box_r.translate(Vec2::new(0.0, 2.0)),
        8.0,
        Color32::from_rgba_premultiplied(0x33, 0x2A, 0x1E, 0x30),
    );
    painter.rect_filled(box_r, 8.0, Color32::WHITE);
    painter.rect_stroke(box_r, 8.0, Stroke::new(1.0_f32, BEIGE_LIGHT), StrokeKind::Outside);

    // 先量出各行高度，避免 galley 被 move 后再借用
    let h_num = g_num.size().y;
    let h_time = g_time.size().y;

    let mut cy = box_r.top() + pad_y;
    painter.galley(Pos2::new(box_r.left() + pad_x, cy), g_num, TEXT_SUB);
    cy += h_num + line_gap;
    painter.galley(Pos2::new(box_r.left() + pad_x, cy), g_time, BEAT);
    cy += h_time + line_gap;
    painter.galley(Pos2::new(box_r.left() + pad_x, cy), g_bar, TEXT_SUB);
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

/// 指针在可视窗口内的相对位置（0~1），用作缩放锚点
fn anchor_ratio(ui: &Ui, rect: Rect) -> f32 {
    let pointer_x = ui
        .input(|i| i.pointer.hover_pos().map(|p| p.x))
        .unwrap_or(rect.center().x);
    ((pointer_x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0)
}

/// ② 节拍层（随缩放抽稀）
///
/// `hover`：当前鼠标悬停的拍号索引（从 0 起），该拍会加粗、实线并加顶部手柄条
fn draw_beat_layer(
    painter: &Painter,
    rect: Rect,
    result: &AnalysisResult,
    view: &WaveViewState,
    hover: Option<usize>,
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
    let stride = beat_stride(px_per_beat);

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
                    let hi = hover == Some(idx as usize);
                    let (line_color, handle_color) = if is_downbeat {
                        (DOWNBEAT, DOWNBEAT)
                    } else {
                        (BEAT, BEAT)
                    };
                    if is_downbeat || hi {
                        // 小节重拍 / 悬停中的拍：实线
                        let width: f32 = if hi { 2.5 } else { 1.5 };
                        painter.line_segment(
                            [Pos2::new(x, rect.top() + 6.0), Pos2::new(x, rect.bottom())],
                            Stroke::new(width, if hi { line_color } else { line_color.linear_multiply(0.75) }),
                        );
                        // 顶部手柄条
                        let hw = if hi { 16.0 } else { 12.0 };
                        painter.rect_filled(
                            Rect::from_min_size(
                                Pos2::new(x - hw / 2.0, rect.top() + 1.0),
                                Vec2::new(hw, 4.0),
                            ),
                            2.0,
                            handle_color,
                        );
                    } else {
                        // 普通拍：虚线（手动分段模拟）
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 60 秒 / 120 BPM（0.5s 一拍）的结果，首拍偏移为 0
    fn fake_result() -> AnalysisResult {
        let period = 0.5;
        let beats: Vec<f32> = (0..120).map(|i| i as f32 * period).collect();
        AnalysisResult {
            bpm: 120.0,
            confidence: 0.9,
            first_beat_offset: 0.0,
            beats,
            duration: 60.0,
            sample_rate: 44100,
            bits_per_sample: 16,
            channels: 2,
            waveform_peaks: vec![0.5; 4096],
            beats_per_bar: 4,
        }
    }

    fn stage_rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 300.0))
    }

    /// 复算滑块左端 x，用于验证「拖动跟手」
    fn thumb_x(view: &WaveViewState, duration: f32, track_w: f32) -> f32 {
        let (t0, t1) = view.window(duration);
        let ratio = ((t1 - t0) / duration).clamp(0.0, 1.0);
        let thumb_w = (track_w * ratio).max(SCROLL_THUMB_MIN_W).min(track_w);
        (t0 / duration).clamp(0.0, 1.0) * (track_w - thumb_w).max(0.0)
    }

    #[test]
    fn 抽稀步长随间距变化() {
        assert_eq!(beat_stride(20.0), 1);
        assert_eq!(beat_stride(6.0), 1);
        assert_eq!(beat_stride(5.9), 4);
        assert_eq!(beat_stride(1.0), 16);
        assert_eq!(beat_stride(0.1), 64);
    }

    #[test]
    fn 滚动条拖动滑块跟手() {
        let duration = 100.0;
        let track_w = 1000.0;
        let mut view = WaveViewState { zoom: 4.0, start_sec: 0.0 };
        let (t0, t1) = view.window(duration);
        let vis = t1 - t0;
        let thumb_w = (track_w * (vis / duration)).max(SCROLL_THUMB_MIN_W).min(track_w);
        let max_off = (track_w - thumb_w).max(1.0);

        let before = thumb_x(&view, duration, track_w);
        // 模拟向右拖动 120px
        view.pan(120.0 / max_off * duration, duration);
        let after = thumb_x(&view, duration, track_w);
        assert!(
            (after - before - 120.0).abs() < 0.5,
            "滑块应随鼠标移动 120px，实际 {:.2}px",
            after - before
        );
    }

    #[test]
    fn 滚动条平移不越界() {
        let duration = 100.0;
        let mut view = WaveViewState { zoom: 10.0, start_sec: 0.0 };
        view.pan(-999.0, duration);
        assert_eq!(view.window(duration).0, 0.0);
        view.pan(999.0, duration);
        let (t0, t1) = view.window(duration);
        assert!((t1 - duration).abs() < 1e-3, "右端应贴到曲末，实际 {t1}");
        assert!(t0 > 0.0);
    }

    #[test]
    fn 悬停命中节拍线() {
        let r = fake_result();
        let rect = stage_rect();
        let view = WaveViewState { zoom: 1.0, start_sec: 0.0 };

        // 全曲视图：第 4 拍（0 基）在 t=2.0s → x = 2/60*1000
        let x = view.time_to_x(2.0, rect, r.duration);
        let hit = hovered_beat(Pos2::new(x, 150.0), rect, &r, &view);
        assert_eq!(hit.map(|(i, _)| i), Some(4));
        assert!((hit.unwrap().1 - 2.0).abs() < 1e-4);

        // 偏 3px 仍命中同一拍
        let hit2 = hovered_beat(Pos2::new(x + 3.0, 150.0), rect, &r, &view);
        assert_eq!(hit2.map(|(i, _)| i), Some(4));

        // 小节重拍（第 8 拍 = 第 3 小节第 1 拍）
        let xd = view.time_to_x(4.0, rect, r.duration);
        assert_eq!(hovered_beat(Pos2::new(xd, 10.0), rect, &r, &view).map(|(i, _)| i), Some(8));
    }

    #[test]
    fn 放大后两拍之间不误命中() {
        let r = fake_result();
        let rect = stage_rect();
        let view = WaveViewState { zoom: 8.0, start_sec: 1.0 };
        // 窗口 [1.0, 8.5]，t=2.0 与 t=2.5 之间取中点 2.25s
        let x = view.time_to_x(2.25, rect, r.duration);
        assert!(hovered_beat(Pos2::new(x, 150.0), rect, &r, &view).is_none());
        // 正好落在 t=2.5 的线上应命中
        let x2 = view.time_to_x(2.5, rect, r.duration);
        assert_eq!(hovered_beat(Pos2::new(x2, 150.0), rect, &r, &view).map(|(i, _)| i), Some(5));
    }

    #[test]
    fn 窗口外的拍不参与命中() {
        let r = fake_result();
        let rect = stage_rect();
        let view = WaveViewState { zoom: 8.0, start_sec: 20.0 };
        // t=2.0 在窗口 [20, 27.5] 之外
        let x = view.time_to_x(2.0, rect, r.duration);
        assert!(hovered_beat(Pos2::new(x, 150.0), rect, &r, &view).is_none());
    }
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
