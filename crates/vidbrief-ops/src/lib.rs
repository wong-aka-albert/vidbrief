//! 项目上的**动作层**:建项目、跑一步、取材料、存结论。
//!
//! 设计见 `docs/DESIGN.md` §3.2(四步与工具组)、§11(MCP 工具清单)。
//!
//! # 这一层为什么存在
//!
//! 同一个动作有两个入口:人和脚本用的 `vidbrief` 命令行,以及 agent 用的 MCP 服务。
//! 两个入口**必须做同一件事** —— 否则"CLI 跑出来的项目"和"MCP 跑出来的项目"会悄悄分家,
//! 而它们写的是同一批 JSON 文件。
//!
//! 所以编排逻辑不能待在任何**入口**里:
//!
//! | 层 | 有什么 | 没有什么 |
//! |----|--------|---------|
//! | 契约 [`vidbrief_project`] | 文件格式、目录、锁 | 不知道"一步"是什么 |
//! | 能力 `vidbrief-core` / `-asr` / `-viz` | 单个算法或渲染 | **不跨 crate 编排** |
//! | **动作**(本 crate) | "建项目"= 探测+搬运+落盘;"跑步 1"= 视觉+转录+标记完成 | 不打印、不解析参数、不认识 JSON-RPC |
//! | 入口 `vidbrief-cli` / `vidbrief-mcp` | 参数 / JSON-RPC、把结果说给人或 agent 听 | 不含任何流程判断 |
//!
//! 判据很直接:**把这一层删掉,CLI 和 MCP 就得各写一份一模一样的流程** —— 那正是它存在的理由。
//!
//! # 两个贯穿约定
//!
//! 1. **本层不往 stdout 写任何东西。** 进度通过回调传出去(与 `vidbrief-core::pipeline` 一致):
//!    库不该替调用方决定说什么。MCP 尤其吃这个 —— stdio 上的 stdout 是协议通道,
//!    一行 `println!` 就能把整个会话搞坏。
//! 2. **输出类型都能 `Serialize`**,输入类型不强制。理由:输出要变成 JSON 给 agent 看,
//!    而输入来自命令行或 JSON,形态本就不同。

pub mod conclusions;
pub mod inspect;
pub mod materials;
pub mod preprocess;
pub mod project;
pub mod review;
pub mod shots;
pub mod view;

/// 测试夹具(造一个材料齐全的假项目)。见 `Cargo.toml` 的 `testing` 特性。
///
/// `cfg(test)` 这一支是给本 crate 自己的单测用的 —— 自己开不了自己的 feature。
#[cfg(any(test, feature = "testing"))]
pub mod testkit;

pub use conclusions::{
    CorrectionInput, CorrectionOutcome, ShotCheckInput, WriteMode, apply_corrections,
    record_shot_check, write_note, write_report_md, write_summary,
};
pub use inspect::{
    HealthLevel, HealthView, InspectOptions, InspectReport, Materials, ShotCounts, inspect,
    inspect_opened,
};
pub use materials::{
    SegmentView, ShotDetail, TranscriptView, change_curve, list_regions, shot_detail, shot_counts,
    shots, transcript, transcript_search,
};
pub use preprocess::{
    AnalysisSummary, PreprocessOptions, PreprocessReport, Stage, TranscriptOutcome, preprocess,
};
pub use project::{
    CreateOptions, Created, Notice, OpenProject, ProjectSummary, create, list_projects, resolve,
};
pub use review::{approve, counts as review_counts, gate_calibrate};
pub use shots::{
    DEFAULT_SUPPLEMENT_WINDOW, ShotAction, extract_shot, find_supplement_candidates,
    supplement_window, update_shot,
};
pub use view::{render_timeline, serve_and_open};

/// 页面动作的转发 —— **`vidbrief-mcp` 不必直接依赖 `vidbrief-viz`**。
///
/// 依赖图是 `{cli, mcp} → ops → viz`(DESIGN §10.3)。查看服务的类型属于 viz,
/// 但"给查看服务装一个执行者"是宿主的事;从 ops 转一手,那条边就不用加。
pub use vidbrief_viz::{Action, ActionHandler, set_action_handler};

/// 默认转录片段里"同期"取多长(秒)。±这个值,见 `docs/DESIGN.md` §6.2 ①。
pub const NEIGHBOR_WINDOW: f64 = 6.0;
