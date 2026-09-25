//! 存结论:D 组工具(`record_shot_check` / `apply_corrections` / `write_note` /
//! `write_summary` / `write_report_md`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具不判断对错,只搬运与保存。**
//! 这里是"agent 的结论落成文件"的唯一入口。

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use vidbrief_project::{
    Confidence, Correction, CorrectionType, RelPath, ShotCheck, Shots, Transcript,
};

use crate::project::OpenProject;

// ── 截图结论 ─────────────────────────────────────────────────────────

/// `record_shot_check` 的入参。
#[derive(Debug, Clone)]
pub struct ShotCheckInput {
    /// 画面内容与同期转录是否相符。
    pub r#match: bool,
    /// 画面与语音在时间上是否对得上。
    pub audio_visual_sync: bool,
    /// 从画面读到的内容。**画面无法确认时如实写「画面无法确认」**,不要顺着转录编。
    pub type_read: String,
    /// 发现的问题,每条一句。
    pub issues: Vec<String>,
    pub confidence: Confidence,
    /// 该时刻前后 ±6s 的转录原文。`None` = 由工具自动填(agent 手上本来就有,省一次搬运)。
    pub neighbor_text: Option<String>,
}

/// 把一张图的判断结论写回 `shots.json`。
///
/// 写回的是**全量**替换:agent 复核过一遍再写一次,应以最后一次为准。
pub fn record_shot_check(
    opened: &OpenProject,
    id: vidbrief_project::ShotId,
    input: &ShotCheckInput,
) -> Result<ShotCheck> {
    let path = opened.layout().shots_json();
    let mut shots = Shots::load(&path).context("读取 shots.json 失败")?;
    let shot = shots
        .get(id)
        .with_context(|| format!("项目里没有截图 {id}"))?
        .clone();

    let neighbor_text = match &input.neighbor_text {
        Some(text) => Some(text.clone()),
        None => crate::materials::effective_transcript(opened)
            .ok()
            .map(|(t, _, _)| {
                t.render_range(
                    (shot.t - crate::NEIGHBOR_WINDOW).max(0.0),
                    shot.t + crate::NEIGHBOR_WINDOW,
                )
            })
            .filter(|s| !s.is_empty()),
    };

    let check = ShotCheck {
        r#match: input.r#match,
        audio_visual_sync: input.audio_visual_sync,
        type_read: input.type_read.clone(),
        neighbor_text,
        issues: input.issues.clone(),
        confidence: input.confidence,
        checked_at: vidbrief_project::now(),
    };

    shots
        .get_mut(id)
        .expect("上面刚查到过,这里必然还在")
        .check = Some(check.clone());
    shots.save(&path).context("写入 shots.json 失败")?;
    Ok(check)
}

// ── 转录纠错 ─────────────────────────────────────────────────────────

/// `apply_corrections` 的一条。
#[derive(Debug, Clone)]
pub struct CorrectionInput {
    /// 转录段 id(`transcript.json` 里的序号)。
    pub segment_id: u32,
    /// 改后的整句。**`None` = 不改**(标存疑或只留个说明)。
    pub corrected_text: Option<String>,
    pub kind: CorrectionType,
    /// 改动内容,如 `派森 → Python`。不给就按前后缀差异自动掐出来;
    /// `corrected_text` 为 `None` 时**必须给**,否则这条记录没有任何信息量。
    pub note: Option<String>,
    /// 判断依据(画面文字 / 上下文 / 术语表)。
    pub reason: String,
}

/// 纠错的落盘结果。
#[derive(Debug, Clone, Serialize)]
pub struct CorrectionOutcome {
    /// 全量纠错稿的位置。
    pub fixed_path: PathBuf,
    /// 给人核对的对照表。
    pub diff_path: PathBuf,
    /// 全稿段数。
    pub total_segments: usize,
    /// 本次写了几条。
    pub applied: usize,
    /// 全稿里被真改过的段数(**累计**,不是本次)。
    pub changed: usize,
    /// 全稿里"存疑但未改"的段数(累计)。
    pub uncertain: usize,
}

/// 把 agent 的纠错写进 `02_calibrate/transcript.fixed.json`,并刷新对照表。
///
/// **原始转录永不就地改写**(`docs/schema/README.md`)。本函数读原文、写副本 ——
/// 这样"agent 到底改了什么"永远可查;就地改的话原文就消失了,核对纠错是否合理就没了依据。
///
/// 可以反复调用:每次都在**现有纠错稿**上继续改,所以 agent 可以分批发现、分批写回。
pub fn apply_corrections(
    opened: &OpenProject,
    items: &[CorrectionInput],
) -> Result<CorrectionOutcome> {
    if items.is_empty() {
        bail!("没有要写的纠错条目");
    }
    let layout = opened.layout();
    let raw = Transcript::load(&layout.transcript()).with_context(|| {
        format!(
            "读取原始转录失败:{} —— 步 1 可能还没做转录",
            layout.transcript().display()
        )
    })?;

    // 已经有纠错稿就在它上面继续改,不要每次从原文重来(否则上一批的结论会被抹掉)
    let mut fixed = if layout.transcript_fixed().is_file() {
        Transcript::load(&layout.transcript_fixed()).context("读取已有的纠错稿失败")?
    } else {
        raw.clone()
    };

    // 先整体校验一遍再动手:半路失败会留下"改了一半"的文件,比整批拒绝更难收拾
    let mut unknown = Vec::new();
    for item in items {
        if !fixed.segments.iter().any(|s| s.id == item.segment_id) {
            unknown.push(item.segment_id);
        }
    }
    if !unknown.is_empty() {
        bail!(
            "这些转录段不存在:{unknown:?}(共 {} 段,id 从 1 起)",
            fixed.segments.len()
        );
    }

    for item in items {
        let index = fixed
            .segments
            .iter()
            .position(|s| s.id == item.segment_id)
            .expect("上面刚校验过");
        let original = fixed.segments[index].text.clone();
        let corrected = item.corrected_text.clone();

        let note = match (&item.note, &corrected) {
            (Some(note), _) => note.clone(),
            (None, Some(new)) => changed_span(&original, new),
            (None, None) => bail!(
                "第 {} 段标了「不改」却没给 note —— 存疑结论必须写清疑似写法,否则这条记录没有信息量",
                item.segment_id
            ),
        };

        fixed.segments[index].corrected_text = corrected;
        fixed.segments[index].correction = Some(Correction {
            kind: item.kind,
            note,
            reason: item.reason.clone(),
        });
    }

    fixed
        .save(&layout.transcript_fixed())
        .context("写入 transcript.fixed.json 失败")?;
    let diff = render_diff(&raw, &fixed);
    vidbrief_project::write_atomic_str(&layout.transcript_diff(), &diff)
        .context("写入 transcript.diff.md 失败")?;

    Ok(CorrectionOutcome {
        fixed_path: layout.transcript_fixed(),
        diff_path: layout.transcript_diff(),
        total_segments: fixed.segments.len(),
        applied: items.len(),
        changed: fixed.segments.iter().filter(|s| s.is_corrected()).count(),
        uncertain: fixed.segments.iter().filter(|s| s.is_uncertain()).count(),
    })
}

/// 掐掉公共前后缀,只留真正改动的那一小段,给 `Correction::note` 用。
///
/// 为什么不用裸切片:中文一个字 3 字节,按字节切会 panic(`AGENTS.md` §1.1)。
/// 这里先在 `Vec<char>` 上定位,再按字符收集 —— 全程不碰字节下标。
fn changed_span(old: &str, new: &str) -> String {
    let a: Vec<char> = old.chars().collect();
    let b: Vec<char> = new.chars().collect();

    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head && tail < b.len() - head && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }

    let a_mid: String = a[head..a.len() - tail].iter().collect();
    let b_mid: String = b[head..b.len() - tail].iter().collect();

    if a_mid.is_empty() && b_mid.is_empty() {
        return "整句重写".to_string();
    }
    format!("{} → {}", shorten(&a_mid), shorten(&b_mid))
}

/// 太长的改动片段在对照表里只会把表格撑爆。
fn shorten(text: &str) -> String {
    const LIMIT: usize = 24;
    let mut out: String = text.chars().take(LIMIT).collect();
    if text.chars().count() > LIMIT {
        out.push('…');
    }
    if out.is_empty() {
        out.push('∅'); // 纯新增/纯删除的那一侧
    }
    out
}

/// 人可核对的对照表。
fn render_diff(raw: &Transcript, fixed: &Transcript) -> String {
    let mut out = String::new();
    out.push_str("# 转录纠错对照\n\n");
    out.push_str(&format!(
        "- 原始转录:`{}`(**永不就地改写**)\n",
        vidbrief_project::layout::TRANSCRIPT_FILE
    ));
    out.push_str(&format!(
        "- 本稿:`{}`\n",
        vidbrief_project::layout::TRANSCRIPT_FIXED_FILE
    ));
    out.push_str(&format!("- 生成时间:{}\n", vidbrief_project::now()));

    let changed = fixed.segments.iter().filter(|s| s.is_corrected()).count();
    let uncertain = fixed.segments.iter().filter(|s| s.is_uncertain()).count();
    let touched = fixed
        .segments
        .iter()
        .filter(|s| s.correction.is_some())
        .count();
    out.push_str(&format!(
        "- 全稿 {} 段:改过 {changed} 段,存疑未改 {uncertain} 段,合计动过 {touched} 段\n\n",
        fixed.segments.len()
    ));

    if touched == 0 {
        out.push_str("还没有任何纠错。\n");
        return out;
    }

    out.push_str("| # | 时刻 | 原文 | 改后 | 类型 | 说明 | 依据 |\n");
    out.push_str("|---|------|------|------|------|------|------|\n");
    for seg in &fixed.segments {
        let Some(correction) = &seg.correction else {
            continue;
        };
        let original = raw
            .segments
            .iter()
            .find(|s| s.id == seg.id)
            .map(|s| s.text.as_str())
            .unwrap_or(&seg.text);
        let after = match &seg.corrected_text {
            Some(text) => cell(text),
            None => "(未改,存疑)".to_string(),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            seg.id,
            clock(seg.start),
            cell(original),
            after,
            kind_name(correction.kind),
            cell(&correction.note),
            cell(&correction.reason),
        ));
    }
    out
}

/// Markdown 表格单元格:竖线会把列拆掉,换行会把行拆掉,都得先拍平。
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn kind_name(kind: CorrectionType) -> &'static str {
    match kind {
        CorrectionType::Homophone => "同音字",
        CorrectionType::Terminology => "术语",
        CorrectionType::Number => "数字",
        CorrectionType::Uncertain => "存疑",
        CorrectionType::Other => "其它",
    }
}

/// `mm:ss.s`,与视图上的时间轴一致。
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0);
    let minutes = (total / 60.0).floor();
    let rest = total - minutes * 60.0;
    format!("{minutes:02.0}:{rest:04.1}")
}

// ── 落盘 ─────────────────────────────────────────────────────────────

/// 写入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// 覆盖(整篇重写)。
    Overwrite,
    /// 追加(日志类文件:每轮往后面接一段,不是把上一轮抹掉)。
    Append,
}

/// 写一份 Markdown 到项目里(`write_note`)。
///
/// `rel` 必须是项目根相对路径且落在允许的目录里 —— 路径校验有两道:
/// [`RelPath`] 保证不越出项目根(挡目录穿越),这里再限一层"只能写哪几个目录",
/// 免得 agent 顺手把 `project.json` 覆盖掉。
pub fn write_note(
    opened: &mut OpenProject,
    rel: &RelPath,
    md: &str,
    mode: WriteMode,
    count_round: bool,
) -> Result<PathBuf> {
    let allowed = [
        vidbrief_project::layout::CALIBRATE_DIR,
        vidbrief_project::layout::SUMMARY_DIR,
    ];
    let path = rel.as_str();
    if !allowed.iter().any(|dir| path.starts_with(dir)) {
        bail!(
            "只能写在 {allowed:?} 下面,不允许写 `{path}` —— 材料文件有自己的工具,别绕过它们"
        );
    }
    if !path.ends_with(".md") {
        bail!("只写 Markdown(`.md`),不允许写 `{path}`");
    }

    let full = opened.layout().rel(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("建目录失败:{}", parent.display()))?;
    }

    match mode {
        WriteMode::Overwrite => {
            vidbrief_project::write_atomic_str(&full, md)
                .with_context(|| format!("写入失败:{}", full.display()))?;
        }
        WriteMode::Append => {
            let mut text = String::new();
            if full.is_file() {
                let existing = std::fs::read_to_string(&full)
                    .with_context(|| format!("读取失败:{}", full.display()))?;
                if !existing.is_empty() && !existing.ends_with('\n') {
                    text.push('\n');
                }
                if !existing.is_empty() {
                    text.push('\n'); // 两段之间空一行,免得粘成一坨
                }
            }
            text.push_str(md);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full)
                .with_context(|| format!("打开失败:{}", full.display()))?;
            file.write_all(text.as_bytes())
                .with_context(|| format!("追加失败:{}", full.display()))?;
        }
    }

    // "一轮校准"的落点就是 `calibration_log.md`(`docs/DESIGN.md` §6.3),
    // 但**由调用方显式声明**这一次算一轮 —— 写个笔记不等于校准了一轮
    if count_round {
        opened.project_mut().progress.calibrate.bump();
        opened.save()?;
    }

    Ok(full)
}

/// 写 `03_summary/summary.md` 并标记步 3 完成。
pub fn write_summary(opened: &mut OpenProject, md: &str) -> Result<PathBuf> {
    let path = opened.layout().summary_md();
    vidbrief_project::write_atomic_str(&path, md).context("写入 summary.md 失败")?;
    opened.project_mut().progress.summarize.mark_done();
    opened.save()?;
    Ok(path)
}

/// 写 `04_report/report.md` 并标记步 4 完成。
///
/// 只落 Markdown;**图表化 HTML 是 `build_report`**(M6,`vidbrief-report`)。
/// 这里就把步 4 标成完成,是因为 `report.md` 才是这一稿的实体,
/// HTML 只是同一稿的另一种渲染 —— 渲染不出来不该让"报告写好了"变成假话。
pub fn write_report_md(opened: &mut OpenProject, md: &str) -> Result<PathBuf> {
    let path = opened.layout().report_md();
    vidbrief_project::write_atomic_str(&path, md).context("写入 report.md 失败")?;
    opened.project_mut().progress.report.mark_done();
    opened.save()?;
    Ok(path)
}

// ── 给测试用的小工具 ─────────────────────────────────────────────────

/// 项目根相对路径的便捷构造(只在测试里用,入口层直接拿 `RelPath`)。
#[cfg(test)]
pub(crate) fn rel(path: &str) -> Result<RelPath> {
    RelPath::new(path).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    #[test]
    fn changed_span_only_keeps_the_difference() {
        assert_eq!(changed_span("派森列表结构", "Python 列表结构"), "派森 → Python ");
        assert_eq!(changed_span("abc", "abc"), "整句重写");
        assert_eq!(changed_span("", "新增"), "∅ → 新增");
        assert_eq!(changed_span("删掉", ""), "删掉 → ∅");
    }

    #[test]
    fn changed_span_never_panics_on_chinese() {
        // 曾经的写法会在字节下标上 panic(AGENTS.md §1.1),这条测试就是钉住这件事
        let long = "这是一段很长的中文文本用来测试掐前后缀不会切在字符中间".repeat(3);
        let changed = changed_span(&long, &format!("{long}尾巴"));
        assert!(changed.contains('→'), "{changed}");
        // 整段换掉时两边都该被截断,而不是把 100 多字塞进对照表
        let swapped = changed_span(&"甲".repeat(50), &"乙".repeat(50));
        assert!(swapped.ends_with('…'), "{swapped}");
        assert_eq!(swapped.matches('…').count(), 2, "{swapped}");
    }

    #[test]
    fn corrections_go_to_the_copy_not_the_original() {
        let fake = Fake::new("concl-corr").with_transcript(&[
            (0.0, 2.0, "派森列表"),
            (2.0, 4.0, "元祖笔记"),
        ]);
        let mut opened = fake.open();
        let _ = &mut opened;

        let outcome = apply_corrections(
            &opened,
            &[
                CorrectionInput {
                    segment_id: 1,
                    corrected_text: Some("Python 列表".into()),
                    kind: CorrectionType::Terminology,
                    note: None,
                    reason: "截图画面写着「Python 列表」".into(),
                },
                CorrectionInput {
                    segment_id: 2,
                    corrected_text: None,
                    kind: CorrectionType::Uncertain,
                    note: Some("疑似「元组」".into()),
                    reason: "上下文在讲培训课程".into(),
                },
            ],
        )
        .unwrap();

        assert_eq!(outcome.applied, 2);
        assert_eq!(outcome.changed, 1);
        assert_eq!(outcome.uncertain, 1);
        assert_eq!(outcome.total_segments, 2);

        // 原文不动
        let raw = Transcript::load(&fake.layout().transcript()).unwrap();
        assert_eq!(raw.segments[0].text, "派森列表");
        assert!(raw.segments[0].corrected_text.is_none());

        // 副本改了
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "Python 列表");
        assert_eq!(fixed.segments[1].effective_text(), "元祖笔记");
        assert!(fixed.segments[1].is_uncertain());

        // 对照表里两条都在,且标出了"未改,存疑"
        let diff = std::fs::read_to_string(fake.layout().transcript_diff()).unwrap();
        assert!(diff.contains("派森列表"), "{diff}");
        assert!(diff.contains("Python 列表"), "{diff}");
        assert!(diff.contains("(未改,存疑)"), "{diff}");
        assert!(diff.contains("派森 → Python "), "自动掐出来的改动片段:{diff}");
    }

    #[test]
    fn corrections_accumulate_across_calls() {
        let fake = Fake::new("concl-accum").with_transcript(&[(0.0, 2.0, "甲"), (2.0, 4.0, "乙")]);
        let opened = fake.open();

        apply_corrections(
            &opened,
            &[CorrectionInput {
                segment_id: 1,
                corrected_text: Some("甲甲".into()),
                kind: CorrectionType::Other,
                note: None,
                reason: "第一次".into(),
            }],
        )
        .unwrap();
        let second = apply_corrections(
            &opened,
            &[CorrectionInput {
                segment_id: 2,
                corrected_text: Some("乙乙".into()),
                kind: CorrectionType::Other,
                note: None,
                reason: "第二次".into(),
            }],
        )
        .unwrap();

        assert_eq!(second.changed, 2, "第二次不该把第一次的结论抹掉");
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "甲甲");
        assert_eq!(fixed.segments[1].effective_text(), "乙乙");
    }

    #[test]
    fn unknown_segment_ids_write_nothing() {
        let fake = Fake::new("concl-unknown").with_transcript(&[(0.0, 2.0, "甲")]);
        let opened = fake.open();

        let err = apply_corrections(
            &opened,
            &[
                CorrectionInput {
                    segment_id: 1,
                    corrected_text: Some("甲甲".into()),
                    kind: CorrectionType::Other,
                    note: None,
                    reason: "好的那条".into(),
                },
                CorrectionInput {
                    segment_id: 99,
                    corrected_text: Some("无中生有".into()),
                    kind: CorrectionType::Other,
                    note: None,
                    reason: "坏的那条".into(),
                },
            ],
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("99"), "{err}");
        assert!(
            !fake.layout().transcript_fixed().exists(),
            "整批拒绝时不该落下半个文件"
        );
    }

    #[test]
    fn uncertain_without_note_is_refused() {
        let fake = Fake::new("concl-nonote").with_transcript(&[(0.0, 2.0, "甲")]);
        let opened = fake.open();
        let err = apply_corrections(
            &opened,
            &[CorrectionInput {
                segment_id: 1,
                corrected_text: None,
                kind: CorrectionType::Uncertain,
                note: None,
                reason: "说不上来".into(),
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("note"), "{err}");
    }

    #[test]
    fn record_shot_check_fills_neighbor_text_by_itself() {
        let fake = Fake::new("concl-shot")
            .with_transcript(&[(8.0, 12.0, "这里是 Python 列表结构")])
            .with_shot(1, 9.0);
        let opened = fake.open();

        let check = record_shot_check(
            &opened,
            vidbrief_project::ShotId::new(1).unwrap(),
            &ShotCheckInput {
                r#match: true,
                audio_visual_sync: true,
                type_read: "Python 列表 结构".into(),
                issues: vec![],
                confidence: Confidence::High,
                neighbor_text: None,
            },
        )
        .unwrap();

        assert_eq!(check.neighbor_text.as_deref().map(str::is_empty), Some(false));
        assert!(
            check
                .neighbor_text
                .as_deref()
                .is_some_and(|t| t.contains("Python 列表"))
        );
        assert!(!check.needs_review());

        // 写回磁盘了
        let shots = Shots::load(&fake.layout().shots_json()).unwrap();
        assert!(shots.get(vidbrief_project::ShotId::new(1).unwrap()).unwrap().check.is_some());
    }

    #[test]
    fn write_note_refuses_paths_outside_the_allowed_dirs() {
        let fake = Fake::new("concl-note");
        let mut opened = fake.open();

        for bad in ["project.json", "01_preprocess/shots.json", "04_report/report.md"] {
            let err = write_note(
                &mut opened,
                &rel(bad).unwrap(),
                "x",
                WriteMode::Overwrite,
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("只能写在"), "{bad} 竟然被放过了:{err}");
        }

        // 非 md 也不行
        assert!(
            write_note(
                &mut opened,
                &rel("03_summary/notes/x.json").unwrap(),
                "x",
                WriteMode::Overwrite,
                false
            )
            .is_err()
        );
    }

    #[test]
    fn write_note_appends_without_losing_the_previous_round() {
        let fake = Fake::new("concl-append");
        let mut opened = fake.open();
        let path = rel("02_calibrate/calibration_log.md").unwrap();

        write_note(&mut opened, &path, "## 第 1 轮\n改了 3 张", WriteMode::Append, true).unwrap();
        write_note(&mut opened, &path, "## 第 2 轮\n改了 1 张", WriteMode::Append, true).unwrap();

        let text = std::fs::read_to_string(fake.layout().calibration_log()).unwrap();
        assert!(text.contains("第 1 轮"), "{text}");
        assert!(text.contains("第 2 轮"), "{text}");

        // count_round 才动轮数
        let project = vidbrief_project::Project::load(&fake.layout().project_json()).unwrap();
        assert_eq!(project.progress.calibrate.rounds, 2);
    }

    #[test]
    fn writing_a_note_without_counting_rounds_leaves_the_counter_alone() {
        let fake = Fake::new("concl-noround");
        let mut opened = fake.open();
        write_note(
            &mut opened,
            &rel("02_calibrate/cross_validate.md").unwrap(),
            "交叉印证",
            WriteMode::Overwrite,
            false,
        )
        .unwrap();
        let project = vidbrief_project::Project::load(&fake.layout().project_json()).unwrap();
        assert_eq!(project.progress.calibrate.rounds, 0);
    }

    #[test]
    fn summary_and_report_mark_their_steps() {
        let fake = Fake::new("concl-steps");
        let mut opened = fake.open();

        write_summary(&mut opened, "# 总结\n").unwrap();
        write_report_md(&mut opened, "# 报告\n").unwrap();

        let project = vidbrief_project::Project::load(&fake.layout().project_json()).unwrap();
        assert!(project.progress.summarize.done);
        assert!(project.progress.report.done);
        assert!(fake.layout().summary_md().is_file());
        assert!(fake.layout().report_md().is_file());
    }

    #[test]
    fn clock_matches_the_timeline_format() {
        assert_eq!(clock(0.0), "00:00.0");
        assert_eq!(clock(188.6), "03:08.6");
        assert_eq!(clock(985.43), "16:25.4");
        assert_eq!(clock(-1.0), "00:00.0");
    }
}
