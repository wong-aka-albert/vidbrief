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

### 1.2 ⛔ 严禁**无条件**搜索:任何 `grep` / `rg` 之前先定范围,不许扫 `target`、`node_modules` 这类大目录

**这条没有例外、没有"这次应该没事"。** 「无条件」指的就是不带范围、不带排除的搜法:

```bash
rg pattern                      # ❌ 不写范围 = 从仓库根扫起
grep -rn pattern .              # ❌ 同上,而且 grep 比 rg 更慢
rg pattern --hidden             # ❌ 连 .git 一起扫
```

它们会一头扎进构建产物与依赖里:**几十万文件 / 数 GB 二进制,慢到像卡死,结果被噪声淹没,
而你要找的那一行源码根本浮不上来。**

| 不许碰的目录 | 是什么 |
|------|--------|
| `target/` | Rust 构建产物(含 `deps/*.pdb`,数 GB) |
| `node_modules/` | 前端依赖(将来若引入) |
| `tools/whisper/models/` | ggml 模型(1.5GB + 466MB,二进制) |
| `tools/whisper/Release/` | whisper.cpp 二进制与 dll |
| `.git/` | 版本库内部数据 |

**每次搜索都要写范围**,三选一(可叠加):

```bash
rg 'pattern' crates/ docs/ ui/ skill/     # ① 指定范围(首选:你本来就该知道东西在哪一层)
rg 'pattern' --type rust                  # ② 按文件类型
rg 'pattern' -g '!target' -g '!*.bin'     # ③ 明确排除(范围只能落到仓库根时)
```

**别把这条读成"性能建议"。** 它防的是三种假象:

- **假结果**:在 `target/` 里搜到同名符号,你以为那是源码;
- **假没找到**:输出被噪声冲爆,你只看到尾巴,于是"没搜到";
- **卡住整轮**:一次搜索几分钟不返回,exec 超时被取消,前面做的事一起丢。

**唯一例外**:你有一个**非这么做不可的理由**,并且**先说出来让用户确认** —— 不许自行开搜。

---

## 2. 项目速览

**定位**:长知识类视频(30 分钟级)→ 图文总结的**面向 agent 的 MCP 工具集**。
算法负责粗筛(差帧、转录、截图),agent 负责理解(看图、纠错、总结),人通过对话把关。

**三条铁律**(详见 [DESIGN §0.1](docs/DESIGN.md)):

1. **识别与判断归 agent** —— 工具不做任何模型推理
2. **工具零密钥** —— 不存在调用外部模型 API 的代码路径
3. **协议通用** —— 不绑定具体 agent,只假设"能看图 + 能调工具"

**四步工作流**:① 前处理(纯算法,一件**独立进程里的作业**,三个入口 —— §5.4)→ ② 校准(迭代,agent 反复调工具)→ ③ 总结(原子+可重做)→ ④ 报告(原子+可重做)。
每步产物落盘为文件 + 生成 HTML 视图,用户通过对话检查/纠偏。

**项目工作区**:一个视频 = 一个项目目录(`vidbrief init` 建出来),结构见
[`docs/DESIGN.md` §4.1](docs/DESIGN.md),里面每个 JSON 文件的格式见 [`docs/schema/`](docs/schema/)。
默认放在 `./projects/`(已 gitignore)。

**当前进度**:M0–M4(数据模型 → 步 1 前处理 → 时间线视图 → MCP stdio 服务 → 步 2 校准循环)、
**M5(步 3 总结)**、**M6(步 4 报告:`vidbrief-report` + `build_report` + P5)** 均已完成;
现 26 件工具,速查见 [`skill/tools.md`](skill/tools.md)。每步都走过**自家客户端 + 真 agent**
联调,并各有若干轮实测返工(见 devlog)。下一步 **M7**(加固:增量重跑、失败重试、批量、
性能、进度通知、外挂接口)—— 其中**进度通知**、**长任务拆短轮询**(2026-09-26,清账五)
与**增量重跑**(2026-09-27,清账六)、**外挂接口**(清账七:环境看得见 + 外挂版本进指纹)
已做;**失败重试**的主体也被增量重跑覆盖(失败后再跑一趟,做成的部分沿用;判据已验),
只剩**批量**。
详见 [`docs/devlog/PLAN.md`](docs/devlog/PLAN.md)。

**三条实现层面的硬约束**(踩过就知道疼):

- **stdio 上的 stdout 是协议通道。** `vidbrief-mcp` 一旦跑起来,任何 `println!`、进度条、
  调试输出掉进 stdout,客户端就会收到一段不是 JSON-RPC 的字节流,整个会话当场坏掉。
  **库层(`vidbrief-ops` 往下)一律不打印**;诊断走 stderr。
  这条由 `crates/vidbrief-cli/tests/mcp.rs` 逐行解析 stdout 守着 —— 不靠人记。
- **`project.lock` 是 0 字节的哨兵,谁都不许往里写。** 一旦有内容,Windows 上的强制文件锁
  就会让那句话**恰恰在最需要它的时候读不到**,于是又得引入平台 FFI 或旁路文件 ——
  那笔账算不过去(`docs/DESIGN.md` §4.7;`vidbrief_project::lock` 的模块文档里有全过程)。
  有测试专门拦这个冲动。
- **报错/提示文案里不出现"删"。** 读这些话的常常是**模型** —— 你写"删",它就会删,
  而工具碰得到的目录里装着转录、截图和 agent 的校准结论。
  **该说的永远是「怎么修好」,不是「怎么清掉」。**
  撞名/撞位的正确去路是"换个名字另起一个"或"问使用者",不是"腾地方"。
  (`health.rs` 里软链接失效那条曾经劝人"删掉项目用复制重建" —— 全仓最危险的一句,已改)

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
- 跑验证命令时**别把输出接进 `grep`/`head`/`tail`**:管道的退出码是**过滤器**的 ——
  命令失败也能报成成功;真出错时决定性的那几行(`-->`、`note:`、上下文)恰好不在你的模式里,
  于是"没看见"悄悄变成"没问题"。**要看某几行:先落文件、再查文件**(原始输出必须留着);
  长命令放后台任务表,别原地等、也别 sleep 轮询

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
| `crates/vidbrief-core/` | 步 1 算法核心。**纯函数占一半**(`feature`/`regions`/`shots`/`supplement` 不碰进程),便于毫秒级调试 |
| `crates/vidbrief-asr/` | ffmpeg 抽音 + whisper-cli 封装 → 结构化转录。**不做任何纠错** |
| `crates/vidbrief-project/` | **项目目录 + 全部数据结构 + 写锁与并发控制**(格式的代码侧权威) |
| `crates/vidbrief-viz/` | HTML 视图渲染 + **本地查看服务**(绑 `127.0.0.1:0`、进程内单例、挂载表、Range 支持) |
| `ui/` | 视图模板与前端时间线组件(被 `vidbrief-viz` 用 `include_str!` 编进二进制) |
| `crates/vidbrief-report/` | Markdown → 图表化 HTML |
| `crates/vidbrief-ops/` | **动作层**:一步的编排与项目上的读写。CLI 与 MCP 共用这一层;它不打印、不解析参数、不认识 JSON-RPC |
| `crates/vidbrief-mcp/` | MCP 服务(供材料 / 存结论 / 算法动作 / 提示模板)。**改工具看 `src/tools/`,改模板看 `src/prompts.rs`** |
| `crates/vidbrief-cli/` | `vidbrief` 独立命令行(脱离 agent 也能自测)。**只做参数解析 + 打印** |
| `fixtures/test_sample.mp4` | 回归素材:60s 合成视频,已知切换点 **t=20/30/50** |
| `tools/whisper/` | whisper.cpp 二进制 + ggml 模型(**体积大,不入 git**) |

**验证命令**(声明完成前必须跑):

> ⛔ 跑它们的时候:①**搜索先定范围**(§1.2),别让 `target/` 混进来;
> ②**别把输出接进 `grep`/`head`/`tail`**(§3.3)—— 要看某几行就先把完整输出落文件、再对文件查。

```bash
cargo build --workspace          # 编译
cargo test  --workspace          # 全部测试(默认不跑 whisper,见下)
# 冒烟:建项目 → 跑步 1 → 看状态(用素材**副本**,别直接硬链仓库里的 fixtures ——
# 硬链接会穿透,往项目里那个视频写东西等于改仓库跟踪文件)
cp fixtures/test_sample.mp4 /tmp/smoke.mp4
./target/debug/vidbrief init /tmp/smoke.mp4 -p /tmp/smoke-proj
./target/debug/vidbrief preprocess /tmp/smoke-proj --no-transcribe
./target/debug/vidbrief status /tmp/smoke-proj --verify
# 视图:渲染 + 起服务。--no-open 免得每次跑测试都弹浏览器;它会一直活着,记得关
./target/debug/vidbrief ui /tmp/smoke-proj --no-open
```

**MCP 服务的验收**:`crates/vidbrief-cli/tests/mcp.rs` 是**手写的 JSON-RPC 客户端**
(故意不用 SDK —— 三条铁律里有一条是"协议通用"),它真的把 `vidbrief mcp` 拉起来,
走完 建项目 → 跑步 1 → `get_shot` 拿到 JPEG → 写回结论 → 渲染视图 → 读资源,
外加一条"第二个会话只读能过、写被挡下"的锁测试。手工试的时候:

```bash
./target/debug/vidbrief mcp --stdio
# 往 stdin 写三行(每行一个 JSON-RPC):
# {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}
# {"jsonrpc":"2.0","method":"notifications/initialized"}
# {"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

> ⛔ 调 MCP 时**永远不要在服务进程里加 `println!`**:stdout 是协议通道。

**需要显式跑的**(要拉 whisper-cli 与 466MB 模型,约 4 秒):

```bash
cargo test -p vidbrief-asr --offline -- --ignored --nocapture
```

> 拉不到 crate 索引时加 `--offline`(依赖都在本地缓存里)。

---

## 5. devlog 使用方式

**位置**:`docs/devlog/`。**规则**:按月分档,**只追加,不改写历史**。

> 本节讲的是这套流程怎么跑。**本仓库只带格式规范,不带实际记录文件** ——
> `PLAN.md` 与 `YYYY-MM.md` 属本地开发档案,不入库(见 [`docs/devlog/README.md`](docs/devlog/README.md))。

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
2. 列 todo(多步任务)                 → 开工前把步骤写下来;**每完成一步立刻标 completed**
3. 开工(遵守 §1 两条禁令、§3 规范;**搜索先定范围**)
4. cargo build --workspace && cargo test --workspace   → 验证通过再声明完成
   (输出别接 `grep`/`head`/`tail`:见 §3.3;长命令放后台,别原地等、也别 sleep 轮询)
5. 更新 PLAN.md                      → 勾掉待办、改状态
6. 补 devlog 条目(决策/进度/问题/计划变更)
7. git status 确认改动范围 → git commit,message 引用条目 ID
```

> **进度清单有一条硬要求:它必须与实际相符。**
> 忘了标 = 用户看到的是一份**假进度**,比不列清单更糟(会让人以为没做)。
> 两条纪律:① 每完成一步**立刻**标,别攒到最后;② 收工前扫一眼清单,
> 还有 `pending`/`in_progress` 就说明要么没做完、要么没标 —— 两种都得处理。
> 宁可开工时不列,也不要把清单撂在那儿不管。
