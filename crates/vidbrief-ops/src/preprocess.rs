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

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{RegionParams, RelPath};

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
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            fps: vidbrief_project::curve::DEFAULT_FPS_SAMPLED,
            width: vidbrief_project::curve::DEFAULT_WIDTH,
            transcribe: true,
            shots: true,
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

    // ── 视觉部分:差帧图 → 密集区 → 截图 ────────────────────────────
    let curve_opts = vidbrief_core::CurveOptions {
        fps: opts.fps,
        width: opts.width,
        ..vidbrief_core::CurveOptions::for_video(&meta)
    };
    let params = RegionParams::default();

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
    let (shot_total, skipped, shot_checks_cleared) = if opts.shots {
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
    let transcript = if opts.transcribe {
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
        done,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    })
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
    }
}
