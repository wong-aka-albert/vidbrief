//! 把项目文件组装成前端要用的那一坨 JSON。
//!
//! **刻意做成"一份自包含的数据"**:时间线 HTML 打开就能看,不依赖任何后端接口。
//! 代价是每轮校准后要重新渲染(见 `docs/DESIGN.md` §6.3),但换来的是
//! "这个 HTML 发给别人也还能看"—— 除了视频和图片仍然走相对路径。
//!
//! 采样点是**等间隔**的,所以只发分数数组,时刻由前端按 `step` 算出来 ——
//! 986 个点的项目里,这比逐个发 `{t, score}` 省掉一半体积。

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{
    ChangeCurve, DenseRegions, Layout, Project, Shots, Transcript, VideoMeta,
};

/// 前端要的全部数据。
#[derive(Debug, Serialize)]
pub struct TimelineData {
    pub project: ProjectBrief,
    /// 视频的**相对 URL**(相对 timeline.html 所在目录)。
    pub video_url: String,
    /// 一个极小的、必然存在的文件的 URL。前端拿它当**健康探针**:
    /// 视频卡住时 fetch 一下,失败就说明本地服务已经断开 —— 比一直转圈有用得多。
    pub probe_url: String,
    pub curve: CurveBrief,
    pub regions: Vec<RegionBrief>,
    pub shots: Vec<ShotBrief>,
    pub segments: Vec<SegmentBrief>,
}

#[derive(Debug, Serialize)]
pub struct ProjectBrief {
    pub id: String,
    pub name: String,
    pub duration: f64,
}

#[derive(Debug, Serialize)]
pub struct CurveBrief {
    /// 采样间隔(秒)。
    pub step: f64,
    /// 曲线里的最大分数,前端据此定纵轴。
    pub max_score: f64,
    /// 逐点变化率。时刻 = `index * step`。
    pub scores: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct RegionBrief {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub peak_t: f64,
    pub peak_score: f64,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct ShotBrief {
    pub id: String,
    pub t: f64,
    /// 图片的相对 URL。
    pub url: String,
    pub origin: String,
    pub status: String,
    pub region_id: Option<u32>,
    pub reason: Option<String>,
    pub check: Option<CheckBrief>,
}

#[derive(Debug, Serialize)]
pub struct CheckBrief {
    pub r#match: bool,
    pub audio_visual_sync: bool,
    pub type_read: String,
    pub issues: Vec<String>,
    pub confidence: String,
}

#[derive(Debug, Serialize)]
pub struct SegmentBrief {
    pub start: f64,
    pub end: f64,
    /// 已经过 [`vidbrief_project::Segment::effective_text`] —— 有纠错就用纠错后的。
    pub text: String,
    pub corrected: bool,
    pub correction: Option<CorrectionBrief>,
}

#[derive(Debug, Serialize)]
pub struct CorrectionBrief {
    pub note: String,
    pub reason: String,
}

/// 读齐一个项目的全部材料,组装成视图数据。
pub fn collect(layout: &Layout, timeline_rel: &str) -> Result<TimelineData> {
    let project = Project::load(&layout.project_json()).context("读 project.json 失败")?;
    let meta = VideoMeta::load(&layout.video_meta()).context("读 video.meta.json 失败")?;
    let curve = ChangeCurve::load(&layout.change_curve())
        .context("读 change_curve.json 失败 —— 先跑 `vidbrief preprocess`")?;
    let regions = DenseRegions::load(&layout.dense_regions()).context("读 dense_regions.json 失败")?;
    let shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());
    // 还没有转录是**正常状态**(源视频可能没音轨),不该让视图渲染不出来
    let transcript = Transcript::load(&layout.transcript()).ok();

    Ok(TimelineData {
        project: ProjectBrief {
            id: project.id.to_string(),
            name: project.name.clone(),
            duration: meta.duration,
        },
        video_url: relative_url(timeline_rel, project.source.path.as_str())?,
        probe_url: relative_url(timeline_rel, vidbrief_project::layout::PROJECT_FILE)?,
        curve: CurveBrief {
            step: curve.step().unwrap_or(1.0),
            max_score: curve.max_score(),
            scores: curve.changes.iter().map(|c| c.score).collect(),
        },
        regions: regions
            .regions
            .iter()
            .map(|r| RegionBrief {
                id: r.id,
                start: r.start,
                end: r.end,
                peak_t: r.peak_t,
                peak_score: r.peak_score,
                reason: format!("{:?}", r.reason)
                    .chars()
                    .enumerate()
                    .flat_map(|(i, c)| {
                        if i > 0 && c.is_uppercase() {
                            vec!['_', c.to_ascii_lowercase()]
                        } else {
                            vec![c.to_ascii_lowercase()]
                        }
                    })
                    .collect(),
            })
            .collect(),
        shots: shots
            .shots
            .iter()
            .map(|s| {
                Ok(ShotBrief {
                    id: s.id.to_string(),
                    t: s.t,
                    url: relative_url(timeline_rel, s.file.as_str())?,
                    origin: snake(&format!("{:?}", s.origin)),
                    status: snake(&format!("{:?}", s.status)),
                    region_id: s.region_id,
                    reason: s.reason.clone(),
                    check: s.check.as_ref().map(|c| CheckBrief {
                        r#match: c.r#match,
                        audio_visual_sync: c.audio_visual_sync,
                        type_read: c.type_read.clone(),
                        issues: c.issues.clone(),
                        confidence: snake(&format!("{:?}", c.confidence)),
                    }),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        segments: transcript
            .map(|t| {
                t.segments
                    .iter()
                    .map(|s| SegmentBrief {
                        start: s.start,
                        end: s.end,
                        text: s.effective_text().to_string(),
                        corrected: s.is_corrected(),
                        correction: s.correction.as_ref().map(|c| CorrectionBrief {
                            note: c.note.clone(),
                            reason: c.reason.clone(),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 从 `timeline.html` 所在目录出发,指向项目内另一个文件的**相对 URL**。
///
/// 用相对路径而不是 `http://127.0.0.1:…/项目id/…`:这样 HTML 直接双击打开
/// (或整个项目目录打包发给别人)也还能看到图和字幕,只有视频会因为 `file://`
/// 的限制而播不了 —— 那正是本地查看服务要解决的事。
pub fn relative_url(from_rel: &str, to_rel: &str) -> Result<String> {
    use vidbrief_project::RelPath;
    let from = RelPath::new(from_rel)?;
    let to = RelPath::new(to_rel)?;
    let depth = from.as_str().split('/').count().saturating_sub(1);
    let mut url = "../".repeat(depth);
    url.push_str(to.as_str());
    Ok(url)
}

/// `HighDensity` → `high_density`(结构与 `serde` 的 `snake_case` 一致)。
///
/// 不走 `serde_json::to_value` 再取字符串,是因为枚举值的序列化形态已经是契约的一部分
/// (`docs/schema/README.md` §4),用它自己 Debug 出来的名字转换反而更直观。
fn snake(debug_name: &str) -> String {
    let mut out = String::with_capacity(debug_name.len() + 4);
    for (i, c) in debug_name.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_url_walks_up_from_the_view_location() {
        assert_eq!(
            relative_url("01_preprocess/timeline.html", "source/video.mp4").unwrap(),
            "../source/video.mp4"
        );
        assert_eq!(
            relative_url("01_preprocess/timeline.html", "01_preprocess/shots/s001_0020.0.jpg").unwrap(),
            "../01_preprocess/shots/s001_0020.0.jpg"
        );
        // 放在项目根时不用往上走
        assert_eq!(
            relative_url("timeline.html", "source/video.mp4").unwrap(),
            "source/video.mp4"
        );
        // 校准阶段的视图在第二层
        assert_eq!(
            relative_url("02_calibrate/timeline.html", "source/video.mp4").unwrap(),
            "../source/video.mp4"
        );
    }

    #[test]
    fn relative_url_rejects_escapes() {
        assert!(relative_url("01_preprocess/timeline.html", "../secret").is_err());
        assert!(relative_url("01_preprocess/timeline.html", "/abs/path").is_err());
    }

    #[test]
    fn snake_conversion_matches_the_schema_enum() {
        assert_eq!(snake("HighDensity"), "high_density");
        assert_eq!(snake("IsolatedPeak"), "isolated_peak");
        assert_eq!(snake("LongStaticBreak"), "long_static_break");
        assert_eq!(snake("DenseRegion"), "dense_region");
        assert_eq!(snake("AgentRequest"), "agent_request");
        assert_eq!(snake("Keep"), "keep");
    }
}
