# 视频图文总结 MCP — 从零设计

> 版本:v0.6(补充多会话并发:每进程单例端口不冲突;同项目并发编辑用写锁 + 修订号)
> 状态:待评审
> 关联:./BRD.md(需求对话)、./RESEARCH.md(技术核实与实测记录)、./WHISPER_SETUP.md(转录环境与实测)

---

## 0. 一句话定位

**一套面向 agent 的 MCP 服务**:把长知识类视频(30 分钟级)加工成"可看、可校、可总结"的材料——**差帧图 + 带时间戳转录 + 预筛选截图**——供 agent 调用工具完成**校准、总结、报告生成**;用户**通过与 agent 对话**把关、纠偏、二次修改。

### 0.1 三条铁律(本设计的基本假设)

1. **识别与判断归 agent,不归工具。** 图文匹配、转录纠错、交叉印证、总结——这些"需要看图/读懂语义"的事,**全部由 agent 自己的多模态能力完成**。工具只提供材料、执行确定性动作、保存 agent 给出的结论。
2. **工具不接触任何模型 API、不持有任何密钥。** 因为识别由 agent 做,工具内**不存在**任何"调用视觉/语言模型"的代码路径,自然也没有 key 泄露面。
3. **MCP 是通用协议,不绑定具体 agent。** 凡支持 MCP 的 agent 都应能接入;本设计不为某个特定 agent 定制,只依赖两点通用能力:**能看图(多模态)**、**能调工具**。

> 推论:本工具**要求接入的 agent 具备多模态能力**。若 agent 不能看图,步 2/3 无法进行(步 1 仍可用)。

---

## 1. 背景与要解决的问题

### 1.1 问题

长知识类视频直接交给多模态大模型总结,有两组矛盾:

1. **成本/上下文矛盾**:整段视频 token 量大(30 分钟约 50 万 token),而绝大部分画面是静止重复信息。
2. **信息漏失矛盾**:均匀采样(1fps)会漏掉持续 <1s 的关键画面(PPT 翻页、板书切换、演示中的标注);只靠语音转录又丢掉画面独有信息(图表、数值区间、标注线、形态)。

### 1.2 核心思路

> **算法先粗筛"哪里值得看",agent 再看这些地方,人通过对话把关。**

实测依据(RESEARCH.md 第 5 节):知识类视频的**画面切换点 ≈ 内容切换点**。教学视频里每个案例切换都对应差帧图上的一个峰,且与语音时间戳几乎一一吻合。

---

## 2. 设计原则

| 原则 | 说明 |
|------|------|
| **识别归 agent** | 工具**不做任何模型推理**。所有"看图/理解/判断"由 agent 完成;工具负责给材料(图、文、曲线)、干算法活(差帧、抽帧、扫描)、存结论(把 agent 的判断落盘)。 |
| **工具零密钥** | 工具内不存在调用外部模型 API 的路径,不读取任何 API key。 |
| **面向 agent、协议通用** | 交付物是标准 MCP 服务;不为特定 agent 定制,只假设"能看图 + 能调工具"。 |
| **原子步 + 可迭代** | 步 1 一次调用做完(无中间态);步 2 天生迭代(agent 反复调工具);步 3/4 单次产出但允许凭新提示重做。 |
| **一切以时间轴对齐** | 差帧图、截图、转录、校准结论挂在同一条时间轴上,视图按轴联动,**并可回跳视频播放**。 |
| **材料即接口** | 步与步之间只通过**项目目录里的文件**通信,不靠内存状态。任何一步可单独重跑,agent 与人可直接读。 |
| **本地优先** | 计算与文件全本地;工具本身不需要外网。 |

---

## 3. 总体架构

### 3.1 三层角色

```
┌──────────┐   对话(自然语言)   ┌─────────────────┐   MCP 工具调用   ┌──────────────────┐
│   用户    │ ◄────────────────► │      agent       │ ◄──────────────► │  video-reader     │
│          │   看视图/下指令      │ (自带多模态能力)  │  (stdio/HTTP)    │  MCP 服务         │
└──────────┘                    └────────┬────────┘                  └────────┬─────────┘
      ▲                                  │                                     │
      │                                  │ 看图/判断(agent 内部完成,           │ 本地计算
      │                                  │  工具不参与、不持密钥)               │
      │                                  ▼                                     ▼
      │                        ┌─────────────────┐                  ┌──────────────────┐
      └───── 打开 HTML 视图 ───│  agent 多模态推理 │                  │ 项目目录(文件)   │
              (含视频回跳)      └─────────────────┘                  └──────────────────┘
```

- **agent** 是编排者兼识别者:读用户指令 → 调 MCP 工具取材料 → **自己看图/读文并判断** → 调工具把结论存回 → 呈现结果/视图给人。
- **MCP 服务**只做三件事:**供材料、跑算法、存结论**。
- **项目目录**是唯一事实源。
- **HTML 视图**是"图形化界面",由工具渲染,浏览器打开,**时间轴与视频播放联动**。

### 3.2 四步与工具组

```
① 前处理(原子)        ② 校准(迭代)             ③ 总结(原子+可重做)    ④ 报告(原子+可重做)
preprocess()          [供] get_shot()           [供] search_transcript()  build_report()
                      [供] get_transcript()     [供] get_change_curve()   (md | html)
  ↓ 一次做完            [存] record_shot_check()  [补] request_shot()
差帧图                 [存] apply_corrections()  [写] write_summary()
转录                   [算] find_supplement_
截图                        candidates()
                      [算] extract_shot()
```

工具不按"步"硬隔离——步 2 的工具在步 3 也能用(agent 可随时补图/查转录)。"步"是**工作流叙事**,不是权限边界。

### 3.3 控制流

| 步 | 触发者 | 中断? | 迭代? |
|----|--------|-------|-------|
| ① 前处理 | agent 调 `preprocess()`,一次做完 | 否 | 否(要改需显式重跑) |
| ② 校准 | 用户与 agent 对话,agent 反复"取材料→看图判断→存结论" | 是(对话驱动) | **是**,核心特征 |
| ③ 总结 | agent 探索材料后调 `write_summary()` | 否(相对原子) | 允许二次修改 |
| ④ 报告 | agent 调 `build_report()` | 否 | 允许二次修改 |

---

## 4. 核心数据模型

### 4.1 项目目录结构(材料即接口)

```
projects/<project-name>/
├── project.json                 # 元数据 + 各步进度标记
├── source/
│   ├── video.mp4                # 原视频(或软链接)
│   └── video.meta.json          # 时长/分辨率/帧率
│
├── 01_preprocess/
│   ├── change_curve.json        # 差帧图:逐时刻变化率(见 §4.3)
│   ├── dense_regions.json       # 算法识别出的"变化密集区"
│   ├── transcript.srt/.txt/.vtt # 语音转文字(带时间戳)
│   ├── transcript.json          # 结构化转录
│   ├── shots/                   # 预筛选截图
│   │   └── s001_0086.0.jpg
│   └── timeline.html            # 四层时间线视图(含视频回跳,§9)
│
├── 02_calibrate/
│   ├── transcript.fixed.json    # 经 agent 纠错后的转录
│   ├── transcript.diff.md       # 纠错对照(原文 vs 修正)
│   ├── shot_checks.json         # agent 对每张截图的结论(§6.3)
│   ├── supplement_requests.json # 补图请求
│   ├── shots_extra/             # 补图产出
│   ├── timeline.html            # 校准后时间线视图(可重渲染)
│   └── calibration_log.md       # 迭代轨迹
│
├── 03_summary/
│   ├── agent_log.md             # agent 探索轨迹
│   ├── notes/                   # 中间笔记
│   ├── summary.md               # 总结稿
│   └── shots_extra2/            # agent 二次补图
│
└── 04_report/
    ├── report.md
    ├── report.html
    └── assets/
```

> 所有"校准结论"文件(`transcript.fixed.json`、`shot_checks.json`)**由 agent 通过工具写入**,不是工具自己算出来的。

### 4.2 项目进度标记(`project.json`)

```jsonc
{
  "name": "demo",
  "source": { "path": "source/video.mp4", "duration": 985.0 },
  "created_at": "2026-09-09T21:00:00+08:00",
  "progress": {
    "preprocess": { "done": true,  "at": "..." },
    "calibrate":  { "rounds": 3, "last_at": "..." },   // 迭代轮数,不设"通过/未通过"
    "summarize":  { "done": true,  "at": "..." },
    "report":     { "done": true,  "at": "..." }
  }
}
```

> **不设强制审核门**。编排在 agent 侧,"放行"是对话行为;这里只记录进度供参考。

### 4.3 差帧图 `change_curve.json`(步 1 产出 ①)

**定义**:`差帧` = 相邻两帧画面差异算出的**一个数值**;把每对相邻帧的差异值沿**时间轴**排成直方图,就是"差帧图"。**这是视图顶层"像素直方图"的数据源。**

```jsonc
{
  "source": "source/video.mp4",
  "fps_sampled": 1.0,
  "width": 480, "height": 270,
  "duration": 985.0,
  "changes": [
    { "t": 0.0, "score": 0.0 },
    { "t": 1.0, "score": 0.017 }
    // 每采样帧一个点,score ∈ [0,1] = 该帧与上一帧的差异
  ]
}
```

- `score` 沿用现有实现:**颜色直方图 L1 距离(0.75)+ 亮度均值差(0.25)**,归一化 `[0,1]`。
- 展示为**柱状/面积直方图**:横轴=时间,纵轴=变化率。保留未平滑原始值。

### 4.4 密集区 `dense_regions.json`

```jsonc
{
  "regions": [
    { "id": 3, "start": 84.0, "end": 92.0, "mass": 3.41,
      "peak_t": 86.0, "peak_score": 1.0,
      "reason": "high_density" }   // high_density | isolated_peak | long_static_break
  ]
}
```

### 4.5 转录 `transcript.json`

```jsonc
{
  "model": "ggml-medium",
  "language": "zh",
  "segments": [
    { "id": 12, "start": 479.0, "end": 484.0,
      "text": "这是国内派森教程的第一小节",
      "corrected_text": "这是国内 Python 教程的第一小节",   // agent 写入
      "correction": { "type": "terminology", "note": "派森 → Python", "applied": true } }
  ]
}
```

### 4.6 截图清单 `shots.json`

```jsonc
{
  "shots": [
    { "id": "s007", "t": 479.0,
      "file": "01_preprocess/shots/s007_0479.0.jpg",
      "origin": "dense_region",   // dense_region | isolated_peak | llm_supplement | agent_supplement | manual
      "region_id": 3, "reason": null, "status": "keep",   // keep | drop
      "check": {                  // agent 通过 record_shot_check 写入
        "match": true, "audio_visual_sync": true,
        "type_read": "Python 教程 第一节",
        "neighbor_text": "这是国内 Python 教程的第一小节",
        "issues": [], "confidence": "high", "checked_at": "..." } }
  ]
}
```

### 4.7 并发与多会话

MCP 走 **stdio 时是 1:1**——每个 agent 会话由客户端拉起**自己的** `vr mcp` 进程,进程间相互独立。因此:

- **端口天然不冲突**:每个会话 = 一个 MCP 进程 = 自己的查看服务 = 各自绑 `127.0.0.1:0` 拿到的空闲端口。所谓"单例"是**进程内单例**,不是全局单例。
- **浏览器可多开**:查看服务是只读静态服务,任意多个标签页/会话同时看都没问题。

真正的并发风险不在端口,而在**同一项目被多个会话同时写入**(互相覆盖)。规则:

| 机制 | 做法 |
|------|------|
| **项目写锁** | 项目根放 `project.lock`(含 owner/pid/opened_at/mode)。首个打开者取得**写锁**;其他会话只能**只读打开**,并被告知"该项目正被另一会话编辑"。 |
| **原子写** | 所有写文件走"临时文件 + 原子 rename",避免读到半截文件。 |
| **修订号(乐观并发)** | `project.json` 带 `rev`;写操作须带上读到的 `rev`,不匹配则返回 `stale`,agent 重读后再改。 |
| **进程内串行** | 同一 MCP 进程内对同一项目的写操作串行化(内部互斥)。 |

> 结论:多会话**同时查看**任意;**同时编辑同一项目**由写锁 + 修订号保证不互相踩。若将来提供 HTTP 传输供多客户端共享一个服务,则改为"单服务 + 按 session 路由 + 同样的写锁/修订号"。

---

## 5. 步 1:前处理(原子工具)

> MCP 工具:`preprocess(project_id)` —— **一次调用完成全部,无中间态、无中断、不调模型。**

### 5.1 完成三件事

**① 差帧图** — ffmpeg 按 `fps=1`、`scale=480:-1` 抽帧;逐帧算 8-bin/通道 RGB 直方图 + 亮度均值;与上一帧差分 → `score` → `change_curve.json`。

**② 语音转文字(带时间戳)**
1. `ffmpeg -i video.mp4 -ar 16000 -ac 1 -c:a pcm_s16le audio.wav`
2. `whisper-cli.exe -m ggml-medium.bin -l zh -f audio.wav -otxt -osrt -ovtt -of transcript`
3. 解析 SRT → `transcript.json`。
   - 已知局限:中文专有名词必错(实测「达摩院」→「打墨院」、「派森」→「Python」),**由 agent 在步 2 纠错**。

**③ 预筛选关键截图(纯算法,基于差帧图)**
- 取**"变化密集区"的代表帧**,而非只取孤立峰值——信息密集段(连续演示、多图快切)在差帧图上表现为一段高值区。
- 算法见 §5.2;截图质量:宽度 ≥1280px、JPEG q85、文件名带时间戳。

### 5.2 密集区截图算法

```
输入: changes[] (t, score), 采样间隔 ≈1s
参数: W = 8s(窗口), PEAK_THRESH = 0.25

1) 滑窗累计: mass(i) = Σ_{j∈[i,i+W)} score[j]
2) 标定阈值: MASS_THRESH = max(中位数 × k, 绝对下限)     // 防止静止视频全被选中
3) 密集区:        mass 连续超阈值的极大区间 → 合并、取并集
4) 孤立硬切换:    score ≥ PEAK_THRESH 且邻域均值远低,且不属于任何密集区
5) 长静止打断:    长时间(>120s)静止后首次变化 → 标记章节起点
6) 每区取代表帧:  密集区取区间峰值时刻(必要时两端各补一帧);孤立峰取峰值;长静止取变化后首帧
7) ffmpeg 精确抽帧 → shots/sXXX_tttt.jpg + shots.json
```

### 5.3 完成即产出可视化

`preprocess()` 结束前自动调 `render_timeline()` 生成 `01_preprocess/timeline.html`(§9,含视频回跳)。agent 可把该文件呈现给用户作为步 1 的"检查界面"。用户不满意就对话让 agent 调 `extract_shot` / `update_shot` 增删,或显式重跑 `preprocess`。

---

## 6. 步 2:校准(迭代工具组)

> **没有"一次运行完的 calibrate"。** 校准 = agent 反复执行 **"取材料 → 自己看图判断 → 把结论存回"** 的循环,由用户对话驱动。

**本步的定位:核心目标就是解决"识别错误"。** 两类错误:

1. **语音识别错误(ASR)**:同音错字、专有名词听错(实测「Python」→「派森」、「达摩院」→「打墨院」)。
2. **画面识别错误**:看图看错对象/规格/状态,或图与语音对不上。

**自动修不好的,由兜底机制处理——"审查节点"**(即用户与 agent 的对话):用户直接指出"这里还是错的 / 应该是 X",让 agent 立即改。校准的迭代本质,就是 **"自动纠错 → 人工审查 → agent 再改"** 的往复。

### 6.1 循环的三个动作

| 动作 | 工具 | 谁在做 |
|------|------|--------|
| **取材料** | `get_shot`(返回图像)、`get_transcript`、`list_shots` | 工具(供) |
| **看图判断** | —(agent 内部多模态推理) | **agent** |
| **存结论** | `record_shot_check`、`apply_corrections` | 工具(存) |
| **确定性动作** | `find_supplement_candidates`、`extract_shot`、`update_shot` | 工具(算) |

**工具不判断对错,只搬运与保存。**

### 6.2 四个子任务(agent 视角)

**① 图文匹配 + 音画同步**
- agent 调 `get_shot(shot_id)` 拿到**图像本身** + 该时刻前后转录(±6s)。
- agent 自己看图、对照转录,得出结论,调 `record_shot_check` 写回:
  ```
  { match, audio_visual_sync, type_read, neighbor_text, issues[], confidence }
  ```
> 依据(RESEARCH.md 5.5b):单看图会把「样本 15 分钟」臆断成「结构」;图 + 同期转录一起判断,会服从转录、诚实标注"画面无法确认",并主动指出"转录提到 ETC 但画面只有样本"这类矛盾。**这是 agent 的判断,工具只负责把图和文一起递给它。**

**② 转录纠错(ASR 术语校正)**
- agent 调 `get_transcript` 读全量,自查同音错字/专有名词,用 `apply_corrections` 逐条写回。
- **优先依据画面文字**:截图里常直接写着正确写法(如「Python 列表」),agent 应把画面文字当纠错基准;上下文语义次之。
- **术语表默认关闭、可选**:若用户额外提供一份正确写法的领域词表(如 `[样本, PYTHON, 索引, …]`),agent 用它做对照;不给则自行判断。
- 产出 `transcript.fixed.json` + `transcript.diff.md`(人可核对)。
- 校对规则(写进提示模板 §10.4):只改明确识别错误,不润色、不改语序;数字/单位/时间保留;无把握标 `uncertain` 不臆造。

**③ 图文交叉印证**
- agent 综合 `transcript.fixed` 与全部 `shot_checks`,自己列出矛盾点,写进 `cross_validate.md`(通过 `write_note` 之类的写入工具)。

**④ 补充截图(算法漏掉的)** — `find_supplement_candidates`(纯算法)+ `extract_shot`
- 算法扫描转录里的**指代性表达**("如图/这个图/看这里/这边显示/大家看屏幕/这一段/形态")及"提到但附近(±3s)无截图"的时刻 → `supplement_requests.json`。
- agent 判断哪些确实需要,调 `extract_shot` 补帧 → `shots_extra/`,登记进 `shots.json`(`origin=llm_supplement`)。

### 6.3 迭代形态(对话怎么进行)

```
用户: 看一下截图和音频对不对得上
agent: → list_shots + get_shot(逐张) + get_transcript
         【agent 自己看图对照】→ record_shot_check(每张) → render_timeline()
         "11 张匹配;第 6 张(t=602)画面像结构,但同期语音说的是结构图,存疑"
用户: 第6张重新截 602 附近;另外"派森"应该是"Python"
agent: → extract_shot(602) → get_shot(新)【再看】→ 呈现
       → get_transcript → 【自查】→ apply_corrections([...]) → 呈现 diff
用户: 都对,继续
agent: → find_supplement_candidates() → 判断后 extract_shot 补 3 张
```

- 每轮改动写 `calibration_log.md`(哪轮、改了哪张图/哪段字、为什么)。
- 每轮后重渲染 `02_calibrate/timeline.html`,让人看到最新状态。

---

## 7. 步 3:总结(agent 探索 + 原子产出)

> 相对原子:agent 自主探索后一次写出总结;允许用户拿到一版后带新提示重做。

### 7.1 agent 可用工具

| 工具 | 作用 |
|------|------|
| `list_shots` / `get_shot` | 看有哪些截图、**把图递给 agent 看** |
| `search_transcript` / `get_transcript` | 转录检索/读取 |
| `get_change_curve` / `list_regions` | 差帧图与密集区(决定聚焦哪段) |
| `request_shot(t, reason)` | **补图**:发现缺画面时临时抽帧(同 `extract_shot`) |
| `write_note(path, md)` | 写中间笔记 |
| `write_summary(md)` | 落盘 `summary.md` |
| (agent 自带) | 联网检索背景知识(术语、事件、对象) |

### 7.2 建议的探索策略(写进提示模板 §10.4)

1. **先读全局**:transcript.fixed + change_curve + 密集区 → "总—分"框架。
2. **再核局部**:对候选关键段,`get_shot` 看图 + 读同期转录,**agent 图文印证**。
3. **缺口驱动补看**:语音提到某形态/图表却没图 → `request_shot`;外部知识缺口 → 联网检索。
4. **边看边记**:每主题写 `notes/`,保留引用来源(截图 id + 转录时间戳)。
5. **汇总**:`write_summary`。

### 7.3 溯源要求

总结里每个结论应可追溯:**截图 id + 转录时间戳**,便于回跳原视频与人工抽查。

### 7.4 二次修改

用户不满意时给新提示("第 3 个案例补上数值区间和形态"),agent 带旧 `summary.md` + 新材料重做,覆盖或另存新版本。

---

## 8. 步 4:报告(原子产出 + 可重做)

> MCP 工具:`build_report(project_id, format, options)`。

1. **`report.md`**:agent 基于 `summary.md` + 素材写 Markdown(标题层级、段落、引用转录、内嵌关键帧)。
2. **`report.html`**:工具用模板把 MD **图表化**:
   - 顶部时间轴导航(点击章节跳到关键帧)
   - 关键帧图册 + **可回跳的视频播放器**
   - 数据图表:差帧图重现、案例时间分布
   - 图文对照卡片:每张图 + 同期转录 + 校准结论
   - 附录:术语/纠错对照
3. **图像与体积**:图片用**相对路径引用 `04_report/assets/`**,避免 base64 塞进单文件(旧样例报告曾因此膨胀到 889KB)。需单文件分享时提供 `self_contained=true`。
4. **可重做**:用户不满意 → agent 重写 MD → 重新渲染 HTML。

---

## 9. 可视化:工具生成的 HTML 视图

### 9.1 形态与"弹出 UI"

- MCP 工具 `render_timeline(project_id, source="preprocess"|"calibrate", out_path?)` → 生成 HTML,**并可顺带启动本地查看服务 + 自动打开默认浏览器**(即"弹出 UI")。
- 这是每一步的"图形化检查界面"。
- **本地查看服务是什么**:一个绑在 `127.0.0.1` 的极简 HTTP 静态服务,把项目目录当站点发布(`http://127.0.0.1:<port>/…`),由工具按需拉起。
  - **为什么需要**:浏览器直接 `file://` 打开 HTML 时,`<video>` 常因跨域/安全限制无法加载本地视频,"拖时间轴→视频回跳"就会失效;走 `http://127.0.0.1` 后,视图/图片/视频同源,`<video>` 的 seek/播放正常。
  - **附带好处**:多轮迭代时页面可自动刷新。
  - **与控制通道的关系**:MCP 走 stdio 做 agent↔工具的控制;本地服务只负责"给人看",两者互不冲突。
  - **端口分配(避免冲突)**:服务默认绑 `127.0.0.1:0`,由**操作系统自动分配空闲端口**,不可能撞车;**每个 MCP 进程内只起一个长驻服务**(多项目走不同路径),不是每次渲染都新起;进程退出时 OS 自动释放端口。若用户显式指定固定端口且被占用,则自动 +1 重试。
  - **多会话**:stdio 是"1 会话 1 进程",各会话各起自己的服务、各拿自己的端口,互不冲突;并发写同一项目另见 §4.7。
- agent 也可只引用 HTML 路径;在支持内嵌预览的环境里直接展示,不启服务。

### 9.2 四层共轴时间线 + 视频回跳(核心组件)

```
┌────────────────────────────────────────────────────────────────────┐
│ ① 差帧图(变化率沿时间的直方图)  ▁▂▅█▃▁▂▇█▆▂▁▃█▂                  │
│    ▲ 播放头(红线)                                                  │
│────────────────────────────────────────────────────────────────────│
│ ② ═══════════════════════ 时间轴 / 刻度 ═══════════════════════════ │
│────────────────────────────────────────────────────────────────────│
│ ③  [截图]      [截图]         [截图][截图]        [截图]            │
│    (颜色区分 origin;点击看大图 + agent 校准结论)                     │
│────────────────────────────────────────────────────────────────────│
│ ④ 语音转文字   语音转文字      语音转文字        语音转文字          │
│    (显示 corrected_text;悬停看原文;点击看纠错对照)                   │
├────────────────────────────────────────────────────────────────────┤
│ ⑤ [▶ 视频播放器] —— 与时间轴双向联动                                │
└────────────────────────────────────────────────────────────────────┘
```

**时间轴 ↔ 视频双向联动**(本轮新增要求):
- 拖动播放头 / 点时间轴 → `video.currentTime = t`,视频跳到对应处。
- 视频播放 → `timeupdate` 回推播放头,四层随之高亮。
- 点某张截图 → 视频跳到该截图时刻。
- 点某句字幕 → 视频跳到该句起点。

**加载本地视频的前提**:浏览器直接 `file://` 打开时对本地媒体有跨域/权限限制,稳妥做法是由本地查看服务提供视频与视图(§9.1)。

### 9.3 其他视图

| 视图 | 内容 | 步 |
|------|------|----|
| 校准对照表 `calibrate.html` | 逐条纠错(原文/修正/理由/是否采纳)+ 截图匹配结论(不匹配高亮) | 2 |
| 补图清单 | 补图请求 + 已补图 | 2 |
| 报告预览 | `report.html`(同样含视频回跳) | 4 |

---

## 10. 技术选型与模块划分

### 10.1 语言(统一 Rust,可外挂)

- **核心与 MCP 服务:统一 Rust**。抽帧/差帧是计算与数据流密集;已有可用实现;单二进制分发;MCP 服务与计算同进程。
- **可外挂**:某环节在 Rust 里成本过高时,允许以**子进程**外挂(Python/Node),工具内部 `Command` 调用。外挂须:输入输出走文件/STDIN-STDOUT,无状态,可单测。
- **绝不引入模型 SDK**:工具不调外部模型,故无需任何 LLM/视觉 SDK 与密钥管理。

### 10.2 MCP 协议实现(已定:stdio + 弹出 UI)

- **传输:MCP stdio JSON-RPC**(agent 本地拉起)。**协议层与具体 agent 无关。**
- **可视化交互**:渲染视图的工具在返回 HTML 路径的同时,可**启动本地查看服务并自动打开默认浏览器**(弹出 UI,见 §9.1)。stdio 只做控制,本地服务只负责展示。
- 实现:优先 Rust 生态 MCP SDK(如 `rmcp`);API 不稳则直接实现 tools 的 JSON-RPC。
- 同时暴露:
  - **Tools**:动作与服务(§11)
  - **Resources**:只读项目文件(`change_curve.json`、`transcript.json`、`shots/*.jpg` 等),便于 agent 直接读
  - **Prompts**:内置校准/总结/报告的提示模板(§10.4),降低接入门槛

### 10.3 模块划分

```
video-reader/
├── crates/
│   ├── vr-core/     # 差帧、密集区、抽帧(重写;旧实现已不在本仓库)
│   ├── vr-asr/      # 调 whisper-cli,解析 srt → transcript.json
│   ├── vr-project/  # 项目目录 + 数据结构 + project.json
│   ├── vr-viz/      # 时间线/对照表/报告 HTML 渲染(含视频回跳 + 前端脚本)
│   ├── vr-report/   # MD → HTML 图表化模板
│   ├── vr-mcp/      # MCP 服务:供材料 + 存结论 + 算法动作 + 提示模板
│   └── vr-cli/      # 独立命令行(脱离 agent 也能测/用)
├── ui/              # 视图模板 + 前端(时间线组件)
└── docs/
```

> 注意:没有 "vision" 模块——识别由 agent 完成,工具侧无对应代码。

### 10.4 MCP 提示模板(由本工具提供,内容自定)

**P1 `calibrate_shots`** —— 校准截图
```
你是视频图文校准助手。请对项目中的每张关键截图完成图文匹配与音画同步检查:
1. 调用 list_shots 获取截图清单;对每张调用 get_shot 获取图像与同期转录。
2. 亲自查看图像,并严格对照同期转录:转录明确说的信息(对象/规格/方向)必须服从,
   画面独有的信息(形态/标注线/数值)如实读出并标注"从画面推断"。
3. 若转录与画面矛盾,写入 issues。
4. 用 record_shot_check 逐张存回结论;不要臆测,不确定就标 low confidence / "画面无法确认"。
5. 完成后调用 render_timeline 刷新视图。
```

**P2 `fix_transcript`** —— 转录纠错
```
你是中文金融/技术视频的转录校对员。
逐句检查转录,只修正明确的识别错误(同音错字、专有名词、术语)。纠错依据优先级:
1) 关键帧画面上的文字(最可靠,截图常直接写着正确写法);
2) 上下文语义;
3) 术语表(可能为空;为空时忽略此项):{glossary}
遵守:
- 不改语序、不润色、不改写意思;
- 数字/单位/时间除非明显识别错,否则保留;
- 无把握的句子保留原文并标注 uncertain,不得臆造。
用 apply_corrections 逐条写回(附 reason);完成后调用 render_calibration_table。
```

**P3 `find_missing_shots`** —— 补图
```
调用 find_supplement_candidates 取回候选补图请求(语音提到指代对象但附近无截图)。
逐条判断是否确实需要补图;需要则调用 extract_shot(t, reason)。
完成后 render_timeline 刷新。
```

**P4 `summarize_video`** —— 总结
```
你要为这个视频产出一份图文总结:
1. 先读 transcript.fixed 与 change_curve/dense_regions,建立整体结构。
2. 对每个关键段,用 get_shot 看图 + 读同期转录,以图文互证得出结论。
3. 若某处缺画面(语音提到某形态/图表却没有图),调用 request_shot 补看。
4. 每条结论必须附来源:截图 id + 转录时间戳。
5. 用 write_summary 落盘;需要时先写 notes/ 笔记。
```

**P5 `build_report`** —— 报告
```
基于 summary.md 与素材写一份结构化 Markdown 报告(含关键帧与转录引用),
用 write_report_md 落盘;然后调用 build_report(format="html") 生成图表化 HTML。
用户提出修改时,带着旧稿与新材料重写并重新渲染。
```

### 10.5 命令行(便于不接 agent 时自测)

```bash
vr init <video> [--name x]        # 建项目
vr preprocess <project>           # 步 1(纯算法)
vr ui         <project>           # 本地视图 + 视频服务
vr mcp        [--stdio|--http]    # 启动 MCP 服务
vr status     <project>
```

---

## 11. MCP 工具清单

**约定**:【供】= 把材料给 agent;【存】= 把 agent 的结论落盘;【算】= 纯算法动作。**没有任何工具调用外部模型 API。**

### A. 项目
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `create_project` | 算 | `video_path, name?` | `project_id, dir` |
| `list_projects` | 供 | — | 项目列表 + 进度 |
| `get_project` | 供 | `project_id` | `project.json` |

### B. 步 1 前处理(原子)
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `preprocess` | 算 | `project_id, [fps, width]` | 差帧图/转录/截图/`timeline.html` 的路径与摘要 |

### C. 材料服务(给 agent 看)
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `list_shots` | 供 | `project_id` | 截图清单(origin/t/是否已校准) |
| `get_shot` | 供 | `project_id, shot_id` | **图像内容** + 元数据 + 同期转录 |
| `get_transcript` | 供 | `project_id, [range]` | 转录分段(含 corrected_text) |
| `search_transcript` | 供 | `project_id, query` | 命中分段 |
| `get_change_curve` | 供 | `project_id, [range]` | 变化率曲线 |
| `list_regions` | 供 | `project_id` | 密集区清单 |

### D. 结论回写(存 agent 的判断)
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `record_shot_check` | 存 | `project_id, shot_id, {match, audio_visual_sync, type_read, issues, confidence}` | 更新 `shots.json` |
| `apply_corrections` | 存 | `project_id, [{segment_id, corrected_text, reason}]` | 更新 `transcript.fixed.json` + `transcript.diff.md` |
| `write_note` | 存 | `project_id, path, md` | 落盘 |
| `write_summary` | 存 | `project_id, md` | 落盘 `summary.md` |
| `write_report_md` | 存 | `project_id, md` | 落盘 `report.md` |

### E. 算法动作
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `find_supplement_candidates` | 算 | `project_id` | 补图请求清单 |
| `extract_shot` / `request_shot` | 算 | `project_id, t, reason` | 新 shot_id |
| `update_shot` | 算 | `project_id, shot_id, action(keep/drop/retime)` | 更新结果 |
| `build_report` | 算 | `project_id, format(md/html), self_contained?` | `path` |

### F. 可视化
| 工具 | 类型 | 入参 | 出参 |
|------|------|------|------|
| `render_timeline` | 算 | `project_id, source?, out_path?` | `html_path` |
| `render_calibration_table` | 算 | `project_id` | `html_path` |

---

## 12. 典型 agent 会话(端到端)

```
用户: 处理 D:\videos\第一讲.mp4
agent: → create_project → preprocess                       # 步1 一次做完(纯算法)
       → 呈现 timeline.html 摘要("12 个变化密集区,12 张图,转录 985s")
用户: 看看图对不对
agent: → list_shots → 对每张 get_shot 【agent 自己看图 + 对照转录】
       → record_shot_check(每张) → render_timeline()
       "11 张匹配;第 6 张(t=602)画面像结构,同期语音说结构图,存疑"
用户: 第6张重新截 602 附近;"派森"应为"Python"
agent: → extract_shot(602) → get_shot(新图)【再看】
       → get_transcript 【自查】→ apply_corrections → 呈现 diff
用户: 都对,继续
agent: → find_supplement_candidates → 判断后 extract_shot 补 3 张
用户: 出总结
agent: → search_transcript/get_shot/list_regions ...       # 步3 探索
       → request_shot(缺图处) → write_summary → 呈现
用户: 第一个案例补上数值区间
agent: → 带新提示改写 summary.md
用户: 出报告
agent: → write_report_md → build_report(html) → 呈现 report.html   # 步4(含视频回跳)
用户: 加个"形态一览"表
agent: → 重写 MD → build_report(html) 重渲染
```

---

## 13. 里程碑

| 阶段 | 内容 | 依赖 |
|------|------|------|
| **M0 schema 定稿** | §4 的 JSON schema + 目录结构落成真实文件 | 本文评审 |
| **M1 vr-core + 步1** | 差帧 + 密集区 + 截图 + ASR,CLI 可跑 | 复用现有 video-reader / transcribe.sh |
| **M2 时间线视图** | `render_timeline` 四层 + 视频回跳 + 本地服务 | M1 |
| **M3 MCP 骨架** | `vr-mcp` 服务 + 项目/材料/算法工具接上 | M1,M2 |
| **M4 步2 循环** | `get_shot`/`record_shot_check`/`apply_corrections` + 提示模板 P1–P3 + 对照表视图 | M3 |
| **M5 步3 总结** | 只读/补图/写入工具 + 提示模板 P4 | M4 |
| **M6 步4 报告** | MD 生成 + HTML 模板 + P5 | M5 |
| **M7 加固** | 增量重跑、失败重试、批量、性能、外挂接口 | 全部 |

---

## 14. 关键决策(已定稿)

| # | 决策 | 说明 |
|---|------|------|
| 1 | **MCP 传输:stdio** | agent 本地拉起;渲染视图时**可弹出 UI**(启本地查看服务 + 打开浏览器)。 |
| 2 | **术语表:默认关闭、可选** | 纠错优先依据"画面文字 + 上下文";用户需要时可补一份领域词表。 |
| 3 | **报告 HTML:相对路径 assets** | 图片引用 `04_report/assets/`,避免 base64 单文件膨胀;自包含为可选模式。 |
| 4 | **视频回跳:本地查看服务** | 绑 `127.0.0.1:0`(OS 自动分配空闲端口)的极简 HTTP 静态服务,保证 `<video>` 可加载可 seek;与 stdio 控制通道分离;**每 MCP 进程单例长驻**(多会话 = 多进程 = 各端口,不冲突,见 §4.7)。 |

> 至此无遗留待定项,可进入 M0 / M1。

---

## 15. 与现有资产的关系

> 旧实现(单文件 `src/main.rs` + `scripts/` 下的 Python/bash)已在重建时**从工作区移除**,本仓库不再保留它的存档。下表说明这些资产的**去向**:哪些思路被继承、哪些被废弃。

| 旧资产(已归档) | 在新设计中的去向 |
|----------------|-----------------|
| `src/main.rs`(video-reader) | 差帧 / 峰值 / 抽帧算法**重写进 `vr-core`**,并新增变化密集区检测(§5.2) |
| `scripts/transcribe.sh` | 思路并入 `vr-asr`:抽 wav → whisper-cli → 结构化 `transcript.json` |
| `tools/whisper/`(二进制 + 模型) | **仍在工作区,原样复用**(体积大,不入 git) |
| `scripts/describe_frames.py` | **废弃**(识别改由 agent 做);其提示词作 §10.4 参考 |
| `scripts/describe_frames_with_context.py` | **废弃**;"图文联合"思路已内化为提示模板 P1 |
| `scripts/build_report.py` | 思路并入 `vr-report`:MD → HTML,图片改相对路径 |
| `timeline_demo.json` / `report_demo.md` | 实测样例;`fixtures/test_sample.mp4` 保留作回归素材 |
| `BRD.md` / `RESEARCH.md` / `WHISPER_SETUP.md` | 文档,已整理至 `docs/` |

---

## 16. 风险

| 风险 | 等级 | 缓解 |
|------|------|------|
| 接入的 agent 不具多模态能力 | 高 | 明确前置要求;步 1 仍可用,步 2/3 需看图能力 |
| agent 读图误判(对象/规格) | 中 | 提示模板强制图文联合 + 区分"转录明确/画面推断";对话中人工裁决 |
| ASR 术语错误累积 | 中 | 提示模板 P2 + diff 对照人工确认 |
| 密集区阈值不适配不同视频类型 | 中 | 阈值由曲线统计量自适应 + 视图可视化调参重跑 |
| agent 在步 2/3 发散、成本失控 | 中 | 工具粒度可控 + 补图/检索预算 + 轨迹落盘可审 |
| 多会话并发编辑同一项目互相覆盖 | 中 | 项目写锁 + 原子写 + 修订号(§4.7) |
| 浏览器加载本地视频受限 | 低 | 默认走本地查看服务(§9.1) |
| 视图在长视频下渲染卡顿 | 低 | 时间线窗口化渲染、缩略图懒加载 |
| 报告 HTML 体积膨胀 | 低 | 默认相对路径 assets;自包含模式显式可选 |
| Rust MCP SDK 尚不成熟 | 低 | 抽象传输层;必要时直接实现 tools 的 JSON-RPC |
