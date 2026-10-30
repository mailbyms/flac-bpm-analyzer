# 音频节拍分析器（flac-bpm-analyzer）

Windows 桌面 GUI 工具：导入单个 **FLAC 或 MP3** 文件 → 自动分析 BPM → 显示音频波形并标出每个节拍的时间点与位置。
纯 Rust 实现，UI 使用 egui/eframe，解码使用 symphonia，播放使用 rodio（cpal）。

- 支持格式：`.flac`（无损）、`.mp3`（有损，常见 32–320 kbps / 44.1–48 kHz）
- 单次只分析一个文件，导入新文件即替换当前结果；不支持批量队列（批量请用命令行版或 `tools/` 里的脚本）
- 节拍时间点为只读展示，不提供人工校正；**拍号可以点一下手动改**（4/4 ↔ 3/4 ↔ 2/4）
- 可只编「命令行分析」部分在终端 / 服务器运行，见[终端 / 无界面版](#终端--无界面版服务器ci容器)

## 下载（免编译）

不装 Rust 也能用：到 [Releases](https://github.com/mailbyms/flac-bpm-analyzer/releases) 下载
`flac-bpm-analyzer-v0.2.0-windows-x64.exe`，双击运行即可。

- 单文件、无需安装；已静态链接 C++ 运行时，不需要额外装 VC++ Redistributable
- 系统要求：Windows 10 / 11（x64）
- 未做代码签名，首次运行若提示「未知发布者」，点「更多信息 → 仍要运行」

## 编译与运行

环境：Rust **1.88 及以上**（MSRV 写在 `Cargo.toml` 的 `rust-version`，本机用 1.99.1 在
Windows 10 x86_64-pc-windows-msvc 下验证通过）。

> `.cargo/config.toml` 已把 `+crt-static` 固化到 msvc 目标上，因此 `cargo build --release`
> 直接产出可分发的单文件 exe（不依赖 `vcruntime140.dll`），发布时无需额外加参数。
> 产物在 `target/release/flac-bpm-analyzer.exe`。

```bash
cd flac-bpm-analyzer        # 仓库根目录

cargo build --release     # 发布构建
cargo run --release       # 直接运行

# 命令行辅助模式（不启动界面，用于验证算法与解码链路）
cargo run --release -- --selftest               # DSP 自检（合成鼓点，不需要音频文件）
cargo run --release -- --analyze "某曲目.flac"   # 命令行分析单个 FLAC 并打印结果
cargo run --release -- --playtest "某曲目.flac"  # 播放自检：解码→播放 2s→检查播放头是否真的推进

# 单元测试 + 离屏界面冒烟测试（不需要窗口/GPU）
cargo test --release
```

`cargo test` 覆盖两部分：`ui::waveform::tests` 验证滚动条拖动映射（滑块跟手）、平移边界、
节拍抽稀步长与悬停命中判定；`app::smoke_tests` 用 `egui::Context::run` 离屏跑真实帧，
验证各缩放档位下界面不 panic、悬停时确实绘制出提示气泡、放大后确实出现滚动条滑块。

首次构建需要下载依赖，仓库默认使用系统配置的 crates.io 镜像（本机为 rsproxy 镜像）。
Windows 下全部为纯 Rust 依赖，不需要额外安装系统库；Linux 下需要装 ALSA 开发包，见下。

### 在 Linux 上编译（alsa-sys 报错）

播放链路是 rodio → cpal → `alsa-sys`，`alsa-sys` 的构建脚本会用 `pkg-config` 找 `alsa.pc`。
只装了运行时库（`libasound2`）而没装**开发头文件**时，会停在：

```
error: failed to run custom build command for `alsa-sys v0.4.0`
...
Package alsa was not found in the pkg-config search path.
```

按发行版装开发包即可（顺带把 `pkg-config` 装上，两者缺一不可）：

```bash
# Debian / Ubuntu
sudo apt-get install -y libasound2-dev pkg-config
# Fedora / RHEL / CentOS / Rocky / Alma
sudo dnf install -y alsa-lib-devel pkgconf-pkg-config
# Arch / Manjaro
sudo pacman -S --needed alsa-lib pkgconf
# Alpine（容器里常见）
apk add --no-cache alsa-lib-dev pkgconf
```

验证：`pkg-config --libs alsa` 应输出 `-lasound`，然后重新 `cargo build --release`。

Linux 下**只需要这一个 C 库**：窗口/输入走 winit（`dlopen` 方式加载 X11 与 Wayland，不需要 dev 包），
文件对话框是 rfd 0.17 的纯 Rust 实现，不带 GTK 依赖。运行界面仍需有 X11/Wayland 会话
和可用的音频输出设备；没有声卡时程序会自动降级为「模拟走时」，波形与节拍照常可用。

### 若报错「rustc x.y is not supported by the following package」

说明锁定的某个传递依赖把 MSRV 抬高了。例如：

```
error: rustc 1.88.0 is not supported by the following package:
  uuid@1.29.0 requires rustc 1.89.0
```

`uuid` 是 eframe 在 Linux 上的无障碍栈（`accesskit_unix` → `zbus`）带进来的，
**Windows 下并不参与编译**，但 cargo 在解析阶段会校验整张依赖图的 MSRV，照样报错。

本仓库已经处理好：`Cargo.toml` 设了 `rust-version = "1.88"` 并启用 `resolver = "3"`
（MSRV 感知解析，`cargo update` / `cargo add` 会优先选兼容 1.88 的版本），`Cargo.lock`
把 `uuid` 钉在 `1.26.1`（最后一个仍支持 1.88 的版本）。

遇到新的同类报错，按提示降到兼容版本即可，不要动 `rust-toolchain`：

```bash
cargo update <name>@<当前版本> --precise <兼容版本>
# 例：cargo update -p uuid --precise 1.26.1
```

## 终端 / 无界面版（服务器、CI、容器）

界面与音频输出是两个独立 feature，可以只编出「分析」这部分，服务器上不需要 ALSA、不需要显示器：

```bash
# 纯命令行：无窗口、无音频、不链接任何 C 库
cargo build --release --no-default-features

# 命令行 + 播放（还想跑 --playtest 时；Linux 仍需 alsa 开发包）
cargo build --release --no-default-features --features playback

# 桌面版（默认）
cargo build --release
```

产物都在 `target/release/flac-bpm-analyzer`（Windows 为 `.exe`），体积差一个数量级：

| 构建 | features | Windows release 体积 | 能力 |
| --- | --- | --- | --- |
| 桌面版 | 默认（`gui` + `playback`） | 6.5 MB | 波形界面 + 播放 |
| 命令行 + 播放 | `--no-default-features --features playback` | 1.5 MB | 终端分析 + `--playtest` |
| 纯命令行 | `--no-default-features` | 1.0 MB | 只做分析，零系统库依赖 |

用法与行为：

```bash
flac-bpm-analyzer --analyze song.flac             # 单个文件
flac-bpm-analyzer --analyze a.flac b.mp3 c.flac   # 多个文件，末尾打印汇总表
flac-bpm-analyzer --selftest                      # DSP 自检（合成鼓点，不需要音频文件）
flac-bpm-analyzer -h                              # 完整用法
```

- 输出被管道/重定向时**不刷进度条**，只留结果行，方便 `grep` / `awk`
- 退出码：`0` 成功 / `1` 分析或自检失败 / `2` 参数错误，可直接串进脚本的 `set -e`
- `--help` 最后一行会打印当前构建包含的能力（如 `无界面 + 无音频输出`），排查时先看这行
- 单曲耗时（release）：175 秒的 FLAC 约 0.3 秒，比界面版少掉整条 GUI 初始化

想要真正能到处拷的静态二进制（连 glibc 都不依赖）：

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --no-default-features --target x86_64-unknown-linux-musl
```

## 批量核对标签 BPM

`tools/check_bpm_vs_tags.sh` 拿一个目录，把每个 `.flac` 的**标签 BPM**（metaflac 读取）
和**本工具算出的 BPM** 并排放，默认输出 **Markdown 表格**（可直接贴进 issue / PR / README），
另可切换成终端对齐表格或 CSV。适合验证算法在你自己的曲库上准不准。

```bash
tools/check_bpm_vs_tags.sh "D:/Music"                  # 递归扫描，输出 Markdown 表格
tools/check_bpm_vs_tags.sh "D:/Music" > report.md      # 存成 .md，贴到 GitHub 直接渲染
tools/check_bpm_vs_tags.sh "D:/Music" --table          # 终端对齐表格（带颜色）
tools/check_bpm_vs_tags.sh "D:/Music" --no-recursive   # 只看本层
tools/check_bpm_vs_tags.sh "D:/Music" --csv > out.csv  # 导出 CSV 做二次统计
tools/check_bpm_vs_tags.sh "D:/Music" -t 0.5           # 收紧「一致」容差到 0.5 BPM
```

输出形如（上面是 Markdown 源码，贴进 GitHub 会渲染成表格）：

```markdown
**目录**：`/data/music`

**样本**：3 个 .flac（含子目录）

| 文件 | 标签BPM | 测得BPM | 差值 | 结论 | 置信度 |
| :--- | ---: | ---: | ---: | :---: | ---: |
| `Creep.flac` | 125 | 124.99 | -0.01 | 一致 | 50% |
| `2X 半速.flac` | 62.5 | 124.99 | +62.49 | 翻倍 | 50% |
| `无标签.flac` | - | 119.98 | - | 无标签 | 86% |

**小结**：共 3 个文件，2 个有可用 BPM 标签

- 一致：1
- 翻倍：1（测得是标签 2 倍，多为半速误判）
- 无标签：1
- 平均绝对误差：0.01 BPM（容差 1.0，统计 1 个同倍频样本，不含翻倍/减半）
- 最大偏差：0.01 BPM（`Creep.flac`）
```

表格单元格里文件名一律包了反引号，`|` 会转义成 `\|`（否则会把表格切断），
文件名里的 `*`、`_` 也不会被渲染成强调。

标签依次尝试 `BPM` → `TBPM` → `TEMPO`（不同打标软件习惯不同），值里带小数或单位
（`124.00`、`124 BPM`）都能识别。结论分六类：

| 结论 | 含义 |
| --- | --- |
| 一致 | 差值在容差内（默认 1.0 BPM，覆盖标签取整的误差） |
| 翻倍 | 测得 ≈ 标签 × 2，多为半速误判（见下文「半速陷阱」） |
| 减半 | 测得 ≈ 标签 ÷ 2，反向的倍频错误 |
| 偏差 | 既不在容差内，也不是 2 倍关系 |
| 无标签 | 文件里没有 BPM / TBPM / TEMPO |
| 异常 | 标签值不是有效数字，或解码/分析失败 |

统计里的**平均绝对误差与最大偏差只统计「同倍频」样本**（一致 + 偏差）——
翻倍/减半的差值天生是几十上百 BPM，混进去会让均值失去意义，它们单独计数。

用法上两个前提：`metaflac` 要在 `PATH` 里（FLAC 工具集自带）；`target/release` 下有可执行文件
（或 `-e` 指定）。每首都要完整解码一遍，约 0.3~2 秒/首，几百首的库请耐心等。


## 目录结构

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 入口：命令行分发（`--analyze` / `--selftest` / `--playtest`）+ 界面启动（仅 `gui` feature） |
| `src/app.rs` | egui 界面与状态机（Empty / Loading / Analyzing / Ready / Error）、快捷键、拖放 |
| `src/theme.rs` | 所有颜色与尺寸常量（设计稿色值集中在此） |
| `src/ui/waveform.rs` | 波形舞台：工具栏、时间刻度、分层绘制（波形 → 节拍 → 播放头 → 悬浮提示）、横向滚动条、播放控制 |
| `src/analysis/mod.rs` | 分析管线总控：后台线程 + std channel 回传进度/结果 |
| `src/analysis/decode.rs` | symphonia 解码：元数据读取、混音、原始 PCM 保留（播放用）、进度上报 |
| `src/analysis/dsp.rs` | DSP：降采样、能量包络、onset 检测、自相关测速、相位对齐、节拍生成、置信度、拍号与重拍推断 |
| `src/analysis/types.rs` | 数据结构（`AnalysisResult`、`AnalysisEvent`、`FileMeta` 等） |
| `src/audio.rs` | 播放控制（真实输出；无设备或未编 `playback` 时降级为模拟走时） |
| `src/export.rs` | 节拍表 CSV 导出（UTF-8 BOM，Excel 可直接打开；随 `gui` 一起编译） |
| `src/selftest.rs` | 自检与命令行分析（`--selftest` / `--analyze` / `--playtest`，后两者不依赖界面） |
| `build.rs` | 构建脚本：调 Windows SDK 的 `rc.exe` 把 `.ico` 嵌进 exe（找不到则只告警、不中断） |
| `assets/icon.ico` | 应用图标多尺寸（16/24/32/48/64/128 BMP + 256 PNG） |
| `assets/icon_256.rgba` | 窗口/任务栏图标，原始 RGBA，运行时 `include_bytes!` 直接用（免解码依赖） |
| `tools/gen_icon.py` | 图标生成脚本（纯标准库：超采样渲染 + PNG/ICO 手写编码） |
| `tools/check_icon.py` | 验证 exe 是否真的嵌入了图标（解析 PE 资源表） |
| `tools/check_bpm_vs_tags.sh` | 批量比对「标签 BPM」与「算出的 BPM」，输出 Markdown 表格 / 终端表格 / CSV |
| `tools/diag_*.py` | 测速链路的诊断脚本（复现管线、包络对照、合成信号对照），不影响编译 |

## 应用图标

图标与界面左上角那个应用图标同源：**橙色圆角方块 `#E07856` + 白色波形**
（几何直接取自界面里的 24×24 SVG：`rect rx=7` + 五段 `stroke-width=1.8` 的圆头线）。

- **exe 图标**（资源管理器/桌面）：`build.rs` 生成临时 `.rc`，用 Windows SDK 的 `rc.exe`
  编译成 `.res`，再作为 `rustc-link-arg` 交给 `link.exe`。因此需要装 Windows SDK；
  找不到 `rc.exe` 时构建照常成功，只是 exe 没有图标（会打一行 `cargo:warning`）。
- **窗口/任务栏图标**：`src/main.rs::app_icon()` 读 `assets/icon_256.rgba` 构造 `egui::IconData`。

改了图标后重新生成并验证：

```bash
python tools/gen_icon.py                                   # 产出 ico/png/rgba
cargo build --release
python tools/check_icon.py target/release/flac-bpm-analyzer.exe   # 应列出 7 个尺寸
```

## 渲染要点（滚动不闪烁的两个前提）

波形滚动时曾出现「整片一闪一闪」，原因是两点，改动时不要退回去：

1. **波形柱子锚定在「全局固定的桶网格」上**，而不是「每个像素列取该列时间区间的最大值」。
   后者在滚动时区间随 `start_sec` 滑动，区间最大值会在相邻桶之间跳变，柱高逐帧突变——
   实测同一时间位置的柱高稳定率只有 38%（即每秒有大批柱子高度在变）。
   锚定后同一位置的柱高恒定，滚动只是整体平移（`waveform_bars` 里有回归测试守着）。
2. **稀疏细线对齐物理像素**（`snap_x`）。1px 宽的节拍线 / 播放头 / 光标参考线若落在亚像素位置，
   egui 会抗锯齿成两列半透明像素，移动时在「实心 / 两列半透明」之间来回变化，肉眼就是闪烁。

> 波形柱子在聚合模式下**故意不做**像素对齐：柱间距约 1px，对齐会让相邻组挤到同一像素互相覆盖。
> 柱高已稳定，轻微的亚像素抗锯齿反而让整张波形平移得更连续。

## 依赖说明

| 依赖 | 版本 | 用途 | 可选 feature |
| --- | --- | --- | --- |
| `eframe` / `egui` | 0.32 | GUI 与桌面壳层 | `gui` |
| `symphonia` | 0.5（仅 `flac` + `mp3` feature） | FLAC / MP3 解码与元数据读取，不引入其它格式的编解码器 | 必选 |
| `rodio` | 0.23（仅 `playback` feature） | 音频输出（内部走 cpal/WASAPI/ALSA），不引入自带解码器 | `playback` |
| `rfd` | 0.17 | 打开文件 / 保存 CSV 的原生对话框 | `gui` |

关闭 `gui` / `playback` 后对应依赖完全不参与编译（连 `cargo tree` 里都不会出现），
所以纯命令行版在 Linux 上不需要 ALSA、X11，构建出的二进制也不 dlopen 它们。

## DSP 流程简介

```
FLAC / MP3 → 交错 PCM(f32) → 混音成单声道
     → 线性插值降采样到 22050 Hz
     → 居中窗 RMS 能量帧 → 对数压缩 → 半波整流一阶差分
     → 短窗(0.15s)局部均值归一化 + 3 点平滑 + 长窗(1s)高通
     → onset 包络
     → 归一化自相关（60~180 BPM 对应的 lag 区间）取候选 + 抛物线插值细化周期
     → 梳状评分（k=1..4 倍周期）解决倍频/分频，叠加对数速度先验（中心 125 BPM）
     → 半速陷阱修正：结果 < 70 BPM 时，若两倍速候选的梳状分仍保住 ≥ 70%，判定为
       「相邻拍强弱交替」并把结果翻倍（见下方「半速陷阱」说明）
     → 相位扫描（1/4 帧步长 + 二次插值）得到首拍偏移，并补偿半个分析窗的系统性提前
     → 递推生成整曲节拍表
     → 置信度 = 峰值相对高度 + 次峰分离度，再打 0.9 折保守输出
```

关键实现细节（都在 `src/analysis/dsp.rs` 顶部集中在常量区）：

- `TARGET_ANALYSIS_SR = 22050`、`FRAME_LEN = 1024`、`HOP = 256`（把包络帧率提到 ~86 Hz，
  否则只有 1~2 帧宽的脉冲会在「非整数帧周期」上被采样抖动削弱，导致结果系统性偏向半速）。
- 周期虽然可以用整数 lag 求自相关，但候选的梳状评分必须用**分数 lag 的真实相关值**
  （对信号本身做插值），否则对比不成立，这是正确区分 120 BPM 与其半速 60 BPM 的关键。
- 波形峰值数组 `waveform_peaks` 在解码后**一次遍历**聚合成固定 8192 桶并归一化到 0~1，
  绘图阶段只做 O(像素列数) 的取窗最大值，不再触碰原始 PCM，长曲目也能秒级出结果。

### 半速陷阱（octave error）

4/4 流行 / 摇滚的鼓组是 kick / snare 交替，**平移 1 拍后强峰对弱峰**，1 拍处的自相关会腰斩，
而 2 拍处才是完整循环。结果是 ACF 主导周期变成 2 拍，BPM 被判成一半。

实测 Creep.flac（真 125 BPM）：`r(1拍)=0.32`、`r(2拍)=0.65`，被判成 62.5。而 onset 峰间隔分布里
47% 集中在 0.50s（即 125 BPM 的拍间隔），说明真实拍速确实是 125。

修正做法（`dsp.rs::fix_half_time`）：结果 < 70 BPM 时，比较两倍速候选的梳状分能保住多少：

| 素材 | comb(2倍)/comb(当前) | 结论 |
| --- | --- | --- |
| 真 62.5 / 70 / 80 / 90 BPM 的合成慢曲 | 0.05 ~ 0.35 | 不翻倍 |
| Creep.flac（真 125，强弱交替） | 0.75 | 翻倍 |

阈值取 0.7，两侧都有充足裕量（`HALF_TIME_RATIO`）。翻倍后置信度改用「原两拍周期」处的 ACF
评估——否则会拿半拍处的弱峰去算，得到偏低的误导性数字。

> 诊断脚本（不影响构建）：`tools/diag_tempo.py` 复现整条测速链路、`tools/diag_variants.py`
> 对比不同包络处理的得分、`tools/diag_octave.py` 用合成信号验证判据的区分度。

## 交互说明

- 单文件模式：一次只处理一个文件（`.flac` / `.mp3`，大小写不敏感），导入新文件会替换上一次的结果；
  没有批量队列。文件选择对话框提供「音频文件 (FLAC / MP3)」「FLAC」「MP3」三个过滤器。
- 只读展示：不做任何节拍编辑（无增删拍、拖动微调、半速/倍速、偏移量输入）。
  允许「重新分析」与「导出节拍表(CSV)」。
- 波形区：点击/拖拽定位播放头（靠近节拍线 6px 内会吸附并在状态栏显示该拍信息）。
- **横向滚动条**：位于波形舞台正下方。放大后拖动滑块即可横向浏览整轨，滑块严格跟手
  （拖动 N 像素，滑块就移动 N 像素）；点击轨道空白处会把该位置移到窗口中心；
  100%（全曲视图）时滑块铺满轨道，表示无可滚动内容。
- **跟随播放头**：播放控制行右侧的开关按钮（默认开启，激活时实心橙）。
  - 开启：播放中**播放头始终钉在窗口正中**，波形连续横向滚动（居中锁定，不是翻页）。
  - 受曲首/曲末约束：前半个可视窗口内窗口贴住起点、最后半个窗口贴住终点，
    播放头此时会偏离中心——这是居中跟随的固有行为。
  - **手动平移（滚轮 / 拖滚动条）会暂停跟随 2 秒**，避免刚滚开看别的段落就被拽回来。
  - 全曲视图下整曲都在窗口内，不会发生滚动。
- **手工测速（右键打点）**：自动检测不准时用它直接量。
  - 在波形区**右键点击**一次 = 打一拍；两次点击之间的时间差就是「一个节拍」，据此反推 BPM。
  - 继续右键可多次打点，BPM 取**最近 6 次点击的平均间隔**（tap tempo），比只量两拍稳。
    间隔 < 100ms 的点击视为误触，直接丢弃。
  - 波形上会画出每个打点（橙色圆点 + 虚线），并高亮最近一个节拍区间，顶部显示 `BPM · 间隔` 胶囊。
  - 播放控制行右侧出现 `手工 123.4 BPM · 间隔 0.487s · N 次` 与 **采用此 BPM** / **清除**。
  - 点「采用」会用该 BPM 重算整条节拍线：**以最后一次区间的起点作为首拍**，因此节拍线与刚量的那一拍对齐；
    指标卡徽章从「置信度 xx%」变为「手工测量」（人工测量没有置信度可言）。
  - 左键点击仍然是定位播放头，两者互不干扰。
- **拍号（4/4 ↔ 3/4 ↔ 2/4）**：分析时自动推断，**拍号卡片可以点**。
  - 判据是「小节内强弱是否每 3 拍重复一次」：单独算一路低频（≤110 Hz，底鼓）包络，
    在拍网格上取样得到「每拍有多重」，再对它求自相关——`r(3)` 明显为正就是 3 拍组。
  - 因此只能区分 **3 拍组 / 4 拍组**：4/4 与 2/4 在信号上等价、6/8 与 3/4 同周期，都不做区分。
  - 卡片徽章显示把握度（`推测 85%`）；把握为 0（每拍一样重、拿不到证据）时按 4/4 兜底。
  - 点击卡片循环 `4/4 → 3/4 → 2/4`，徽章变成「手动」；小节线、悬浮提示、CSV 导出的
    「小节 / 小节内拍号 / 是否重拍」三列立刻跟着改。换文件或重新分析会回到自动推断。
- **光标时间提示**：鼠标在波形区任意位置移动时，都有一条淡色竖向参考线跟随光标，
  指针旁实时浮出该处的时间码 `mm:ss.mmm`（不是只有节拍线上才有）。
- **节拍悬浮提示**：当指针落在节拍竖线上（±5px 内），气泡内容升级为节拍信息——
  「节拍 #N」、时间码、「第 X 小节 · 第 Y 拍」，该拍同时加粗为实线并显示顶部手柄条。
  同一位置只出现一个气泡，不会两套提示叠在一起。
  被抽稀掉的拍（间距过密时不绘制）不参与命中，避免误导。
- 滚轮语义：
  - **单滚轮** = 横向平移（Shift+滚轮步长按 3 倍，用于快速浏览）
  - **Ctrl + 滚轮** = 以指针位置为锚点缩放（每档 ×1.15）
  - **只按 Ctrl 而滚轮没动** = 波形纹丝不动
  - 触控板双指捏合同样缩放；工具行 −/%/+ 与「全曲视图」也可缩放
  > 注：egui 默认把 `Ctrl/⌘+滚轮` 当缩放手势（`InputOptions::zoom_modifier` 默认 `COMMAND`），
  > 且用的是**平滑后**的滚轮增量——滚轮停下后余量还会释放几帧，表现为「只按 Ctrl 波形自己放大」。
  > 本项目启动时将该项置为 `NONE`（见 `FlacBpmApp::configure`），改由 `ui::waveform` 用**未平滑**
  > 的 `raw_scroll_delta` 判定「本帧是否真的滚了」，三条语义全部自控。
- 快捷键：`空格` 播放/暂停，`←/→` 上一拍/下一拍，`Ctrl+O` 打开文件；也支持文件拖放。

## 已知限制

- **播放降级**：代码中实现了基于 rodio/cpal 的真实音频输出；若系统没有可用的音频输出设备
  （例如某些远程桌面会话返回 `NoDevice`），会自动降级为「模拟走时」（播放头按秒推进、无声音），
  状态栏与「设置」面板会提示当前模式。
- onset 用的是**能量差分**而非谱通量：对打击感弱、持续音为主的音乐（古典、氛围电子、纯器乐）
  准确率明显下降；换成 STFT 谱通量（rustfft）可改善，此处为控制复杂度未实现。
- 全局单一速度估计，不做速度跟踪；曲中存在变速时会给出折中值。
- 拍号只做「3 拍组 / 4 拍组」二选一，而且**依赖底鼓**：
  - 4/4 与 2/4 在信号上是同一件事（只差记谱习惯），6/8 与 3/4 同周期，都没有可靠判据；
  - 每拍都有底鼓的 four-on-floor 舞曲、以及没鼓的曲子（古典 / 氛围 / 纯器乐）拿不到强弱证据，
    会按 4/4 兜底并显示「推测 0%」；
  - 误判时点一下拍号卡片即可手动改，不要指望它百发百中。
- 小节重拍（downbeat）相位同样是推测值：4/4 摇滚的底鼓在第 1、3 拍且一样重，
  只能缩到这两个候选（要靠 crash / 加花 / 和声变化才能定），four-on-floor 则完全无信息。
  UI 的「第 X 小节」与 CSV 的小节列都会跟着推断出的相位走，相位错了整条小节线平移几拍。
- 6/8 存在 BPM 语义问题：按八分音符 180 写的 6/8 会被估成 90 BPM（半速），
  拍号便落在 3 拍组——这不是拍号检测错了，而是「一拍」的定义变了。
- 倍频二义性只能压窄、不能根除：`fix_half_time` 处理的是「< 70 BPM 且两倍速仍有强周期」这一种
  半速陷阱（强弱交替素材）。若一首曲子相邻拍完全对称、又没有拍间细节，60 / 120 的区分在
  纯自相关意义上是信息不足的，只能靠速度先验取舍。
- 反过来，真正的**慢速曲**（< 70 BPM）若拍间有大量 onset（切分、密集配器），
  两倍速的梳状分可能被抬到 0.7 以上而被误翻倍——这是该判据已知的边界，遇到时可用
  「右键打点手工测速」直接给出正确 BPM。
- 置信度为保守估计（真实音乐常见 15%~50%，合成干净鼓点可达 80%+），不代表绝对精度。
- **MP3 相关**：
  - 解码开了 `enable_gapless`，带 LAME 头的 MP3 会裁掉编码器延迟（约 576+ samples），
    首拍偏移与同名 FLAC 一致；**无 LAME 头**（部分老编码器产出）时会残留约 10–30ms 前导静音，
    首拍偏移整体略偏后，对 BPM 无影响。
  - MP3 没有「位深」概念，元数据行对 `.mp3` 只显示采样率，不显示 bit。
  - 有损压缩会削弱高频瞬态，onset 检测对低码率（<128 kbps）或强低通滤波的素材灵敏度略降。
- 未做手动校正（按产品约束）：不提供撤销重做、增删节拍、偏移量微调等功能。
