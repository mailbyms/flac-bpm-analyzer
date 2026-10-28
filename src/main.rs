//! 程序入口：默认启动界面（`gui` feature）；命令行模式不创建窗口，可在终端 / 服务器运行
//!
//! 构建组合（见 `Cargo.toml` 的 `[features]`）：
//! * 默认：界面 + 音频输出（桌面版）
//! * `--no-default-features`：纯命令行分析，不链接 GUI 与音频库
//! * `--no-default-features --features playback`：命令行 + 播放自检

mod analysis;
// `test` 也放进来：纯命令行构建下仍要能跑 audio 的「无播放后端」单元测试；
// 没有界面时它只服务 --playtest 与测试，界面专用方法会成为死代码，单独放行。
#[cfg(any(feature = "gui", feature = "playback", test))]
#[cfg_attr(not(feature = "gui"), allow(dead_code))]
mod audio;
#[cfg(feature = "gui")]
pub mod app;
#[cfg(feature = "gui")]
mod export;
mod selftest;
#[cfg(feature = "gui")]
mod theme;
#[cfg(feature = "gui")]
mod ui;

use std::process::ExitCode;

#[cfg(feature = "gui")]
use theme::WINDOW_SIZE;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--selftest") {
        return if selftest::run() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if args.iter().any(|a| a == "--analyze") {
        return analyze_cli(&args);
    }
    #[cfg(feature = "playback")]
    if let Some(file) = values_after(&args, "--playtest").into_iter().next() {
        selftest::playtest(std::path::Path::new(&file));
        return ExitCode::SUCCESS;
    }

    // 关闭 playback 的构建里没有 --playtest，给出准确提示而不是「无法识别的参数」
    #[cfg(not(feature = "playback"))]
    if args.iter().any(|a| a == "--playtest") {
        eprintln!("本构建未包含音频输出（playback feature 关闭），--playtest 不可用。");
        eprintln!("重新构建：cargo build --release --features playback");
        return ExitCode::from(2);
    }

    if !args.is_empty() {
        eprintln!("无法识别的参数：{}", args.join(" "));
        eprintln!("用 --help 查看用法。");
        return ExitCode::from(2);
    }

    #[cfg(feature = "gui")]
    {
        run_gui()
    }
    #[cfg(not(feature = "gui"))]
    {
        eprintln!("当前是无界面构建（gui feature 已关闭），不会打开窗口。");
        eprintln!("用法：flac-bpm-analyzer --analyze <文件...>，完整用法见 --help");
        ExitCode::from(2)
    }
}

/// 命令行分析：支持一次给多个文件，任一失败则退出码为 1
fn analyze_cli(args: &[String]) -> ExitCode {
    let files = values_after(args, "--analyze");
    if files.is_empty() {
        eprintln!("--analyze 后面需要至少一个音频文件路径（FLAC / MP3）");
        return ExitCode::from(2);
    }

    let mut ok: Vec<(&str, f32, f32)> = Vec::new();
    let mut failed = 0usize;
    for (i, file) in files.iter().enumerate() {
        if i > 0 {
            // 多文件时用空行分段，单文件输出与旧版逐字一致（批量脚本按行解析）
            println!();
        }
        match selftest::analyze_file(std::path::Path::new(file)) {
            Some(result) => ok.push((file.as_str(), result.bpm, result.confidence)),
            None => failed += 1,
        }
    }

    // 多文件时补一张汇总表，批量核对时不用往上翻屏
    if files.len() > 1 && !ok.is_empty() {
        println!("\n=== 汇总 ===");
        for (name, bpm, confidence) in &ok {
            println!("{name:<44}{bpm:>7.2} BPM   置信度 {:>3.0}%", confidence * 100.0);
        }
    }

    if failed > 0 {
        eprintln!("共 {} 个文件，其中 {} 个分析失败", files.len(), failed);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// 取 `flag` 之后直到下一个以 `-` 开头的参数之前的全部值
fn values_after(args: &[String], flag: &str) -> Vec<String> {
    let Some(pos) = args.iter().position(|a| a == flag) else {
        return Vec::new();
    };
    args[pos + 1..]
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .cloned()
        .collect()
}

fn print_help() {
    println!("音频节拍分析器 {}", env!("CARGO_PKG_VERSION"));
    println!("分析单个 FLAC / MP3 的 BPM，输出节拍时间点与波形（界面版）或直接打印结果（命令行版）");
    println!();
    println!("用法：");
    #[cfg(feature = "gui")]
    println!("  flac-bpm-analyzer                       启动图形界面");
    #[cfg(not(feature = "gui"))]
    println!("  （本构建不含图形界面）");
    println!("  flac-bpm-analyzer --analyze <文件...>   命令行分析一个或多个 FLAC / MP3");
    println!("  flac-bpm-analyzer --selftest            DSP 自检（合成鼓点，无需音频文件）");
    #[cfg(feature = "playback")]
    println!("  flac-bpm-analyzer --playtest <文件>      播放链路自检（需要可用音频设备）");
    println!("  flac-bpm-analyzer -V | --version        打印版本");
    println!("  flac-bpm-analyzer -h | --help           本帮助");
    println!();
    println!("退出码：0 成功 / 1 分析或自检失败 / 2 参数错误");
    println!("当前构建：{}", feature_summary());
}

/// 当前构建包含的能力，用于 `--help` 与排查（例：`界面 + 音频输出`）
fn feature_summary() -> String {
    let gui = if cfg!(feature = "gui") {
        "界面"
    } else {
        "无界面"
    };
    let playback = if cfg!(feature = "playback") {
        "音频输出"
    } else {
        "无音频输出"
    };
    format!("{gui} + {playback}")
}

#[cfg(feature = "gui")]
fn run_gui() -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size([1120.0, 760.0])
            .with_icon(app_icon()),
        centered: true,
        ..Default::default()
    };

    let spawned = eframe::run_native(
        "音频节拍分析器",
        options,
        Box::new(move |cc| {
            FlacBpmApp::configure(&cc.egui_ctx);
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(FlacBpmApp::new()))
        }),
    );

    match spawned {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("启动界面失败：{e}");
            eprintln!("若机器没有显示器，改用 --analyze <文件> 在命令行分析。");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "gui")]
use app::FlacBpmApp;

/// 窗口 / 任务栏图标：与界面左上角那个应用图标同源（橙色圆角方块 + 白色波形）
///
/// 用 256×256 原始 RGBA 直接喂给 egui，不依赖任何图片解码器；
/// 该文件由 `python tools/gen_icon.py` 生成，和 `assets/icon.ico` 出自同一套几何。
#[cfg(feature = "gui")]
fn app_icon() -> egui::IconData {
    const RGBA: &[u8] = include_bytes!("../assets/icon_256.rgba");
    egui::IconData {
        rgba: RGBA.to_vec(),
        width: 256,
        height: 256,
    }
}

/// 注册系统中文字体（否则中文会显示为方块）
#[cfg(feature = "gui")]
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
#[cfg(feature = "gui")]
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
mod cli_tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 解析分析参数() {
        assert_eq!(
            values_after(&args(&["--analyze", "a.flac", "b.mp3"]), "--analyze"),
            vec!["a.flac".to_string(), "b.mp3".to_string()]
        );
    }

    #[test]
    fn 分析参数遇到下一个开关就停() {
        assert_eq!(
            values_after(&args(&["--analyze", "a.flac", "--selftest"]), "--analyze"),
            vec!["a.flac".to_string()]
        );
    }

    #[test]
    fn 没有该开关或没有值都返回空() {
        assert!(values_after(&args(&["--selftest"]), "--analyze").is_empty());
        assert!(values_after(&args(&["--analyze"]), "--analyze").is_empty());
    }
}

#[cfg(all(test, feature = "gui"))]
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
