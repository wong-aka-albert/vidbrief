# vidbrief

长知识类视频(30 分钟级)→ **图文总结**的工具集,形态是**面向 agent 的 MCP 服务**。

算法负责粗筛(差帧、转录、截图),agent 负责理解(看图、纠错、总结),人通过对话把关。

> 📐 完整设计见 **[docs/DESIGN.md](./docs/DESIGN.md)**
> 📋 需求背景见 [docs/BRD.md](./docs/BRD.md) · 技术核实与实测见 [docs/RESEARCH.md](./docs/RESEARCH.md) · 本地 Whisper 见 [docs/WHISPER_SETUP.md](./docs/WHISPER_SETUP.md)

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
  vidbrief-project/  项目目录、数据结构、写锁与并发控制
  vidbrief-viz/      HTML 视图渲染 + 本地查看服务(视频回跳)
  vidbrief-report/   Markdown → 图表化 HTML
  vidbrief-mcp/      MCP 服务(供材料 / 存结论 / 算法动作 / 提示模板)
  vidbrief-cli/      `vidbrief` 独立命令行(脱离 agent 也能自测)
docs/          文档(DESIGN 设计 / BRD 需求 / RESEARCH 技术核实 / WHISPER_SETUP 转录环境)
fixtures/      测试素材
tools/whisper/ whisper.cpp 二进制 + ggml 模型(注意:体积大,不入 git)
```

## 构建

```bash
cargo build           # 编译整个 workspace
./target/debug/vidbrief     # 查看 CLI 用法(子命令待各里程碑实现)
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

见 [docs/DESIGN.md §13](./docs/DESIGN.md)。当前:**骨架已铺,进入 M0/M1**。
