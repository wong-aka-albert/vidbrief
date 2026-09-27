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
///
/// # 结论那一半来自状态,不来自明文
///
/// `shots.json` / `transcript.fixed.json` 里那几栏是**投影**(见
/// [`vidbrief_project::state`]):谁都能手改,所以这里不拿它当判据 ——
/// 读得到状态就以状态为准,读不到才退回投影(视图是"看一眼"的东西,
/// 状态坏了也得能画出来;真话由 `vidbrief status` 说)。
pub fn collect(layout: &Layout, timeline_rel: &str) -> Result<TimelineData> {
    let project = Project::load(&layout.project_json()).context("读 project.json 失败")?;
    let meta = VideoMeta::load(&layout.video_meta()).context("读 video.meta.json 失败")?;
    let curve = ChangeCurve::load(&layout.change_curve())
        .context("读 change_curve.json 失败 —— 先跑 `vidbrief preprocess`")?;
    let regions = DenseRegions::load(&layout.dense_regions()).context("读 dense_regions.json 失败")?;
    let mut shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());
    // 还没有转录是**正常状态**(源视频可能没音轨),不该让视图渲染不出来
    let mut transcript = Transcript::load(&layout.transcript()).ok();

    match vidbrief_project::ProjectState::read_file(&layout.state_file()) {
        Ok(Some(state)) => {
            state.apply_to_shots(&mut shots);
            if let Some(t) = transcript.as_mut() {
                state.apply_to_transcript(t);
            }
        }
        // 状态不在 / 读不动:退回明文投影(它至少是上一次写出去的样子)
        Ok(None) | Err(_) => {}
    }

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
                        correction: s.conclusion.as_ref().map(|c| CorrectionBrief {
                            note: c.note().to_string(),
                            reason: c.reason().to_string(),
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

// ── 校准对照表(DESIGN §9.3)─────────────────────────────────────────

/// 校准对照表要的全部数据。**与时间线同一条规矩:一份自包含的数据,打开就能看。**
///
/// 这一页要替人回答联调提出的四个问题(`docs/devlog/PLAN.md` 近期待办):
///
/// | 问题 | 这一页怎么答 |
/// |------|-------------|
/// | 哪些图是空的 | 缩略图按时刻并排,空的肉眼即见;弃用标记与理由是 agent 写下的 |
/// | 哪几张是同一张幻灯片 | 同上 —— 并排的小图,重复一眼可见;agent 的弃用理由也在这 |
/// | 哪几段转录被动过 | 每段的结论(改了/决定不改/存疑未改)+ 原文与现读法并排 |
/// | 这个结论是哪一轮写的 | **只答一半**:每条结论带 `checked_at`(写下的精确时刻)。轮号没记 ——
///   从前靠页尾内嵌的校准日志原文让人自己对着看,那既要读者自己做对账、
///   又押在 agent 手写的轮次时段上(实测对不上;要答满得让工具在落结论时记轮号) |
///
/// 不做的事:**不取校准日志** —— 它是 agent 的自由文本(给下一轮与审计看),不是页面素材;
/// 页尾贴一份原文,等于让读者拿一段手写时段去对上面每一行的时刻,而信息本该长在被需要的地方。
/// 也不算"哪两张图像"(那是判断,判断归 agent;工具负责让人一眼能判)。
#[derive(Debug, Serialize)]
pub struct CalibrationData {
    pub project: ProjectBrief,
    /// 渲染时刻(给人读)。
    pub generated_at: String,
    /// 已完成的校准轮数。**`None` = 状态文件不在或读不动** ——
    /// 那时结论栏来自明文投影(谁都能改),页面会把这句话亮出来。
    pub rounds: Option<u32>,
    pub shots: Vec<CalibShot>,
    pub segments: Vec<CalibSegment>,
    /// 指向时间线视图的相对链接(那页有视频回跳)。
    pub timeline_url: String,
}

#[derive(Debug, Serialize)]
pub struct CalibShot {
    pub id: String,
    pub t: f64,
    /// 图片的相对 URL。
    pub url: String,
    pub origin: String,
    pub status: String,
    pub reason: Option<String>,
    /// 弃用的图也留着结论 —— 第四轮联调里 agent 就给重复图补了 check 说明。
    pub check: Option<CalibCheck>,
}

#[derive(Debug, Serialize)]
pub struct CalibCheck {
    pub r#match: bool,
    pub audio_visual_sync: bool,
    pub type_read: String,
    /// 下结论时对照的 ±6s 转录(留档的那一份)。
    pub neighbor_text: Option<String>,
    pub issues: Vec<String>,
    pub confidence: String,
    pub evidence: Option<EvidenceBrief>,
    /// 这条结论**是哪一刻写下的**(被后轮改写时,这里跟着变;旧结论在校准日志里)。
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
pub struct EvidenceBrief {
    pub segment: u32,
    pub quote: String,
}

#[derive(Debug, Serialize)]
pub struct CalibSegment {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    /// ASR 原文(**永不就地改写**)。
    pub text: String,
    /// 现在的读法(改过 = 改后的整句)。
    pub effective: String,
    /// `unchecked` / `checked` / `needs_user` —— 与审核门同一个口径([`Segment::review`])。
    pub state: String,
    pub conclusion: Option<ConclusionBrief>,
    pub checked_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConclusionBrief {
    /// 「改了」/「决定不改」/「存疑未改」。
    pub label: &'static str,
    pub kind: Option<String>,
    pub note: String,
    pub reason: String,
}

/// 组装校准对照表的数据。读法与 [`collect`] 同一条规矩:
/// **结论以状态为准,状态读不到才退回明文投影**(那时 `rounds` 是 `None`,页面会明说)。
pub fn collect_calibration(layout: &Layout, view_rel: &str) -> Result<CalibrationData> {
    let project = Project::load(&layout.project_json()).context("读 project.json 失败")?;
    // 时长只是页头一行字;meta 还没跑出来时不该让整个视图渲不出来
    let duration = VideoMeta::load(&layout.video_meta()).map(|m| m.duration).unwrap_or(0.0);
    let mut shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());
    let mut transcript = Transcript::load(&layout.transcript()).ok();

    let rounds = match vidbrief_project::ProjectState::read_file(&layout.state_file()) {
        Ok(Some(state)) => {
            state.apply_to_shots(&mut shots);
            if let Some(t) = transcript.as_mut() {
                state.apply_to_transcript(t);
            }
            Some(state.progress.calibrate.rounds)
        }
        Ok(None) | Err(_) => None,
    };

    Ok(CalibrationData {
        project: ProjectBrief {
            id: project.id.to_string(),
            name: project.name.clone(),
            duration,
        },
        generated_at: vidbrief_project::now().to_string(),
        rounds,
        shots: shots
            .shots
            .iter()
            .map(|s| {
                Ok(CalibShot {
                    id: s.id.to_string(),
                    t: s.t,
                    url: relative_url(view_rel, s.file.as_str())?,
                    origin: snake(&format!("{:?}", s.origin)),
                    status: snake(&format!("{:?}", s.status)),
                    reason: s.reason.clone(),
                    check: s.check.as_ref().map(|c| CalibCheck {
                        r#match: c.r#match,
                        audio_visual_sync: c.audio_visual_sync,
                        type_read: c.type_read.clone(),
                        neighbor_text: c.neighbor_text.clone(),
                        issues: c.issues.clone(),
                        confidence: snake(&format!("{:?}", c.confidence)),
                        evidence: c.evidence.as_ref().map(|e| EvidenceBrief {
                            segment: e.segment,
                            quote: e.quote.clone(),
                        }),
                        checked_at: c.checked_at.to_string(),
                    }),
                })
            })
            .collect::<Result<Vec<_>>>()?,
        segments: transcript
            .map(|t| {
                t.segments
                    .iter()
                    .map(|s| CalibSegment {
                        id: s.id,
                        start: s.start,
                        end: s.end,
                        text: s.text.clone(),
                        effective: s.effective_text().to_string(),
                        state: match s.review() {
                            vidbrief_project::SegmentReview::Unchecked => "unchecked",
                            vidbrief_project::SegmentReview::Checked => "checked",
                            vidbrief_project::SegmentReview::NeedsUser => "needs_user",
                        }
                        .to_string(),
                        conclusion: s.conclusion.as_ref().map(|c| ConclusionBrief {
                            label: c.label(),
                            kind: c.kind().map(|k| snake(&format!("{k:?}"))),
                            note: c.note().to_string(),
                            reason: c.reason().to_string(),
                        }),
                        checked_at: s.checked_at.map(|at| at.to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        timeline_url: relative_url(view_rel, vidbrief_project::layout::PREPROCESS_TIMELINE_FILE)?,
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
