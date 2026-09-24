# AGENTS.md — vidbrief 开发规范、约束与索引

> 项目名:**vidbrief**(长视频 → 图文总结的 MCP 工具集)。
> 面向在本仓库工作的 agent(以及人)。**开工前先读本文件 + [`docs/devlog/PLAN.md`](docs/devlog/PLAN.md)**(当前迭代安排)。
> 收工前:更新 PLAN.md 状态、补 devlog 条目、提交(commit message 引用条目 ID)。

---

## 1. ⛔ 两条硬性禁令

### 1.1 严禁使用字符串裸切片!中文用 `[]` 切极易 panic

Rust 的 `&s[a..b]` 按**字节**切,而一个中文字符在 UTF-8 里占 **3 字节**;切在字符中间会直接 panic:

```rust
let s = "这是中文示例";
let t = &s[0..2];   // ❌ panic: byte index 2 is not a char boundary
```

正确做法(任选):

```rust
// 1) 按字符取
let head: String = s.chars().take(2).collect();

// 2) 用 get:越界/非边界返回 None,不 panic
let safe = s.get(0..2).unwrap_or("");

// 3) 用 char_indices 找合法字节边界
let end = s.char_indices().nth(2).map(|(i, _)| i).unwrap_or(s.len());
let t = &s[..end];

// 4) 按显示宽度截断(中文宽度=2)时用 unicode-width crate
```

**本项目尤其危险**:转录文本、截图文件名、报告内容、提示模板**全是中文**;且工具要按时间戳/关键词截取转录片段,极易踩到。

**唯一例外**:对**纯 ASCII** 串(文件扩展名、十六进制哈希、格式化数字)切片是安全的,但请在代码注释里说明为什么安全。

### 1.2 严禁对 `node_modules`、`target` 等目录执行 grep!

**除非你有非这么做不可的理由;有的话先报出来让用户确认,不要自行开搜。**

原因:这些是生成物/依赖/二进制,体积巨大且与源码无关 —— 搜索极慢,结果被噪声淹没。

禁止直接搜的范围:

| 目录 | 是什么 |
|------|--------|
| `target/` | Rust 构建产物 |
| `node_modules/` | 前端依赖(将来若引入) |
| `tools/whisper/models/` | ggml 模型(1.5GB + 466MB,二进制) |
| `tools/whisper/Release/` | whisper.cpp 二进制与 dll |
| `.git/` | 版本库内部数据 |

正确做法 —— **收窄范围或用 glob 排除**:

```bash
rg 'pattern' crates/ docs/              # 指定范围(首选)
rg 'pattern' --type rust                # 按文件类型
rg 'pattern' -g '!target' -g '!*.bin'   # glob 排除
```

---

## 2. 项目速览

**定位**:长知识类视频(30 分钟级)→ 图文总结的**面向 agent 的 MCP 工具集**。
算法负责粗筛(差帧、转录、截图),agent 负责理解(看图、纠错、总结),人通过对话把关。

**三条铁律**(详见 [DESIGN §0.1](docs/DESIGN.md)):

1. **识别与判断归 agent** —— 工具不做任何模型推理
2. **工具零密钥** —— 不存在调用外部模型 API 的代码路径
3. **协议通用** —— 不绑定具体 agent,只假设"能看图 + 能调工具"

**四步工作流**:① 前处理(原子,纯算法)→ ② 校准(迭代,agent 反复调工具)→ ③ 总结(原子+可重做)→ ④ 报告(原子+可重做)。
每步产物落盘为文件 + 生成 HTML 视图,用户通过对话检查/纠偏。

**项目工作区**:一个视频 = 一个项目目录(`vidbrief init` 建出来),结构见
[`docs/DESIGN.md` §4.1](docs/DESIGN.md),里面每个 JSON 文件的格式见 [`docs/schema/`](docs/schema/)。
默认放在 `./projects/`(已 gitignore)。

**当前进度**:M0(数据模型定稿 + 目录骨架 + `vidbrief init`)已完成;下一步 M1(差帧 / 密集区 / 抽帧 / 转录)。

---

## 3. 开发规范

### 3.1 提交规范

- 格式:`<type>: <摘要>`,`type` ∈ `feat` / `fix` / `docs` / `chore` / `refactor` / `test`
- **每条 commit message 必须引用 devlog 条目 ID**(如 `[260924-05]`),使"提交 ↔ 条目"双向可查
- 正文写**为什么**,不写"改了什么"(diff 自己会说)
- **`git add -A` 之前先 `git status`**:确认没有夹带不属于本次改动的内容
  (教训来源:某次 `git add -A` 把用户手动编辑的 `.gitignore` 一起提交了)。不清楚的改动就指名 `git add <路径>`。

```bash
git status --short
git commit -m "feat(vidbrief-core): 实现变化密集区检测 [260925-01]"
```

### 3.2 破坏性操作前先建可回溯点

删除/大范围重写前,先 `git add -A && git commit`(或至少确认工作区干净)。
教训来源:[devlog 260924-04](docs/devlog/2026-09.md)(仓库曾无版本控制,清理差点不可逆)。

### 3.3 代码规范

- **统一 Rust**;某环节在 Rust 里成本过高时可**子进程外挂**(Python/Node),但须:输入输出走文件/STDIN-STDOUT、无状态、可单测
- 每个 crate 的 `lib.rs` 顶部保留 `//! 设计见 docs/DESIGN.md §X` 的引用
- 改动后**必须跑 `cargo build --workspace`**(必要时 `cargo test --workspace`)再声明完成;失败就修到通过,不要报告未验证的成功

### 3.4 数据格式规范(改动 `docs/schema/` 前后必读)

项目目录里的每个 JSON 文件都有**唯一的格式权威**:[`docs/schema/`](docs/schema/)。

- 改格式的固定动作见 [`docs/schema/README.md`](docs/schema/README.md) §7:
  改 schema → 改 Rust 类型 → 改 `examples/` → (不兼容时) `schema_version` +1 → `cargo test -p vidbrief-project` → 补 devlog → 同步 DESIGN §4
- Rust 结构体**一律** `#[serde(deny_unknown_fields)]`,与 schema 的 `additionalProperties: false` 严格对齐
- **路径字段一律用 `vidbrief_project::RelPath`**,不要用 `String` 或 `PathBuf`(它构造即校验,挡住目录穿越与跨平台失败)
- 时间戳一律走 `vidbrief_project::now()`(已截到整秒);不要自己 `Local::now()`
- `docs/schema/examples/*.json` **是测试夹具不是文档** —— 改动它们等于改动契约

### 3.5 文档规范

- **设计变更**改 [`docs/DESIGN.md`](docs/DESIGN.md),并 bump 顶部版本号
- **历史记录不改写**(如 RESEARCH.md 的实测过程),有新情况就加归档/更正说明
- 新增文档一律放 `docs/`;开发过程记录放 `docs/devlog/`

---

## 4. 材料索引

| 路径 | 用途 |
|------|------|
| [`docs/DESIGN.md`](docs/DESIGN.md) | **主设计文档**(架构、数据模型、四步细节、MCP 工具清单、提示模板 P1–P5) |
| [`docs/schema/`](docs/schema/) | **项目材料格式的唯一权威**:JSON Schema + 示例(同时是测试夹具)+ 全局约定。**改数据格式先看这里** |
| [`docs/schema/README.md`](docs/schema/README.md) | 全局约定(命名/时间/路径/null/版本)+ 标识符规则 + 改 schema 的固定动作 |
| [`docs/BRD.md`](docs/BRD.md) | 需求来源(与大模型的多轮对话记录) |
| [`docs/RESEARCH.md`](docs/RESEARCH.md) | 技术核实与 M1/M2 实测报告(含旧实现结论,仍有效) |
| [`docs/WHISPER_SETUP.md`](docs/WHISPER_SETUP.md) | 本地 Whisper:硬件要求、模型规格、实测记录 |
| [`docs/devlog/`](docs/devlog/) | **开发过程记录**(迭代安排 / 进度 / 决策 / 问题 / 计划变更) |
| [`docs/devlog/PLAN.md`](docs/devlog/PLAN.md) | 当前迭代安排与里程碑状态 |
| `crates/vidbrief-core/` | 差帧、变化密集区、精确抽帧(步 1 算法核心) |
| `crates/vidbrief-asr/` | whisper-cli 封装 → 结构化转录 |
| `crates/vidbrief-project/` | **项目目录 + 全部数据结构 + 写锁与并发控制**(格式的代码侧权威) |
| `crates/vidbrief-viz/` | HTML 视图渲染 + 本地查看服务(视频回跳) |
| `crates/vidbrief-report/` | Markdown → 图表化 HTML |
| `crates/vidbrief-mcp/` | MCP 服务(供材料 / 存结论 / 算法动作 / 提示模板) |
| `crates/vidbrief-cli/` | `vidbrief` 独立命令行(脱离 agent 也能自测);已实现 `init` |
| `fixtures/test_sample.mp4` | 回归素材:60s 合成视频,已知切换点 **t=20/30/50** |
| `tools/whisper/` | whisper.cpp 二进制 + ggml 模型(**体积大,不入 git**) |

**验证命令**(声明完成前必须跑):

```bash
cargo build --workspace          # 编译
cargo test  --workspace          # 全部测试
./target/debug/vidbrief init fixtures/test_sample.mp4 --projects-dir /tmp/smoke   # 冒烟:建个项目看看
```

---

## 5. devlog 使用方式

**位置**:`docs/devlog/`。**规则**:按月分档,**只追加,不改写历史**。

### 什么时候写

| 情形 | 条目类型 |
|------|---------|
| 做了方向性选择(架构、协议、取舍) | `决策` |
| 完成一个里程碑/阶段性工作 | `进度` |
| 踩坑并解决 | `问题` |
| 计划/里程碑调整 | `计划变更` |
| 调整迭代安排本身 | `迭代安排`(并同步 PLAN.md) |

### 怎么写

```markdown
### [YYMMDD-NN] 类型 · 标题

- **标签**: `架构` `MCP`
- **提交**: `1a2b3c4`
- **背景**: 为什么要做 / 遇到什么
- **做法/决定**: 具体动作或结论
- **理由**: 为什么这样
- **影响**: 对后续的约束、连带改动
```

### 怎么检索

```bash
rg '^### \[' docs/devlog/ -g '[0-9]*.md'      # 列出全部条目
rg '^### \[.*\] 决策' docs/devlog/ -g '[0-9]*.md'   # 按类型(换 决策/进度/问题/计划变更)
rg '\*\*标签\*\*.*并发' docs/devlog/ -g '[0-9]*.md' # 按标签
rg '端口' docs/devlog/ -g '[0-9]*.md'          # 按关键词
rg '提交.*1a2b3c4' docs/devlog/ -g '[0-9]*.md' # 反查 # 反查:某 commit 对应哪条
```

> `-g '[0-9]*.md'` 用于排除 devlog README 里的格式示例行,避免噪声。

完整配方见 [`docs/devlog/README.md`](docs/devlog/README.md)。

---

## 6. 标准工作循环

```
1. 读 docs/devlog/PLAN.md            → 确认当前里程碑与待办
2. 开工(遵守 §1 两条禁令、§3 规范)
3. cargo build --workspace && cargo test --workspace   → 验证通过再声明完成
4. 更新 PLAN.md                      → 勾掉待办、改状态
5. 补 devlog 条目(决策/进度/问题/计划变更)
6. git status 确认改动范围 → git commit,message 引用条目 ID
```
