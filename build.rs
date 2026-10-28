//! 构建脚本：把 `assets/icon.ico` 嵌入 Windows 可执行文件，让 exe 在资源管理器里显示图标。
//!
//! 做法：生成一个临时 .rc → 调 Windows SDK 的 rc.exe 编译成 .res → 作为链接参数交给 link.exe。
//! 找不到 rc.exe 时只打 warning、不中断构建（图标只是锦上添花，不该阻塞编译）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico = manifest.join("assets").join("icon.ico");
    if !ico.is_file() {
        println!(
            "cargo:warning=未找到 {}，跳过 exe 图标（可运行 python tools/gen_icon.py 生成）",
            ico.display()
        );
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // 生成临时 .rc。用绝对路径 + 引号 + 双反斜杠，避免工作目录与空格路径带来的解析问题。
    let rc_path = out_dir.join("app_icon.rc");
    let ico_escaped = ico.display().to_string().replace('\\', "\\\\");
    if let Err(e) = std::fs::write(&rc_path, format!("1 ICON \"{ico_escaped}\"\n")) {
        println!("cargo:warning=写入 .rc 失败：{e}");
        return;
    }

    let Some(rc_exe) = find_rc() else {
        println!("cargo:warning=未找到 rc.exe（Windows SDK），跳过 exe 图标嵌入");
        return;
    };

    let res_path = out_dir.join("app_icon.res");
    let output = Command::new(&rc_exe)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            // MSVC 的 link.exe 可以直接吃 .res 文件
            println!("cargo:rustc-link-arg={}", res_path.display());
        }
        Ok(o) => println!(
            "cargo:warning=rc.exe 失败（{}）：{}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => println!("cargo:warning=无法运行 rc.exe：{e}"),
    }
}

/// 查找 Windows SDK 的 rc.exe：环境变量 RC → PATH → SDK 目录（取版本最高的 x64）
fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RC") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join("rc.exe");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }

    let sdk_bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(sdk_bin) {
        for entry in entries.flatten() {
            let cand = entry.path().join("x64").join("rc.exe");
            if !cand.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let ver: Vec<u64> = name
                .split('.')
                .filter_map(|s| s.parse::<u64>().ok())
                .collect();
            if best.as_ref().is_none_or(|(best_ver, _)| ver > *best_ver) {
                best = Some((ver, cand));
            }
        }
    }
    best.map(|(_, p)| p)
}
