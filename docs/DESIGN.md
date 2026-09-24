# 视频图文总结 MCP — 从零设计

> 版本:v0.8(§4 数据模型定稿:JSON Schema 与 Rust 类型落成真实文件,M0)
> 状态:待评审
> 关联:./BRD.md(需求对话)、./RESEARCH.md(技术核实与实测记录)、./WHISPER_SETUP.md(转录环境与实测)、[./schema/](./schema/)(**项目材料格式的唯一权威**)与 [./devlog/](./devlog/)(开发过程记录)

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
│   用户    │ ◄────────────────► │      agent       │ ◄──────────────► │  vidbrief         │
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

> **本节已定稿(M0)。** 格式的**唯一权威**是 [`docs/schema/`](./schema/) ——
> 那里有 JSON Schema、示例文件(同时是测试夹具)和全局约定。
> 本节的代码侧对应物是 `crates/vidbrief-project/`。
> 两边由 `cargo test -p vidbrief-project` 做**三方锚定**校验(示例 ↔ schema ↔ Rust 类型),
> 任何一侧漂移都会红。

### 4.1 项目目录结构(材料即接口)

```
projects/<project-name>/
├── project.json                 # 元数据 + 各步进度标记(§4.2)
├── project.lock                 # 写锁:存在即表示有会话在编辑(§4.7)
├── source/
│   ├── video.<ext>              # 源视频:硬链接 / 软链接 / 复制(§4.8)
│   └── video.meta.json          # 时长/分辨率/帧率/编码/sha256
│
├── 01_preprocess/
│   ├── change_curve.json        # 差帧图:逐时刻变化率(§4.3)
│   ├── dense_regions.json       # 算法识别出的"变化密集区"(§4.4)
│   ├── transcript.json          # 结构化转录(原始,永不就地改写)
│   ├── transcript.srt/.txt/.vtt # whisper 原始输出(便于人工核对)
│   ├── shots.json               # 截图清单 + agent 的校准结论(§4.6)
│   ├── shots/                   # 步 1 抽出的截图
│   │   └── s001_0086.0.jpg
│   └── timeline.html            # 四层时间线视图(含视频回跳,§9)
│
├── 02_calibrate/
│   ├── transcript.fixed.json    # 经 agent 纠错后的转录(全量副本,§4.5)
│   ├── transcript.diff.md       # 纠错对照(原文 vs 修正)
│   ├── supplement_requests.json # 补图候选 + agent 的取舍(§4.9)
│   ├── cross_validate.md        # 图文交叉印证的矛盾点清单
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

> 所有"校准结论"文件(`transcript.fixed.json`、`shots.json` 的 `check` 字段、`supplement_requests.json` 的 `status`)**由 agent 通过工具写入**,不是工具自己算出来的。

**路径规则**:项目内所有路径都是**项目根相对路径**,用 `/` 分隔(如 `01_preprocess/shots/s007_0479.0.jpg`)。
禁止绝对路径、盘符、反斜杠与 `..` —— 项目必须能整体搬走。Rust 侧由 `RelPath` 类型强制(构造即校验)。

**为什么没有 `shot_checks.json`**:v0.7 曾把校准结论放在 `02_calibrate/shot_checks.json`,
但截图清单本身(§4.6)已经有 `check` 字段 —— 同一件事两处落点必然不同步。
M0 定稿时**取消**该文件,以 `01_preprocess/shots.json` 为唯一权威。

### 4.2 项目进度标记(`project.json`)

```jsonc
{
  "schema_version": 1,           // 该项目下所有 JSON 的格式版本;不符直接报错,不做静默兼容
  "rev": 7,                      // 乐观并发修订号:每次成功写入 +1(§4.7)
  "id": "demo",              // 同时是目录名;ASCII 小写/数字/-/_
  "name": "第一讲 开场",        // 人类可读名,可含中文
  "created_at": "2026-09-24T21:00:00+08:00",
  "source": {
    "path": "source/video.mp4",  // 项目内位置
    "original": "D:/videos/第一讲.mp4",  // 原始绝对路径,纯信息性,不用于访问文件
    "link": "hardlink"           // hardlink | symlink | copy(§4.8)
  },
  "glossary": null,              // 术语表,默认 null(关闭)
  "progress": {
    "preprocess": { "done": true,  "at": "..." },
    "calibrate":  { "rounds": 3, "last_at": "..." },   // 迭代轮数,不设"通过/未通过"
    "summarize":  { "done": true,  "at": "..." },
    "report":     { "done": true,  "at": "..." }
  }
}
```

> **不设强制审核门**。编排在 agent 侧,"放行"是对话行为;这里只记录进度供参考。
>
> **时长不在这里**:`duration` 归 `source/video.meta.json` 独家所有。
> 一个数字两个落点迟早会不一致,宁可让 `list_projects` 多读一个文件。

### 4.3 差帧图 `change_curve.json`(步 1 产出 ①)

**定义**:`差帧` = 相邻两帧画面差异算出的**一个数值**;把每对相邻帧的差异值沿**时间轴**排成直方图,就是"差帧图"。**这是视图顶层"像素直方图"的数据源。**

```jsonc
{
  "source": "source/video.mp4",
  "duration": 985.0,
  "fps_sampled": 1.0,
  "width": 480, "height": 270,   // 参与打分的缩小后尺寸
  "metric": "hist_l1_0.75+luma_0.25",   // 打分口径标识
  "generated_at": "...",
  "changes": [
    { "t": 0.0, "score": 0.0 },
    { "t": 1.0, "score": 0.017 }
    // 每采样帧一个点,score ∈ [0,1] = 该帧与上一帧的差异;首点(无前一帧)恒为 0
  ]
}
```

- `score` 沿用现有实现:**颜色直方图 L1 距离(0.75)+ 亮度均值差(0.25)**,归一化 `[0,1]`。
- 展示为**柱状/面积直方图**:横轴=时间,纵轴=变化率。保留未平滑原始值。
- `metric` 必须随算法一起改 —— 否则无法分辨磁盘上的旧文件是哪套口径算的。

### 4.4 密集区 `dense_regions.json`

```jsonc
{
  "generated_at": "...",
  "params": {                    // 可调输入
    "window": 8.0,               // 滑窗宽度 W(秒)
    "peak_threshold": 0.25,      // 孤立硬切换阈值
    "mass_k": 3.0,               // 密集区阈值 = max(窗口中位数 × k, floor)
    "mass_floor": 0.6,           // 绝对下限:防止静止视频整段被判为密集
    "merge_gap": 4.0,            // 间隔小于此值(秒)的密集区合并
    "static_break": 120.0        // 长静止判据(秒)
  },
  "stats": {                     // 本次运行算出的中间量
    "sampled_points": 985,
    "mass_median": 0.10,
    "mass_threshold": 0.60       // 实际生效的阈值
  },
  "regions": [
    { "id": 3, "start": 84.0, "end": 92.0, "mass": 3.41,
      "peak_t": 86.0, "peak_score": 1.0,
      "reason": "high_density" }   // high_density | isolated_peak | long_static_break
  ]
}
```

> **为什么连 `stats` 一起存**:密集区阈值必须随视频类型自适应。出问题时只有靠这两组数字
> 才能复现"当时为什么这样判",并据此调参重跑。`params` 的默认值在 M1 用真实长视频标定。

### 4.5 转录 `transcript.json` / `transcript.fixed.json`

两个文件**共用同一 schema 与同一 Rust 类型**:

- `01_preprocess/transcript.json` —— 步 1 产出,**原始**。`corrected_text` 与 `correction` **恒为 `null`**。
- `02_calibrate/transcript.fixed.json` —— 步 2 产出,**全量副本** + 填充纠错结论。

```jsonc
{
  "source": "source/video.mp4",
  "model": "ggml-medium",
  "language": "zh",
  "duration": 985.0,
  "generated_at": "...",         // fixed 沿用此值,不因纠错而更新
  "segments": [
    { "id": 12, "start": 479.0, "end": 484.0,
      "text": "这是国内派森教程的第一小节",              // ASR 原文,永不就地改写
      "corrected_text": "这是国内 Python 教程的第一小节",     // agent 写入
      "correction": { "type": "terminology", "note": "派森 → Python",
                      "reason": "截图 s001 画面文字为「Python 列表」" } }
  ]
}
```

- `correction.type` ∈ `homophone`(同音错字)| `terminology`(术语/专有名词)| `number`(数字/单位/时间)| `uncertain`(存疑未改)| `other`。
- **`type: "uncertain"` 时 `corrected_text` 必须保持 `null`** —— 存疑不等于可以臆造,
  疑似写法只写在 `note` 里,留给用户在审查节点裁决。

> **为什么不就地改原文**:改了原文,"agent 到底动了哪里"就查不出来了,
> 用户也就无法核对纠错是否合理。原始产出永远是只读证据。

### 4.6 截图清单 `shots.json`

```jsonc
{
  "generated_at": "...",         // 首次生成时间;后续增删改不更新
  "shots": [
    { "id": "s007", "t": 479.0,
      "file": "01_preprocess/shots/s007_0479.0.jpg",
      "origin": "dense_region",  // dense_region | isolated_peak | agent_request | manual
      "region_id": 3,            // 算法产物指向 dense_regions.json;补图为 null
      "status": "keep",          // keep | drop
      "reason": null,            // 补图理由(agent_request / manual 时应有值)
      "created_at": "...",
      "check": {                 // agent 通过 record_shot_check 写入;未校准为 null
        "match": true, "audio_visual_sync": true,
        "type_read": "Python 教程 第一节",       // 从画面读到的内容
        "neighbor_text": "这是国内 Python 教程的第一小节",
        "issues": [], "confidence": "high", "checked_at": "..." } }
  ]
}
```

**命名规则**:文件名 = `{id}_{t:06.1}.jpg`,如 `s007_0479.0.jpg`
(`{:06.1}` = 整数部分补零到至少 4 位;超过 9999 秒自然变宽,不截断)。

**id 永不复用**:`drop` 只改 `status` 不回收号,补图继续往后发。这样任何日志、报告、对话里
出现过的 `s007`,永远指同一时刻的同一张图。

> v0.7 的 `llm_supplement` 在 M0 定稿时并入 `agent_request` —— 补图都是 agent 判的,
> 分两个名字只会让人猜区别在哪。步 3 的二次补图靠存放目录(`shots_extra2/`)区分。

### 4.7 并发与多会话

MCP 走 **stdio 时是 1:1**——每个 agent 会话由客户端拉起**自己的** `vidbrief mcp` 进程,进程间相互独立。因此:

- **端口天然不冲突**:每个会话 = 一个 MCP 进程 = 自己的查看服务 = 各自绑 `127.0.0.1:0` 拿到的空闲端口。所谓"单例"是**进程内单例**,不是全局单例。
- **浏览器可多开**:查看服务是只读静态服务,任意多个标签页/会话同时看都没问题。

真正的并发风险不在端口,而在**同一项目被多个会话同时写入**(互相覆盖)。规则:

| 机制 | 做法 |
|------|------|
| **项目写锁** | 项目根放 `project.lock`(含 `pid` / `host` / `session` / `opened_at`)。首个打开者取得**写锁**;其他会话只能**只读打开**,并被告知"该项目正被另一会话编辑"。持锁进程已死 = 死锁可回收。 |
| **原子写** | 所有写文件走"临时文件 + 原子 rename",避免读到半截文件。 |
| **修订号(乐观并发)** | `project.json` 带 `rev`;写操作须带上读到的 `rev`,不匹配则返回 `stale`,agent 重读后再改。 |
| **进程内串行** | 同一 MCP 进程内对同一项目的写操作串行化(内部互斥)。 |

> 结论:多会话**同时查看**任意;**同时编辑同一项目**由写锁 + 修订号保证不互相踩。若将来提供 HTTP 传输供多客户端共享一个服务,则改为"单服务 + 按 session 路由 + 同样的写锁/修订号"。
>
> **实现进度**:M0 只定 `project.lock` 的格式与类型(`vidbrief_project::ProjectLock`),
> 获取/释放/死锁检测的逻辑随 M3(MCP 多会话真正出现时)一起做。

### 4.8 源视频如何进入项目(`source.link`)

`init` 按"代价从低到高"依次尝试,并**把实际用了哪种记进 `project.json.source.link`**:

| 方式 | 代价 | 何时可用 |
|------|------|---------|
| `hardlink` | 零拷贝、零权限 | 同一卷(Windows 上建硬链接不需要管理员) |
| `symlink` | 零拷贝 | 需权限:Windows 要开发者模式或管理员 |
| `copy` | 占一份磁盘 | 兜底,永远可用 |

**为什么要记下来**:只有 `copy` 的项目是**自包含**的。硬链接/软链接的项目一旦离开原视频就废了 ——
"报告里能不能引用视频""项目能不能打包发给别人"都取决于这个字段。

硬链接的语义要注意:项目和原路径指向**同一个文件**,删掉原文件不影响项目,但**就地修改**
原文件会让项目看到新内容 —— 这正是 `video.meta.json.sha256` 存在的意义。

文件位置固定为 `source/video.<原扩展名>`(如 `.mkv` 就保持 `.mkv`,不改成 `.mp4`)。

**建项目时会先预判**:素材与目标目录是否同卷。不同卷则硬链接必失败,于是 **在动手复制之前**
就打印将要占用的空间和"把项目建到素材同卷"的具体命令 —— 几百 MB 的复制要好几秒,
不该让人干等到结束才知道在等什么。

**事后可体检**:`vidbrief status` 按 `source.link` 分别检查(见 §10.5)。区别很关键:

| 怎么进来的 | 会怎么坏 | 检查什么 | 坏了是什么级别 |
|-----------|---------|---------|--------------|
| `hardlink` | **不会坏**(数据在项目里) | 两边是否仍是同一份数据 | 源文件被移动/删除/替换都只是**提示**,不是问题 |
| `symlink` | **会坏**:源文件一移走项目就废 | 链接目标是否还在 | **✗ 必须处理** |
| `copy` | 不会坏 | 无需检查 | — |

> 为什么非要单独做这一步:`init` 时警告过一次之后,**再没有任何地方会提醒你**。
> 软链接的项目可能已经悄悄坏了几周,直到有人想看视频才发现。

### 4.9 补图候选 `supplement_requests.json`

```jsonc
{
  "generated_at": "...",
  "window": 3.0,                 // "附近有图"的判定半径(秒)
  "requests": [
    { "id": 1, "t": 18.8, "segment_id": 7,
      "text": "画面停住了大家可以把结论抄下来",     // 该段原文,让人一眼看出为什么被列出来
      "trigger": "no_shot_nearby",  // deictic_phrase | no_shot_nearby | both
      "nearest_shot": "s003", "gap": 3.2,
      "status": "accepted",         // pending | accepted | rejected
      "shot_id": "s004",            // accepted 时补出的截图
      "note": "画面停住后的定型画面,补一张便于读数" }
  ]
}
```

> 算法只负责**列候选**(指代性表达 + 时间距离),**要不要补由 agent 判断** ——
> 用户说"看这里"不代表真有个图要看。`status` 的初值恒为 `pending`。

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

1) 滑窗累计: mass(i) = Σ_{j∈[i-W/2, i+W/2]} score[j]      // 居中窗口,见下
2) 标定阈值: MASS_THRESH = max(中位数 × k, 绝对下限)        // 防止静止视频全被选中
3) 密集区:        mass 连续超阈值的极大区间 → 回退半窗 → 按"贡献下限"修边
4) 孤立硬切换:    score ≥ PEAK_THRESH 且 邻域均值远低(突出度 ≥ 3×),且不属于任何密集区
5) 长静止打断:    长时间静止(>120s)后首次变化 → 重新标注为章节起点
6) 每区取代表帧:  密集区取区间峰值(区间比窗口长时两端各补一帧);孤立峰取峰值;长静止取变化后首帧
7) ffmpeg 精确抽帧 → shots/sXXX_tttt.jpg + shots.json
```

实现要点(`vidbrief-core::regions`,**纯函数**,不碰进程):

- **窗口居中**:若窗口往后看(`[i, i+W)`),一个尖峰会让它**之前**的所有窗口都超阈值,
  区间整体前移 —— 定位出来是错的。居中窗口的 `mass[i]` 才表达"i 附近有多热闹"。
- **修边**:居中窗口会把区间向两侧摊平半个窗宽。修边依据是**贡献下限** =
  `MASS_THRESH / 窗口样本数`,即"每帧至少要贡献这么多才撑得起这个阈值"。
- **突出度**(`PEAK_PROMINENCE = 3.0`,沿用旧实现 `src/main.rs` 的实测值):
  `score ≥ 邻域均值 × 3`。这是 §5.2 里"邻域均值**远低**"的可操作定义。
  **少了它会出事**:回归素材前半段有个每 4 秒一次的小动画(0.26–0.28),刚好越过
  `PEAK_THRESH = 0.25`,会被逐个误报成硬切换。
- **长静止是"重新标注"而不是独立判据**:同一个事件,"长时间静止后的首次变化"
  比"一次硬切换"信息量大(它说明这是**章节边界**),所以优先用前者标注。
- **静止判据是派生值**:`static_level = PEAK_THRESH × 0.2`。现在没有真实数据能标定它,
  先不暴露调不动的旋钮。
- **单帧不算密集区**:一次硬切换会让它前后共 `2×half+1` 个窗口都超阈值(每个窗口里都有
  那一个尖峰),很容易被误判成"一段热闹"。按定义密集区要**持续**活跃,单点交给孤立峰判据。

### 5.2.1 在真实长视频上的标定结果(2026-09-24)

用样本 D(985 秒)验证,
对照是**两条独立路径**:

1. ffmpeg 自带的场景检测(`select='gte(scene,0)'`),阈值 0.15;
2. `tools/whisper/demo.srt` 里那几个时刻**人在讲什么** —— 每个检出点都落在一次主题切换上。

| 结论 | 数据 |
|------|------|
| **不漏** | 检出 11 个区域,与对照的 11 个切换点一一对应 |
| **不假阳** | 阈值下方最高分 **0.0569**,离 `peak_threshold = 0.25` 有 **4.4 倍**分离度 |
| **比对照更灵敏** | `t=192` / `t=240` 两处 ffmpeg 在阈值 0.15 下**漏了**(它们的 scene 分正好 0.102),本工具找到且转录证实那里在换图/换论点 |
| **时序误差** | 持续 **+0.4~0.9 秒**,即 1fps 采样的量化误差(切换在 478.60,只能在 479 采到) |
| **性能** | 985 秒全程(抽帧+评分+密集区+抽 11 张图)**7.3 秒**;流式实现只留两个特征向量,不存 380MB 像素 |

**时序误差是良性的**:峰值取的是变化后的**第一个**采样点,所以截出来的图总是切换**之后**
那一侧 —— 正是要给人看的新内容。只是记下来的 `t` 比真实切换晚最多一个采样间隔。

**还没标定的**:`mass_k` 在这条视频上从未生效(中位数 0.0115 × 3 = 0.0345 < 下限 0.6,
下限全权决定)。也就是说**自适应那一支没有真实数据检验过** —— 它要等一条"背景一直有噪声"
的视频(如摄像头录屏)才能验。

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
- agent 综合 `transcript.fixed` 与 `shots.json` 里全部 `check` 结论,自己列出矛盾点,写进 `cross_validate.md`(通过 `write_note` 之类的写入工具)。

**④ 补充截图(算法漏掉的)** — `find_supplement_candidates`(纯算法)+ `extract_shot`
- 算法扫描转录里的**指代性表达**("如图/这个图/看这里/这边显示/大家看屏幕/这一段/形态")及"提到但附近(±3s)无截图"的时刻 → `supplement_requests.json`。
- agent 判断哪些确实需要,调 `extract_shot` 补帧 → `shots_extra/`,登记进 `shots.json`(`origin=agent_request`)。

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
- **本地查看服务是什么**:一个绑在 `127.0.0.1` 的极简 HTTP 静态服务,把项目目录当站点发布(`http://127.0.0.1:<port>/<项目 id>/…`),由工具按需拉起。**(已实现,见 `vidbrief-viz::server`)**
  - **为什么需要**:浏览器直接 `file://` 打开 HTML 时,`<video>` 常因跨域/安全限制无法加载本地视频,"拖时间轴→视频回跳"就会失效;走 `http://127.0.0.1` 后,视图/图片/视频同源,`<video>` 的 seek/播放正常。
  - **必须支持 Range 请求**:没有 `206 Partial Content` + `Accept-Ranges: bytes`,浏览器不会来 seek。
  - **附带好处**:多轮迭代时页面可自动刷新(`Cache-Control: no-store` 已经这么做了)。
  - **与控制通道的关系**:MCP 走 stdio 做 agent↔工具的控制;本地服务只负责"给人看",两者互不冲突。
  - **端口分配(避免冲突)**:服务默认绑 `127.0.0.1:0`,由**操作系统自动分配空闲端口**,不可能撞车;**每个 MCP 进程内只起一个长驻服务**(多项目靠**挂载表**路由,不是每次渲染都新起);进程退出时 OS 自动释放端口。若用户显式指定固定端口且被占用,则自动 +1 重试。
  - **安全**:这是唯一会把磁盘内容发出去的地方,路径要过两道关 —— 先由 `RelPath` 校验 URL 段(拒 `..`/`.`/反斜杠/盘符),再 `canonicalize` 确认仍在挂载根之内(挡符号链接指向外面)。
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
vidbrief/
├── crates/
│   ├── vidbrief-core/     # 差帧、密集区、抽帧(重写;旧实现已不在本仓库)
│   ├── vidbrief-asr/      # 调 whisper-cli,解析 srt → transcript.json
│   ├── vidbrief-project/  # 项目目录 + 数据结构 + project.json
│   ├── vidbrief-viz/      # 时间线/对照表/报告 HTML 渲染(含视频回跳 + 前端脚本)
│   ├── vidbrief-report/   # MD → HTML 图表化模板
│   ├── vidbrief-mcp/      # MCP 服务:供材料 + 存结论 + 算法动作 + 提示模板
│   └── vidbrief-cli/      # 独立命令行(脱离 agent 也能测/用)
├── ui/              # 视图模板 + 前端时间线组件(被 `vidbrief-viz` 用 include_str! 编进去)
├── docs/
│   ├── DESIGN.md
│   ├── schema/      # 项目材料格式的**唯一权威**:JSON Schema + 示例(同时是测试夹具)
│   └── devlog/      # 开发过程记录
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
vidbrief init <video> [-n id] [-t title] [-p dir]   # 建项目(已实现)
vidbrief preprocess <project> [options]             # 步 1:差帧图/密集区/截图/转录(已实现)
vidbrief ui <project> [--no-open]                   # 时间线视图 + 本地服务(已实现)
vidbrief status <project> [--verify]                # 状态 + 健康检查(已实现)
vidbrief mcp        [--stdio|--http]   # 启动 MCP 服务
vidbrief help                          # 用法
```

`ui` 会渲染 `01_preprocess/timeline.html`、把项目目录挂到本地服务上、打开默认浏览器。
**服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,时间轴点了不跳。

`preprocess` 的选项:`-f/--fps`(采样帧率,默认 1.0)、`-w/--width`(参与打分的缩小宽度,
默认 480)、`--no-transcribe`、`--no-shots`。**一次调用做完步 1 的全部产出**;
源视频没有音轨时转录自动跳过(不是错误);whisper 用不了时**不标记步 1 完成并以非零退出**
(材料保留,重跑即可)—— "我知道我在跳过"必须由用户显式说(`--no-transcribe`)。

`status` 的 `<project>` 两种写法都认:项目目录本身,或 `projects/` 下的项目名。
`--verify` 会重算源视频 sha256 与记录比对(要读一遍视频,几百 MB 要几秒),
用来发现"源文件被**就地改写**" —— 硬链接与软链接都会跟着变,而链接关系看起来毫无异常。

**有硬伤时 `status` 以非零退出码结束**,方便脚本与 agent 判断。

`init` 的行为:校验视频 → 定项目 id(`-n` 显式指定 → `-p` 目录名的净化结果 → 视频文件名净化;
纯中文名净化不出来就要求 `-n`)→ 建目录骨架 → 按硬链接/软链接/复制的顺序把源视频放进 `source/` →
`ffprobe` 探测并写 `source/video.meta.json` → 写 `project.json`。

| 选项 | 含义 |
|------|------|
| `-n, --name <ID>` | 项目 id(ASCII)。默认由文件名净化 |
| `-t, --title <TEXT>` | 人类可读名,可含中文。默认取视频文件名 |
| `-p, --project-dir <DIR>` | **项目目录本身**(直接把项目建在 `<DIR>` 里)。默认 `projects/<id>`,已存在时自动加后缀 |

> `-p` 是"项目目录",不是"项目们的父目录"。给了 `-p` 就用它本身,不再套一层;
> 该目录已存在则**报错**而不加后缀 —— 路径是你显式给的,工具不该替你改名。
> 默认路径 (`projects/<id>`) 因为 id 是工具推出来的,冲突时才自动加 `-2` / `-3`。

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
| **M0 schema 定稿** ✅ | §4 的 JSON schema + 目录结构落成真实文件 | 本文评审 |
| **M1 vidbrief-core + 步1** ✅ | 差帧 + 密集区 + 截图 + ASR,CLI 可跑 | 算法思路见 §15;ASR 用 `tools/whisper/` |
| **M2 时间线视图** ✅ | `render_timeline` 四层 + 视频回跳 + 本地服务 | M1 |
| **M3 MCP 骨架** | `vidbrief-mcp` 服务 + 项目/材料/算法工具接上 | M1,M2 |
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
| `src/main.rs`(vidbrief) | 差帧 / 峰值 / 抽帧算法**重写进 `vidbrief-core`**,并新增变化密集区检测(§5.2) |
| `scripts/transcribe.sh` | 思路并入 `vidbrief-asr`:抽 wav → whisper-cli → 结构化 `transcript.json` |
| `tools/whisper/`(二进制 + 模型) | **仍在工作区,原样复用**(体积大,不入 git) |
| `scripts/describe_frames.py` | **废弃**(识别改由 agent 做);其提示词作 §10.4 参考 |
| `scripts/describe_frames_with_context.py` | **废弃**;"图文联合"思路已内化为提示模板 P1 |
| `scripts/build_report.py` | 思路并入 `vidbrief-report`:MD → HTML,图片改相对路径 |
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
