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
/// 波形柱子密度：聚合模式下每根柱大约占多少逻辑像素（1.0 = 与像素同密度）
const BARS_PER_PIXEL: f32 = 1.0;

/// 把 x 对齐到物理像素网格。
///
/// 1px 宽的竖线若落在亚像素位置，egui 会做抗锯齿变成两个半透明像素；
/// 滚动时这条线在「实心 / 两列半透明」之间来回变化，肉眼就是「一闪一闪」。
/// 对齐到物理像素后位置只会整数跳动，亮度恒定。
fn snap_x(x: f32, ppp: f32) -> f32 {
    if ppp <= 0.0 {
        return x;
    }
    (x * ppp).round() / ppp
}

/// 手工测速：右键点击波形，把两次点击的间隔当作一个节拍来反推 BPM。
///
/// 连续点击会累积多次间隔并取平均（tap tempo），比只取两次稳得多。
#[derive(Default, Clone)]
pub struct TapTempo {
    /// 最近的点击时间（秒），最旧的在前面
    taps: Vec<f32>,
    /// 已采用的手工 BPM（用于指标卡标注；重新分析或重新测量不影响它）
    pub applied: Option<f32>,
}

impl TapTempo {
    /// 最多保留最近几次点击（取平均用）
    const MAX_TAPS: usize = 6;
    /// 两次点击的最小间隔（秒）：更短视为误触（否则会算出几百上千 BPM）
    const MIN_INTERVAL: f32 = 0.1;

    pub fn taps(&self) -> &[f32] {
        &self.taps
    }

    pub fn clear(&mut self) {
        self.taps.clear();
        self.applied = None;
    }

    /// 记录一次点击；返回是否被接受（太近的误触会被丢弃）
    pub fn on_click(&mut self, t: f32) -> bool {
        if !t.is_finite() {
            return false;
        }
        if let Some(&last) = self.taps.last() {
            if (t - last).abs() < Self::MIN_INTERVAL {
                return false;
            }
        }
        self.taps.push(t);
        if self.taps.len() > Self::MAX_TAPS {
            self.taps.remove(0);
        }
        true
    }

    /// 最近一次「前一拍 → 这一拍」的区间，用于在波形上高亮
    pub fn last_span(&self) -> Option<(f32, f32)> {
        let n = self.taps.len();
        if n < 2 {
            return None;
        }
        Some((self.taps[n - 2], self.taps[n - 1]))
    }

    /// 平均间隔（秒）
    pub fn mean_interval(&self) -> Option<f32> {
        if self.taps.len() < 2 {
            return None;
        }
        let mut sum = 0.0;
        let mut count = 0;
        for w in self.taps.windows(2) {
            let d = w[1] - w[0];
            if d >= Self::MIN_INTERVAL {
                sum += d;
                count += 1;
            }
        }
        if count == 0 {
            None
        } else {
            Some(sum / count as f32)
        }
    }

    /// 由平均间隔换算 BPM
    pub fn bpm(&self) -> Option<f32> {
        self.mean_interval()
            .map(|d| 60.0 / d)
            .filter(|b| b.is_finite() && *b > 0.0)
    }
}

/// 波形视图状态（缩放 + 起始时间 + 跟随播放头）
#[derive(Clone, Copy)]
pub struct WaveViewState {
    /// 缩放倍数：1.0 = 整曲铺满
    pub zoom: f32,
    /// 可视窗口起始时间（秒）
    pub start_sec: f32,
    /// 播放时是否让视图跟随播放头自动横向滚动
    pub follow_playhead: bool,
    /// 手动平移后的冷却截止时间（egui 时间，秒）。冷却期内不自动跟随，
    /// 否则用户刚拖开看别的段落就被拽回去。
    pub follow_hold_until: f64,
}

impl Default for WaveViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            start_sec: 0.0,
            // 默认开启：放大后播放头跑出窗口是最常见的困惑点
            follow_playhead: true,
            follow_hold_until: 0.0,
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

    /// 记录一次手动平移：冷却期内不自动跟随
    pub fn mark_manual_pan(&mut self, now: f64) {
        self.follow_hold_until = now + Self::MANUAL_PAN_HOLD;
    }

    /// 手动平移后暂停跟随的时长（秒）
    pub const MANUAL_PAN_HOLD: f64 = 2.0;

    /// 居中跟随：返回让播放头落在窗口正中的新起点；已居中则返回 None。
    ///
    /// 受曲首/曲末约束：靠近两端时窗口会先贴住边界，播放头随之偏离中心
    /// （曲首前半个窗口、曲末后半个窗口），这是居中跟随的固有行为。
    pub fn follow_target(&self, duration: f32, pos: f32) -> Option<f32> {
        let (t0, t1) = self.window(duration);
        let vis = t1 - t0;
        if vis <= 0.0 || !pos.is_finite() {
            return None;
        }
        let max_start = (duration - vis).max(0.0);
        let target = (pos - vis * 0.5).clamp(0.0, max_start);
        // 目标与当前一致（例如全曲视图、或已贴住曲首/曲末边界）就不必动
        if (target - t0).abs() < 1e-4 {
            return None;
        }
        Some(target)
    }

    /// 按需跟随播放头；返回是否发生了滚动
    pub fn follow(&mut self, duration: f32, pos: f32) -> bool {
        match self.follow_target(duration, pos) {
            Some(s) => {
                self.start_sec = s;
                true
            }
            None => false,
        }
    }
}

/// 绘制整张「波形与节拍」卡片
pub fn show_waveform_card(
    ui: &mut Ui,
    card_size: Vec2,
    result: &mut AnalysisResult,
    audio: &mut AudioEngine,
    view: &mut WaveViewState,
    tap: &mut TapTempo,
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
                draw_stage(ui, stage_r, result, audio, view, tap, info_text);
                draw_scrollbar(ui, scroll_r, view, duration);
                draw_transport(ui, transport_r, result, audio, view, tap);
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
        // 手动拖滚动条同样暂停自动跟随
        view.mark_manual_pan(ui.input(|i| i.time));
    } else if resp.clicked() {
        // 点击轨道空白处 → 把点击位置移到窗口中心（点在滑块上则不动）
        if let Some(p) = resp.interact_pointer_pos() {
            if !thumb.contains(p) {
                let r = ((p.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
                view.start_sec = (r * duration - vis * 0.5).clamp(0.0, scrollable);
                view.mark_manual_pan(ui.input(|i| i.time));
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
    tap: &mut TapTempo,
    info_text: &mut Option<String>,
) {
    let duration = result.duration;

    // ---- 交互 ----
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());

    // 右键点击 = 手工测速打点（clicked() 只认左键，所以不会误移动播放头）
    if resp.secondary_clicked() {
        if let Some(p) = resp.hover_pos() {
            let t = view.x_to_time(p.x, rect, duration).clamp(0.0, duration);
            if tap.on_click(t) {
                if let Some(bpm) = tap.bpm() {
                    *info_text = Some(format!(
                        "手工测速 · 平均间隔 {:.3}s · {:.1} BPM（已点击 {} 次）",
                        tap.mean_interval().unwrap_or(0.0),
                        bpm,
                        tap.taps().len()
                    ));
                } else {
                    *info_text = Some("手工测速：再右键点击一次确定一个节拍".to_string());
                }
            }
        }
    }
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
            // 手动滚动后短暂暂停自动跟随，避免刚拖开就被拽回播放头
            view.mark_manual_pan(ui.input(|i| i.time));
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

    let ppp = ui.ctx().pixels_per_point();
    draw_waveform_layer(&painter, rect, cy, half_h, result, view, playhead, ppp);
    draw_beat_layer(
        &painter,
        rect,
        result,
        view,
        hover_beat.map(|(i, _)| i),
        ppp,
    );
    draw_playhead(&painter, rect, cy, result, view, playhead, ppp);
    draw_tap_marks(&painter, rect, result, view, tap, ppp);

    // 光标：竖向参考线 + 时间提示（落在节拍线上时升级为节拍气泡）
    if resp.hovered() {
        if let Some(p) = resp.hover_pos() {
            draw_cursor_line(&painter, rect, p.x, ppp);
            match hover_beat {
                Some((idx, t)) => draw_beat_tooltip(&painter, rect, p, idx, t, result),
                None => {
                    let t = view
                        .x_to_time(p.x, rect, result.duration)
                        .clamp(0.0, result.duration);
                    draw_time_tooltip(&painter, rect, p, t);
                }
            }
        }
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

/// 通用浮动提示气泡：若干行文本，跟随指针、越界自动翻转，始终收敛在舞台内
fn draw_tooltip_box(
    painter: &Painter,
    rect: Rect,
    pointer: Pos2,
    lines: Vec<(std::sync::Arc<egui::Galley>, Color32)>,
) {
    if lines.is_empty() {
        return;
    }
    let pad_x = 11.0;
    let pad_y = 9.0;
    let line_gap = 3.0;
    let w = lines
        .iter()
        .map(|(g, _)| g.size().x)
        .fold(0.0_f32, f32::max)
        + pad_x * 2.0;
    let h = lines.iter().map(|(g, _)| g.size().y).sum::<f32>()
        + line_gap * (lines.len() as f32 - 1.0)
        + pad_y * 2.0;

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

    let mut cy = box_r.top() + pad_y;
    for (g, color) in lines {
        let h = g.size().y;
        painter.galley(Pos2::new(box_r.left() + pad_x, cy), g, color);
        cy += h + line_gap;
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
    let g_num = painter.layout_no_wrap(
        format!("节拍 #{}", idx + 1),
        FontId::proportional(12.0),
        TEXT_SUB,
    );
    let g_time = painter.layout_no_wrap(format_timecode(t), FontId::monospace(16.0), BEAT);
    let g_bar = painter.layout_no_wrap(
        format!("第 {} 小节 · 第 {} 拍", idx / bpb + 1, idx % bpb + 1),
        FontId::proportional(11.0),
        TEXT_SUB,
    );
    draw_tooltip_box(
        painter,
        rect,
        pointer,
        vec![(g_num, TEXT_SUB), (g_time, BEAT), (g_bar, TEXT_SUB)],
    );
}

/// 光标时间提示：指针在舞台上任何位置都实时显示该处时间码
fn draw_time_tooltip(painter: &Painter, rect: Rect, pointer: Pos2, t: f32) {
    let g = painter.layout_no_wrap(format_timecode(t), FontId::monospace(15.0), TEXT);
    draw_tooltip_box(painter, rect, pointer, vec![(g, TEXT)]);
}

/// 手工测速标记层：点击点 + 最近一个节拍区间 + BPM 胶囊
fn draw_tap_marks(
    painter: &Painter,
    rect: Rect,
    result: &AnalysisResult,
    view: &WaveViewState,
    tap: &TapTempo,
    ppp: f32,
) {
    let taps = tap.taps();
    if taps.is_empty() {
        return;
    }
    let duration = result.duration;
    let (t0, t1) = view.window(duration);
    let light = Color32::from_rgba_unmultiplied(0xE0, 0x78, 0x56, 130);

    // 最近一个节拍区间：浅橙底 + 两条实线
    if let Some((a, b)) = tap.last_span() {
        let xa = snap_x(view.time_to_x(a, rect, duration), ppp);
        let xb = snap_x(view.time_to_x(b, rect, duration), ppp);
        if xb > xa {
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(xa, rect.top()), Pos2::new(xb, rect.bottom())),
                0.0,
                Color32::from_rgba_unmultiplied(0xE0, 0x78, 0x56, 20),
            );
        }
        for x in [xa, xb] {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.5_f32, PRIMARY),
            );
        }
    }

    // 每次点击：细虚线 + 顶部圆点
    for &t in taps {
        if t < t0 || t > t1 {
            continue;
        }
        let x = snap_x(view.time_to_x(t, rect, duration), ppp);
        let mut y = rect.top();
        let mut on = true;
        while y < rect.bottom() {
            let len = if on { 5.0 } else { 4.0 };
            let y_end = (y + len).min(rect.bottom());
            if on {
                painter.line_segment(
                    [Pos2::new(x, y), Pos2::new(x, y_end)],
                    Stroke::new(1.0_f32, light),
                );
            }
            y = y_end;
            on = !on;
        }
        painter.circle_filled(Pos2::new(x, rect.top() + 4.0), 3.0, PRIMARY);
    }

    // BPM 胶囊：锚在最后一次点击处，越界自动收敛在舞台内
    if let (Some(bpm), Some(&last)) = (tap.bpm(), taps.last()) {
        let x = view.time_to_x(last, rect, duration);
        let text = format!(
            "{:.1} BPM · {:.3}s",
            bpm,
            tap.mean_interval().unwrap_or(0.0)
        );
        let g = painter.layout_no_wrap(text, FontId::monospace(14.0), Color32::WHITE);
        let pad_x = 10.0;
        let w = g.size().x + pad_x * 2.0;
        let h = g.size().y + 8.0;
        let left = (x - w / 2.0).clamp(rect.left() + 4.0, (rect.right() - 4.0 - w).max(rect.left() + 4.0));
        let box_r = Rect::from_min_size(Pos2::new(left, rect.top() + 6.0), Vec2::new(w, h));
        painter.rect_filled(box_r, 7.0, DOWNBEAT);
        painter.galley(
            Pos2::new(box_r.left() + pad_x, box_r.top() + 4.0),
            g,
            Color32::WHITE,
        );
    }
}

/// 跟随光标的竖向参考线（比播放头更淡，避免与橙色播放头混淆）
fn draw_cursor_line(painter: &Painter, rect: Rect, x: f32, ppp: f32) {
    let color = Color32::from_rgba_unmultiplied(0x8C, 0x7F, 0x6B, 70);
    let x = snap_x(x, ppp);
    painter.line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(1.0_f32, color),
    );
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
    ppp: f32,
) {
    // 中轴线
    painter.line_segment(
        [Pos2::new(rect.left(), cy), Pos2::new(rect.right(), cy)],
        Stroke::new(1.0_f32, BEIGE_LIGHT),
    );

    let bars = waveform_bars(rect, result.duration, &result.waveform_peaks, view, half_h, ppp);

    // 先画未播放（米色），再画已播放（橙色）
    let mut played_shapes: Vec<Shape> = Vec::with_capacity(bars.len());
    let mut rest_shapes: Vec<Shape> = Vec::with_capacity(bars.len());
    for bar in bars {
        let (x, h) = (bar.x, bar.h);
        let points = [Pos2::new(x, cy - h), Pos2::new(x, cy + h)];
        if bar.t <= playhead {
            played_shapes.push(Shape::LineSegment {
                points,
                stroke: Stroke::new(1.0_f32, PRIMARY),
            });
        } else {
            rest_shapes.push(Shape::LineSegment {
                points,
                stroke: Stroke::new(1.0_f32, BEIGE),
            });
        }
    }
    painter.extend(rest_shapes);
    painter.extend(played_shapes);
}

/// 一根波形柱子
#[derive(Clone, Copy, Debug)]
struct WaveBar {
    /// 屏幕 x 坐标
    x: f32,
    /// 半幅（像素）
    h: f32,
    /// 该柱代表的时间（秒）。聚合模式下是桶组中心，插值模式下是像素列左端。
    /// 与播放头比较决定颜色（已播放橙 / 未播放米色）。
    t: f32,
}

/// 计算波形柱子。
///
/// 抽成纯函数是为了可测——「滚动时柱高必须稳定」这个性质只能靠比对前后两帧来验证。
fn waveform_bars(
    rect: Rect,
    duration: f32,
    peaks: &[f32],
    view: &WaveViewState,
    half_h: f32,
    ppp: f32,
) -> Vec<WaveBar> {
    if peaks.is_empty() || duration <= 0.0 {
        return Vec::new();
    }
    let (t0, t1) = view.window(duration);
    let visible = (t1 - t0).max(1e-6);
    let cols = (rect.width().ceil() as usize).max(1);
    let scale = peaks.len() as f32 / duration;
    let bucket_per_px = scale * visible / rect.width().max(1.0);

    let mut bars: Vec<WaveBar> = Vec::with_capacity(cols);

    if bucket_per_px > 1.0 {
        // ---- 聚合模式（缩小到桶比像素密）----
        // 关键：分组锚定在**全局固定的桶网格**上（每 k 个桶一根柱），而不是「每个像素列
        // 取该列时间区间的最大值」。后者在滚动时区间滑动，最大值会在相邻桶之间跳变，
        // 柱高逐帧突变 —— 视觉上就是整片波形在闪。锚定到桶后，滚动只是整体平移。
        let k = (bucket_per_px * BARS_PER_PIXEL).round().max(1.0) as usize;
        let g_first = ((t0 * scale) / k as f32).floor() as i64 - 1;
        let g_last = ((t1 * scale) / k as f32).ceil() as i64 + 1;
        for g in g_first.max(0)..=g_last {
            let b0 = g as usize * k;
            if b0 >= peaks.len() {
                break;
            }
            let b1 = (b0 + k).min(peaks.len());
            let amp = peaks[b0..b1].iter().copied().fold(0.0f32, f32::max);
            if amp <= 0.0005 {
                continue;
            }
            let t_center = (b0 as f32 + (b1 - b0) as f32 * 0.5) / scale;
            // 这里**不做像素对齐**：柱间距约 1px，对齐会让相邻组挤到同一物理像素上互相覆盖。
            // 高度已稳定，轻微亚像素抗锯齿反而是平滑的（波形像一张图整体平移）。
            let x = view.time_to_x(t_center, rect, duration);
            bars.push(WaveBar {
                x,
                h: (amp * half_h).max(0.5),
                t: t_center,
            });
        }
    } else {
        // ---- 插值模式（放大到桶比像素稀）----
        // 每像素一列、线性插值取样：取值随 t0 连续变化，不会跳变。
        for i in 0..cols {
            let x = snap_x(rect.left() + i as f32, ppp);
            let ta = t0 + visible * (i as f32 / cols as f32);
            let p = ta * scale;
            let i0 = p.floor() as usize;
            let frac = p - i0 as f32;
            let v0 = peaks[i0.min(peaks.len() - 1)];
            let v1 = peaks[(i0 + 1).min(peaks.len() - 1)];
            let amp = v0 + (v1 - v0) * frac;
            if amp <= 0.0005 {
                continue;
            }
            bars.push(WaveBar {
                x,
                h: (amp * half_h).max(0.5),
                t: ta,
            });
        }
    }

    bars
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
    ppp: f32,
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
                // 对齐物理像素：细线在亚像素位置滚动时会忽明忽暗
                let x = snap_x(view.time_to_x(t, rect, duration), ppp);
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
    ppp: f32,
) {
    let (t0, t1) = view.window(result.duration);
    if playhead < t0 || playhead > t1 {
        return;
    }
    let x = snap_x(view.time_to_x(playhead, rect, result.duration), ppp);
    painter.line_segment(
        [Pos2::new(x, rect.top() - 6.0), Pos2::new(x, rect.bottom())],
        Stroke::new(1.5_f32, PRIMARY),
    );
    painter.circle_filled(Pos2::new(x, rect.top() - 6.0), 4.5, PRIMARY);
    painter.circle_filled(Pos2::new(x, cy), 3.0, PRIMARY.linear_multiply(0.6));
}

/// 播放/暂停按钮：图标用手绘图形而非字符字形
///
/// 用字符（❚❚ / ▶）时形状完全取决于字体，在部分字体下会渲染成两个小方框线框，
/// 既不统一也不好看。这里改成市面播放器的通用画法：
///   * 播放 = 实心三角形
///   * 暂停 = 两根实心圆角竖条
fn play_pause_button(ui: &mut Ui, size: f32, playing: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let painter = ui.painter();
    // hover 时底色略提亮，保留可点击的反馈
    let bg = if resp.hovered() {
        PRIMARY.gamma_multiply(1.12)
    } else {
        PRIMARY
    };
    painter.circle_filled(rect.center(), size / 2.0, bg);
    for shape in play_pause_shapes(rect.center(), playing) {
        painter.add(shape);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

/// 生成播放/暂停图标的形状（纯函数，便于单元测试）
fn play_pause_shapes(c: Pos2, playing: bool) -> Vec<Shape> {
    if playing {
        // 暂停：两根竖条，细长且带小圆角，视觉上「高瘦」而非方块
        let bar_w = 4.0;
        let bar_h = 18.0;
        let gap = 4.0;
        let top = c.y - bar_h / 2.0;
        let bar = |x: f32| {
            Shape::Rect(egui::epaint::RectShape::filled(
                Rect::from_min_size(Pos2::new(x, top), Vec2::new(bar_w, bar_h)),
                1.5,
                Color32::WHITE,
            ))
        };
        vec![bar(c.x - gap / 2.0 - bar_w), bar(c.x + gap / 2.0)]
    } else {
        // 播放：实心三角形；重心偏左，故整体右移 1px 让视觉居中
        let h = 18.0;
        let w = 16.0;
        let cx = c.x + 1.0;
        vec![Shape::convex_polygon(
            vec![
                Pos2::new(cx - w / 2.0, c.y - h / 2.0),
                Pos2::new(cx + w / 2.0, c.y),
                Pos2::new(cx - w / 2.0, c.y + h / 2.0),
            ],
            Color32::WHITE,
            Stroke::NONE,
        )]
    }
}

/// 播放控制行
fn draw_transport(
    ui: &mut Ui,
    rect: Rect,
    result: &mut AnalysisResult,
    audio: &mut AudioEngine,
    view: &mut WaveViewState,
    tap: &mut TapTempo,
) {
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal_centered(|ui| {
            let size = 44.0;
            if play_pause_button(ui, size, audio.is_playing()) {
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
                ui.add_space(6.0);
                if toggle_chip(
                    ui,
                    if view.follow_playhead {
                        "跟随播放头 · 开"
                    } else {
                        "跟随播放头 · 关"
                    },
                    view.follow_playhead,
                    "开启后播放时波形自动横向滚动，让播放头始终保持在窗口正中；\n手动滚动波形会暂停跟随 2 秒，避免相互打架。",
                ) {
                    view.follow_playhead = !view.follow_playhead;
                }
                ui.add_space(6.0);
                draw_tap_controls(ui, result, tap);

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

/// 手工测速的显示与操作。
///
/// 调用点在 `right_to_left` 布局里，因此**先 add 的显示在更右边**：
/// 最终视觉顺序是 `文本 · 采用 · 清除`。
fn draw_tap_controls(ui: &mut Ui, result: &mut AnalysisResult, tap: &mut TapTempo) {
    if let Some(bpm) = tap.bpm() {
        if mini_button(ui, "清除", false) {
            tap.clear();
            return;
        }
        ui.add_space(4.0);
        if mini_button(ui, "采用此 BPM", true) {
            if let Some((first, _)) = tap.last_span() {
                // 以「区间起点」作为首拍，这样节拍线会和刚量出的那一拍对齐
                apply_tap_bpm(result, bpm, first);
                tap.applied = Some(bpm);
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "手工 {:.1} BPM · 间隔 {:.3}s · {} 次",
                bpm,
                tap.mean_interval().unwrap_or(0.0),
                tap.taps().len()
            ))
            .size(12.0)
            .color(PRIMARY),
        );
    } else if tap.taps().len() == 1 {
        ui.label(
            RichText::new("再右键点击一次确定一个节拍")
                .size(12.0)
                .color(PRIMARY),
        );
    } else {
        ui.label(
            RichText::new("右键点击波形可手工测速")
                .size(11.5)
                .color(TEXT_SUB),
        );
    }
}

/// 把手工测得的 BPM 应用到分析结果：更新 BPM、首拍偏移，并按新周期重建节拍序列。
///
/// 抽成独立函数既方便测试，也避免把「改结果」的副作用散落在 UI 代码里。
pub fn apply_tap_bpm(result: &mut AnalysisResult, bpm: f32, first_beat: f32) {
    if !bpm.is_finite() || bpm <= 0.0 {
        return;
    }
    result.bpm = bpm;
    result.first_beat_offset = first_beat.max(0.0);
    result.beats = crate::analysis::dsp::generate_beats(
        result.first_beat_offset,
        60.0 / bpm,
        result.duration,
    );
}

/// 小号按钮（测量条用）
fn mini_button(ui: &mut Ui, text: &str, primary: bool) -> bool {
    let (fg, fill, stroke) = if primary {
        (Color32::WHITE, PRIMARY, Stroke::NONE)
    } else {
        (TEXT_SUB, Color32::TRANSPARENT, outline_stroke())
    };
    ui.add(
        egui::Button::new(RichText::new(text).size(12.0).color(fg))
            .fill(fill)
            .stroke(stroke)
            .corner_radius(8.0)
            .min_size(Vec2::new(0.0, 28.0)),
    )
    .clicked()
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

/// 可切换按钮：开启时实心橙 + 白字，关闭时描边灰字
fn toggle_chip(ui: &mut Ui, text: &str, active: bool, hint: &str) -> bool {
    let (fg, fill, stroke) = if active {
        (Color32::WHITE, PRIMARY, Stroke::NONE)
    } else {
        (TEXT_SUB, Color32::TRANSPARENT, outline_stroke())
    };
    let btn = egui::Button::new(RichText::new(text).size(12.5).color(fg))
        .fill(fill)
        .stroke(stroke)
        .corner_radius(CONTROL_RADIUS)
        .min_size(Vec2::new(0.0, 30.0));
    ui.add(btn).on_hover_text(hint).clicked()
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

    /// 暂停图标必须是两根「实心长竖条」，不能是字体渲染出的线框小方块
    #[test]
    fn 暂停图标是两根实心竖条() {
        let c = Pos2::new(100.0, 100.0);
        let shapes = play_pause_shapes(c, true);
        assert_eq!(shapes.len(), 2, "暂停应由两根竖条组成");

        let mut sum_cx = 0.0;
        for s in &shapes {
            match s {
                Shape::Rect(rs) => {
                    assert_eq!(rs.fill, Color32::WHITE, "竖条应为白色");
                    assert_eq!(rs.stroke.width, 0.0, "实心填充不应有描边，否则就是线框");
                    assert!(
                        rs.rect.height() > rs.rect.width() * 3.0,
                        "竖条应明显高瘦，实际 {:.1}×{:.1}",
                        rs.rect.width(),
                        rs.rect.height()
                    );
                    sum_cx += rs.rect.center().x;
                }
                _ => panic!("暂停图标应由实心矩形组成"),
            }
        }
        assert!(
            (sum_cx / 2.0 - c.x).abs() < 0.01,
            "两根竖条应关于按钮中心对称"
        );
    }

    /// 播放图标是实心三角形（与暂停竖条保持同一套视觉语言）
    #[test]
    fn 播放图标是实心三角形() {
        let shapes = play_pause_shapes(Pos2::new(100.0, 100.0), false);
        assert_eq!(shapes.len(), 1);
        match &shapes[0] {
            Shape::Path(ps) => {
                assert_eq!(ps.fill, Color32::WHITE, "三角形应为白色实心");
                assert_eq!(ps.points.len(), 3, "三角形应有三个顶点");
            }
            _ => panic!("播放图标应是实心三角形"),
        }
    }

    /// 居中跟随：播放头应被钉在窗口正中（受曲首/曲末边界约束）
    #[test]
    fn 跟随播放头保持居中() {
        let duration = 100.0;
        let mut view = WaveViewState {
            zoom: 10.0,
            start_sec: 20.0,
            ..Default::default()
        };
        let vis = view.visible_duration(duration); // 10s，窗口 [20, 30]

        // 已经在中心 → 不动
        assert_eq!(view.follow_target(duration, 25.0), None, "已居中不应滚动");

        // 偏右 → 滚动到居中
        let target = view.follow_target(duration, 26.0).expect("偏离中心应滚动");
        view.start_sec = target;
        let (t0, t1) = view.window(duration);
        assert!((26.0 - (t0 + t1) * 0.5).abs() < 1e-3, "播放头应在窗口正中");
        assert!((26.0 - t0 - vis * 0.5).abs() < 1e-3, "偏移应为半窗口");

        // 偏左（用户拖到后面又播放）同样居中
        view.start_sec = 60.0;
        let target = view.follow_target(duration, 30.0).expect("偏左应滚动");
        assert!((30.0 - target - vis * 0.5).abs() < 1e-3);

        // 曲首：窗口贴住 0，播放头无法居中（这是居中跟随的固有边界行为）
        let target = view.follow_target(duration, 1.0).expect("应滚动");
        assert_eq!(target, 0.0, "曲首应贴住起点");

        // 曲末：起点不能超过 max_start
        view.start_sec = 80.0;
        let target = view.follow_target(duration, 99.9).expect("曲末应滚动");
        assert!(
            target <= duration - vis + 1e-3,
            "起点超出可滚动范围：{target}"
        );

        // 全曲视图：整曲都在窗口内，居中位置就是起点 0
        let full = WaveViewState {
            zoom: 1.0,
            start_sec: 0.0,
            ..Default::default()
        };
        assert_eq!(full.follow_target(duration, 50.0), None);
    }

    /// 手工测速：两次点击间隔即一个节拍
    #[test]
    fn 两次点击算出bpm() {
        let mut tap = TapTempo::default();
        assert!(tap.on_click(10.0));
        assert_eq!(tap.bpm(), None, "只点一次还不能算");
        assert!(tap.on_click(10.5));
        let bpm = tap.bpm().expect("两次点击应能算出 BPM");
        assert!((bpm - 120.0).abs() < 1e-3, "间隔 0.5s 应为 120 BPM，实际 {bpm}");
        assert_eq!(tap.last_span(), Some((10.0, 10.5)));
    }

    /// 多次点击取平均（tap tempo），比单次更稳
    #[test]
    fn 多次点击取平均间隔() {
        let mut tap = TapTempo::default();
        for t in [0.0, 0.5, 1.02, 1.5] {
            tap.on_click(t);
        }
        // 间隔 0.5 / 0.52 / 0.48 → 平均 0.5
        let mean = tap.mean_interval().unwrap();
        assert!((mean - 0.5).abs() < 1e-3, "平均间隔应约 0.5s，实际 {mean}");
        assert!((tap.bpm().unwrap() - 120.0).abs() < 0.5);
    }

    /// 过近的点击视为误触，直接忽略（否则会算出几百上千 BPM）
    #[test]
    fn 过近的点击被忽略() {
        let mut tap = TapTempo::default();
        assert!(tap.on_click(5.0));
        assert!(!tap.on_click(5.02), "20ms 内应视为误触");
        assert_eq!(tap.taps().len(), 1);
        assert!(tap.on_click(5.5));
        assert_eq!(tap.taps().len(), 2);
    }

    /// 只保留最近若干次点击
    #[test]
    fn 点击次数有上限() {
        let mut tap = TapTempo::default();
        for i in 0..20 {
            tap.on_click(i as f32 * 0.5);
        }
        assert!(tap.taps().len() <= TapTempo::MAX_TAPS);
        // 保留的应是最近的（间隔仍为 0.5s）
        assert!((tap.mean_interval().unwrap() - 0.5).abs() < 1e-3);
    }

    /// 清除后回到未测量状态（同时清掉"已采用"标记）
    #[test]
    fn 清除测速状态() {
        let mut tap = TapTempo::default();
        tap.on_click(1.0);
        tap.on_click(1.5);
        tap.applied = Some(120.0);
        tap.clear();
        assert!(tap.taps().is_empty());
        assert_eq!(tap.bpm(), None);
        assert_eq!(tap.applied, None);
    }

    /// 采用手工 BPM：更新数值、首拍偏移并重建节拍序列
    #[test]
    fn 采用手工bpm会重建节拍序列() {
        let mut r = fake_result(); // 60s / 120 BPM / 首拍 0
        apply_tap_bpm(&mut r, 60.0, 1.0);
        assert!((r.bpm - 60.0).abs() < 1e-4);
        assert!((r.first_beat_offset - 1.0).abs() < 1e-4);
        assert!((r.beats[0] - 1.0).abs() < 1e-4, "首拍应对齐测量起点");
        assert!((r.beats[1] - 2.0).abs() < 1e-4, "周期应为 1s");
        assert_eq!(r.beats.len(), 59, "1s 起、1s 周期、到 60s 共 59 拍");
    }

    /// 滚动时柱高必须稳定：否则柱高逐帧跳变会让整片波形「一闪一闪」。
    ///
    /// 造一份有区分度的 peaks，比较滚动前后的柱高集合——聚合模式锚定在固定桶网格上，
    /// 滚动只是整体平移，柱高应几乎完全一致（只有边缘进出可视区的少数柱子不同）。
    #[test]
    fn 滚动时波形柱高保持稳定() {
        let duration = 240.0;
        let n = 8192;
        // 有区分度的伪随机包络（不用 rand，保证可复现）
        let mut seed = 12345u64;
        let peaks: Vec<f32> = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let r = ((seed >> 33) as f32) / (u32::MAX as f32);
                (0.15 + 0.8 * r) * (0.6 + 0.4 * ((i as f32) * 0.01).sin())
            })
            .collect();

        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1376.0, 300.0));
        let half_h = 140.0;
        // 必须 zoom > 1：整曲视图下 start_sec 会被 clamp 到 0，两帧其实是同一个视图
        // zoom=2 → 可视 120s，bucket_per_px≈2.98 → 聚合模式 k=3
        let mk = |start: f32| WaveViewState {
            zoom: 2.0,
            start_sec: start,
            ..Default::default()
        };

        // 滚动 0.05s ≈ 0.57px，匹配窗口取 1.0px 即可稳定对齐同一根柱子
        let bars_a = waveform_bars(rect, duration, &peaks, &mk(60.0), half_h, 1.0);
        let bars_b = waveform_bars(rect, duration, &peaks, &mk(60.05), half_h, 1.0);
        assert!(bars_a.len() > 500, "应有足够多的柱子（{}）", bars_a.len());

        // 按「柱子代表的时间」匹配：聚合模式的组网格固定，同一组在两帧里 t 完全相同，
        // 所以只比较同名 t 的柱高即可。这样不会因为 x 只差 1px 而误配到相邻组。
        let key = |t: f32| (t * 1000.0).round() as i64;
        let map_b: std::collections::HashMap<i64, f32> =
            bars_b.iter().map(|b| (key(b.t), b.h)).collect();

        let mut same = 0usize;
        let mut compared = 0usize;
        for b in &bars_a {
            if let Some(hb) = map_b.get(&key(b.t)) {
                compared += 1;
                if (b.h - hb).abs() < 1e-3 {
                    same += 1;
                }
            }
        }
        assert!(compared > 300, "可比较的柱子太少（{compared}）");
        let ratio = same as f32 / compared as f32;
        assert!(
            ratio > 0.98,
            "同一时间位置的柱高应完全稳定（实际稳定率 {:.1}%），否则滚动时波形会闪",
            ratio * 100.0
        );
    }

    /// 手动平移会开启 2 秒冷却，冷却期内不自动跟随
    #[test]
    fn 手动平移暂停跟随两秒() {
        let mut view = WaveViewState::default();
        view.mark_manual_pan(10.0);
        assert!((view.follow_hold_until - 12.0).abs() < 1e-6);
        // follow 本身不看冷却（由 app 层判断），这里只验证时间戳写对
        assert!(view.follow_hold_until > 10.0);
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
        let mut view = WaveViewState { zoom: 4.0, start_sec: 0.0, ..Default::default() };
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
        let mut view = WaveViewState { zoom: 10.0, start_sec: 0.0, ..Default::default() };
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
        let view = WaveViewState { zoom: 1.0, start_sec: 0.0, ..Default::default() };

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
        let view = WaveViewState { zoom: 8.0, start_sec: 1.0, ..Default::default() };
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
        let view = WaveViewState { zoom: 8.0, start_sec: 20.0, ..Default::default() };
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
