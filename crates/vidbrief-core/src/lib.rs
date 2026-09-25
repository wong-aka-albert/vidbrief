//! 步 1 的算法核心:ffmpeg 调用、差帧、变化密集区、精确抽帧。
//!
//! 设计见 `docs/DESIGN.md` §5;格式见 `docs/schema/`(契约类型住在 `vidbrief-project`)。
//!
//! | 模块 | 干什么 | 碰进程吗 |
//! |------|--------|:-------:|
//! | [`ffmpeg`] | 拉起 ffmpeg / ffprobe 的**唯一入口** | ✅ |
//! | [`probe`] | `ffprobe` → [`VideoMeta`](vidbrief_project::VideoMeta) | ✅ |
//! | [`feature`] | 帧特征与打分 | ❌ **纯函数** |
//! | [`regions`] | 变化密集区检测 | ❌ **纯函数** |
//! | [`shots`] | 规划要抽哪几帧 | ❌ **纯函数** |
//! | [`supplement`] | 补图候选(步 2 子任务 ④) | ❌ **纯函数** |
//! | [`curve`] | 流式抽帧 → 差帧图 | ✅ |
//! | [`extract`] | 按时刻抽单帧 → JPEG | ✅ |
//! | [`pipeline`] | 把上面几步串起来并落盘 | ✅ |
//!
//! **纯函数占了近一半,这不是巧合**:打分口径与密集区判据是整条流水线上最需要反复调的
//! 东西,它们的反馈环不能卡在外部进程上(60 秒测试片跑一轮 ffmpeg 要几百毫秒,
//! 真实长视频要几十秒)。`feature` / `regions` / `shots` 都不碰文件系统,单测在毫秒级。

pub mod curve;
pub mod extract;
pub mod feature;
pub mod ffmpeg;
pub mod pipeline;
pub mod probe;
pub mod regions;
pub mod shots;
pub mod supplement;

pub use curve::{CurveOptions, build_curve};
pub use extract::{ShotQuality, extract_frame};
pub use feature::{FrameFeature, score};
pub use pipeline::{VisualReport, analyze, extract_shots, plan_shots, run_visual};
pub use probe::{has_audio_stream, probe};
pub use regions::detect;
pub use supplement::find_candidates;
