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
run combined         --showuse --showmemuse --showtemp --showpower --showclocks --showproductname --showmeminfo vram --showpids
run default          # 无参数默认输出

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
