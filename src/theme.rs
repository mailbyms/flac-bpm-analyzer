//! 全局视觉常量：颜色 / 尺寸 / 圆角
//! 所有可调外观参数集中在此文件，方便统一修改。

use egui::{Color32, Stroke, Vec2};

// ---------------------------------------------------------------------------
// 主题配色（严格按设计稿）
// ---------------------------------------------------------------------------

/// 应用背景：暖白 #FAF6EE
pub const APP_BG: Color32 = Color32::from_rgb(0xFA, 0xF6, 0xEE);
/// 卡片底色：纯白
pub const CARD_BG: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);
/// 主色：橙 #E07856
pub const PRIMARY: Color32 = Color32::from_rgb(0xE0, 0x78, 0x56);
/// 辅助米色 #C9BCA8（未播放波形 / 状态栏）
pub const BEIGE: Color32 = Color32::from_rgb(0xC9, 0xBC, 0xA8);
/// 更浅的米色（网格线、分隔线）
pub const BEIGE_LIGHT: Color32 = Color32::from_rgb(0xEB, 0xE3, 0xD6);
/// 卡片描边
pub const CARD_BORDER: Color32 = Color32::from_rgb(0xEC, 0xE4, 0xD8);
/// 节拍靛蓝 #5046E5
pub const BEAT: Color32 = Color32::from_rgb(0x50, 0x46, 0xE5);
/// 小节重拍深靛 #3F34D6
pub const DOWNBEAT: Color32 = Color32::from_rgb(0x3F, 0x34, 0xD6);
/// 正文色 #0F1419
pub const TEXT: Color32 = Color32::from_rgb(0x0F, 0x14, 0x19);
/// 次要文字 #8C7F6B
pub const TEXT_SUB: Color32 = Color32::from_rgb(0x8C, 0x7F, 0x6B);
/// 置信度徽章底色（绿）#E7F6EF
pub const BADGE_GREEN_BG: Color32 = Color32::from_rgb(0xE7, 0xF6, 0xEF);
/// 置信度徽章文字（绿）#0E8F63
pub const BADGE_GREEN_FG: Color32 = Color32::from_rgb(0x0E, 0x8F, 0x63);
/// 卡片阴影色（极低透明度的暖灰）
pub const SHADOW: Color32 = Color32::from_rgba_premultiplied(0x33, 0x2A, 0x1E, 0x14);

// ---------------------------------------------------------------------------
// 尺寸
// ---------------------------------------------------------------------------

/// 窗口初始尺寸
pub const WINDOW_SIZE: Vec2 = Vec2::new(1440.0, 920.0);
/// 顶栏高度
pub const TOP_BAR_H: f32 = 52.0;
/// 底部状态栏高度
pub const STATUS_BAR_H: f32 = 36.0;
/// 内容区左右留白
pub const PAGE_MARGIN: f32 = 24.0;
/// 卡片之间的间距
pub const CARD_GAP: f32 = 16.0;
/// 卡片圆角
pub const CARD_RADIUS: f32 = 16.0;
/// 小控件圆角（按钮、输入框）
pub const CONTROL_RADIUS: f32 = 8.0;
/// 指标卡固定高度（四张等高）
pub const METRIC_H: f32 = 104.0;

// ---------------------------------------------------------------------------
// 常用样式快捷函数
// ---------------------------------------------------------------------------

/// 卡片描边样式
pub fn card_stroke() -> Stroke {
    Stroke::new(1.0_f32, CARD_BORDER)
}

/// 描边按钮的默认描边
pub fn outline_stroke() -> Stroke {
    Stroke::new(1.0_f32, BEIGE)
}

/// 卡片阴影（egui 0.32 的 Shadow 使用整数偏移）
pub fn card_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: SHADOW,
    }
}
