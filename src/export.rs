//! CSV 导出：把节拍表写成 UTF-8（带 BOM，Excel 可直接打开）

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use crate::analysis::types::AnalysisResult;

/// 格式化秒 → mm:ss.mmm
pub fn format_timecode(t: f32) -> String {
    let secs = t.max(0.0);
    let m = (secs / 60.0).floor() as u32;
    let s = (secs % 60.0).floor() as u32;
    let ms = ((secs - secs.floor()) * 1000.0).round() as u32;
    format!("{m:02}:{s:02}.{ms:03}")
}

/// 弹出保存对话框并写入 CSV。
/// 返回 Ok(None) 表示用户取消。
pub fn export_beats_csv(result: &AnalysisResult, suggested_name: &str) -> Result<Option<PathBuf>, String> {
    let default_name = format!("{suggested_name}_beats.csv");
    let path = rfd::FileDialog::new()
        .set_file_name(default_name)
        .add_filter("CSV 表格", &["csv"])
        .save_file()
        .ok_or_else(|| "导出已取消".to_string())?;

    write_csv(path.as_path(), result)?;
    Ok(Some(path))
}

fn write_csv(path: &std::path::Path, result: &AnalysisResult) -> Result<(), String> {
    let mut file = File::create(path).map_err(|e| format!("创建文件失败：{e}"))?;
    // UTF-8 BOM
    file.write_all(&[0xEF, 0xBB, 0xBF])
        .map_err(|e| format!("写入失败：{e}"))?;
    writeln!(
        file,
        "序号,时间(秒),时间,小节,小节内拍号,是否重拍,BPM,置信度(%)"
    )
    .map_err(|e| format!("写入失败：{e}"))?;

    for (i, &t) in result.beats.iter().enumerate() {
        // 小节号与小节内位置要跟着重拍相位走，不能拿「第 0 拍就是小节头」硬算
        let (bar, beat_in_bar) = result.bar_position(i);
        let is_downbeat = if result.is_downbeat(i) { 1 } else { 0 };
        writeln!(
            file,
            "{},{:.6},{},{},{},{},{:.2},{:.1}",
            i + 1,
            t,
            format_timecode(t),
            bar,
            beat_in_bar,
            is_downbeat,
            result.bpm,
            result.confidence * 100.0
        )
        .map_err(|e| format!("写入失败：{e}"))?;
    }
    file.flush().map_err(|e| format!("保存失败：{e}"))?;
    Ok(())
}
