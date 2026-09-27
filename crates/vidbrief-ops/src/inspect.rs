//! 项目体检:四步走到哪了、材料都在不在、源视频还可不可用。
//!
//! 对应 CLI 的 `status` 与 MCP 的 `get_project`。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use vidbrief_project::{
    DenseRegions, Health, Layout, Project, Shots, Transcript, VideoMeta, check_source_link,
    verify_content,
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
    /// **有情况、值得人看一眼**的(把握低 / 报了问题 / 判定图文不符)。
    ///
    /// ⚠️ 这是**视图口径**,不是门口径。它叫 `needs_look` 而不是 `needing_review`,
    /// 就是因为后者在审核门那边指的是另一件事:"**模型自己说它定不了**"
    /// (`confidence: low`)。同名不同义,读的人(尤其模型)必然把它们混成一件 ——
    /// M4 第四轮实测:模型在 `list_shots` 里看到 `needing_review: 7`,以为那 7 张
    /// 就是门要的"待拍板",于是反复重写结论也过不去,最后只能签字放行。
    ///
    /// 两个问题都得问,但答案不一样:**这个**说"这张图有内容,值得看";
    /// **门那个**说"这件事我拿不了主意,得人来"。
    pub needs_look: usize,
}

impl ShotCounts {
    pub fn of(shots: &Shots) -> Self {
        Self {
            total: shots.shots.len(),
            keep: shots.usable().count(),
            unchecked: shots.unchecked().count(),
            needs_look: shots.needing_review().count(),
        }
    }
}

/// 状态文件的情况。
///
/// 三态,和 [`Materials::unreadable`] 同一个道理:把"读不动"显示成"没有",
/// 人就会以为一切正常 —— 而这两件事要人做的事正好相反。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStatus {
    /// 在,而且读得动。
    #[default]
    Ok,
    /// 不在。按"什么都还没核验"算 —— **拦**,但材料照旧能用。
    Missing,
    /// 在,但读不动。**每一件工具都会拒绝开工**(见 `OpenProject`),得先修它。
    Unreadable,
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
    /// 转录里**留下过结论**的段数(改了 + 决定不改 + 存疑)。
    ///
    /// 状态读得动时数的是**状态**(判据),读不动时退回明文投影 —— 那是"上一次写出去的样子",
    /// 对旧格式的项目来说它还是唯一的一份。
    pub concluded_segments: Option<usize>,
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
    /// 报告的**判据**(`04_report/report.json`)在不在。
    ///
    /// M6 联调撞出来的:进度里 ④ 标着完成、而 `04_report/` 下什么都没有 ——
    /// 那时 agent 只能再花一步去 `glob` 确认。判据/投影分开列,一眼看得出来是哪一份不在。
    pub report_json: bool,
    pub report_md: bool,
    pub report_html: bool,
    /// **文件在、读不动**的材料(项目根相对路径)。空 = 都读得动。
    ///
    /// 为什么单列一栏:上面那些计数是 `Option`,`None` 同时表示"还没生成"和"读不动" ——
    /// 而这两件事要人做的事正好相反(前者什么都不用做,后者得处理)。
    /// M4 第五轮实测:一份读坏的候选清单被当成"没有候选",门跟着一路放行。
    pub unreadable: Vec<String>,
    /// 状态文件**读得动 / 不在 / 读不动**。机器看这一栏。
    pub state_status: StateStatus,
    /// 状态文件不在 / 读不动时的说明(正常是 `None`)。
    ///
    /// 状态不在时上面那些计数**取自明文投影** —— 那是"上一次写出去的样子",
    /// 不是判据。门不看它(门读状态),但人得知道自己在看什么。
    pub state_note: Option<String>,
    /// 明文投影与状态**对不上**的地方(人读的一行一条)。空 = 没发现。
    ///
    /// 只报告、不当判据:门读的是状态,手改投影骗不过它。
    /// 这一栏是给"坏了能发现"用的 —— 这也是状态改走非明文之后,
    /// 明文那半边还留着的那点价值。
    pub projection_stale: Vec<String>,
    /// **材料内容**跟内容底账对不上的地方(一行一条)。空 = 对得上。
    ///
    /// 与上一栏是两件事:`projection_stale` 是"投影里那几栏状态被手改了"(无害,门不看它);
    /// 这一栏是"**材料本身**不是工具写出去的那一份了" —— 门会为此拦下来,
    /// 因为门的第①条是从这几份文件里数"还有哪些没核验"的,行被拿掉了活就看不见了。
    pub content_stale: Vec<String>,
}

/// 扫一遍项目目录里有哪些材料。
///
/// **文件不在 → 当"没有"** —— 这里是盘点,不是校验;想校验走 [`inspect`] 的 `--verify`。
///
/// **文件在、读不动 → 记进 `unreadable`。** 那不是"没有":盘点是给人看的,
/// 把"坏了"显示成"零"会让人以为一切正常(实测就是这么放行过一道门的)。
pub fn scan_materials(layout: &Layout) -> Materials {
    let mut unreadable = Vec::new();
    let root = layout.root();
    let curve = load_or_note(root, &layout.change_curve(), &mut unreadable, vidbrief_project::ChangeCurve::load);
    let regions = load_or_note(
        root,
        &layout.dense_regions(),
        &mut unreadable,
        DenseRegions::load,
    );
    let mut shots = load_or_note(root, &layout.shots_json(), &mut unreadable, Shots::load);
    let mut transcript = load_or_note(root, &layout.transcript(), &mut unreadable, Transcript::load);
    let fixed = load_or_note(
        root,
        &layout.transcript_fixed(),
        &mut unreadable,
        Transcript::load,
    );
    let mut supplement = load_or_note(
        root,
        &layout.supplement_requests(),
        &mut unreadable,
        vidbrief_project::SupplementRequests::load,
    );

    // 手改投影的判据:先留一份"没盖过状态"的
    let plain_shots = shots.clone();
    let plain_fixed = fixed.clone();
    let plain_supplement = supplement.clone();

    // ── 状态 ────────────────────────────────────────────────────────
    //
    // 三态,**一个都不许混**:读得动 → 盖到材料上;不在 / 读不动 → 退回明文投影算数,
    // 但要把这件事记进 `state_note` 说给人听。门不走这条路(见 `OpenProject`),
    // 所以这里松一点不会放行任何东西 —— 它只是"报告"。
    let (state, state_status, state_note) = match vidbrief_project::ProjectState::read_file(&layout.state_file()) {
        Ok(Some(state)) => {
            if let Some(shots) = shots.as_mut() {
                state.apply_to_shots(shots);
            }
            if let Some(transcript) = transcript.as_mut() {
                state.apply_to_transcript(transcript);
            }
            if let Some(list) = supplement.as_mut() {
                state.apply_to_supplements(list);
            }
            (Some(state), StateStatus::Ok, None)
        }
        Ok(None) => {
            // 旧格式(v1)的项目:状态当年就写在明文里,还没搬进 state.bin ——
            // 那不是"记录不见了",是"还没迁"。两句话要分清楚,人要做的也不是同一件事。
            let legacy = vidbrief_project::Project::peek_version(&layout.project_json())
                .map(|v| v < vidbrief_project::SCHEMA_VERSION)
                .unwrap_or(false);
            let note = if legacy {
                format!(
                    "这是旧格式(v1)的项目:状态还在明文里。跑一次 `vidbrief upgrade {}` 就把它收进状态文件 —— 下面这些计数是照明文投影算的。",
                    layout.root().display()
                )
            } else {
                format!(
                    "状态文件不在({})—— 下面这些计数取自明文投影,不是判据;门按「都还没核验」算",
                    layout.state_file().display()
                )
            };
            (None, StateStatus::Missing, Some(note))
        }
        Err(e) => (None, StateStatus::Unreadable, Some(format!("{e:#}"))),
    };

    // **材料内容**跟底账对不上的地方(门会为此拦)。
    //
    // 判断收在 `materials::content_mismatches_of` 一处,门与这里共用 ——
    // 两边各写一份的话,"体检说没事、门却拦着"这种自相矛盾迟早出现。
    // 手里已经有这三份材料了(上面刚读过),所以走"递进来"那个版本,不再读一遍文件。
    let content_stale = match &state {
        Some(state) => crate::materials::content_mismatches_of(
            state,
            transcript.as_ref(),
            shots.as_ref(),
            supplement.as_ref(),
        ),
        None => Vec::new(),
    };

    // 明文投影跟状态对不上的地方。
    //
    // **要拿"没盖过状态"的那几份来比** —— 上面刚把状态盖进去,拿盖过的比等于自己跟自己比,
    // 那份手改永远发现不了(这个坑当场踩过一次:改完 18 张图,`status` 一声没吭)。
    let projection_stale = match &state {
        Some(state) => state.mismatches(
            plain_shots.as_ref(),
            plain_fixed.as_ref(),
            plain_supplement.as_ref(),
        ),
        None => Vec::new(),
    };

    Materials {
        curve_points: curve.as_ref().map(|c| c.changes.len()),
        regions: regions.as_ref().map(|r| r.regions.len()),
        shots: shots.as_ref().map(ShotCounts::of),
        transcript_segments: transcript.as_ref().map(|t| t.segments.len()),
        concluded_segments: match &state {
            // 状态读得动:以它为准(明文那份可能被人手改过)
            Some(_) => transcript.as_ref().map(|t| t.stats().touched()),
            // 状态不在 / 读不动:退回明文投影 —— 旧格式项目的结论本来就只在那儿
            None => fixed.as_ref().map(|t| t.stats().touched()),
        },
        supplement_total: supplement.as_ref().map(|s| s.requests.len()),
        supplement_pending: supplement.as_ref().map(|s| s.pending().count()),
        timeline_html: layout.preprocess_timeline().is_file(),
        calibrate_timeline_html: layout.calibrate_timeline().is_file(),
        cross_validate: layout.cross_validate().is_file(),
        calibration_log: layout.calibration_log().is_file(),
        summary_md: layout.summary_md().is_file(),
        // 报告的**判据**是 report.json;那两份是它的投影,可能单独不在
        report_json: layout.report_json().is_file(),
        report_md: layout.report_md().is_file(),
        report_html: layout.report_html().is_file(),
        unreadable,
        state_status,
        state_note,
        projection_stale,
        content_stale,
    }
}

/// 绝对路径 → 项目根相对路径(给人看的那一份)。
fn rel_name(root: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// 读一份材料:不在 → `None`;在、读得动 → `Some`;在、读不动 → 记进 `unreadable` 并 `None`。
fn load_or_note<T>(
    root: &std::path::Path,
    path: &std::path::Path,
    unreadable: &mut Vec<String>,
    load: impl FnOnce(&std::path::Path) -> anyhow::Result<T>,
) -> Option<T> {
    if !path.is_file() {
        return None;
    }
    match load(path) {
        Ok(v) => Some(v),
        Err(_) => {
            unreadable.push(rel_name(root, path));
            None
        }
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

/// 一个**外挂**(子进程)现在什么样:找不找得到、在哪、什么版本。
///
/// 为什么放进体检:环境齐不齐,从前只有跑到 `init` / `preprocess` 才知道 ——
/// 而 agent 进场的第一件事就是 `status`,那时候就该看见。
/// "没装"与"装了但版本不对"是两条不同的修法,所以这里两样分开说。
#[derive(Debug, Clone, Serialize)]
pub struct ToolView {
    pub name: &'static str,
    /// 认哪个环境变量(要换一个就跑它)。
    pub env: &'static str,
    /// 实际会跑的那个:环境变量指的路径,或 `PATH` 上的名字。
    pub path: String,
    /// `--version` 的那一行;`None` = **跑不起来**(没装 / 路径不对)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 额外的实话(whisper 那栏用它说模型找到了没有)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// 三个外挂现在什么样。**顺序固定**(ffmpeg / ffprobe / whisper),输出好对照。
///
/// 代价是三个子进程一起问一次 `--version`(实测合计不到 200ms)——
/// 换来的是"环境不对当场看见",而不是跑五分钟在转录那一步才炸。
fn external_tools() -> Vec<ToolView> {
    let ffmpeg_bin = vidbrief_core::ffmpeg::ffmpeg_bin();
    let ffprobe_bin = vidbrief_core::ffmpeg::ffprobe_bin();
    let model = vidbrief_asr::whisper::model_display();
    vec![
        ToolView {
            name: "ffmpeg",
            env: "VIDBRIEF_FFMPEG",
            path: ffmpeg_bin.to_string_lossy().into_owned(),
            version: vidbrief_core::ffmpeg::ffmpeg_version(),
            note: None,
        },
        ToolView {
            name: "ffprobe",
            env: "VIDBRIEF_FFPROBE",
            path: ffprobe_bin.to_string_lossy().into_owned(),
            version: vidbrief_core::ffmpeg::ffprobe_version_opt(),
            note: None,
        },
        ToolView {
            name: "whisper",
            env: "VIDBRIEF_WHISPER_BIN",
            path: vidbrief_asr::whisper::bin_display(),
            version: vidbrief_asr::whisper::version(),
            note: Some(match &model {
                Some(m) => format!("模型 {m}"),
                None => "模型没找到 —— 设 VIDBRIEF_WHISPER_MODEL,或把它放进 tools/whisper/models/"
                    .to_string(),
            }),
        },
    ]
}

/// 一次体检的完整结论。
#[derive(Debug, Clone, Serialize)]
pub struct InspectReport {
    pub project: Project,
    pub dir: PathBuf,
    pub meta: VideoMeta,
    /// `project.json` 里**读到**的版本号。
    ///
    /// 不是 [`vidbrief_project::SCHEMA_VERSION`] —— 旧项目要如实说自己是旧项目,
    /// 否则人看到"v2"会以为迁过了(实测:一句假话能让整条排查走反)。
    pub schema_version: u32,
    /// 修订号 —— 来自状态文件(读不动时是 0,原因在 `materials.state_note` 里)。
    pub rev: u64,
    /// 四步进度 —— 同样来自状态文件。
    pub progress: vidbrief_project::Progress,
    /// 按 `source.link` 分别检查(见 `docs/DESIGN.md` §4.8)。
    pub link_health: HealthView,
    /// 只有 `verify` 时才有的内容哈希比对。
    pub content_health: Option<HealthView>,
    pub materials: Materials,
    /// 三个外挂(ffmpeg / ffprobe / whisper)现在什么样 —— 见 [`ToolView`]。
    pub tools: Vec<ToolView>,
}

impl InspectReport {
    /// 有硬伤时为真 —— CLI 用它定退出码,MCP 用它定 `isError`。
    ///
    /// 状态文件**读不动**算硬伤:这个项目现在什么都干不了,得先修它。
    /// "不在"不算 —— 那按"什么都还没核验"算,工具照常能用(见 [`StateStatus`])。
    pub fn unhealthy(&self) -> bool {
        self.link_health.is_problem()
            || self.materials.state_status == StateStatus::Unreadable
            || self
                .content_health
                .as_ref()
                .is_some_and(HealthView::is_problem)
    }
}

/// 体检一个项目。
///
/// **状态读不动也要能体检** —— 这是诊断工具,坏了的时候正是它最该说话的时候。
/// 所以这条路**不走** [`OpenProject`](crate::project::OpenProject)(它在状态读不动时
/// 会直接拒绝打开),而是自己按层读:身份、元数据、状态、材料各自报各自的。
pub fn inspect(arg: &str, opts: &InspectOptions) -> Result<InspectReport> {
    let layout = crate::project::resolve(arg)?;
    // 旧格式也要能体检 —— "换个版本的工具打开"与"这份文件坏了"是两条路,
    // 而人要判断走哪条,恰恰得先看见项目里现在有什么(材料在不在、跑到哪一步了)。
    //
    // 旧项目连进度都不在状态里(它们那会儿写在 `project.json` 里),所以那两栏
    // 也从旧文件里读 —— 报成"0 轮、未做"等于把人的活说没了。
    let (project, legacy) = match crate::project::load_identity(&layout) {
        Ok(p) => (p, Option::<vidbrief_project::LegacyStateV1>::None),
        Err(e) => match crate::project::load_legacy(&layout) {
            Some((project, legacy)) => (project, Some(legacy)),
            None => return Err(e),
        },
    };
    let meta = VideoMeta::load(&layout.video_meta()).with_context(|| {
        format!(
            "读取视频元数据失败:{} —— 项目可能没建完",
            layout.video_meta().display()
        )
    })?;
    let (rev, progress) = match (&legacy, crate::project::load_state(&layout)) {
        // 旧格式:那两栏还在明文里,照实读出来
        (Some(legacy), _) => (legacy.rev, legacy.progress.clone()),
        (None, Ok((state, _))) => (state.rev, state.progress.clone()),
        // 读不动 / 不在:修订号和进度都无从谈起,原因由 `scan_materials` 写进 state_note
        (None, Err(_)) => (0, vidbrief_project::Progress::default()),
    };
    let schema_version = Project::peek_version(&layout.project_json()).unwrap_or(0);

    let link_health = HealthView::from(check_source_link(&project, &layout));
    let content_health = if opts.verify {
        Some(HealthView::from(verify_content(&project, &meta, &layout)?))
    } else {
        None
    };

    Ok(InspectReport {
        project,
        dir: layout.root().to_path_buf(),
        schema_version,
        rev,
        progress,
        meta,
        link_health,
        content_health,
        materials: scan_materials(&layout),
        tools: external_tools(),
    })
}

/// 已经打开的项目直接体检(少读一遍 `project.json`)。
pub fn inspect_opened(opened: &OpenProject, opts: &InspectOptions) -> Result<InspectReport> {
    let layout = opened.layout();
    let rev = opened.state().rev;
    let progress = opened.state().progress.clone();
    let schema_version = vidbrief_project::SCHEMA_VERSION;
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
        schema_version,
        rev,
        progress,
        meta: opened.meta().clone(),
        link_health: HealthView::from(link_health),
        content_health,
        materials: scan_materials(layout),
        tools: external_tools(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个外挂都要在名单里,顺序固定 —— 这栏是人对着看的,少一行就少一条线索。
    #[test]
    fn the_tool_list_covers_the_three_externals() {
        let tools = external_tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        assert_eq!(names, ["ffmpeg", "ffprobe", "whisper"]);
        for tool in &tools {
            assert!(
                tool.env.starts_with("VIDBRIEF_"),
                "每一栏都要说清认哪个环境变量:{tool:?}"
            );
            assert!(!tool.path.is_empty(), "路径那一栏不能空:{tool:?}");
        }
        // whisper 那栏还要说模型 —— 那是这一栏最容易漏说的半件事
        let whisper = tools.iter().find(|t| t.name == "whisper").unwrap();
        assert!(whisper.note.is_some(), "{whisper:?}");
    }

    #[test]
    fn health_view_keeps_the_level() {        let ok = HealthView::from(Health::Ok("好".into()));
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
        // 材料全空、没有坏文件、也没有对不上的投影
        let mut expected = Materials::default();
        expected.state_status = m.state_status;
        expected.state_note = m.state_note.clone();
        assert_eq!(m, expected);
        assert_eq!(m.state_status, StateStatus::Missing);
        assert!(
            m.state_note.as_deref().unwrap_or("").contains("状态文件不在"),
            "状态不在要说出来:{:?}",
            m.state_note
        );

        // 真项目里状态是**建出来就有**的:立起来之后就不该再有那句话
        vidbrief_project::ProjectState::new()
            .write(&layout.state_file())
            .unwrap();
        let m = scan_materials(&layout);
        assert_eq!(m.state_note, None);
        assert_eq!(m, Materials::default());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
