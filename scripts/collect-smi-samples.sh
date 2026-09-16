#!/usr/bin/env bash
# collect-smi-samples.sh — 采集 hy-smi/rocm-smi 各命令的原始输出，
# 用于 dtop 解析器适配。产物打包为 smi-samples-<host>-<date>.tar.gz
set -u

SMI=""
for cand in hy-smi rocm-smi; do
    if command -v "$cand" >/dev/null 2>&1; then
        SMI="$cand"
        break
    fi
done

if [ -z "$SMI" ]; then
    echo "ERROR: neither hy-smi nor rocm-smi found in PATH" >&2
    echo "hint: DTK default location is /opt/dtk/bin, try: export PATH=/opt/dtk/bin:\$PATH" >&2
    exit 1
fi

HOST=$(hostname 2>/dev/null || echo unknown)
DATE=$(date +%Y%m%d-%H%M%S)
DIR="smi-samples-${HOST}-${DATE}"
mkdir -p "$DIR"

echo "using smi tool: $(command -v "$SMI")"
echo "output dir:     $DIR"

run() {
    local name="$1"; shift
    echo "  collecting $name ..."
    timeout 15 "$SMI" "$@" >"$DIR/${name}.txt" 2>"$DIR/${name}.err"
    echo "exit=$? cmd=$SMI $*" >"$DIR/${name}.meta"
    timeout 15 "$SMI" "$@" --json >"$DIR/${name}.json" 2>/dev/null
}

# 环境信息
{
    echo "host: $HOST"
    echo "date: $(date -Is)"
    echo "smi:  $(command -v "$SMI")"
    echo "smi version:"
    timeout 10 "$SMI" --version 2>&1
    echo "kernel: $(uname -r)"
    echo "os: $(cat /etc/os-release 2>/dev/null | grep PRETTY_NAME)"
    echo "dtk: $(cat /opt/dtk/*.txt /opt/dtk/version* 2>/dev/null | head -5)"
    echo "/dev/kfd: $(ls -l /dev/kfd 2>&1)"
    echo "drm cards: $(ls /sys/class/drm/ 2>/dev/null | tr '\n' ' ')"
    echo "driver modules: $(lsmod 2>/dev/null | grep -iE 'hydcu|amdgpu|kfd' | tr '\n' ';')"
} >"$DIR/env.txt" 2>&1

# dtop 用到的全部子命令（文本 + JSON 两种输出）
run showuse          --showuse
run showmemuse       --showmemuse
run showtemp         --showtemp
run showpower        --showpower
run showclocks       --showclocks
run showproductname  --showproductname
run showmeminfo      --showmeminfo vram
run showpids         --showpids
run showbus          --showbus
run combined         --showuse --showmemuse --showtemp --showpower --showclocks --showproductname --showmeminfo vram --showpids
run default          # 无参数默认输出（含 Dec%/Enc%/Perf/Mode 列）

# ── 深度监控功能所需的额外能力探测 ────────────────────────────────
# 风扇转速 / 性能状态 / 降频 / PCIe 吞吐 / 拓扑
run showfan          --showfan
run showperflevel    --showperflevel
run showpciebw       --showpciebw
run showreplaycount  --showreplaycount
run showtopo         --showtopo
run showtopotype     --showtopotype
run showtopoweight   --showtopoweight
run showtoponuma     --showtoponuma
run showmaxpower     --showmaxpower
run showvoltage      --showvoltage
# 每进程 GPU 相关
run showpidhcus      --showpidhcus
run showcontainermem --showcontainermem
run showallinfo      --showallinfo
# 默认表格里已经含 Dec%/Enc%，这里再单独探一次编解码相关开关
run showmemuse_decenc --showmemuse --showuse

# 探测该 smi 支持哪些 flag（用于后续适配）
timeout 10 "$SMI" --help >"$DIR/smi-help.txt" 2>&1
# 每个 flag 是否被接受（快速探测，只看退出码与是否有 stderr 报错）
: >"$DIR/flag-support.txt"
for flag in --showfan --showperflevel --showpciebw --showreplaycount --showtopo \
            --showtopotype --showtopoweight --showtoponuma --showmaxpower \
            --showvoltage --showpidhcus --showcontainermem --showallinfo; do
    if timeout 8 "$SMI" "$flag" >/dev/null 2>"$DIR/_flag.err"; then
        echo "$flag OK" >>"$DIR/flag-support.txt"
    else
        echo "$flag FAIL: $(head -c 120 "$DIR/_flag.err" | tr '\n' ' ')" >>"$DIR/flag-support.txt"
    fi
done
rm -f "$DIR/_flag.err"

# ── 进程归属与每进程 GPU 占用所需的本机信息 ───────────────────────
# 取几个占用 DCU 的进程，采集 cgroup（容器/Pod 归属）与 fdinfo（引擎时间）
# 注意: /proc/<pid>/fdinfo 只有进程属主或 root 可读；若进程属 root，
#      请用 sudo 运行本脚本，否则 fdinfo 会是 Permission denied。
{
    echo "note: fdinfo requires process owner or root; rerun with sudo if denied"
    mapfile -t PIDS < <(timeout 10 "$SMI" --showpids 2>/dev/null | grep -oE 'PID: [0-9]+' | awk '{print $2}')
    echo "pids from showpids: ${PIDS[*]:-none}"
    for pid in "${PIDS[@]:0:5}"; do
        echo "===== pid $pid ====="
        echo "--- cgroup ---"
        cat "/proc/$pid/cgroup" 2>&1
        echo "--- comm/cmdline ---"
        cat "/proc/$pid/comm" 2>&1
        tr '\0' ' ' <"/proc/$pid/cmdline" 2>&1
        echo
        echo "--- fdinfo (first 3 fds) ---"
        for fd in $(ls "/proc/$pid/fdinfo" 2>/dev/null | head -3); do
            echo "[fd $fd]"
            cat "/proc/$pid/fdinfo/$fd" 2>&1
        done
    done
} >"$DIR/proc-attribution.txt" 2>&1

# dtop 实际运行验证（如果 dtop 在 PATH 或当前目录）
DTOP=$(command -v dtop 2>/dev/null || echo "./dtop")
if [ -x "$DTOP" ]; then
    echo "  collecting dtop --once output ..."
    timeout 20 "$DTOP" --once -b "${SMI}" -n 5 >"$DIR/dtop-once.txt" 2>&1
fi

TAR="${DIR}.tar.gz"
tar czf "$TAR" "$DIR"
echo
echo "done: $TAR ($(du -h "$TAR" | cut -f1))"
echo "send this file back for parser adaptation."
