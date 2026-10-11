# BRD 落地方案研究报告

> 目标:用 Rust 实现"长视频(30 分钟知识类)→ 帧切换检测 + 变化率评分 → 时间线 JSON → 喂给大模型按需看帧 → 总结"的流水线。
> 本文核实了 BRD 中每一项技术声明的真伪,并给出可落地的实施方案。
>
> ⚠️ **归档说明(2026-09-24)**:本文记录的是 M1/M2 阶段(旧架构)的实测过程。文中提到的实现产物——`src/main.rs`、`scripts/`、`timeline.json`、`timeline_<项目>.json`、`frames/`、`report_<项目>.md`——已在按 [DESIGN.md](./DESIGN.md) 重建时从工作区移除,本仓库不再保留旧实现的存档。**结论与实测数据依然有效。**

---

## 一、BRD 技术声明逐条核实

### 1.1 ffmpeg-sidecar ✅ 真实、活跃、推荐

| 项 | 核实结果 |
|----|---------|
| 存在性 | ✅ crates.io 真实存在,最新 **2.5.2**(2026-05-30 发布) |
| 活跃度 | 总下载 178 万+,近 30 天 50 万+ |
| 说明 | 包装独立 FFmpeg 二进制,提供**迭代器接口**逐帧吐原始 RGB |
| 亮点 | `auto_download()` 自动下载 FFmpeg(支持 Windows/Mac/Linux),`filter_frames()` 直接拿 RGB 帧 |
| 注意 | 帧对象是 `frame.data: Vec<u8>`(原始 RGB),**自带 width/height**,不需要像 BRD 那样硬编码尺寸 |

**BRD 代码勘误**:BRD 示例里 `frame_data[..frame_size].to_vec()` + 手动拼 `RgbImage` 是多此一举——
现在用 `iter.filter_frames()` 拿到的 `frame` 直接含 `width`/`height`/`data`,跳过 `image` crate 转换。

### 1.2 av-scenechange ✅ 真实,但 API 与 BRD 写的不一样

| 项 | 核实结果 |
|----|---------|
| 存在性 | ✅ rust-av 官方维护,最新 **0.24.1**(2026-06-22) |
| 活跃度 | 总下载 2500 万+,近 30 天 1200 万+(被 rav1e 等大量引用) |
| 定位 | 基于 rav1e 的场景检测,**为编码器找关键帧位置优化**,不是给人看的"场景切分" |
| ⚠️ 关键提示 | **0.14.1(2025-05)开始依赖 `av-decoders`(统一解码器抽象)取代 `ffmpeg-the-third`**;0.24.1 是 edition 2024,MSRV 1.95 |

**BRD 代码勘误**(BRD 里的 `DetectionOptions { analysis_speed, detect_flashes, min_scenecut_distance, max_scenecut_distance }` 已过时):

0.24.1 的真实顶层 API:

```
pub use av_decoders;                        // 解码器抽象(re-export)
pub struct Decoder;                         // 统一解码器,自动选后端
pub struct DetectionOptions;                // 检测参数
pub struct DetectionResults;                // 检测结果
pub struct SceneChangeDetector;             // 逐帧分析器
pub struct ScenecutResult;                  // 单帧分数
pub enum SceneDetectionSpeed;               // 检测算法档位
pub fn detect_scene_changes(decoder, opts, cb, Some(len)) -> DetectionResults;
pub fn new_detector(opts) -> SceneChangeDetector;   // 逐帧驱动
```

- 结果里每帧有 **scenecut 分数**,可直接当"变化率评分"用
- 但要注意:它面向**编码关键帧放置**,对"PPT 翻页/淡入淡出/小文字变化"不敏感;BRD 想要的"全局时间线评分"建议**自研帧差异**(见 2.2)

### 1.3 rsmpeg ✅ 真实,字节跳动维护,但对接 av-scenechange 的说法已变

| 项 | 核实结果 |
|----|---------|
| 存在性 | ✅ 最新 0.18.0+ffmpeg.8.0(2025-08-24) |
| 活跃度 | 总下载 17 万+,近 30 天 3.3 万+ |
| 说明 | FFmpeg 6/7/8 的 safe 绑定,`ffmpeg7`/`ffmpeg8` 等 feature 可选 |
| 注意 | **它与 av-scenechange 已无直接接线**——av-scenechange 0.15+ 改用 `av-decoders`,`ffmpeg-the-third` 时代结束 |

**结论**:`rsmpeg + av-scenechange` 组合写不成(版本错位);要么用 `av-scenechange` 自己的 `Decoder`,要么用 sidecar 抽帧自研差异。

### 1.4 ez-ffmpeg ✅ 真实,活跃度高

| 项 | 核实结果 |
|----|---------|
| 存在性 | ✅ 最新 0.18.1(2026-09-01) |
| 活跃度 | 总下载 22 万+,近 30 天 3.7 万+,仍在高速迭代(0.1→0.18 只用了 6 个月) |
| 特点 | 高层 API,`static` feature 可静态编译 FFmpeg;文档里确有 `FrameExtractor` 概念 |
| ⚠️ 注意 | API 变更极快(0.x 尚未稳定),锁定版本使用;代码量大(~10 万行),首次编译久 |

### 1.5 Whisper 方案(音频转录)✅ 两条路都成立

| 方案 | crate | 说明 |
|------|-------|------|
| whisper.cpp 绑定 | `whisper-rs` 0.16.0(2026-03-12),近 30 天 62 万下载 | 本地推理,`cuda`/`vulkan`/`openblas`/`metal` 等 feature 全;要自己下 ggml 模型 |
| 走 API | OpenAI Whisper API 或 Groq/阿里等 | 成本极低(30 分钟约 ¥0.01),无需本地编译 C++ |

> whisper-rs 是 C++ 绑定,首次编译会编 whisper.cpp 全家桶,耗时明显;**本地优先建议直接用 `ffmpeg-sidecar` 的 `ffprobe`+whisper.cpp 命令行**,或先走 API 验证。
>
> 📄 **本地跑 Whisper 的硬件配置要求**详见 [WHISPER_SETUP.md](./WHISPER_SETUP.md)(模型规格表 / CPU vs GPU / 各档位机器推荐 / 中文知识视频组合建议)。

---

## 二、推荐架构(结合 BRD 第四轮对话 + 本轮核实)

BRD 里"帧检测→时间线评分→音频引导按需采样"的思路完全成立,工具链修正后如下:

```
ffmpeg-sidecar (auto_download 自带 FFmpeg)
      │  抽帧: -vf fps=1,scale=480:-1  →  filter_frames() 拿 RGB
      ▼
自研帧差异评分 (直方图 + 像素均差 + 亮度,轻量,见下)
      │
      ▼
时间线 JSON (segments + change_score + 峰值时刻)
      │
      ▼
[可选] whisper-rs / Whisper API 转录带时间戳文本
      │
      ▼
LLM 决策: 读转录 + 时间线 → 输出想看哪些时间段/帧
      │
      ▼
ffmpeg-sidecar 按需精确抽帧 → 精选帧 + 转录 → LLM 最终总结
```

### 2.1 为什么"帧检测自研"而不直接用 av-scenechange

av-scenechange 是给**编码器找关键帧**设计的(官方文档原话:"may not be the best tool if your use case is to generate scene changes as a human would interpret them")。
而你的需求是"画面变化率高 ≈ 可能有重要信息"(PPT 翻页、板书切换、演示操作),这类**内容变化**和编码关键帧的**判定标准不同**。自研直方图+像素差只要几十行,效果可控、可解释。

### 2.2 帧差异评分实现要点(已写进示例工程)

```rust
// 每个采样帧:计算与上一帧的差异
// 1) 全局像素平均绝对差(缩略到 ~480px 宽,30min@1fps 也只有 1800 帧)
// 2) 颜色直方图(如 8 bins/channel)L1 距离
// 3) 亮度均值变化
// 合并加权 → 0.0~1.0 变化率;移动平均去噪;找局部峰值
```

抽帧参数建议:
- 知识类/固定机位讲座:1 fps 足够(1800 帧),画面信息主要靠音频兜底
- PPT 讲解:可在变化率峰值前后加密(±2s 内 5fps)补偿翻页瞬间
- 缩放到 480px 宽即可,省 CPU 省管道带宽

### 2.3 成本对照(30 分钟知识视频,2026 年价)

| 方案 | 单次成本 | 备注 |
|------|---------|------|
| 帧检测+时间线(本地) | ≈0 | sidecar 自带 FFmpeg,CPU 秒级~分钟级 |
| Whisper API 转录 | ~¥0.01 | 本地 whisper-rs 为 0 |
| LLM 决策(读转录+时间线) | ~¥0.005 | 纯文本,几十 K token |
| LLM 最终总结(转录+精选帧) | ~¥0.02 | 图像 token 计入 |
| **合计** | **~¥0.03–0.05** | 比直接传视频给 Gemini Flash-Lite(~¥0.07)便宜且更精准 |

---

## 三、实施路径与风险

### 3.1 里程碑

1. **M1(本项目已做)**:sidecar 抽帧 + 帧差异评分 + 时间线 JSON 输出 → 验证核心可行性
2. **M2**:接 Whisper(先 API,再考虑 whisper-rs 本地化)得到带时间戳转录
3. **M3**:LLM 决策循环(转录+时间线→帧请求→精选帧→总结)
4. **M4**:批量处理、缓存、并发、失败重试

### 3.2 风险清单

| 风险 | 等级 | 缓解 |
|------|------|------|
| av-scenechange API 快速变动(0.14→0.24 换了解码器层) | 中 | 锁版本;或干脆自研 |
| ez-ffmpeg 0.x 未稳定,API 漂移 | 中 | 锁版本;本项目不依赖它 |
| whisper-rs 首次编译慢(C++ 全家桶) | 低 | 先走 API 验证;本地化放 M2 后期 |
| Windows 上 ffmpeg-sidecar auto_download 需要外网 | 低 | 可手动放 ffmpeg.exe;或离线部署 |
| 管道传输 RGB 帧内存拷贝 | 低 | 缩略图 480px + 1fps 后数据量很小 |

### 3.3 推荐结论

- **快速验证/不碰 FFI**:`ffmpeg-sidecar` + 自研帧差异 ✅(本项目采用)
- **上线高性能**:`ez-ffmpeg`(锁 0.18.1)或 `rsmpeg`(锁 0.18.0)进程内解码
- **场景切分要"人工观感"**:`av-scenechange` 0.24.1 + 它自己的 `Decoder`(不是 rsmpeg)
- **音频转录(已拍板)**:本地 `whisper.cpp` 命令行(`whisper-cli` + ggml 模型),中文知识视频默认 `ggml-medium.bin`;**不用 Python 生态**(funasr/SenseVoice 已否决——用户要求零 Python 依赖)。见 [WHISPER_SETUP.md](./WHISPER_SETUP.md) 第〇节实测记录。

> 一句话:BRD 的整体思路是对的,但**它引用的三个库的接法和 API 已过时**;修正后的最小可用栈就是本仓库这个例子。

---

## 四、实测验证(M1 已完成,2026-09-09)

### 4.1 环境与产物

- Rust 1.96 / Windows;ffmpeg 通过 winget 装 BtbN.FFmpeg.GPL,复制到 `target/debug/ffmpeg.exe`(sidecar 同目录优先)
- 产物:`src/main.rs`(约 400 行单文件可执行)+ `timeline.json`(样例输出)
- 命令:`cargo run --release -- test_sample.mp4 --fps 1 --output timeline.json`

### 4.2 端到端结果

用 ffmpeg 合成 60 秒测试视频:`testsrc2(20s) + 蓝(10s) + 棕(20s) + 绿(10s)`,真实切换点 **t=20 / 30 / 50**。

检测输出:

| 项 | 结果 |
|----|------|
| 帧数 | 60(1fps×60s),尺寸 480×270 |
| 三个硬切换点 | **t=20、30、50 全部命中(score=1.0)** ✅ |
| 平稳段 | 纯色段变化率为 0 ✅ |
| 伪峰值 | testsrc2 动态测试图案产生若干 0.28 级小峰(预期,真实视频无此问题) |

**结论**:直方图 L1 + 亮度加权(75/25)对"PPT 翻页/硬切换"非常有效;`find_peaks` 用**原始分数局部极大 + 邻域突出度判据**(而非平滑后阈值),能抓住孤立硬切换。

### 4.3 关键实现要点(避免踩坑)

1. `FfmpegCommand::iter()` 返回 **`anyhow::Result<FfmpegIterator>`**,要 `?` 解开再 `while let Some(ev) = it.next()`
2. 帧事件变体是 **`FfmpegEvent::OutputFrame(OutputVideoFrame)`**,不是 `OutputVideoFrame`;字段 `width/height/timestamp/data` 齐全,RGB24 无需自己拼
3. `.input()` 收 `AsRef<str>`(传字符串,别传 `&PathBuf`);没有 `video_codec()`,用 `.rawvideo()` 预设 `-f rawvideo -pix_fmt rgb24 -`
4. `wait()` 返回 `io::Result<ExitStatus>`,`exit.success()` 是**方法**
5. ffmpeg-sidecar 的 auto_download 直连 gyan.dev 很慢且易断;实测 **winget 装 FFmpeg 并把 exe 复制到 exe 同目录** 最省事(Windows)

---

## 五、M2+ 完整流水线实测:图文对齐(2026-09-09,真实视频验证 ✅)

> 用户提出关键洞察:**仅靠转录总结会丢失画面信息**。知识类/教学视频里大量信息在图表、标注、幻灯片上,文字描述不出来,必须**抽帧核对**。本节在一段真实教学视频(时长十几分钟,含讲者镜头、幻灯片与画面演示交替)上完整验证了这套"图文补全"流水线。**素材与转录都不入仓库**,本节只记方法与结论,不复述任何一帧的内容。

### 5.1 输入

一段外部教学视频录屏(十几分钟;**素材与转录都不入仓库**)。选择它的理由是它具备知识类视频的典型形态:讲者镜头、幻灯片、画面演示三类画面交替出现,且讲者会边说边切换画面。

### 5.2 流水线(四个环节全部实测跑通)

```
① ffmpeg 抽音频 → whisper-cli(medium) 转录 → 带时间戳的 txt/.srt
② LLM 校对转录(专有名词纠错:把同音错字改对)
③ 帧时间线分析(video-reader.exe)找画面切换点 peaks
   + 从 srt 定位讲解关键词时间戳 → 两者对齐
④ ffmpeg 精确抽帧 → 视觉 API(多模态 LLM)识别画面
   → 关键帧描述 + 转录片段 合并成图文报告
```

### 5.3 对齐结果:转录时间戳 × 画面切换点 高度吻合

在整支视频上逐处比对"讲者开始讲一个新点的语音时刻"与"画面切换 peak",**绝大多数都能对上**(切换后立即开讲);少数几处字幕比画面略早或略晚,但都在一两秒内 —— 足以作为"抽帧核对"的定位依据。

### 5.4 视觉识别补全的信息——转录完全无法提供

对每张关键帧单独看图,普遍出现这类情况:**转录只说了「先看这个」「这张图」,而画面上的信息(具体对象、数值区间、标注线、结构与状态)只有看图才拿得到**。也就是说,不抽帧的话,总结里就会缺掉视频最关键的那部分内容。

### 5.5 关键结论

1. **画面切换点 = 内容切换点**:知识教学视频换图(幻灯片/演示)时刻,帧时间线的 peaks 能精确捕捉,与转录时间戳几乎一一对应。**这就是"抽帧核对"的定位依据** —— 每一处讲什么,视觉模型可以直接读图。
2. **图文结合产生转录无法给的信息**:画面上的具体对象、数值区间、标注线位置、关键标记、结构与状态。这些**必须在总结报告里以图呈现**,否则读者无法理解。
3. **视觉模型还能反过来纠错转录**:转录听错的词(同音字)、转录认错的载体(说是实拍,其实是幻灯片)—— 图比转录文字更接近事实。
4. **成本**:关键帧识别 ≈ 1 次 API 调用/张,30 分钟视频抽 ~20-40 张关键帧,成本远低于直接传整段视频给多模态模型,且信息密度更高(专门挑切换点附近的图)。

### 5.5b 图文联合输入(关键升级,2026-09-09 实测)

> 用户指出:单模态"看图转述"还是容易错(纯看图时,视觉模型会凭画面**臆断**一个转录里其实说清了的规格)。正确做法是**图 + 同期转录一起喂给多模态模型,用文本约束视觉**。

实测(一张关键帧 + 同期转录):

| 项 | 单看图 | 图文联合 |
|----|------------|---------------|
| 对象 | ⚠️ 凭画面猜 | ✅ 结合画面上的文字,把转录里的同音错字纠正过来 |
| 规格 | ❌ 臆断一个画面里其实没有的规格 | ✅ 服从转录给出的规格,且诚实标注"画面无法直接确认" |
| 矛盾检测 | 无 | ✅ 主动指出"转录提到的对象与画面不符"——纯单模态发现不了的交叉校验 |

**结论**:
- 图文联合输入能**纠正单模态误判**(规格不再臆断)、**双向纠错**(转录错→图纠;图错→转录纠)
- 还能做**交叉一致性检查**(转录提到的 vs 画面实际显示的),这对"知识视频总结"质量是质变
- 所以 M3 的正确形态不是"先描述图再让 LLM 总结",而是**每个关键帧带上下文转录一起进多模态模型**

### 5.6 产物

- 转录:带时间戳的 txt/.srt/.vtt(素材不入库)
- 时间线:`timeline.json`(画面切换 peaks)
- 关键帧:`frames/*.jpg`(含图表/幻灯片)
- 视觉描述:`frames/descriptions.txt`(单看图输出)
- 图文联合测试:`frames/ctx_test_*.txt`(图+转录同输入)
- 脚本:`scripts/describe_frames.py`(单看图)、`scripts/describe_frames_with_context.py`(图文联合)、`scripts/build_report.py`(内嵌图 Markdown 报告,生成 `report.md`)
- 全部用 Python 标准库(urllib/base64/json),零第三方依赖

> 下一步(M3):把 ①~④ 串进 Rust 编排程序;关键帧用**图文联合**方式喂多模态模型;输出"关键帧内嵌"的 Markdown/HTML 报告。

