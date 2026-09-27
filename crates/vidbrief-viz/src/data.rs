//! 把项目文件组装成前端要用的那一坨 JSON。
//!
//! **刻意做成「一份自包含的数据」**:页面打开就能看,不依赖任何后端接口。
//! 代价是每轮校准后要重新渲染(见 `docs/DESIGN.md` §6.3),但换来的是
//! 「这个 HTML 发给别人也还能看」—— 除了视频和图片仍然走相对路径。
//!
//! 采样点是**等间隔**的,所以只发分数数组,时刻由前端按 `step` 算出来 ——
//! 986 个点的项目里,这比逐个发 `{t, score}` 省掉一半体积。
//!
//! # 只有一处组装
//!
//! 这里曾经有三个收集器:时间线一份、校准对照表一份、工作台一份。三份页面合并成
//! 「一个页面看全部」之后,前两个就成了同一批材料的第二遍读法 —— 两次
//! [`Shots::load`]、两处「结论以状态为准」的判断,迟早会漂。现在**一次读齐**:
//! 页面上要什么,这儿就给什么,字段名就是页面里读的那个名字。

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{
    ChangeCurve, DenseRegions, Layout, Project, Shots, Transcript, VideoMeta,
};

/// 工作台要的全部数据 —— **一个页面看全部**:共轴那几层(曲线 / 密集区 / 截图 /
/// 转录 / 知识点)加结论那一半(每张图的看图结论、每段转录的结论),再加报告。
///
/// 为什么在**造数据这一侧**合、而不是前端拿几份 payload 自己拼:字段怎么对齐、
/// 哪一份是准的是**数据的形状**,不是页面的自由 —— 在这儿定死一次,前端只管画。
///
/// 不做的事:**不取校准日志** —— 它是 agent 的自由文本(给下一轮与审计看),
/// 不是页面素材。把它贴到页面上,等于让读者拿一段手写的轮次时段去对每一行的时刻,
/// 而信息本该长在被需要的地方(每条结论自己带着 `checked_at`)。
#[derive(Debug, Serialize)]
pub struct WorkbenchData {
    pub project: ProjectBrief,
    /// 视频的**相对 URL**(相对 workbench.html 所在目录)。
    pub video_url: String,
    /// 校准轮数;**`None` = 状态文件不在或读不动**(结论栏来自明文投影,页面要说出来)。
    pub rounds: Option<u32>,
    pub curve: CurveBrief,
    pub regions: Vec<RegionBrief>,
    pub shots: Vec<WorkbenchShot>,
    pub segments: Vec<SegmentBrief>,
    /// 组(核验单位)—— 段是真值,这里只是它的一个视图。
    pub groups: Vec<GroupBrief>,
    /// 报告(步 4 之前为 `None`—— 工作台在步 2 就要能用)。
    pub report: Option<ReportBrief>,
}

/// 页头那行字。
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

/// 轴上那一条密集区。**只发画得出来的两样** —— 峰在哪儿、为什么判成密集区,
/// 是算法侧的判断过程,页面上没有地方显示它。
#[derive(Debug, Serialize)]
pub struct RegionBrief {
    pub start: f64,
    pub end: f64,
}

/// 一张截图:共轴上的一个点 + 抽屉里的一张卡。
#[derive(Debug, Serialize)]
pub struct WorkbenchShot {
    pub id: String,
    pub t: f64,
    /// 图片的相对 URL。
    pub url: String,
    /// 怎么来的(`dense_region` / `isolated_peak` / `agent_request` / `manual`)。
    pub origin: String,
    /// `keep` / `drop`。
    pub status: String,
    /// 弃用理由(弃用的图)或补图理由(补的图)。
    pub reason: Option<String>,
    pub check: Option<CheckBrief>,
}

/// 一次看图核验的结论。
///
/// 下结论时对照的那段 ±6s 转录(`neighbor_text`)**不发** —— 它是留档用的快照,
/// 页面上显示的是现在这句原文(点段芯片能跳过去看)。
#[derive(Debug, Serialize)]
pub struct CheckBrief {
    pub r#match: bool,
    pub audio_visual_sync: bool,
    pub type_read: String,
    pub issues: Vec<String>,
    pub confidence: String,
    pub evidence: Option<EvidenceBrief>,
    /// 这条结论**是哪一刻写下的**(被后轮改写时,这里跟着变)。
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
pub struct EvidenceBrief {
    pub segment: u32,
    pub quote: String,
}

/// 一段转录:原文与**现在的读法**并排给出来。
#[derive(Debug, Serialize)]
pub struct SegmentBrief {
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
    /// 改动的类型(只有「改了」才有)。
    pub kind: Option<String>,
    pub note: String,
    pub reason: String,
}

/// 报告那一段(工作台里要显示的那部分)。
#[derive(Debug, Serialize)]
pub struct ReportBrief {
    /// 口味那句话(`ReportStyle::describe`);旧报告可能没有这一栏。
    pub style: Option<String>,
    pub summary: String,
    pub topics: Vec<vidbrief_project::ReportTopic>,
    pub generated_at: String,
}

/// 读齐一个项目的全部材料,组装成工作台要的数据。
///
/// # 结论那一半来自状态,不来自明文
///
/// `shots.json` / `transcript.fixed.json` 里那几栏是**投影**(见
/// [`vidbrief_project::state`]):谁都能手改,所以这里不拿它当判据 ——
/// 读得到状态就以状态为准,读不到才退回投影(视图是「看一眼」的东西,
/// 状态坏了也得能画出来;真话由 `vidbrief status` 说)。
pub fn collect_workbench(layout: &Layout, view_rel: &str) -> Result<WorkbenchData> {
    let project = Project::load(&layout.project_json()).context("读 project.json 失败")?;
    // 时长只是页头一行字;meta 还没跑出来时不该让整个视图渲不出来
    let duration = VideoMeta::load(&layout.video_meta()).map(|m| m.duration).unwrap_or(0.0);
    let curve = ChangeCurve::load(&layout.change_curve())
        .context("读 change_curve.json 失败 —— 先跑 `vidbrief preprocess`")?;
    let regions = DenseRegions::load(&layout.dense_regions()).context("读 dense_regions.json 失败")?;
    let mut shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());
    // 还没有转录是**正常状态**(源视频可能没音轨),不该让视图渲染不出来
    let mut transcript = Transcript::load(&layout.transcript()).ok();

    // 状态读不动时 `rounds` 是 `None` —— 那就是「结论栏来自明文投影」这句话的开关
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

    Ok(WorkbenchData {
        project: ProjectBrief {
            id: project.id.to_string(),
            name: project.name.clone(),
            duration,
        },
        video_url: relative_url(view_rel, project.source.path.as_str())?,
        rounds,
        curve: CurveBrief {
            step: curve.step().unwrap_or(1.0),
            max_score: curve.max_score(),
            scores: curve.changes.iter().map(|c| c.score).collect(),
        },
        regions: regions
            .regions
            .iter()
            .map(|r| RegionBrief {
                start: r.start,
                end: r.end,
            })
            .collect(),
        shots: shots
            .shots
            .iter()
            .map(|s| {
                Ok(WorkbenchShot {
                    id: s.id.to_string(),
                    t: s.t,
                    url: relative_url(view_rel, s.file.as_str())?,
                    origin: snake(&format!("{:?}", s.origin)),
                    status: snake(&format!("{:?}", s.status)),
                    reason: s.reason.clone(),
                    check: s.check.as_ref().map(|c| CheckBrief {
                        r#match: c.r#match,
                        audio_visual_sync: c.audio_visual_sync,
                        type_read: c.type_read.clone(),
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
            .as_ref()
            .map(|t| {
                t.segments
                    .iter()
                    .map(|s| SegmentBrief {
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
        // 组:一丝不苟地**现算**(§4.5.1)。转录那一栏按它展示 —— 逐段铺出来是
        // 2269 行碎句,而核验单位本来就是组。段仍是真值(`segments` 一个字没少),
        // 这里只给边界 + 拼好的正文 + 组自己的核验状态。
        groups: transcript
            .as_ref()
            .map(|t| {
                t.groups()
                    .into_iter()
                    .map(|g| GroupBrief {
                        id: g.id,
                        from: g.from,
                        to: g.to,
                        start: g.start,
                        end: g.end,
                        text: g.text(t),
                        state: match g.review(t) {
                            vidbrief_project::GroupReview::Unchecked => "unchecked",
                            vidbrief_project::GroupReview::Checked => "checked",
                            vidbrief_project::GroupReview::NeedsUser => "needs_user",
                        }
                        .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        report: load_report_brief(layout)?,
    })
}

/// 一组(一口气说完的一小段)—— 转录那一栏的展示单位。
#[derive(Debug, Serialize)]
pub struct GroupBrief {
    pub id: u32,
    /// 首段段 id(含)。
    pub from: u32,
    /// 末段段 id(含)。
    pub to: u32,
    pub start: f64,
    pub end: f64,
    /// 整组的现读法(各段接起来)。
    pub text: String,
    /// `unchecked` / `checked` / `needs_user`(与门同一口径)。
    pub state: String,
}

/// 报告那一段:没有 `report.json`(还没跑步 4)就是 `None` —— 工作台在步 2 就要能用。
fn load_report_brief(layout: &Layout) -> Result<Option<ReportBrief>> {
    let path = layout.report_json();
    if !path.is_file() {
        return Ok(None);
    }
    let report = vidbrief_project::Report::load(&path).context("读 report.json 失败")?;
    Ok(Some(ReportBrief {
        // 先借后移:三个字段的初始化顺序就是这套借用的顺序。
        style: report.style.as_ref().map(|s| s.describe()),
        generated_at: report.generated_at.to_string(),
        summary: report.summary,
        topics: report.topics,
    }))
}

/// 从视图所在目录出发,指向项目内另一个文件的**相对 URL**。
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
            relative_url("01_preprocess/workbench.html", "source/video.mp4").unwrap(),
            "../source/video.mp4"
        );
        assert_eq!(
            relative_url("01_preprocess/workbench.html", "01_preprocess/shots/s001_0020.0.jpg")
                .unwrap(),
            "../01_preprocess/shots/s001_0020.0.jpg"
        );
        // 放在项目根时不用往上走
        assert_eq!(
            relative_url("views.html", "source/video.mp4").unwrap(),
            "source/video.mp4"
        );
        // 报告页在第二层,与工作台同一档
        assert_eq!(
            relative_url("04_report/report.html", "source/video.mp4").unwrap(),
            "../source/video.mp4"
        );
    }

    #[test]
    fn relative_url_rejects_escapes() {
        assert!(relative_url("01_preprocess/workbench.html", "../secret").is_err());
        assert!(relative_url("01_preprocess/workbench.html", "/abs/path").is_err());
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
