# vidbrief

**把长知识类视频(30 分钟级)变成图文总结。**

算法负责粗筛(差帧、转录、截图),agent 负责理解(看图、纠错、总结),人通过对话把关。
形态是**面向 agent 的 MCP 服务**:不绑定具体 agent,只假设它"能看图 + 能调工具"。

![Rust](https://img.shields.io/badge/rust-1.89%2B-orange)
![MCP](https://img.shields.io/badge/MCP-stdio-blue)
![Platform](https://img.shields.io/badge/verified-Windows-lightgrey)

## 为什么要它

30 分钟的知识类视频里,最关键的信息大量**只在画面上**:图表、标注、幻灯片、演示操作。
只把音轨转成文字再总结,会稳定丢掉这一半;而每 15 秒塞一帧图给多模态模型,又贵又糊。

vidbrief 的做法是:先算出**画面在哪几秒发生了变化**(差帧图),按变化密集区取代表帧(预筛选截图),
再连同带时间戳的转录一起递给 agent —— 让它在该看的地方看图,而不是把整条视频翻一遍。

**三条铁律**

1. **识别与判断归 agent** —— 工具不做任何模型推理,只搬运材料、执行确定性动作、保存结论
2. **工具零密钥** —— 不存在调用外部模型 API 的代码路径
3. **协议通用** —— 不绑定具体 agent

## 快速开始

```bash
# 0) 前置:Rust ≥ 1.89,ffmpeg / ffprobe 在 PATH
#    (whisper 与模型是可选的 —— 没有它就没有转录,其余照常,见下一节)
cargo build --workspace

# 1) 建项目:建目录骨架 + 探测元数据 + 把源视频搬进来
./target/debug/vidbrief init /path/to/你的视频.mp4

# 2) 步 1(纯算法):差帧图 + 变化密集区 + 预筛选截图
./target/debug/vidbrief preprocess 你的视频 --no-transcribe   # 装了 whisper 就去掉这个开关

# 3) 看:渲染时间线视图 + 起本地服务 + 开浏览器(点时间轴能跳回视频)
./target/debug/vidbrief ui 你的视频
```

- Windows 下把 `./target/debug/vidbrief` 换成 `.\target\debug\vidbrief.exe`。
- 项目默认建在 `projects/<id>/`,`<id>` 由视频文件名净化而来;之后的命令给**项目名**或**项目目录**都行。
- ⚠️ **纯中文文件名**:净化不出 ASCII id,`init` 会直接报错 —— 加 `-n 项目id` 即可(`-t "中文标题"` 给它一个人看的名字)。

## 装依赖:仓库里没有的东西

模型权重和 whisper 二进制**不入库**(体积大),每台机器自己备一份。ffmpeg 是必需的,whisper 是可选的。

### ffmpeg / ffprobe —— 必需

只要在 `PATH` 上就行:

| 平台 | 怎么装 |
|------|--------|
| Windows | `winget install Gyan.FFmpeg`,或 `scoop install ffmpeg` / `choco install ffmpeg`;也可以从 [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) 下 release 包,把 `bin/` 加进 PATH |
| macOS | `brew install ffmpeg` |
| Debian / Ubuntu | `sudo apt install ffmpeg` |
| Fedora / RHEL | `sudo dnf install ffmpeg` |

装不到 PATH、或想自带一份副本:用 `VIDBRIEF_FFMPEG` / `VIDBRIEF_FFPROBE` 指路(见「环境变量」)。

> 版本没有硬性下限。本项目实测用的是 `ffmpeg N-125875-g5d4d3bdc61`(2026-07 的 nightly);近年的 6/7/8 都行。

### whisper.cpp + ggml 模型 —— 可选(没有它就没有转录)

**什么时候需要**:视频有音轨、并且你想要带时间戳的转录。没音轨 / 暂时不想转录 → `preprocess --no-transcribe`,其它一切照常。

**放在哪**(程序按这个顺序找;前两条相对**仓库根**和**当前目录**各找一遍,所以从哪儿跑都找得到):

| | 查找顺序 |
|---|---|
| 二进制 | `VIDBRIEF_WHISPER_BIN` → `tools/whisper/Release/whisper-cli.exe`(或 `whisper-cli`)→ `tools/whisper/whisper-cli.exe` → `PATH` 上的 `whisper-cli` |
| 模型 | `VIDBRIEF_WHISPER_MODEL` → `tools/whisper/models/ggml-medium.bin` → `ggml-small.bin` → `ggml-base.bin` |

**怎么下**(Windows 为例,下面这些链接都核实过可下载):

```bash
mkdir -p tools/whisper/Release tools/whisper/models

# 1) whisper.cpp 预编译二进制(b4938 这个 tag 里带 whisper-bin-x64.zip)
curl -L -o /tmp/whisper-bin-x64.zip \
  https://github.com/ggml-org/whisper.cpp/releases/download/b4938/whisper-bin-x64.zip
unzip -o /tmp/whisper-bin-x64.zip -d tools/whisper/Release

# 2) ggml 模型(**中文要用多语种版,别下 ggml-*.en.bin**)
curl -L -o tools/whisper/models/ggml-small.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin      # 466 MB,够用
curl -L -o tools/whisper/models/ggml-medium.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin     # 1.5 GB,更准
```

其它平台:在 [whisper.cpp releases](https://github.com/ggml-org/whisper.cpp/releases) 里挑对应包,或者自己 CMake 编;
whisper.cpp 仓库里也有 `sh models/download-ggml-model.sh small` 这类官方下载脚本。

**选哪个档**:`small` 够用,`medium` 明显更准但慢约三倍(16 线程纯 CPU 实测:small ≈ 6.8× 实时,medium ≈ 2.1× 实时)。
纯 CPU 完全够用,**不需要显卡**。完整的档位 / 内存 / 耗时表与实测记录见 [docs/WHISPER_SETUP.md](./docs/WHISPER_SETUP.md)。

> `tools/whisper/models/` 与 `tools/whisper/Release/` 已在 `.gitignore` 里 —— 别把它们提交上来。

## 接到 agent 上(MCP)

服务走 **stdio**,由 agent 客户端把进程拉起来(不占端口、不用先起服务):

```jsonc
// 以某个 MCP 客户端为例 —— command 要写绝对路径
{
  "mcpServers": {
    "vidbrief": { "command": "/绝对路径/vidbrief", "args": ["mcp", "--stdio"] }
  }
}
```

Windows 上大概是 `"command": "E:/path/to/vidbrief.exe"`。

- 一共 **25 件工具**:建项目 / 跑步 1 / 取材料(图、转录、截图清单)/ 存结论(纠错、看图结论、补图)/ 写总结 / 出报告 / 提示模板。
  速查表 [skill/tools.md](./skill/tools.md);完整清单与提示模板见 [docs/DESIGN.md](./docs/DESIGN.md) §11 与 §10.4。
- **参数怎么填、门的规矩、报错怎么解**:以工具 `get_guide(step=…)` 的说明为准 —— 那份会展开讲。
- 想自己验一遍协议(不装客户端):

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"me","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
| ./target/debug/vidbrief mcp --stdio
```

> ⚠️ **stdout 归协议所有**:调试时任何 `println!` 混进去都会让会话当场坏掉,诊断一律走 stderr。
> 这一条由端到端测试逐行解析 stdout 守着。

## 命令行

**人会用的**:

| 命令 | 干什么 |
|------|--------|
| `init <video>` | 建项目。`-n` 项目 id · `-t` 中文标题 · `-p` 项目目录本身(默认 `projects/<id>`) |
| `preprocess <project>` | 步 1,纯算法。`-f` 采样帧率(默认 1.0)· `-w` 打分宽度(默认 480)· `--no-transcribe` · `--no-shots` · `--force` |
| `ui <project>` | 渲染视图 + 起本地服务 + 开浏览器。`--view workbench\|report` · `--no-open` |
| `status <project>` | 项目状态 + 健康检查,并打一栏**环境体检**(ffmpeg / ffprobe / whisper 在不在、什么版本)。`--verify` 会重算源视频 sha256 |
| `approve <project>` | 人给步 2 签字,打开通往步 3 的门(`-n` 记下你的原话) |
| `upgrade <project>` | 旧格式项目 / 状态文件读不动时,把项目对齐到当前版本 |

**agent 写回的入口**(与 MCP 工具一一对应;脱机自测、排查问题也用它们):

| 命令 | 干什么 |
|------|--------|
| `correct <project> -j F` | 把转录纠错结论写回 |
| `shot-check <project> -j F` | 记一张图的看图结论 |
| `supplements <project> -j F` | 处置一批补图候选 |
| `summary <project> -m F` | 写总结(过步 2 的门) |
| `report <project> -j F` | 出报告(数据 → 页面 → Markdown 导出) |
| `mcp [--stdio]` | 启动 MCP 服务 |

`-j -` / `-m -` = 从 stdin 读;每条命令都有 `-h` 讲清参数与产出。

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

**一个视频 = 一个项目目录**,四步产物分目录落盘,**材料即接口**(agent 读文件、写结论):

```
projects/<id>/
├── project.json         元数据 + 进度
├── source/              源视频(硬链接 / 软链接 / 复制)+ ffprobe 元数据
├── 01_preprocess/       差帧图 / 密集区 / 转录 / 截图 / 时间线视图
├── 02_calibrate/        纠错后的转录 / 补图 / 校准日志 / 视图
├── 03_summary/          总结稿 + 探索笔记
└── 04_report/           报告 MD / HTML / assets
```

每个 JSON 的字段、枚举与约束定义在 [docs/schema/](./docs/schema/),并由「schema ↔ 示例文件 ↔ Rust 类型」三方锚定 ——
示例文件不是文档,是契约(逐字节往返比对,漂移就红)。

## 构建与测试

```bash
cargo build --workspace     # 编译(产物 target/debug/vidbrief)
cargo test  --workspace     # 全部测试;当前 502 passed / 0 failed / 5 ignored
```

**跑测试需要 ffmpeg / ffprobe 在 PATH** —— 回归测试会真的调 ffmpeg,素材是仓库自带的 60 秒合成视频
`fixtures/test_sample.mp4`(已知切换点 t=20/30/50,几秒钟跑完),不需要你自己的视频,也不需要模型。

覆盖范围:算法单元测试 · JSON Schema 与 Rust 类型的三方契约 · CLI 端到端(真建项目、真抽帧)·
MCP 协议(stdout 逐行解析)· 本地查看服务的 HTTP。

**默认跳过的 5 条**要真素材 / 真模型,想跑时显式点名:

```bash
# 真 whisper 链路(要 tools/whisper/Release 与 models/,几十秒)
cargo test -p vidbrief-asr -- --ignored --nocapture

# 真实长视频(要一条真片子;不设就跳过)
VIDBRIEF_REAL_VIDEO=/path/to/长视频.mp4 cargo test -p vidbrief-core -- --ignored --nocapture

# 已建好的真项目(**只读**,不会往你的项目里写东西)
VIDBRIEF_REAL_PROJECT=/path/to/projects/<id> cargo test -p vidbrief-cli -- --ignored --nocapture
```

**平台**:在 Windows 上开发与验证;代码里没有平台专属路径。硬链接相关的测试假设"仓库所在卷支持硬链接"
(NTFS / ext4 / APFS 可以,FAT / exFAT 不行 —— 会失败并告诉你为什么)。

## 环境变量

| 变量 | 作用 | 默认 |
|------|------|------|
| `VIDBRIEF_FFMPEG` | ffmpeg 可执行文件 | `PATH` 上的 `ffmpeg` |
| `VIDBRIEF_FFPROBE` | ffprobe 可执行文件 | `PATH` 上的 `ffprobe` |
| `VIDBRIEF_WHISPER_BIN` | whisper-cli 可执行文件 | 见上「放在哪」 |
| `VIDBRIEF_WHISPER_MODEL` | ggml 模型文件 | 依次找 `tools/whisper/models/ggml-{medium,small,base}.bin` |
| `VIDBRIEF_VIEW_REGISTRY` | 本地查看服务的登记目录(多会话 / CI 各用一份,互不干扰) | 系统临时目录下的 `vidbrief-view` |
| `VIDBRIEF_REGEN_SKILL=1` | 按"活的工具表"重写 `skill/tools.md`(开发用) | 关 |
| `VIDBRIEF_REAL_VIDEO` | **只在测试里用**:真实长视频路径 | 未设(相关测试跳过) |
| `VIDBRIEF_REAL_PROJECT` | **只在测试里用**:已建好的项目路径 | 未设(相关测试跳过) |

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
skill/               给 agent 的技能文档(工具速查 / 工作流 / 风格)
docs/                设计、schema、需求背景、实测记录(见下表)
fixtures/            测试素材:60 秒合成回归视频
tools/whisper/       whisper.cpp 二进制 + ggml 模型(**不入 git,自己下**)
projects/            项目工作区(**不入 git**,由 `vidbrief init` 生成)
```

## 文档

| 文档 | 内容 |
|------|------|
| [docs/DESIGN.md](./docs/DESIGN.md) | 主设计文档(数据模型 / 算法 / 视图 / MCP 工具清单 / 里程碑 / 关键决策) |
| [docs/schema/](./docs/schema/) | 项目材料格式的**唯一权威**:JSON Schema + 示例 |
| [docs/CHANGELOG.md](./docs/CHANGELOG.md) | 里程碑与关键决策(公开摘要) |
| [docs/RESEARCH.md](./docs/RESEARCH.md) | 技术选型核实与实测记录 |
| [docs/WHISPER_SETUP.md](./docs/WHISPER_SETUP.md) | 本地 Whisper:档位、内存、耗时、实测 |
| [docs/BRD.md](./docs/BRD.md) | 需求背景(为什么做这个) |
| [docs/devlog/README.md](./docs/devlog/README.md) | 开发过程记录怎么记(本仓库只带规范,不带记录文件) |

## 现在是什么状态

**开发告一段落(2026-09-27)**:M0–M7 全部完成,四步流程(前处理 / 校准 / 总结 / 报告)都能跑通,全仓测试 502 passed。
里程碑与关键决策见 [docs/CHANGELOG.md](./docs/CHANGELOG.md)。

开发过程记录(逐条的决策、实测数据、问题复盘)**不入库** —— 仓库只带记录规范,见 [docs/devlog/README.md](./docs/devlog/README.md)。

## 许可证

本仓库目前没有附许可证文件 —— 在补上之前,默认是「保留所有权利」。
