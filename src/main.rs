//! 程序入口：eframe 启动 + 中文字体注册

mod analysis;
pub mod app;
mod audio;
mod export;
mod selftest;
mod theme;
mod ui;

use app::FlacBpmApp;
use theme::WINDOW_SIZE;

fn main() -> Result<(), eframe::Error> {
    // 隐藏命令行模式：不启动界面，直接跑流水线 / 自检
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(if selftest::run() { 0 } else { 1 });
    }
    if let Some(pos) = args.iter().position(|a| a == "--analyze") {
        if let Some(file) = args.get(pos + 1) {
            selftest::analyze_file(std::path::Path::new(file));
            return Ok(());
        }
    }
    if let Some(pos) = args.iter().position(|a| a == "--playtest") {
        if let Some(file) = args.get(pos + 1) {
            selftest::playtest(std::path::Path::new(file));
            return Ok(());
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([1120.0, 760.0])
            .with_icon(app_icon()),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "FLAC 节拍分析器",
        options,
        Box::new(move |cc| {
            FlacBpmApp::configure(&cc.egui_ctx);
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(FlacBpmApp::new()))
        }),
    )
}

/// 窗口 / 任务栏图标：与界面左上角那个应用图标同源（橙色圆角方块 + 白色波形）
///
/// 用 256×256 原始 RGBA 直接喂给 egui，不依赖任何图片解码器；
/// 该文件由 `python tools/gen_icon.py` 生成，和 `assets/icon.ico` 出自同一套几何。
fn app_icon() -> egui::IconData {
    const RGBA: &[u8] = include_bytes!("../assets/icon_256.rgba");
    egui::IconData {
        rgba: RGBA.to_vec(),
        width: 256,
        height: 256,
    }
}

/// 注册系统中文字体（否则中文会显示为方块）
fn setup_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};

    let candidates = [
        ("MicrosoftYaHei", "C:/Windows/Fonts/msyh.ttc"),
        ("MicrosoftYaHeiUI", "C:/Windows/Fonts/msyhl.ttc"),
        ("SimHei", "C:/Windows/Fonts/simhei.ttf"),
        ("DengXian", "C:/Windows/Fonts/Deng.ttf"),
    ];

    let mut fonts = FontDefinitions::default();
    let mut loaded = false;
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let data = std::sync::Arc::new(FontData::from_owned(bytes));
            fonts.font_data.insert(name.to_owned(), data);
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .insert(0, name.to_owned());
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, name.to_owned());
            loaded = true;
        }
    }
    if loaded {
        ctx.set_fonts(fonts);
    }
}

/// 全局样式：暖白底 + 圆角
fn setup_style(ctx: &egui::Context) {
    use egui::{CornerRadius, Style};

    let mut style = Style::default();
    style.visuals.panel_fill = theme::APP_BG;
    style.visuals.window_fill = theme::APP_BG;
    style.visuals.override_text_color = Some(theme::TEXT);
    style.spacing.item_spacing = egui::vec2(6.0, 6.0);
    style.visuals.widgets.noninteractive.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(8);
    ctx.set_style(style);
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    /// 窗口图标直接吃原始 RGBA 字节，尺寸/长度对不上会导致运行期不显示图标
    #[test]
    fn 窗口图标数据完整() {
        let icon = app_icon();
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(
            icon.rgba.len(),
            256 * 256 * 4,
            "RGBA 字节数应与 256×256 匹配"
        );

        // 中心：波形或底色，必须不透明
        let center = (128 * 256 + 128) * 4;
        assert_eq!(icon.rgba[center + 3], 255, "中心像素应完全不透明");

        // 左上角落在圆角之外，应透明（证明带 alpha 通道，不是白底方块）
        assert_eq!(icon.rgba[3], 0, "左上角应在圆角外，alpha 为 0");
    }
}
