//! 步 1:前处理。**一次调用做完全部产出** —— 无中间态、无中断、不调模型。
//!
//! 设计见 `docs/DESIGN.md` §5;MCP 工具是 B 组的 `preprocess`。
//!
//! # 一条容易搞错的规则:转录失败 ≠ 跳过
//!
//! 转录失败时**不标记步 1 完成**,材料(差帧图与截图)保留、重跑即可。
//! 想只要视觉部分,必须由调用方**显式**说 `transcribe: false` ——
//! "我知道我在跳过"是用户的选择,不该由工具替他把失败说成跳过。
//!
//! 所以本函数返回 `Ok`(带着 `TranscriptOutcome::Failed`)而不是 `Err`:
//! 视觉部分确实成功了,把它丢掉等于让调用方没法报告"哪些做成了"。
//! **判失败是调用方的事**(CLI 用非零退出码,MCP 用 `isError`)。
//!
//! # 增量重跑:输入没变就不重算,更不作废成果
//!
//! 判据在 [`crate::inputs`]:画面与转录**两半分开判**,各自比输入指纹
//! (源视频 sha256 + 参数/模型 + 口径版本),再把材料过一遍**内容底账**
//! (「这东西还在、还是不是工具写出去的那一份」)。全对得上才沿用;
//! 任何一项说不清 —— 没记录、材料不在、读不动、底账对不上 —— 一律重算。
//! 两种结局都要**报出来**(沿用了哪半、没沿用的为什么),`force` 是无条件重跑那条正门。

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{ChangeCurve, DenseRegions, RegionParams, RelPath, Shots, Transcript};

use crate::inputs::{PreprocessInputs, TranscriptInputs, VisualInputs};
use crate::project::OpenProject;

/// 跑的过程中往外报的进度。
///
/// **库里不打印任何东西** —— 由调用方决定说什么、怎么说(与 `vidbrief-core::pipeline` 同一约定)。
#[derive(Debug, Clone, Copy)]
pub enum Stage {
    /// 开始算差帧图(抽帧 + 逐帧评分,长视频上要几秒到几十秒)。
    ///
    /// 与 [`Stage::Analyzed`] 是一对:一个说「开始了」,一个带着结论说「算完了」。
    /// 分开是因为**中间那段没有任何细粒度进度** —— 只有「开始了」能说明它没卡住。
    Analyzing,
    /// 差帧图与密集区算完了(在抽帧之前,所以这几行能先出来)
    Analyzed(AnalysisSummary),
    /// 抽帧进度
    Shot { done: usize, total: usize },
    /// 开始语音转文字。
    ///
    /// **这一步没有细粒度进度可报**(whisper 不给机器可读的百分比),但它是最久的一段:
    /// 不报的话,几分钟里外面看到的和「卡死」没有区别。所以至少报一个「开始了」。
    Transcribing,
    /// 材料都落盘了,开始渲染时间线。
    Rendering,
}

/// 差帧图与密集区的结论摘要。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct AnalysisSummary {
    pub curve_points: usize,
    pub fps: f64,
    pub max_score: f64,
    pub regions: usize,
    pub mass_threshold: f64,
    pub mass_median: f64,
    pub mass_k: f64,
    pub mass_floor: f64,
}

/// 转录的结局。**四种,不要混**。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TranscriptOutcome {
    /// 转成了。
    Done {
        segments: usize,
        model: String,
        language: String,
        path: PathBuf,
    },
    /// 源视频**没有音轨**。这是合法结果,不是失败。
    NoAudio,
    /// 调用方显式跳过(`transcribe: false`)。
    Skipped,
    /// 失败(whisper 用不了、模型缺失……)。**不标记步 1 完成。**
    Failed { error: String },
}

impl TranscriptOutcome {
    /// 这一轮该不该把步 1 标成完成。
    pub fn allows_completion(&self) -> bool {
        !matches!(self, Self::Failed { .. })
    }
}

/// 这一趟**沿用了上次哪些成果**,以及没沿用的原因。
///
/// 为什么要报出来:静默作废别人的活最不该干 —— 反过来**静默复用**也不行。
/// 沿用意味着"这批截图、这份转录还是上次那一份,连看图结论与核验戳一起接着用",
/// 人得看得见这件事、看得见依据(哪几个指纹对上了),才谈得上信不信。
#[derive(Debug, Clone, Serialize)]
pub struct ReuseOutcome {
    /// 画面那半沿用了(没重算、看图结论一条没作废)。
    pub visual: bool,
    /// 转录那半沿用了(没重跑、核验戳一条没作废)。
    pub transcript: bool,
    /// 没沿用画面那半的原因(沿用了就是 `None`)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_why_not: Option<String>,
    /// 没沿用转录那半的原因(沿用了就是 `None`)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_why_not: Option<String>,
    /// 当场重算出来的源视频 sha256(`project.json` 里存的是 init 那一刻的)。
    pub source_sha256: String,
    /// 两者对不上 —— **源文件被就地改过**(硬链接场景下项目里这份跟着变)。
    /// 它同样是"不得复用"的一票。
    pub source_changed: bool,
}

/// [`preprocess`] 的选项。
#[derive(Debug, Clone, Copy)]
pub struct PreprocessOptions {
    /// 采样帧率(每秒几个点)。默认 1.0。
    pub fps: f64,
    /// 参与打分的缩小宽度。默认 480。
    pub width: u32,
    /// 要不要转录。
    pub transcribe: bool,
    /// 要不要抽截图。
    pub shots: bool,
    /// **无条件重跑**:不沿用上次的任何成果(画面重算、转录重跑,旧的照旧作废)。
    ///
    /// 「能省则省」不该变成「没法强制重来」:怀疑上次那批材料有问题、
    /// 或者换了算法想干净地重跑一遍时,走这条。
    pub force: bool,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            fps: vidbrief_project::curve::DEFAULT_FPS_SAMPLED,
            width: vidbrief_project::curve::DEFAULT_WIDTH,
            transcribe: true,
            shots: true,
            force: false,
        }
    }
}

/// 抽帧失败的一张图。**不致命**:几十张里坏一张不该让整轮白跑。
#[derive(Debug, Clone, Serialize)]
pub struct SkippedShot {
    pub t: f64,
    pub reason: String,
}

/// 步 1 跑完的完整交代。
#[derive(Debug, Clone, Serialize)]
pub struct PreprocessReport {
    pub project_id: String,
    pub source: RelPath,
    pub duration: f64,
    pub analysis: AnalysisSummary,
    /// 抽了几张截图;`shots: false` 时为 `None`。
    pub shots: Option<usize>,
    pub shots_skipped: Vec<SkippedShot>,
    pub transcript: TranscriptOutcome,
    /// 重算截图时**作废掉**的看图结论条数(`shots: false` 时为 0)。
    ///
    /// 为什么必须报出来:步 1 重算的是画面,而"这张图我看过"说的是旧的那些帧 ——
    /// 结论跟着作废才是诚实的,但**不能静悄悄作废**。人要知道自己那几轮校准白做了。
    pub shot_checks_cleared: usize,
    /// 重转录时**作废掉**的核验戳与结论段数(`--no-transcribe` 时为 0)。
    pub segment_checks_cleared: usize,
    /// 渲染出来的时间线;渲染失败时为 `None`。
    pub timeline: Option<PathBuf>,
    /// 视图为什么没渲染出来。**材料不受影响**(见 §5.3)。
    pub timeline_error: Option<String>,
    /// 这一趟**沿用了上次哪些成果**(增量重跑),以及没沿用的原因。
    pub reuse: ReuseOutcome,
    /// 跑完时 `project.json` 里 `progress.preprocess.done` 的值。
    pub done: bool,
    pub elapsed_seconds: f64,
}

/// 跑步 1。
///
/// `opened` 必须是**已经打开**的项目 —— 调用方多半要先打印/回报一句"开始处理谁",
/// 那需要 `project.json` 里的东西。顺带省一次重复读取。
pub fn preprocess(
    opened: &mut OpenProject,
    opts: &PreprocessOptions,
    on_progress: &mut dyn FnMut(Stage),
) -> Result<PreprocessReport> {
    if opts.fps <= 0.0 || !opts.fps.is_finite() {
        anyhow::bail!("--fps 必须为正数,实际 {}", opts.fps);
    }
    if opts.width == 0 {
        anyhow::bail!("--width 必须为正数");
    }

    let started = Instant::now();
    let video = opened.require_video()?;
    let layout = opened.layout().clone();
    let meta = opened.meta().clone();
    let duration = meta.duration;

    // ── 输入指纹:当场重算,不读记录 ─────────────────────────────────
    //
    // 为什么每次都要重算源视频 sha256(而不是信任 `project.json` 里 init 时记的那个):
    // 项目里的视频多半是**硬链接**(与源文件共用同一份数据),源文件被人就地改时,
    // 项目里这份跟着变,而 `project.json` 里那栏还是老的 —— 只有重算才发现。
    // 代价是读一遍视频(985 秒的素材 36MB,毫秒级;几 GB 的长视频要几十秒),
    // 换来的是"复用绝不会建立在变了的内容上"。
    let source_sha256 = vidbrief_project::sha256_of(&video).context("重算源视频指纹失败")?;
    let source_changed = source_sha256 != meta.sha256;

    let previous = PreprocessInputs::load_optional(&layout);
    let now_visual = PreprocessInputs::visual_now(&source_sha256, opts.fps, opts.width);
    // 这一趟转录会用哪个模型/语言;模型没装就说不清 —— 那半按"重算"算
    let now_transcript = asr_labels()
        .map(|(model, language)| PreprocessInputs::transcript_now(&source_sha256, &model, &language));

    let visual_why_not = if opts.force {
        Some("`force`:这一趟要求全重跑".to_string())
    } else if !opts.shots {
        Some("这一趟不抽帧(`shots: false`),没有画面产出可沿用".to_string())
    } else {
        why_not_reuse_visual(opened, previous.as_ref(), &now_visual)
    };
    let transcript_why_not = if opts.force {
        Some("`force`:这一趟要求全重跑".to_string())
    } else if !opts.transcribe {
        Some("这一趟不转录(`transcribe: false`),没有转录产出可沿用".to_string())
    } else {
        why_not_reuse_transcript(opened, previous.as_ref(), now_transcript.as_ref())
    };
    let visual_reused = visual_why_not.is_none();
    let transcript_reused = transcript_why_not.is_none();

    // ── 视觉部分:差帧图 → 密集区 → 截图 ────────────────────────────
    let curve_opts = vidbrief_core::CurveOptions {
        fps: opts.fps,
        width: opts.width,
        ..vidbrief_core::CurveOptions::for_video(&meta)
    };
    let params = RegionParams::default();

    let (curve, regions) = if visual_reused {
        // 沿用:判据那一步已经确认过"在、读得动、跟底账对得上"
        (
            ChangeCurve::load(&layout.change_curve()).context("沿用上次的差帧图失败")?,
            DenseRegions::load(&layout.dense_regions()).context("沿用上次的密集区失败")?,
        )
    } else {
        let (curve, regions) = {
            on_progress(Stage::Analyzing);
            vidbrief_core::analyze(&video, &meta, curve_opts, params).context("分析差帧图失败")?
        };
        curve
            .save(&layout.change_curve())
            .context("写入 change_curve.json 失败")?;
        regions
            .save(&layout.dense_regions())
            .context("写入 dense_regions.json 失败")?;
        (curve, regions)
    };

    let analysis = AnalysisSummary {
        curve_points: curve.changes.len(),
        fps: curve_opts.fps,
        max_score: curve.max_score(),
        regions: regions.regions.len(),
        mass_threshold: regions.stats.mass_threshold,
        mass_median: regions.stats.mass_median,
        mass_k: regions.params.mass_k,
        mass_floor: regions.params.mass_floor,
    };
    on_progress(Stage::Analyzed(analysis));

    let planned = vidbrief_core::plan_shots(&regions, &curve);
    let (shot_total, skipped, shot_checks_cleared) = if visual_reused {
        // 沿用上一次的截图:**一张不动**,那批看图结论也一条不作废
        let shots = Shots::load(&layout.shots_json()).context("沿用上次的截图清单失败")?;
        (Some(shots.shots.len()), Vec::new(), 0)
    } else if opts.shots {
        let (shots, skipped) = vidbrief_core::extract_shots(
            &video,
            &layout,
            duration,
            &planned,
            vidbrief_core::ShotQuality::default(),
            &mut |done, total| on_progress(Stage::Shot { done, total }),
        )?;
        // 画面重算了,旧的看图结论说的是旧帧 —— 一并作废
        //(数目报给调用方,见 `PreprocessReport::shot_checks_cleared`)
        let cleared = opened.state().shots.len();
        opened.state_mut().shots.clear();
        crate::materials::save_shots(opened, &shots)?;
        (Some(shots.shots.len()), skipped, cleared)
    } else {
        (None, Vec::new(), 0)
    };

    // ── 转录 ─────────────────────────────────────────────────────────
    let mut segment_checks_cleared = 0;
    let transcript = if transcript_reused {
        // 沿用上一次的转录:一段不重跑,核验戳与纠错结论一条不作废
        let t = Transcript::load(&layout.transcript()).context("沿用上次的转录失败")?;
        TranscriptOutcome::Done {
            segments: t.segments.len(),
            model: t.model.clone(),
            language: t.language.clone(),
            path: layout.transcript(),
        }
    } else if opts.transcribe {
        on_progress(Stage::Transcribing);
        match vidbrief_asr::transcribe_project(&video, &layout, &meta) {
            Ok(Some(t)) => {
                // 重新转录 = 换了一份原文:旧结论与核验戳对的是旧文本,
                // 留着它们就是让门替一份已经换掉的文稿背书
                segment_checks_cleared = opened.state().segments.len();
                opened.state_mut().segments.clear();
                crate::materials::save_transcript_projection(opened, &t)?;
                // 原文换了 → 内容底账跟着换(门的第①条数的就是这份文件里的段)
                crate::materials::note_transcript_content(opened, &t)?;
                TranscriptOutcome::Done {
                    segments: t.segments.len(),
                    model: t.model.clone(),
                    language: t.language.clone(),
                    path: layout.transcript(),
                }
            }
            // 没有音轨是**合法**的空结果,不算失败
            Ok(None) => TranscriptOutcome::NoAudio,
            Err(e) => TranscriptOutcome::Failed {
                error: format!("{e:#}"),
            },
        }
    } else {
        TranscriptOutcome::Skipped
    };

    let done = transcript.allows_completion();
    if done {
        opened.state_mut().progress.preprocess.mark_done();
    }
    // 状态(进度 + 上面那两处作废)总要落一次盘
    opened.save()?;

    // 记下这一趟的输入指纹 —— 下次重跑比的就是它。
    // 规则:**这一趟真产出了什么就记什么**;没做 / 做砸的那半保留上次那条记录
    // (盘上那份成果确实是上次那套输入跑出来的,不能因为这一趟没碰它就改口)。
    let mut record = PreprocessInputs {
        schema_version: crate::inputs::INPUTS_SCHEMA_VERSION,
        visual: None,
        transcript: None,
    };
    record.visual = if opts.shots && (visual_reused || shot_total.is_some()) {
        Some(now_visual.clone())
    } else {
        previous.as_ref().and_then(|p| p.visual.clone())
    };
    record.transcript = if opts.transcribe && matches!(transcript, TranscriptOutcome::Done { .. }) {
        now_transcript.clone()
    } else {
        previous.as_ref().and_then(|p| p.transcript.clone())
    };
    record.save(&layout)?;

    // ── 视图 ─────────────────────────────────────────────────────────
    //
    // DESIGN §5.3:步 1 完成即产出可视化。**渲染失败不算步 1 失败** ——
    // 材料已经全部落盘,视图只是给人看的界面,没渲染出来可以再跑一次。
    on_progress(Stage::Rendering);
    let (timeline, timeline_error) = match vidbrief_viz::render_timeline(&layout) {
        Ok(path) => (Some(path), None),
        Err(e) => (None, Some(format!("{e:#}"))),
    };

    Ok(PreprocessReport {
        project_id: opened.id().to_string(),
        source: meta.path.clone(),
        duration,
        analysis,
        shots: shot_total,
        shots_skipped: skipped
            .into_iter()
            .map(|(t, reason)| SkippedShot { t, reason })
            .collect(),
        transcript,
        shot_checks_cleared,
        segment_checks_cleared,
        timeline,
        timeline_error,
        reuse: ReuseOutcome {
            visual: visual_reused,
            transcript: transcript_reused,
            visual_why_not,
            transcript_why_not,
            source_sha256,
            source_changed,
        },
        done,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
}

/// 这一趟转录会用哪个模型与语言;模型没装(或配置读不出来)就是 `None`。
///
/// 拿不到不算错误:那半本来就跑不成,判据会给一句「说不清 → 重算」。
fn asr_labels() -> Option<(String, String)> {
    let cfg = vidbrief_asr::Config::resolve().ok()?;
    Some((cfg.model_label(), cfg.language.clone()))
}

/// 「没有上次的记录」这句话的完整版。
///
/// 三种情况说法不一样:**文件不在**(第一次跑)、**读不动**(被改过或坏了)、
/// **版本旧**(升级之后的一次性重算)—— 它们的下一步完全不同,合成一句就是误导。
fn no_record_reason(opened: &OpenProject) -> String {
    match PreprocessInputs::unreadable_reason(opened.layout()) {
        Some(why) => why,
        None => format!(
            "没有上次的记录(`{}` 不在 —— 这是第一次跑)",
            vidbrief_project::layout::PREPROCESS_INPUTS_FILE
        ),
    }
}

/// 画面那半能不能沿用上次的成果。`None` = 能;`Some(为什么不能)` = 不能。
///
/// 每一条都要给出**照着能查的说法** —— 「没沿用」和「为什么」得一起说,
/// 不然看的人只知道这趟白算了,不知道怎么才能不白算。
///
/// 判据顺序:先比指纹(便宜、说得出差在哪),再查材料(贵一点、要说清缺了哪一样)。
/// 任何一步说不清都走「不能沿用」那一侧 —— 这是这个函数的全部取向。
fn why_not_reuse_visual(
    opened: &OpenProject,
    previous: Option<&PreprocessInputs>,
    now: &VisualInputs,
) -> Option<String> {
    let Some(previous) = previous else {
        return Some(no_record_reason(opened));
    };
    let Some(before) = previous.visual.as_ref() else {
        return Some("上次没留下画面那半的记录".to_string());
    };
    if before != now {
        return Some(format!(
            "画面的输入变了({})",
            describe_visual_change(before, now)
        ));
    }

    // 材料还在不在、还是不是工具写出去的那一份
    let layout = opened.layout();
    let mut missing = Vec::new();
    for (rel, path) in [
        (
            vidbrief_project::layout::CHANGE_CURVE_FILE,
            layout.change_curve(),
        ),
        (
            vidbrief_project::layout::DENSE_REGIONS_FILE,
            layout.dense_regions(),
        ),
        (vidbrief_project::layout::SHOTS_FILE, layout.shots_json()),
    ] {
        if !path.is_file() {
            missing.push(rel);
        }
    }
    if !missing.is_empty() {
        return Some(format!("上次那几份材料不在了:{}", missing.join("、")));
    }
    let Ok(shots) = Shots::load(&layout.shots_json()) else {
        return Some(format!(
            "截图清单读不动({})",
            vidbrief_project::layout::SHOTS_FILE
        ));
    };
    let gone = shots
        .shots
        .iter()
        .filter(|s| !layout.rel(s.file.as_str()).is_file())
        .count();
    if gone > 0 {
        return Some(format!("截图清单里有 {gone} 张图在盘上找不到"));
    }
    opened
        .state()
        .content_stale(
            vidbrief_project::layout::SHOTS_FILE,
            Some(vidbrief_project::shots_print(&shots)),
        )
        .map(|line| format!("截图清单不能当凭据 —— {line}"))
}

/// 转录那半能不能沿用上次的成果。口径同 [`why_not_reuse_visual`]。
fn why_not_reuse_transcript(
    opened: &OpenProject,
    previous: Option<&PreprocessInputs>,
    now: Option<&TranscriptInputs>,
) -> Option<String> {
    let Some(previous) = previous else {
        return Some(no_record_reason(opened));
    };
    let Some(before) = previous.transcript.as_ref() else {
        return Some("上次没留下转录那半的记录".to_string());
    };
    let Some(now) = now else {
        return Some("这一趟说不清会用哪个模型 / 语言(`vidbrief-asr` 的配置读不出来)".to_string());
    };
    if before != now {
        return Some(format!(
            "转录的输入变了({})",
            describe_transcript_change(before, now)
        ));
    }

    let layout = opened.layout();
    let path = layout.transcript();
    let rel = vidbrief_project::layout::TRANSCRIPT_FILE;
    if !path.is_file() {
        return Some(format!("上次那份转录不在了({rel})"));
    }
    let Ok(transcript) = Transcript::load(&path) else {
        return Some(format!("上次那份转录读不动({rel})"));
    };
    opened
        .state()
        .content_stale(rel, Some(vidbrief_project::transcript_print(&transcript)))
        .map(|line| format!("转录不能当凭据 —— {line}"))
}

/// 两套画面输入差在哪(给人看的说法)。
fn describe_visual_change(before: &VisualInputs, after: &VisualInputs) -> String {
    let mut bits = Vec::new();
    if before.source_sha256 != after.source_sha256 {
        bits.push("源视频内容变了".to_string());
    }
    if before.fps != after.fps {
        bits.push(format!("fps {} → {}", before.fps, after.fps));
    }
    if before.width != after.width {
        bits.push(format!("width {} → {}", before.width, after.width));
    }
    if before.algo != after.algo {
        bits.push(format!("抽帧分析的口径 v{} → v{}", before.algo, after.algo));
    }
    if before.tool_version != after.tool_version {
        bits.push(format!(
            "工具版本 {} → {}",
            before.tool_version, after.tool_version
        ));
    }
    if before.ffmpeg_version != after.ffmpeg_version {
        bits.push(format!("ffmpeg {} → {}", before.ffmpeg_version, after.ffmpeg_version));
    }
    if bits.is_empty() {
        // 两套不相等,却说不出哪里不等 —— 不该发生,但绝不因此说"一样"
        "说不清哪里变了,保守起见按变了算".to_string()
    } else {
        bits.join("、")
    }
}

/// 两套转录输入差在哪(给人看的说法)。
fn describe_transcript_change(before: &TranscriptInputs, after: &TranscriptInputs) -> String {
    let mut bits = Vec::new();
    if before.source_sha256 != after.source_sha256 {
        bits.push("源视频内容变了".to_string());
    }
    if before.model != after.model {
        bits.push(format!("模型 {} → {}", before.model, after.model));
    }
    if before.language != after.language {
        bits.push(format!("语言 {} → {}", before.language, after.language));
    }
    if before.algo != after.algo {
        bits.push(format!("转录的口径 v{} → v{}", before.algo, after.algo));
    }
    if before.tool_version != after.tool_version {
        bits.push(format!(
            "工具版本 {} → {}",
            before.tool_version, after.tool_version
        ));
    }
    if before.whisper_version != after.whisper_version {
        bits.push(format!(
            "whisper {} → {}",
            before.whisper_version, after.whisper_version
        ));
    }
    if bits.is_empty() {
        "说不清哪里变了,保守起见按变了算".to_string()
    } else {
        bits.join("、")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_failure_blocks_completion() {
        assert!(TranscriptOutcome::Done {
            segments: 1,
            model: "m".into(),
            language: "zh".into(),
            path: PathBuf::from("x")
        }
        .allows_completion());
        assert!(TranscriptOutcome::NoAudio.allows_completion());
        assert!(TranscriptOutcome::Skipped.allows_completion());
        assert!(
            !TranscriptOutcome::Failed {
                error: "whisper 不见了".into()
            }
            .allows_completion()
        );
    }

    #[test]
    fn defaults_match_the_design() {
        let o = PreprocessOptions::default();
        assert_eq!(o.fps, 1.0);
        assert_eq!(o.width, 480);
        assert!(o.transcribe && o.shots);
        assert!(!o.force, "默认是「能省则省」,要全重跑得显式说");
    }

    // ── 增量重跑:复用判据 ──────────────────────────────────────────

    /// 夹具的 `video.meta.json` 里记的就是这个 sha。
    fn fixture_sha() -> String {
        "0".repeat(64)
    }

    fn reusable_project(tag: &str) -> crate::testkit::Fake {
        crate::testkit::Fake::new(tag)
            .with_curve(5)
            .with_shot(1, 1.0)
            .with_transcript(&[(0.0, 2.0, "各位好")])
    }

    fn visual_record(fps: f64, width: u32) -> PreprocessInputs {
        PreprocessInputs {
            schema_version: crate::inputs::INPUTS_SCHEMA_VERSION,
            visual: Some(PreprocessInputs::visual_now(&fixture_sha(), fps, width)),
            transcript: None,
        }
    }

    fn transcript_record(model: &str, language: &str) -> PreprocessInputs {
        PreprocessInputs {
            schema_version: crate::inputs::INPUTS_SCHEMA_VERSION,
            visual: None,
            transcript: Some(PreprocessInputs::transcript_now(
                &fixture_sha(),
                model,
                language,
            )),
        }
    }

    /// 一切都对得上 → **可以沿用**。这条是「照做就能过」的那一半:
    /// 每加一条"不能沿用"的规矩,都得配一条能消掉它的路径
    /// (DESIGN §6.1 那条硬规矩:没有出路的规则不许进门)。
    #[test]
    fn reuse_visual_is_allowed_when_everything_lines_up() {
        let fake = reusable_project("reuse-visual-ok");
        let opened = fake.open();
        let previous = visual_record(1.0, 480);
        let now = visual_record(1.0, 480);
        assert_eq!(
            why_not_reuse_visual(&opened, Some(&previous), now.visual.as_ref().unwrap()),
            None
        );
    }

    /// 判据的每一条都要**说得出来为什么** —— 只说「没沿用」等于让人白算一趟还查不出原因。
    #[test]
    fn reuse_visual_says_why_not() {
        let fake = reusable_project("reuse-visual-why");
        let opened = fake.open();
        let record = visual_record(1.0, 480);
        let now = record.visual.as_ref().unwrap();

        // ① 第一次跑:没有记录
        let why = why_not_reuse_visual(&opened, None, now).unwrap();
        assert!(why.contains("没有上次的记录"), "{why}");

        // ② 上次跑过,但没留下画面那半
        let bare = PreprocessInputs {
            schema_version: crate::inputs::INPUTS_SCHEMA_VERSION,
            visual: None,
            transcript: None,
        };
        let why = why_not_reuse_visual(&opened, Some(&bare), now).unwrap();
        assert!(why.contains("没留下画面那半"), "{why}");

        // ③ 参数变了 —— 而且要指名道姓说是哪一项
        let why = why_not_reuse_visual(&opened, Some(&visual_record(2.0, 480)), now).unwrap();
        assert!(why.contains("fps") && why.contains('→'), "{why}");

        // ④ 源视频内容变了(重算出来的 sha 与记录里那份不同)
        let mut other_video = visual_record(1.0, 480);
        if let Some(v) = other_video.visual.as_mut() {
            v.source_sha256 = "f".repeat(64);
        }
        let why = why_not_reuse_visual(&opened, Some(&other_video), now).unwrap();
        assert!(why.contains("源视频内容变了"), "{why}");

        // ⑤ 材料不在了(这里放最后:它把文件删了,后面几条得从头造场景)
        std::fs::remove_file(opened.layout().change_curve()).unwrap();
        let why = why_not_reuse_visual(&opened, Some(&record), now).unwrap();
        assert!(why.contains("不在了"), "{why}");
    }

    /// 材料在、但**不是工具写出去的那一份** → 照样不复用。
    ///
    /// 这是"合理怀疑"落到代码上的地方:复用意味着拿旧成果当自己的,
    /// 那就得先证明这份旧成果没被人动过。
    #[test]
    fn reuse_visual_refuses_when_the_material_is_not_what_the_tools_wrote() {
        let fake = reusable_project("reuse-visual-stale");
        let opened = fake.open();
        let record = visual_record(1.0, 480);
        let now = record.visual.as_ref().unwrap();

        // ① 清单上写着的那张图,盘上没了
        let shot = crate::materials::shots(&opened).unwrap().shots[0].clone();
        let image = opened.layout().rel(shot.file.as_str());
        std::fs::remove_file(&image).unwrap();
        let why = why_not_reuse_visual(&opened, Some(&record), now).unwrap();
        assert!(why.contains("找不到"), "{why}");
        std::fs::write(&image, b"\xff\xd8\xff\xd9").unwrap();

        // ② 清单被人手改过(绕过工具写一遍,底账就落在后头了)
        let mut shots = crate::materials::shots(&opened).unwrap();
        shots.shots[0].t = 9.0;
        shots.save(&opened.layout().shots_json()).unwrap();
        let why = why_not_reuse_visual(&opened, Some(&record), now).unwrap();
        assert!(why.contains("不能当凭据"), "{why}");
    }

    #[test]
    fn reuse_transcript_is_allowed_when_everything_lines_up() {
        let fake = reusable_project("reuse-transcript-ok");
        let opened = fake.open();
        let previous = transcript_record("ggml-small", "zh");
        let now = transcript_record("ggml-small", "zh");
        assert_eq!(
            why_not_reuse_transcript(&opened, Some(&previous), now.transcript.as_ref()),
            None
        );
    }

    #[test]
    fn reuse_transcript_says_why_not() {
        let fake = reusable_project("reuse-transcript-why");
        let opened = fake.open();
        let record = transcript_record("ggml-small", "zh");
        let now = record.transcript.as_ref();

        let why = why_not_reuse_transcript(&opened, None, now).unwrap();
        assert!(why.contains("没有上次的记录"), "{why}");

        let bare = PreprocessInputs {
            schema_version: crate::inputs::INPUTS_SCHEMA_VERSION,
            visual: None,
            transcript: None,
        };
        let why = why_not_reuse_transcript(&opened, Some(&bare), now).unwrap();
        assert!(why.contains("没留下转录那半"), "{why}");

        // 换了模型
        let why =
            why_not_reuse_transcript(&opened, Some(&transcript_record("ggml-medium", "zh")), now)
                .unwrap();
        assert!(why.contains("模型") && why.contains("ggml-small"), "{why}");

        // 这一趟说不清用哪个模型(模型没装)
        let why = why_not_reuse_transcript(&opened, Some(&record), None).unwrap();
        assert!(why.contains("说不清"), "{why}");

        // 转录文件不在了
        std::fs::remove_file(opened.layout().transcript()).unwrap();
        let why = why_not_reuse_transcript(&opened, Some(&record), now).unwrap();
        assert!(why.contains("不在了"), "{why}");
    }
}
