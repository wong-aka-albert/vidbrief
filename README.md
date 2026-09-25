# vidbrief

长知识类视频(30 分钟级)→ **图文总结**的工具集,形态是**面向 agent 的 MCP 服务**。

算法负责粗筛(差帧、转录、截图),agent 负责理解(看图、纠错、总结),人通过对话把关。

> 📐 完整设计见 **[docs/DESIGN.md](./docs/DESIGN.md)**
> 🧾 项目材料格式(JSON Schema + 示例)见 **[docs/schema/](./docs/schema/)**
> 📋 需求背景见 [docs/BRD.md](./docs/BRD.md) · 技术核实与实测见 [docs/RESEARCH.md](./docs/RESEARCH.md) · 本地 Whisper 见 [docs/WHISPER_SETUP.md](./docs/WHISPER_SETUP.md)
> 📝 开发过程记录见 [docs/devlog/](./docs/devlog/)

## 三条铁律

1. **识别与判断归 agent** —— 工具不做任何模型推理
2. **工具零密钥** —— 不存在调用外部模型 API 的代码路径
3. **协议通用** —— 不绑定具体 agent,只假设"能看图 + 能调工具"

## 四步工作流

```
① 前处理(原子)        ② 校准(迭代)          ③ 总结(原子+可重做)   ④ 报告(原子+可重做)
差帧图                 agent 看图核对          agent 探索材料         MD → 图表化 HTML
语音转文字             纠错 / 交叉印证         按需补图              含视频回跳
预筛选截图             补图
   ↓                     ↓                      ↓                     ↓
             每步产物落盘 + 生成 HTML 视图(可弹 UI),用户通过对话检查/纠偏
```

- **差帧图**:相邻两帧差异算出一个数值,沿时间轴排成直方图(视图顶层)
- **预筛选截图**:按差帧图上的"变化密集区"取代表帧(纯算法)
- **校准**:核心是**修识别错误**(语音听错、图看错);修不好的由用户在审查节点让 agent 改

## 仓库结构

```
crates/
  vidbrief-core/     步 1 算法核心:ffmpeg 调用、差帧、密集区、精确抽帧
  vidbrief-asr/      语音转文字:ffmpeg 抽音 + whisper-cli → transcript.json
  vidbrief-project/  项目目录 + 全部数据结构(格式的代码侧权威)+ 写锁与并发控制
  vidbrief-viz/      HTML 视图渲染 + 本地查看服务(视频回跳)
  vidbrief-report/   Markdown → 图表化 HTML
  vidbrief-ops/      动作层:一步的编排与项目上的读写,CLI 与 MCP 共用
  vidbrief-mcp/      MCP 服务(供材料 / 存结论 / 算法动作 / 提示模板)
  vidbrief-cli/      `vidbrief` 独立命令行(脱离 agent 也能自测)
ui/                  视图模板(被 vidbrief-viz 用 include_str! 编进二进制)
docs/
  DESIGN.md          主设计文档
  schema/            项目材料格式的唯一权威:JSON Schema + 示例(同时是测试夹具)
  devlog/            开发过程记录(迭代安排 / 进度 / 决策 / 问题 / 计划变更)
  BRD.md RESEARCH.md WHISPER_SETUP.md
fixtures/            测试素材(含已知切换点的回归视频)
tools/whisper/       whisper.cpp 二进制 + ggml 模型(注意:体积大,不入 git)
projects/            项目工作区(不入 git,由 `vidbrief init` 生成)
```

**依赖方向**(只能从上往下):

```
project  ←  core  ←  asr          project  ←  viz          project  ←  report(→ viz)
                    └──────────────┬──────────────┘
                                ops  →  {project, core, asr, viz}
                             ┌────┴────┐
                            cli       mcp        ← 两个入口平等,互不依赖
```

`vidbrief-ops` 存在的理由很直接:**把这一层删掉,CLI 和 MCP 就得各写一份一模一样的流程**
(建项目 = 探测 + 搬运 + 落盘;步 1 = 视觉 + 转录 + 标记完成 + 渲染)—— 而它们写的是同一批 JSON。

## 项目工作区长什么样

一个视频 = 一个项目目录,四步产物分目录存放,**材料即接口**(agent 读文件、写结论):

```
projects/<id>/
├── project.json         元数据 + 进度
├── source/              源视频(硬链接/软链接/复制)+ ffprobe 元数据
├── 01_preprocess/       差帧图 / 密集区 / 转录 / 截图 / 时间线视图
├── 02_calibrate/        纠错后的转录 / 补图 / 校准日志 / 视图
├── 03_summary/          总结稿 + 探索笔记
└── 04_report/           报告 MD / HTML / assets
```

每个 JSON 文件的字段、枚举与约束都定义在 [`docs/schema/`](./docs/schema/),并由测试与 Rust 类型三方锚定。

## 构建与使用

```bash
cargo build --workspace        # 编译
cargo test  --workspace        # 全部测试(单元 + schema 契约 + CLI 端到端 + MCP 协议)

# 建项目(建目录骨架 + 探测源视频 + 写 project.json)
./target/debug/vidbrief init <video> [-n id] [-t "中文名"] [-p 项目目录]
./target/debug/vidbrief init -h        # init 的全部选项

# 步 1:差帧图 + 密集区 + 预筛选截图(+ 有音轨时做转录)
./target/debug/vidbrief preprocess <项目> [--no-transcribe] [--no-shots] [-f fps] [-w 宽度]

# 时间线视图:渲染 + 起本地服务 + 开浏览器(视频回跳靠它)
./target/debug/vidbrief ui <项目> [--no-open]

# 查看状态 + 健康检查(--verify 会重算 sha256,发现源文件被就地改写)
./target/debug/vidbrief status <项目> [--verify]
./target/debug/vidbrief help
```

### 接到 agent 上

MCP 服务走 **stdio**,由 agent 客户端把它拉起来(不占端口、不用先起服务):

```jsonc
// 以某个 MCP 客户端为例
{
  "mcpServers": {
    "vidbrief": { "command": "E:/path/to/vidbrief.exe", "args": ["mcp", "--stdio"] }
  }
}
```

也可以直接在命令行里起它,自己发 JSON-RPC 试:

```bash
./target/debug/vidbrief mcp --stdio
# 然后往 stdin 写(每个请求一行):
# {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}
# {"jsonrpc":"2.0","method":"notifications/initialized"}
# {"jsonrpc":"2.0","id":2,"method":"tools/list"}
```

> ⚠️ **stdout 是协议通道**。调试时任何 `println!` 混进去都会让会话当场坏掉 —— 诊断一律走 stderr。
> 服务不往 stdout 写协议以外的任何东西,这一条由端到端测试逐行解析 stdout 守着。

## 关键决策(摘自设计文档 §14)

| 项 | 决策 |
|----|------|
| MCP 传输 | **stdio**(官方 Rust SDK `rmcp` 3.1);渲染视图时可弹出 UI(启本地查看服务 + 开浏览器) |
| 视频回跳 | 本地查看服务,绑 `127.0.0.1:0`(OS 分配空闲端口,多会话不冲突) |
| 项目写锁 | **操作系统的文件锁**决定谁持有(不探 pid:崩溃时 OS 自动释放,而 pid 会被复用);文件里的 JSON 只是给人看的 |
| 报告形态 | 相对路径 `assets/`(自包含为可选) |
| 术语表 | 默认关闭、可选;纠错优先依据"画面文字 + 上下文" |
| 语言 | 统一 Rust,成本过高处可子进程外挂 |

## 里程碑

见 [docs/DESIGN.md §13](./docs/DESIGN.md) 与 [docs/devlog/PLAN.md](./docs/devlog/PLAN.md)。

- ✅ **M0** 数据模型定稿:JSON Schema + Rust 类型 + 目录骨架 + `vidbrief init`
- ✅ **M1** 步 1 打通:`vidbrief-core` 差帧/密集区/抽帧 + `vidbrief-asr` 转录 + `vidbrief preprocess`
- ✅ **M2** 时间线视图:四层共轴 + 视频回跳 + 本地查看服务
- ✅ **M3** MCP 服务:stdio 传输 + 21 个工具 + 资源 + 提示模板 + 项目写锁
- ⏳ **M4**(下一步)步 2 校准循环:对照表视图 + **在真实 agent 上联调**
