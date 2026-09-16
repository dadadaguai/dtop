# dtop

海光 DCU + CPU 一体化终端监控工具（类 top/htop/nvtop），Rust + ratatui 实现，单二进制交付。

```
┌ CPU ──────────────────────────────┐┌ MEMORY ──────────────┐
│ TOTAL   每核条形图   loadavg      ││ RAM / SWAP / 显存合计 │
├ DCU ──────────────────────────────┤└──────────────────────┘
│ 每卡: 显存(VRAM) braille 曲线       │
│       温度 / 功耗 / SCLK / MCLK   │
├ PROCESSES ────────────────────────┤
│ PID USER CPU% MEM% DCU VRAM COMMAND│
└───────────────────────────────────┘
```

## 特性

- **CPU**：总体 + 每核利用率渐变条形图、历史曲线、loadavg（≥32 核自动分组聚合）
- **内存**：used/cache/avail、SWAP、全部 DCU 的显存(VRAM)合计
- **DCU**：每卡利用率/显存(VRAM)历史曲线（braille 字符）、温度/功耗/频率，温度超阈值变色告警；≥8 卡自适应网格或汇总视图
- **DCU 深度指标**：编解码引擎利用率（DEC/ENC）、性能状态（Perf/Mode）、NUMA 亲和性（卡↔NUMA 节点）、风扇转速（硬件支持时）
- **异常提示**：温度超阈值（黄/红）、降频状态、空闲但占用显存（疑似作业卡住）
- **进程**：按 CPU%/MEM% 排序，显示进程占用的 DCU 卡号、显存，**容器/Pod 归属**（k8s/docker），支持发送 SIGTERM（带确认）
- **数据落地**：运行日志 + JSONL 指标记录，便于事后分析
- **优雅降级**：无 DCU / 驱动未装 / smi 工具缺失时给出明确诊断指引，CPU/内存监控不受影响
- **三主题**：dark / nord / light，真彩色

## 安装

### 方式一：下载预编译二进制（推荐）

```bash
# x86_64 Linux（glibc ≥ 2.17，兼容 CentOS 7 / 麒麟 V10 / 统信 UOS）
sudo cp dtop /usr/local/bin/
dtop --version
```

### 方式二：源码编译

依赖：Rust ≥ 1.75（[rustup 安装](https://rustup.rs)，国内可用 `RUSTUP_DIST_SERVER=https://rsproxy.cn`）

```bash
git clone <repo-url> && cd dtop
cargo build --release
sudo cp target/release/dtop /usr/local/bin/
```

### 方式三：musl 静态编译（国产化 OS / 老 glibc 环境）

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt install musl-tools        # 或 yum install musl-gcc
cargo build --release --target x86_64-unknown-linux-musl
# 产物: target/x86_64-unknown-linux-musl/release/dtop （零依赖，scp 即用）
```

## 使用

```bash
dtop                        # 自动探测 hy-smi → rocm-smi → none
dtop -b hy-smi              # 强制 hy-smi 后端（DCU 机器联调用）
dtop -b mock                # 无硬件仿真 8 卡（开发/演示）
dtop --once -n 5            # 快照输出一次即退出（脚本/巡检用，类 gpustat）
dtop -i 0.5 -g 2            # UI 0.5s 刷新，GPU 2s 采样
dtop -t nord -v summary     # nord 主题，启动即 8 卡汇总视图
dtop -r /var/log/dtop-metrics.jsonl --record-interval 5   # 指标 JSONL 落地
dtop -L /var/log/dtop.log --log-level debug               # 调试日志落地
```

### 命令行参数

| 参数 | 说明 | 默认 |
|------|------|------|
| `-i, --interval <SECS>` | UI 刷新间隔 | 1 |
| `-g, --gpu-interval <SECS>` | GPU 采样间隔（独立线程，不阻塞 UI） | 1 |
| `-t, --theme <THEME>` | dark / nord / light | dark |
| `-b, --backend <B>` | auto / hy-smi / rocm-smi / mock / none | auto |
| `-1, --once` | 快照模式，打印后退出 | - |
| `-v, --view <VIEW>` | 初始视图 detail / summary | detail |
| `-s, --sort <KEY>` | 进程排序 cpu / mem | cpu |
| `-n, --procs <N>` | 进程显示条数 | 10 |
| `-L, --log-file <FILE>` | 日志路径 | `~/.local/state/dtop/dtop.log` |
| `--log-level <LEVEL>` | error/warn/info/debug/trace | info |
| `--no-log` | 关闭文件日志 | - |
| `-r, --record <FILE>` | 指标 JSONL 落地（追加写） | - |
| `--record-interval <S>` | 落地间隔 | 10 |
| `--history <N>` | 曲线保留采样点数 | 120 |
| `--temp-warn <C>` | 温度告警阈值（黄） | 75 |
| `--temp-crit <C>` | 温度严重阈值（红） | 85 |
| `--idle-warn-secs <S>` | DCU 空闲但占显存(VRAM)持续多少秒后告警 | 10 |

### 快捷键

| 键 | 功能 |
|----|------|
| `q` / `Esc` | 退出 |
| `h` / `F1` | 帮助 |
| `Tab` / `v` | 详细视图 ↔ 汇总视图 |
| `t` | 切换主题 |
| `↑↓` / `PgUp PgDn` / 滚轮 | 选择进程 |
| `k` | 对选中进程发 SIGTERM（需确认 `y`） |
| `s` | CPU% ↔ MEM% 排序 |
| `+` / `-` | 调整刷新间隔 |

## 在 DCU 机器上联调

### 1. 部署

```bash
scp target/x86_64-unknown-linux-musl/release/dtop user@dcu-node:/tmp/
ssh user@dcu-node
chmod +x /tmp/dtop
```

### 2. 环境自检

```bash
ls /dev/kfd                 # 存在 → 驱动已加载
which hy-smi                # DTK 环境应有 hy-smi（通常 /opt/dtk/bin/）
hy-smi                      # 应能列出卡的基本信息
```

### 3. 快照验证（第一步，最快确认数据链路）

```bash
/tmp/dtop --once -b hy-smi
```

对照 `hy-smi` 原始输出，检查利用率/显存/温度/功耗/频率是否一致。

### 4. TUI 联调

```bash
/tmp/dtop -b hy-smi --log-level debug -L /tmp/dtop-debug.log
```

出现 N/A 或数值不对时，采集样本反馈（见下）。

### 5. 采集 hy-smi 输出样本（解析适配用）

不同 DTK 版本的 hy-smi 输出格式可能有差异。仓库提供了采样脚本：

```bash
bash scripts/collect-smi-samples.sh          # 自动探测 hy-smi/rocm-smi
# 产物: smi-samples-<host>-<date>.tar.gz，包含各命令的文本 + JSON 输出
```

将压缩包发回开发侧即可离线适配解析器（可用 `-b mock` 回放调试）。

### 6. 常见问题

| 现象 | 原因 | 处理 |
|------|------|------|
| `backend:none`，提示 /dev/kfd missing | 驱动未加载 | 安装/加载海光驱动，`lsmod \| grep hydcu` 检查 |
| 提示 smi tool not found | DTK 未装或不在 PATH | `export PATH=/opt/dtk/bin:$PATH` |
| 指标全 N/A 但卡数正确 | hy-smi 输出格式与解析器不匹配 | 跑采样脚本，反馈样本适配 |
| 进程 DCU 列全是 `-` | `hy-smi --showpids` 无输出或需 root | 用 root/sudo 运行；仍无输出则跑采样脚本反馈 |
| 面板标题红色 timeout | smi 命令执行超过 5s | 检查驱动状态，`hy-smi` 手动执行看是否卡死 |

## 数据落地格式

### 日志（默认 `~/.local/state/dtop/dtop.log`）

启动参数、后端探测结果、GPU 采集失败告警、kill 操作审计均记录在此。

### 指标记录（`-r metrics.jsonl`，每行一个 JSON）

```json
{"ts":"2026-09-15T11:50:14+08:00","host":"node01","cpu_total":23.4,"load":[3.2,2.8,2.4],
 "mem_kb":263782400,"mem_used_kb":196608000,"mem_cache_kb":57950208,"mem_avail_kb":67276800,
 "swap_used_kb":2202009,"swap_total_kb":33554432,
 "gpus":[{"index":0,"name":"K100-AI","util":87.0,"vram_used_mb":52428.8,"vram_total_mb":65536.0,
          "temp_c":45.0,"power_w":100.0,"sclk_mhz":1700,"mclk_mhz":1600}]}
```

可直接用 `jq` 分析或导入监控系统。

## 项目结构

```
src/
├── main.rs            # 入口、事件循环、终端管理、日志初始化
├── cli.rs             # clap 参数定义
├── app.rs             # 应用状态、GPU 采集线程、按键处理
├── ui.rs              # ratatui 渲染（布局/面板/图表/弹窗）
├── theme.rs           # dark/nord/light 主题
├── record.rs          # JSONL 指标落地
├── once.rs            # --once 快照模式
└── collectors/
    ├── cpu.rs         # /proc/stat 差分计算
    ├── mem.rs         # /proc/meminfo
    ├── proc.rs        # /proc/*/stat 进程遍历
    └── gpu/
        ├── mod.rs     # GpuBackend trait、自动探测、NoneBackend 降级
        ├── smi.rs     # hy-smi/rocm-smi 后端（JSON 优先 + 文本兜底解析）
        └── mock.rs    # 无硬件仿真后端
```

## 路线图

- [ ] 真实 DCU 机器联调，适配各 DTK 版本 hy-smi 输出
- [ ] sysfs 直读兜底后端（/sys/class/drm，无 smi 工具依赖）
- [ ] 配置文件 `~/.config/dtop/config.toml`
- [ ] 磁盘 IO / 网络流量面板
- [ ] 利用率/温度/功耗阈值告警（可配置）

## License

MIT
