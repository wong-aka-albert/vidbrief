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
  vidbrief-core/     差帧、变化密集区、精确抽帧(步 1 算法核心)
  vidbrief-asr/      whisper-cli 封装 → 结构化转录
  vidbrief-project/  项目目录 + 全部数据结构(格式的代码侧权威)+ 写锁与并发控制
  vidbrief-viz/      HTML 视图渲染 + 本地查看服务(视频回跳)
  vidbrief-report/   Markdown → 图表化 HTML
  vidbrief-mcp/      MCP 服务(供材料 / 存结论 / 算法动作 / 提示模板)
  vidbrief-cli/      `vidbrief` 独立命令行(脱离 agent 也能自测)
docs/
  DESIGN.md          主设计文档
  schema/            项目材料格式的唯一权威:JSON Schema + 示例(同时是测试夹具)
  devlog/            开发过程记录(迭代安排 / 进度 / 决策 / 问题 / 计划变更)
  BRD.md RESEARCH.md WHISPER_SETUP.md
fixtures/            测试素材(含已知切换点的回归视频)
tools/whisper/       whisper.cpp 二进制 + ggml 模型(注意:体积大,不入 git)
projects/            项目工作区(不入 git,由 `vidbrief init` 生成)
```

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
cargo test  --workspace        # 全部测试(单元 + schema 契约 + CLI 端到端)

# 建项目(建目录骨架 + 探测源视频 + 写 project.json)
./target/debug/vidbrief init <video> [-n id] [-t "中文名"] [-p 项目目录]
./target/debug/vidbrief init -h        # init 的全部选项

# 其余子命令待各里程碑实现
./target/debug/vidbrief help
```

## 关键决策(摘自设计文档 §14)

| 项 | 决策 |
|----|------|
| MCP 传输 | **stdio**;渲染视图时可弹出 UI(启本地查看服务 + 开浏览器) |
| 视频回跳 | 本地查看服务,绑 `127.0.0.1:0`(OS 分配空闲端口,多会话不冲突) |
| 报告形态 | 相对路径 `assets/`(自包含为可选) |
| 术语表 | 默认关闭、可选;纠错优先依据"画面文字 + 上下文" |
| 语言 | 统一 Rust,成本过高处可子进程外挂 |

## 里程碑

见 [docs/DESIGN.md §13](./docs/DESIGN.md) 与 [docs/devlog/PLAN.md](./docs/devlog/PLAN.md)。

- ✅ **M0** 数据模型定稿:JSON Schema + Rust 类型 + 目录骨架 + `vidbrief init`
- ⏳ **M1**(下一步)`vidbrief-core` 差帧 / 密集区 / 抽帧 + `vidbrief-asr` 转录
