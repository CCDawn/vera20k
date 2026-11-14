# VERA20k 防守 AI（Super AI 防守人格）安装与使用指南

本分支（`superai/defense-next`）在 VERA20k 引擎上新增了一套 **防守 AI 人格**：
在对战图上 AI 照常互相攻伐，在波次防守图（生存/守目标类地图）上同一批 AI
会自动切换为协防人格——沿敌军进攻路线修筑工事、驻守目标环、拦截漏兵、
管理电力与经济。切换全自动，无需选图配置。

## 1. 前置条件

- 一份**零售版《尤里的复仇》**安装目录（需要其中的 `ra2md.mix`、
  `language.mix` 等原版资源文件）。本引擎不含任何原版资源。
- Windows / Linux / macOS 任一平台；Windows 下开箱即用，其余平台自行编译。

## 2. 获取与运行

### 方式 A：直接编译

```bash
git clone -b superai/defense-next https://github.com/CCDawn/vera20k.git
cd vera20k
cargo build --profile fast -p vera20k --bin vera20k
```

产物为 `target/fast/vera20k(.exe)`。首次运行会生成配置目录。

### 方式 B：已有构建产物

将 `vera20k.exe` 放在任意目录，旁边建立 `config.toml`（见下节）。

## 3. 配置 `config.toml`

在 exe 同目录创建 `config.toml`：

```toml
[paths]
# 指向你的零售尤复安装目录（含 ra2md.mix 等）
ra2_dir = "C:/yuri"

[gameplay]
# 防守人格总开关：开启后 AI 才会在防守图上自动协防
defense_ai = true
```

## 4. 安装防守地图

自定义防守图（`.yrm`）放入：

```
Windows: C:\Users\<用户名>\AppData\Local\VERA20k\Maps\
```

地图需要原版可解析的 `.yrm` 格式（不需要 Ares 等扩展平台）。
推荐 CnCNet 官方 Survival 地图池（GitHub 仓库
`CnCNet/cncnet-yr-client-package` 的 `package/Maps/Yuri's Revenge/Survival/`
目录），例如 Defend Liberty、Battle Fortress Revenge、Iron Prison Revival、
Survival of the Fittest V2、Face of Death 等。

提示：社区图常在 `[Basic]` 里只写 `GameMode=standard`，意味着只能在
**标准作战 / 自由交战**下选图。若想在小队联盟里也可见，把该行改为
`GameMode=standard,teamgame` 即可（纯地图文本改动）。

## 5. 开一局协防

1. 主菜单 → 自建遭遇战（Skirmish）。
2. 游戏模式选 **标准作战** 或 **小队联盟**——
   **不要选自由交战**（该模式原版禁止结盟，无法组队）。
3. 加若干 AI，并把它们与你自己拉到**同一个小队**（Team A）。
4. 选一张防守图开始。

防守图上，地图自带的波次机器（触发器刷兵）进攻你们，AI 队友自动协防。

## 6. 如何确认协防已生效

开局后在日志中确认两行（日志位置：exe 同目录 `logs/ra2.log`）：

```
[DEF-AI] map classified: defense-map=true (spawn-triggers=N, wave-host-seat=true)   ← 识别为防守图
[DEF-AI] tick=600 house=Computer1 stance=B power=... ring=... corridor=...
```

- `wave-host-seat=true`：波次刷兵触发器集中归属的席位（波主）已识别，
  坐在该席位上的 AI 自动让位，只当波次房主，不参与协防建设。
- `stance=A`：独立防区模式（各自守各自的锚点与路线）。
- `stance=B`：共享目标协防模式（图上存在 1–3 个共享保护目标时启用；
  全体 AI 在同一目标环上分扇驻守、按基地距离认领进攻路线）。

对战图（无波次机器、波次刷兵也不集中于一个席位）不会武装防守人格，
行为与常规对战 AI 一致。

## 7. 已知边界

- 判负规则差异仅作用于防守形态：对战图恢复“打空即负”的原版判定。
- 防守图的波次房主席位若为空（无人、无 AI 的席位），波次队伍可能无法
  生成——按 CnCNet 生存图的惯例给波主席位留一个 AI 即可；坐普通 AI
  或超级 AI 都行，超级 AI 坐上去会自动让位当波主。
- 地图名含中文等非 Latin-1 字符时，选图列表回退显示文件名
  （引擎菜单字体无中文字形）。

## 8. 反馈

遥测行（`[DEF-AI] tick=...`）与回放（`replays/*.json`）是定位问题的主要
依据，反馈时请附上 `logs/ra2.log` 中开局前后的片段。
