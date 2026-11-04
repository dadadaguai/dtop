# dtop

海光 DCU 的 nvitop 风格终端监控工具。Rust + ratatui 实现，单二进制交付。

**直接调用 `librocm_smi64.so` 的 `rsmi_*` 接口**（和 `hy-smi` 同一条底层路径），
不 shell 子进程，零第三方运行时依赖。

```
dtop 0.1.0  host: dcu-node01  devices: 8  up 3d 4h  10:00:00  host cpu 2.4%  mem 8.2%
┌ Devices ──────────────────────────────────────────────────────────────────────┐
│ HCU  Model     Temp   Power   HCU%                CU%   VRAM          SCLK  MCLK│
│  0   DCU-3G    44.0C  323W    ▏████████████ 100.0%  67.2%  ▏███ 137.4/144G 1350M 875M│
│  1   ...                                                                    │
┌ Utilization ──────────────────────────────────────────────────────────────────┐
│ CPU: 2.4%                          AVG GPU UTL: 94.4%                         │
│ ▁▁▂▃▂▁                             ▇▇▇▆▇▇▇                                  │
│ MEM: 8.2%                          AVG GPU MEM: 94.3%                         │
│ ▁▁▁▁                               ▇▇▇▇▇▇                                  │
│ |60s      |30s      |0s  (右端 = 现在)                                       │
┌ Processes ────────────────────────────────────────────────────────────────────┐
│   PID   USER   HCU  VRAM    CU%   CPU%  MEM%  COMMAND                        │
│ > 69131 root    0   135.6G  7.8   99.0  0.9   sglang::scheduler_TP0          │
└───────────────────────────────────────────────────────────────────────────────┘
 q quit | r refresh | ↑/↓ select | p/m/c/u sort | 1-9 filter | a all | t theme | k kill
```

## 特性

- **Devices 表**：每卡一行 —— 型号 / 温度 / 功耗 / 整卡利用率(HCU%)渐变条 / **CU 利用率** / 显存(VRAM)渐变条 / SCLK / MCLK，温度/功耗/利用率阈值变色
- **Utilization 波形**：主机 CPU/MEM + **平均 GPU 利用率/显存** 四路历史波形
- **Processes 表**：**每进程×每卡一行**，含每进程 CU%、VRAM、CPU%、MEM%，容器/Pod 归属（有容器时附在命令后）
- **数据源**：`librocm_smi64.so` 直接 FFI（HCU%/CU% 为驱动阻塞采样，跨卡轮转）
- **异常提示**：温度超阈值（黄/红）、降频、空闲但占显存
- **三主题** dark / nord / light，真彩色

## 安装

```bash
# 预编译（musl 静态，零依赖，CentOS7/麒麟/统信可跑）
sudo cp dtop /usr/local/bin/

# 或源码编译（Rust ≥ 1.75）
cargo build --release --target x86_64-unknown-linux-musl
```

## 使用

```bash
dtop                      # 自动走 rsmi 后端
dtop -b mock              # 无硬件模拟 8 卡（开发/演示）
dtop --once               # 文本快照一次即退（脚本用）
dtop --json               # JSON 快照（schema 稳定，null = 不支持）
dtop -d 0,1,3             # 只看指定卡
dtop --window-ms 200      # HCU%/CU% 采样窗口（驱动阻塞时长）
dtop -i 0.5 -g 2          # UI 0.5s 刷新，GPU 2s 采样
dtop -t nord              # 主题
dtop -r metrics.jsonl     # 指标 JSONL 落地
```

| 参数 | 说明 | 默认 |
|------|------|------|
| `-i, --interval <SECS>` | UI 刷新间隔 | 1 |
| `-g, --gpu-interval <SECS>` | GPU 采样间隔 | 1 |
| `-t, --theme` | dark / nord / light | dark |
| `-b, --backend` | rsmi / mock（auto=rsmi） | auto |
| `-d, --device 0,1,3` | 只看指定卡 | 全部 |
| `--window-ms <MS>` | HCU%/CU% 采样窗口 | 150 |
| `-1, --once` | 文本快照一次即退 | - |
| `--json` | JSON 快照一次即退 | - |
| `-L, --log-file` / `--log-level` | 日志 | `~/.local/state/dtop/dtop.log` / info |
| `-r, --record <FILE>` | 指标 JSONL 落地 | - |
| `--temp-warn/--temp-crit <C>` | 温度告警阈值 | 75 / 85 |
| `--idle-warn-secs <S>` | 空闲但占显存告警 | 10 |

## 按键

`q` 退出 · `r` 重绘 · `↑↓/PgUp/PgDn` 选择 · `p/m/c/u` 按 PID/VRAM/CU%/CPU% 排序 ·
`1-9` 过滤卡（切换） · `a` 全部 · `t` 主题 · `k` SIGTERM 选中进程（确认） · `+/-` 刷新率

## 数据源：librocm_smi64 直接 FFI

搜索顺序：`$DCUTOP_LIBRARY_PATH` → `/opt/hyhal/lib` → `/opt/dtk/lib` → 动态链接器。
找不到时给出可操作的报错，不会 crash（无卡机器用 `dtop -b mock` 演示）。

| rsmi 接口 | 指标 |
|---|---|
| `rsmi_dev_hcu_util_get` / `rsmi_dev_cu_util_get` | 整卡利用率 / **CU 利用率**（阻塞 `window_ms`，跨卡轮转） |
| `rsmi_dev_memory_usage_get` / `memory_total_get` | 显存已用/总量 |
| `rsmi_dev_busy_percent_get` | 瞬时利用率（首帧兜底） |
| `rsmi_dev_power_get` / `power_cap_get` | 功耗 / 功耗墙 |
| `rsmi_dev_temp_metric_get` | 温度（edge 传感器） |
| `rsmi_dev_gpu_clk_freq_get` | SCLK / MCLK |
| `rsmi_dev_pci_id_get` / `topo_get_numa_node_number` / `cu_num_get` | BDF / NUMA / CU 数 |
| `rsmi_compute_process_info_by_pid_get_v2` | 每进程显存 + 每卡占用 |
| `rsmi_dev_proc_usage_get` | 每进程 CU% |

> 已在 8× HYGON DCU-3G（BW1100，librocm_smi64.so.2.8）上由 dcutop 对拍验证过 FFI 签名与单位。

## 架构

```
TUI (ratatui) / --once / --json
      │  只读 SystemSnapshot（含每卡历史）
  GPU 采集线程（瞬时指标全采 + HCU%/CU% 阻塞窗口轮转）
      │
  GpuBackend trait
  ┌───────┴───────┐
  RsmiBackend   MockBackend
  (FFI → librocm_smi64.so → HCU 驱动)
  host: /proc（CPU%/MEM%/USER/COMMAND/容器）
```

## License

MIT
