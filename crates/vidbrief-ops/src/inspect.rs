//! 项目体检:四步走到哪了、材料都在不在、源视频还可不可用。
//!
//! 对应 CLI 的 `status` 与 MCP 的 `get_project`。

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use vidbrief_project::{
    DenseRegions, Health, Layout, Project, Shots, Transcript, VideoMeta, verify_content,
};

use crate::project::OpenProject;

/// 体检结论的**可序列化**镜像。
///
/// [`vidbrief_project::Health`] 是给终端设计的(带 `marker()` 画 ✓/·/✗),没有 serde 实现。
/// 不直接给它加:那是契约层类型,为一个展示用途动契约不合适。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthLevel {
    Ok,
    Note,
    Problem,
}

/// 一条体检结论。`level` 给机器判,`message` 给人读。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthView {
    pub level: HealthLevel,
    pub message: String,
}

impl HealthView {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            level: HealthLevel::Ok,
            message: message.into(),
        }
    }
    pub fn note(message: impl Into<String>) -> Self {
        Self {
            level: HealthLevel::Note,
            message: message.into(),
        }
    }
    pub fn problem(message: impl Into<String>) -> Self {
        Self {
            level: HealthLevel::Problem,
            message: message.into(),
        }
    }
    pub fn is_problem(&self) -> bool {
        self.level == HealthLevel::Problem
    }
    /// 终端用的一行前缀。
    pub fn marker(&self) -> &'static str {
        match self.level {
            HealthLevel::Ok => "✓",
            HealthLevel::Note => "·",
            HealthLevel::Problem => "✗",
        }
    }
}

impl From<Health> for HealthView {
    fn from(h: Health) -> Self {
        let level = match h {
            Health::Ok(_) => HealthLevel::Ok,
            Health::Note(_) => HealthLevel::Note,
            Health::Problem(_) => HealthLevel::Problem,
        };
        Self {
            level,
            message: h.message().to_string(),
        }
    }
}

/// 截图清单的分档计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ShotCounts {
    pub total: usize,
    /// 留用的(被 `drop` 的不算)。
    pub keep: usize,
    /// 还没被 agent 看过的。
    pub unchecked: usize,
    /// 需要人来裁决的(把握低 / 报了问题 / 判定图文不符)。
    pub needing_review: usize,
}

impl ShotCounts {
    pub fn of(shots: &Shots) -> Self {
        Self {
            total: shots.shots.len(),
            keep: shots.usable().count(),
            unchecked: shots.unchecked().count(),
            needing_review: shots.needing_review().count(),
        }
    }
}

/// 材料盘点到哪一步了。
///
/// 全部是**廉价**判据(文件在不在、数组多长),故意不展开内容 ——
/// `list_projects` 要把每个项目都扫一遍。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Materials {
    /// 差帧图的采样点数。
    pub curve_points: Option<usize>,
    /// 密集区个数。
    pub regions: Option<usize>,
    /// 截图分档计数。
    pub shots: Option<ShotCounts>,
    /// 原始转录的段数。
    pub transcript_segments: Option<usize>,
    /// `transcript.fixed.json` 里被标记过的段数(含"存疑但未改")。
    pub corrected_segments: Option<usize>,
    /// 补图候选:总条数 / 还没被 agent 看过的条数。
    pub supplement_total: Option<usize>,
    pub supplement_pending: Option<usize>,
    /// 视图渲染出来没有。
    pub timeline_html: bool,
    pub calibrate_timeline_html: bool,
    /// 步 2/3 的写入产物。
    pub cross_validate: bool,
    pub calibration_log: bool,
    pub summary_md: bool,
    pub report_md: bool,
    pub report_html: bool,
}

/// 扫一遍项目目录里有哪些材料。
///
/// **坏文件一律当"没有"** —— 这里是盘点,不是校验;想校验走 [`inspect`] 的 `--verify`。
pub fn scan_materials(layout: &Layout) -> Materials {
    let curve = vidbrief_project::ChangeCurve::load(&layout.change_curve()).ok();
    let regions = DenseRegions::load(&layout.dense_regions()).ok();
    let shots = Shots::load(&layout.shots_json()).ok();
    let transcript = Transcript::load(&layout.transcript()).ok();
    let fixed = Transcript::load(&layout.transcript_fixed()).ok();
    let supplement =
        vidbrief_project::SupplementRequests::load(&layout.supplement_requests()).ok();

    Materials {
        curve_points: curve.as_ref().map(|c| c.changes.len()),
        regions: regions.as_ref().map(|r| r.regions.len()),
        shots: shots.as_ref().map(ShotCounts::of),
        transcript_segments: transcript.as_ref().map(|t| t.segments.len()),
        corrected_segments: fixed
            .as_ref()
            .map(|t| t.segments.iter().filter(|s| s.correction.is_some()).count()),
        supplement_total: supplement.as_ref().map(|s| s.requests.len()),
        supplement_pending: supplement.as_ref().map(|s| s.pending().count()),
        timeline_html: layout.preprocess_timeline().is_file(),
        calibrate_timeline_html: layout.calibrate_timeline().is_file(),
        cross_validate: layout.cross_validate().is_file(),
        calibration_log: layout.calibration_log().is_file(),
        summary_md: layout.summary_md().is_file(),
        report_md: layout.report_md().is_file(),
        report_html: layout.report_html().is_file(),
    }
}

/// [`inspect`] 的选项。
#[derive(Debug, Clone, Copy, Default)]
pub struct InspectOptions {
    /// 重算源视频 sha256 并与记录比对。
    ///
    /// 能发现"源文件被**就地改写**"(硬链接与软链接都会跟着变,而链接关系看起来毫无异常),
    /// 代价是把整个视频读一遍。
    pub verify: bool,
}

/// 一次体检的完整结论。
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    pub project: Project,
    pub dir: PathBuf,
    pub meta: VideoMeta,
    /// 按 `source.link` 分别检查(见 `docs/DESIGN.md` §4.8)。
    pub link_health: HealthView,
    /// 只有 `verify` 时才有的内容哈希比对。
    pub content_health: Option<HealthView>,
    pub materials: Materials,
}

impl InspectReport {
    /// 有硬伤时为真 —— CLI 用它定退出码,MCP 用它定 `isError`。
    pub fn unhealthy(&self) -> bool {
        self.link_health.is_problem()
            || self
                .content_health
                .as_ref()
                .is_some_and(HealthView::is_problem)
    }
}

/// 体检一个项目。
pub fn inspect(arg: &str, opts: &InspectOptions) -> Result<InspectReport> {
    let opened = OpenProject::open(arg)?;
    inspect_opened(&opened, opts)
}

/// 已经打开的项目直接体检(少读一遍 `project.json`)。
pub fn inspect_opened(opened: &OpenProject, opts: &InspectOptions) -> Result<InspectReport> {
    let layout = opened.layout();
    let link_health = opened.link_health();
    let content_health = if opts.verify {
        Some(HealthView::from(verify_content(
            opened.project(),
            opened.meta(),
            layout,
        )?))
    } else {
        None
    };

    Ok(InspectReport {
        project: opened.project().clone(),
        dir: layout.root().to_path_buf(),
        meta: opened.meta().clone(),
        link_health: HealthView::from(link_health),
        content_health,
        materials: scan_materials(layout),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_view_keeps_the_level() {
        let ok = HealthView::from(Health::Ok("好".into()));
        assert_eq!(ok.level, HealthLevel::Ok);
        assert_eq!(ok.marker(), "✓");
        assert!(!ok.is_problem());

        let bad = HealthView::from(Health::Problem("坏了".into()));
        assert!(bad.is_problem());
        assert_eq!(bad.marker(), "✗");
        assert_eq!(bad.message, "坏了");
    }

    #[test]
    fn health_level_wire_names() {
        assert_eq!(serde_json::to_string(&HealthLevel::Ok).unwrap(), "\"ok\"");
        assert_eq!(
            serde_json::to_string(&HealthLevel::Problem).unwrap(),
            "\"problem\""
        );
    }

    #[test]
    fn scanning_an_empty_project_finds_nothing() {
        let root = std::env::temp_dir().join(format!("vidbrief-ops-scan-{}", std::process::id()));
        let layout = Layout::new(&root);
        layout.create_dirs().unwrap();
        let m = scan_materials(&layout);
        assert_eq!(m, Materials::default());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
