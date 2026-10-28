#!/usr/bin/env bash
# check_bpm_vs_tags.sh —— 批量比对「FLAC 标签里的 BPM」与「本工具算出的 BPM」
#
# 用法：
#   tools/check_bpm_vs_tags.sh <目录> [选项]
#
# 选项：
#   --no-recursive     只看该目录本身，不进子目录（默认递归）
#   --csv              输出 CSV 而非表格（便于丢进 Excel / 二次统计）
#   -t, --tolerance N  判定「一致」的容差，单位 BPM，默认 1.0
#   -e, --exe PATH     指定分析器可执行文件（默认自动找 target/release）
#   -h, --help         显示帮助
#
# 依赖：
#   metaflac          读 Vorbis Comment 里的 BPM / TBPM / TEMPO 标签
#   flac-bpm-analyzer 本项目的二进制（cargo build --release 后就有）
#
# 结论含义：
#   一致    测得值与标签差值在容差内
#   翻倍    测得值 ≈ 标签 × 2（标签大概率记的是半速，或检测器把拍算成了半拍）
#   减半    测得值 ≈ 标签 ÷ 2（反向的倍频错误）
#   偏差    两者既不在容差内，也不是 2 倍关系
#   无标签  文件里没有 BPM/TBPM/TEMPO 标签
#   异常    标签值不是有效数字，或解码/分析失败
#
# 注意：每个文件都要完整解码一遍，长曲目约 0.3~2 秒，几百首的库请耐心等。

set -uo pipefail

# ---------- 参数 ----------
DIR=""
RECURSIVE=1
CSV=0
TOL="1.0"
EXE_ARG=""

usage() {
    cat <<'USAGE'
check_bpm_vs_tags.sh —— 批量比对「FLAC 标签里的 BPM」与「本工具算出的 BPM」

用法：
  tools/check_bpm_vs_tags.sh <目录> [选项]

选项：
  --no-recursive     只看该目录本身，不进子目录（默认递归）
  --csv              输出 CSV 而非表格（便于丢进 Excel / 二次统计）
  -t, --tolerance N  判定「一致」的容差，单位 BPM，默认 1.0
  -e, --exe PATH     指定分析器可执行文件（默认自动找 target/release）
  -h, --help         显示帮助

依赖：
  metaflac           读 Vorbis Comment 里的 BPM / TBPM / TEMPO 标签
  flac-bpm-analyzer  本项目的二进制（cargo build --release 后就有）

结论含义：
  一致    测得值与标签的差值在容差内
  翻倍    测得值 ≈ 标签 × 2（标签多半记的是半速，或检测器把拍算成了半拍）
  减半    测得值 ≈ 标签 ÷ 2（反向的倍频错误）
  偏差    既不在容差内，也不是 2 倍关系
  无标签  文件里没有 BPM / TBPM / TEMPO 标签
  异常    标签值不是有效数字，或解码/分析失败

注意：每个文件都要完整解码一遍，长曲目约 0.3~2 秒，几百首的库请耐心等。
USAGE
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --no-recursive) RECURSIVE=0; shift ;;
        --csv)          CSV=1; shift ;;
        -t|--tolerance) TOL="${2:-}"; shift 2 ;;
        -e|--exe)       EXE_ARG="${2:-}"; shift 2 ;;
        -h|--help)      usage 0 ;;
        -*)             printf '未知选项：%s\n用 -h 看帮助。\n' "$1" >&2; exit 2 ;;
        *)
            if [ -n "$DIR" ]; then
                printf '只能指定一个目录（已给出 %s，又来了 %s）\n' "$DIR" "$1" >&2
                exit 2
            fi
            DIR="$1"; shift ;;
    esac
done

if [ -z "$DIR" ]; then
    printf '缺少目录参数。\n\n' >&2
    usage 2
fi
if [ ! -d "$DIR" ]; then
    printf '目录不存在：%s\n' "$DIR" >&2
    exit 2
fi

command -v metaflac >/dev/null 2>&1 || {
    printf '找不到 metaflac，请先安装 FLAC 工具集（并把 metaflac 放进 PATH）。\n' >&2
    exit 3
}

# ---------- 定位分析器 ----------
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROJECT_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

resolve_exe() {
    if [ -n "$EXE_ARG" ]; then
        if [ -x "$EXE_ARG" ] || [ -f "$EXE_ARG" ]; then printf '%s' "$EXE_ARG"; return; fi
        printf '指定的可执行文件不存在：%s\n' "$EXE_ARG" >&2
        return 1
    fi
    local c
    for c in "$PROJECT_ROOT/target/release/flac-bpm-analyzer.exe" \
             "$PROJECT_ROOT/target/release/flac-bpm-analyzer" \
             "$PROJECT_ROOT/target/debug/flac-bpm-analyzer.exe" \
             "$PROJECT_ROOT/target/debug/flac-bpm-analyzer"; do
        if [ -f "$c" ]; then printf '%s' "$c"; return; fi
    done
    c=$(command -v flac-bpm-analyzer.exe 2>/dev/null || command -v flac-bpm-analyzer 2>/dev/null)
    if [ -n "$c" ]; then printf '%s' "$c"; return; fi
    printf '找不到分析器。先 cargo build --release，或用 -e 指定路径。\n' >&2
    return 1
}

ANALYZER=$(resolve_exe) || exit 3

# ---------- 显示宽度：CJK 占 2 列 ----------
# 首选 wc -L（GNU coreutils 按显示宽度算，最准）；非 GNU 环境没有 -L，退化为按字节估算。
WIDTH_MODE=1
if [ "$(printf 'ab\n' | wc -L 2>/dev/null | tr -d ' \r')" != "2" ]; then
    WIDTH_MODE=0
fi

disp_width() {
    if [ "$WIDTH_MODE" -eq 1 ]; then
        local w
        w=$(printf '%s\n' "$1" | wc -L)
        printf '%s' "${w//[!0-9]/}"
    else
        # 退化估算：CJK 是 3 字节 1 字符，多出的 2 字节正好对应 2 列宽度
        local b c
        b=$(printf '%s' "$1" | wc -c)
        c=${#1}
        printf '%s' $(( c + (b - c) / 2 ))
    fi
}

# 左侧对齐填充（超宽不截断，交给调用方先处理）
pad_r() {
    local s="$1" w="$2" cur n
    cur=$(disp_width "$s")
    n=$(( w - cur ))
    [ "$n" -lt 0 ] && n=0
    printf '%s%*s' "$s" "$n" ""
}

# 右侧对齐填充（数字列用）
pad_l() {
    local s="$1" w="$2" cur n
    cur=$(disp_width "$s")
    n=$(( w - cur ))
    [ "$n" -lt 0 ] && n=0
    printf '%*s%s' "$n" "" "$s"
}

# 超长时保留尾部（文件名比目录名更有信息量）
truncate_tail() {
    local s="$1" max="$2" cut
    if [ "$(disp_width "$s")" -le "$max" ]; then printf '%s' "$s"; return; fi
    cut="${s: -$(( max - 1 ))}"
    while [ "${#cut}" -gt 1 ] && [ "$(disp_width "…$cut")" -gt "$max" ]; do
        cut="${cut:1}"
    done
    printf '…%s' "$cut"
}

# ---------- 颜色（仅终端下生效，管道输出不受影响） ----------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    C_OK=$'\033[32m'; C_BAD=$'\033[31m'; C_WARN=$'\033[33m'; C_DIM=$'\033[2m'; C_RST=$'\033[0m'
else
    C_OK=""; C_BAD=""; C_WARN=""; C_DIM=""; C_RST=""
fi

# ---------- 读标签 ----------
# 依次尝试 BPM / TBPM / TEMPO（不同打标软件习惯不同），返回「键<TAB>值」
read_tag_bpm() {
    local f="$1" key val line num
    for key in BPM TBPM TEMPO; do
        line=$(metaflac --show-tag="$key" "$f" 2>/dev/null | head -n 1)
        [ -z "$line" ] && continue
        val="${line#*=}"
        val="${val//[$'\r\n\t ']/}"
        [ -z "$val" ] && continue
        # 有些打标软件写成 "124.00" 或 "124 BPM"，取前导数字；取不到就原样返回（后面判为异常）
        num=$(printf '%s' "$val" | sed -n 's/^\([0-9][0-9]*\([.][0-9]*\)\{0,1\}\).*/\1/p')
        printf '%s\t%s' "$key" "${num:-$val}"
        return 0
    done
    return 1
}

# ---------- 判定：输出「差值|结论」 ----------
judge() {
    awk -v t="$1" -v m="$2" -v tol="$TOL" 'BEGIN {
        d = m - t
        ad = (d < 0) ? -d : d
        v = "偏差"
        if (ad <= tol) v = "一致"
        else if (t > 0) {
            r = m / t
            if (r >= 1.98 && r <= 2.02) v = "翻倍"
            else if (r >= 0.49 && r <= 0.51) v = "减半"
        }
        printf "%.2f|%s", d, v
    }'
}

# ---------- 采集文件列表 ----------
DEPTH=()
[ "$RECURSIVE" -eq 0 ] && DEPTH=( -maxdepth 1 )

FILES=()
while IFS= read -r -d '' f; do
    FILES+=( "$f" )
done < <(find "$DIR" "${DEPTH[@]}" -type f -iname '*.flac' -print0 2>/dev/null | sort -z)

TOTAL=${#FILES[@]}
if [ "$TOTAL" -eq 0 ]; then
    printf '在 %s 下没有找到 .flac 文件%s。\n' "$DIR" "$([ "$RECURSIVE" -eq 0 ] && echo '（当前只看本层，试试去掉 --no-recursive）')" >&2
    exit 4
fi

# ---------- 逐文件分析 ----------
ABS_DIR=$(cd "$DIR" && pwd)
REL=(); TAG=(); MEA=(); DIFF=(); VERD=(); CONF=(); TAGKEY=(); NOTE=()

START_TS=$(date +%s%N)
i=0
for f in "${FILES[@]}"; do
    i=$(( i + 1 ))
    rel="${f#$ABS_DIR/}"; rel="${rel#$DIR/}"

    if [ "$CSV" -eq 0 ] && [ -t 2 ]; then
        printf '\r\033[K  分析中 [%d/%d] %s' "$i" "$TOTAL" "$(truncate_tail "$rel" 60)" >&2
    fi

    # 1) 标签
    tagval=""; tagkey=""
    if tinfo=$(read_tag_bpm "$f"); then
        tagkey="${tinfo%%$'\t'*}"
        tagval="${tinfo#*$'\t'}"
    fi

    # 2) 分析
    raw=$("$ANALYZER" --analyze "$f" 2>&1 | tr '\r' '\n' | grep -a 'BPM [0-9]' | tail -n 1)
    meas=$(printf '%s' "$raw" | sed -n 's/.*BPM \([0-9][0-9.]*\).*/\1/p')
    conf=$(printf '%s' "$raw" | sed -n 's/.*置信度 \([0-9][0-9]*\)%.*/\1/p')

    REL+=( "$rel" ); TAGKEY+=( "$tagkey" )

    # 3) 判定
    if [ -z "$meas" ]; then
        TAG+=( "${tagval:--}" ); MEA+=( "-" ); DIFF+=( "-" ); VERD+=( "异常" ); CONF+=( "-" )
        NOTE+=( "$rel · 分析失败" )
    elif [ -z "$tagval" ]; then
        TAG+=( "-" ); MEA+=( "$meas" ); DIFF+=( "-" ); VERD+=( "无标签" ); CONF+=( "${conf:-?}%" )
    elif ! awk -v v="$tagval" 'BEGIN { exit !(v ~ /^[0-9]+([.][0-9]+)?$/ && v + 0 > 0) }'; then
        TAG+=( "$tagval" ); MEA+=( "$meas" ); DIFF+=( "-" ); VERD+=( "异常" ); CONF+=( "${conf:-?}%" )
        NOTE+=( "$rel · 标签值不是有效 BPM（$tagkey=$tagval）" )
    else
        res=$(judge "$tagval" "$meas")
        TAG+=( "$tagval" ); MEA+=( "$meas" )
        DIFF+=( "${res%%|*}" ); VERD+=( "${res#*|}" ); CONF+=( "${conf:-?}%" )
        [ "$tagkey" != "BPM" ] && NOTE+=( "$rel · 标签取自 $tagkey" )
    fi
done
[ "$CSV" -eq 0 ] && [ -t 2 ] && printf '\r\033[K' >&2

ELAPSED_MS=$(( ($(date +%s%N) - START_TS) / 1000000 ))

# ---------- 统计 ----------
n_tag=0; n_ok=0; n_dbl=0; n_half=0; n_off=0; n_notag=0; n_bad=0
# 误差统计只覆盖「同倍频」的样本（一致 + 偏差）：
# 倍频类（翻倍/减半）的差值天然是几十上百 BPM，混进来会把均值毁掉
n_cmp=0; sum_abs=0; worst_abs=0; worst_name=""
for idx in "${!REL[@]}"; do
    case "${VERD[$idx]}" in
        一致) n_tag=$((n_tag+1)); n_ok=$((n_ok+1)); n_cmp=$((n_cmp+1)) ;;
        翻倍) n_tag=$((n_tag+1)); n_dbl=$((n_dbl+1)) ;;
        减半) n_tag=$((n_tag+1)); n_half=$((n_half+1)) ;;
        偏差) n_tag=$((n_tag+1)); n_off=$((n_off+1)); n_cmp=$((n_cmp+1)) ;;
        无标签) n_notag=$((n_notag+1)) ;;
        异常)   n_bad=$((n_bad+1)) ;;
    esac
    case "${VERD[$idx]}" in
        一致|偏差) ;;
        *) continue ;;
    esac
    a=$(awk -v d="${DIFF[$idx]}" 'BEGIN { printf "%.2f", (d<0?-d:d) }')
    sum_abs=$(awk -v s="$sum_abs" -v a="$a" 'BEGIN { printf "%.2f", s+a }')
    if awk -v a="$a" -v w="$worst_abs" 'BEGIN { exit !(a > w) }'; then
        worst_abs="$a"; worst_name="${REL[$idx]}"
    fi
done

mean_abs=0
[ "$n_cmp" -gt 0 ] && mean_abs=$(awk -v s="$sum_abs" -v n="$n_cmp" 'BEGIN { printf "%.2f", s/n }')

# ---------- 输出 ----------
if [ "$CSV" -eq 1 ]; then
    # 文件名可能有逗号/引号，按 RFC4180 加引号转义，否则 Excel 会错列
    csv_q() {
        local s="$1"
        if [[ "$s" == *','* || "$s" == *'"'* || "$s" == *$'\n'* ]]; then
            printf '"%s"' "${s//\"/\"\"}"
        else
            printf '%s' "$s"
        fi
    }
    printf '文件,标签BPM,测得BPM,差值,结论,置信度\n'
    for idx in "${!REL[@]}"; do
        printf '%s,%s,%s,%s,%s,%s\n' \
            "$(csv_q "${REL[$idx]}")" "$(csv_q "${TAG[$idx]}")" "$(csv_q "${MEA[$idx]}")" \
            "$(csv_q "${DIFF[$idx]}")" "$(csv_q "${VERD[$idx]}")" "$(csv_q "${CONF[$idx]}")"
    done
    exit 0
fi

# 列宽
W_PATH=4; W_TAG=8; W_MEA=8; W_DIFF=8; W_VERD=6; W_CONF=7
PATH_CAP=52
for idx in "${!REL[@]}"; do
    p="${REL[$idx]}"
    [ "$(disp_width "$p")" -gt "$PATH_CAP" ] && continue
    w=$(disp_width "$p"); [ "$w" -gt "$W_PATH" ] && W_PATH="$w"
done

bar() {
    local n=$(( W_PATH + W_TAG + W_MEA + W_DIFF + W_VERD + W_CONF + 5 * 2 ))
    printf '%s' "$(printf '%*s' "$n" '' | tr ' ' '-')"
}

printf '\n目录：%s\n' "$ABS_DIR"
printf '文件：%d 个 .flac%s\n\n' "$TOTAL" "$([ "$RECURSIVE" -eq 0 ] && echo '（仅本层）' || echo '（含子目录）')"

printf '%s  %s  %s  %s  %s  %s\n' \
    "$(pad_r "文件" "$W_PATH")" \
    "$(pad_l "标签BPM" "$W_TAG")" \
    "$(pad_l "测得BPM" "$W_MEA")" \
    "$(pad_l "差值" "$W_DIFF")" \
    "$(pad_r "结论" "$W_VERD")" \
    "$(pad_l "置信度" "$W_CONF")"
printf '%s\n' "$(bar)"

for idx in "${!REL[@]}"; do
    v="${VERD[$idx]}"
    color=""
    case "$v" in
        一致)   color="$C_OK" ;;
        翻倍|减半) color="$C_WARN" ;;
        偏差|异常) color="$C_BAD" ;;
        无标签) color="$C_DIM" ;;
    esac
    # 差值带符号，便于一眼看出方向
    d="${DIFF[$idx]}"
    case "$d" in
        -) ;;
        -*) ;;
        0.00) ;;
        *) d="+$d" ;;
    esac
    printf '%s  %s  %s  %s  %s  %s\n' \
        "$(pad_r "$(truncate_tail "${REL[$idx]}" "$PATH_CAP")" "$W_PATH")" \
        "$(pad_l "${TAG[$idx]}" "$W_TAG")" \
        "$(pad_l "${MEA[$idx]}" "$W_MEA")" \
        "$(pad_l "$d" "$W_DIFF")" \
        "$(printf '%s%s%s' "$color" "$(pad_r "$v" "$W_VERD")" "$C_RST")" \
        "$(pad_l "${CONF[$idx]}" "$W_CONF")"
done

printf '%s\n\n' "$(bar)"

# ---------- 小结 ----------
printf '共 %d 个文件（%d 个有可用 BPM 标签）\n' "$TOTAL" "$n_tag"
[ "$n_ok"   -gt 0 ] && printf '  %s一致%s      %d\n' "$C_OK"   "$C_RST" "$n_ok"
[ "$n_dbl"  -gt 0 ] && printf '  %s翻倍%s      %d（测得是标签 2 倍，多为半速误判）\n' "$C_WARN" "$C_RST" "$n_dbl"
[ "$n_half" -gt 0 ] && printf '  %s减半%s      %d（测得是标签一半）\n' "$C_WARN" "$C_RST" "$n_half"
[ "$n_off"  -gt 0 ] && printf '  %s偏差%s      %d\n' "$C_BAD"  "$C_RST" "$n_off"
[ "$n_notag" -gt 0 ] && printf '  %s无标签%s    %d\n' "$C_DIM"  "$C_RST" "$n_notag"
[ "$n_bad"  -gt 0 ] && printf '  %s异常%s      %d\n' "$C_BAD"  "$C_RST" "$n_bad"

if [ "$n_cmp" -gt 0 ]; then
    printf '\n平均绝对误差：%.2f BPM（容差 %.2f，统计 %d 个同倍频样本，不含翻倍/减半）\n' \
        "$mean_abs" "$TOL" "$n_cmp"
    printf '最大偏差：%s BPM（%s）\n' "$worst_abs" "$worst_name"
    if awk -v m="$mean_abs" -v t="$TOL" 'BEGIN { exit !(m > t) }'; then
        printf '%s平均误差超出容差，建议抽查上面标红的文件。%s\n' "$C_WARN" "$C_RST"
    fi
elif [ "$n_dbl" -gt 0 ] || [ "$n_half" -gt 0 ]; then
    printf '\n所有可用样本都是倍频关系，误差统计无意义（见上方翻倍/减半计数）。\n'
fi

printf '耗时：%d.%03d s\n' "$(( ELAPSED_MS / 1000 ))" "$(( ELAPSED_MS % 1000 ))"

if [ "${#NOTE[@]}" -gt 0 ]; then
    printf '\n附注：\n'
    for n in "${NOTE[@]}"; do printf '  · %s\n' "$n"; done
fi
printf '\n'
