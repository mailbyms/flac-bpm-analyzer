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

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([1120.0, 760.0]),
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
