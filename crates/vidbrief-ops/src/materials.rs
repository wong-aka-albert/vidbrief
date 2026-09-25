//! 供材料:C 组工具(`list_shots` / `get_shot` / `get_transcript` / `search_transcript` /
//! `get_change_curve` / `list_regions`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具只搬运与保存,不判断对错**。
//! 这一层的全部职责就是:把 agent 需要看的东西准确地、不多不少地递给它。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{
    ChangeCurve, Correction, DenseRegions, RelPath, Segment, Shot, ShotId, Shots, Transcript,
};

use crate::inspect::ShotCounts;
use crate::project::OpenProject;

// ── 截图 ─────────────────────────────────────────────────────────────

/// 读截图清单。
pub fn shots(opened: &OpenProject) -> Result<Shots> {
    Shots::load(&opened.layout().shots_json())
        .with_context(|| "读取 shots.json 失败 —— 步 1 可能还没跑完(`vidbrief preprocess`)".to_string())
}

/// 截图分档计数(留用 / 未校准 / 待裁决)。
pub fn shot_counts(shots: &Shots) -> ShotCounts {
    ShotCounts::of(shots)
}

/// 一张截图的**全部材料**:图像本身 + 元数据 + 同期转录。
///
/// 这是"能看图"这条铁律落到代码上的地方:图像以原始字节交出去,
/// 由调用方(base64 → MCP image content)决定怎么递。**本层不解释画面内容。**
#[derive(Debug, Clone)]
pub struct ShotDetail {
    pub shot: Shot,
    /// 图片在磁盘上的位置。
    pub image_path: PathBuf,
    /// 图片原始字节(JPEG)。**不进 JSON**(serde 已跳过)。
    pub image: Vec<u8>,
    /// `image/jpeg`。
    pub mime: String,
    /// 同期转录,已组装成人可读的多行文本:每行 `(起-止) 文本`。
    pub transcript: String,
    /// 取的是哪一段时间(闭区间,秒)。
    pub window_start: f64,
    pub window_end: f64,
}

/// 取一张截图 + 它前后 `window` 秒的转录。
pub fn shot_detail(opened: &OpenProject, id: ShotId, window: f64) -> Result<ShotDetail> {
    let shots = shots(opened)?;
    let Some(shot) = shots.get(id).cloned() else {
        // 找不到就**把可用的 id 报出来**。
        //
        // 这不是"报错要好看":调用方(尤其 agent)手上那个 id 可能来自转录、
        // 来自用户口述、来自上一轮会话的记忆 —— 它没有别的办法知道哪儿写错了。
        // 报一句"没有这张图"它就得再问一轮;报出可用的 id,它当场就能改。
        anyhow::bail!("{}", missing_shot_hint(&shots, id));
    };

    let image_path = opened.layout().rel(shot.file.as_str());
    let image = std::fs::read(&image_path)
        .with_context(|| format!("读截图失败:{}", image_path.display()))?;

    // 转录可能还没有(没音轨 / --no-transcribe),那不是错误:agent 只拿到图
    let (transcript, start, end) = match effective_transcript(opened) {
        Ok((t, _, _)) => {
            let start = (shot.t - window).max(0.0);
            let end = shot.t + window;
            (t.render_range(start, end), start, end)
        }
        Err(_) => (String::new(), (shot.t - window).max(0.0), shot.t + window),
    };

    Ok(ShotDetail {
        shot,
        image_path,
        image,
        mime: "image/jpeg".to_string(),
        transcript,
        window_start: start,
        window_end: end,
    })
}

/// 找不到截图时该说的话:**说清有哪些**,而不是只说没有。
///
/// 一次最多列 [`MAX_LISTED_IDS`] 个 —— 一张一张数给调用方看没有意义,
/// 它要的是"我手上这个数为什么不对"。
fn missing_shot_hint(shots: &Shots, want: ShotId) -> String {
    const MAX_LISTED_IDS: usize = 12;

    if shots.shots.is_empty() {
        return format!(
            "项目里没有截图 {want} —— 这个项目**一张截图都没有**,步 1(`preprocess`)可能还没跑完。"
        );
    }
    let ids: Vec<String> = shots
        .shots
        .iter()
        .take(MAX_LISTED_IDS)
        .map(|s| s.id.to_string())
        .collect();
    let tail = if shots.shots.len() > MAX_LISTED_IDS {
        format!("(共 {} 张)", shots.shots.len())
    } else {
        String::new()
    };
    // 用逗号拼,不用手动切片 —— 中文按字节切会 panic(见 AGENTS.md)
    format!(
        "项目里没有截图 {want}。可用的 id:{}{tail}",
        ids.join("、")
    )
}

// ── 转录 ─────────────────────────────────────────────────────────────

/// 读**当前有效**的转录。
///
/// 有 `02_calibrate/transcript.fixed.json` 就用它,否则用 `01_preprocess/transcript.json`。
///
/// 为什么要有这个函数:原始转录**永不就地改写**(`docs/schema/README.md`),
/// 所以"agent 该看哪一份"是个必须有人回答的问题。散在各处回答,迟早有一处答错。
/// 返回的第二个值是"这份来自哪个文件",第三个是"是不是纠错稿"。
pub fn effective_transcript(opened: &OpenProject) -> Result<(Transcript, PathBuf, bool)> {
    let layout = opened.layout();
    let fixed = layout.transcript_fixed();
    if fixed.is_file() {
        let t = Transcript::load(&fixed)
            .with_context(|| format!("读取 {} 失败", fixed.display()))?;
        return Ok((t, fixed, true));
    }
    let raw = layout.transcript();
    let t = Transcript::load(&raw).with_context(|| {
        format!(
            "读取 {} 失败 —— 步 1 可能还没做转录(源视频没有音轨时会跳过)",
            raw.display()
        )
    })?;
    Ok((t, raw, false))
}

/// 一段转录的**取用视图**。
///
/// `text` 是**当前有效的文本**(有纠错就是改后的那版)。
///
/// 为什么不直接把契约层的 `Segment` 扔出去:它同时带着 `text`(原始)与
/// `corrected_text`(改后),读哪一个由调用方自己拿主意 —— 而契约层明写着
/// 「所有下游都应通过 `effective_text` 取文本,**否则纠错就白做了**」。
/// 把这条规矩留给每个调用方遵守,等于没有规矩:M4 联调时,`get_transcript`
/// 返回的 `text` 里仍是「张小明」,而同一份材料在 `get_shot` 的同期转录里
/// 已经是改后的 —— **同一个工具集,两种说法**。
///
/// 原始文本不会因此丢掉:它一直在 `01_preprocess/transcript.json` 里(永不就地改写),
/// `transcript.diff.md` 也逐条记着改了什么。
#[derive(Debug, Clone, Serialize)]
pub struct SegmentView {
    /// 等于 SRT 里的序号,从 1 起。
    pub id: u32,
    pub start: f64,
    pub end: f64,
    /// 当前有效的文本。
    pub text: String,
    /// 这一段**被真的改过**吗(区别于"存疑但没改")。
    pub corrected: bool,
    /// 存疑但没改 —— 留着让用户裁决。
    pub uncertain: bool,
    /// 改动的性质与说明;没动过就是 `null`。
    pub correction: Option<Correction>,
}

impl SegmentView {
    fn of(seg: &Segment) -> Self {
        Self {
            id: seg.id,
            start: seg.start,
            end: seg.end,
            text: seg.effective_text().to_string(),
            corrected: seg.is_corrected(),
            uncertain: seg.is_uncertain(),
            correction: seg.correction.clone(),
        }
    }
}

/// 转录的取用视图。带上"这份是什么"的元信息,免得 agent 以为自己读的是原文。
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptView {
    /// 这份转录来自哪个文件(项目根相对)。
    pub source_file: RelPath,
    /// 是不是纠错稿(`transcript.fixed.json`)。
    pub corrected: bool,
    pub model: String,
    pub language: String,
    pub duration: f64,
    /// 全量段数(即使本次只返回了一部分)。
    pub total_segments: usize,
    /// 本次返回的段。
    pub segments: Vec<SegmentView>,
}

/// 取转录。给了 `range` 就只取与 `[start, end]` 重叠的段。
pub fn transcript(opened: &OpenProject, range: Option<(f64, f64)>) -> Result<TranscriptView> {
    let (full, path, corrected) = effective_transcript(opened)?;
    let total = full.segments.len();

    let segments = match range {
        Some((start, end)) if end >= start => full
            .segments_in(start, end)
            .map(SegmentView::of)
            .collect::<Vec<_>>(),
        Some((start, end)) => {
            anyhow::bail!("区间不合法:start={start} > end={end}");
        }
        None => full.segments.iter().map(SegmentView::of).collect(),
    };

    Ok(TranscriptView {
        source_file: RelPath::new(rel_of(opened, &path))?,
        corrected,
        model: full.model,
        language: full.language,
        duration: full.duration,
        total_segments: total,
        segments,
    })
}

/// 在**有效文本**里找子串(大小写不敏感)。
///
/// 中文没有词边界,英文术语又要认大小写差异(`python` 该找到 `PYTHON`),
/// 所以朴素地转小写后 `contains` —— 与 `vidbrief-core` 里指代表达的判定同一取舍。
pub fn transcript_search(opened: &OpenProject, query: &str, limit: usize) -> Result<TranscriptView> {
    let (full, path, corrected) = effective_transcript(opened)?;
    let total = full.segments.len();
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        anyhow::bail!("查询词不能为空");
    }

    let hits: Vec<SegmentView> = full
        .segments
        .iter()
        .filter(|s| s.effective_text().to_lowercase().contains(&needle))
        .take(limit)
        .map(SegmentView::of)
        .collect();

    Ok(TranscriptView {
        source_file: RelPath::new(rel_of(opened, &path))?,
        corrected,
        model: full.model,
        language: full.language,
        duration: full.duration,
        total_segments: total,
        segments: hits,
    })
}

/// 绝对路径 → 项目根相对路径(给输出里带的"这来自哪个文件")。
fn rel_of(opened: &OpenProject, path: &std::path::Path) -> String {
    let root = opened.layout().root();
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

// ── 差帧图与密集区 ───────────────────────────────────────────────────

/// 取差帧图。给了 `range` 就只取落在区间内的采样点。
///
/// 不做降采样:一分钟的视频才 60 个点,真实长视频 1fps 也就一千来个 —— 直接给全量,
/// 让 agent 自己决定怎么用,比在这里替它猜要好。
pub fn change_curve(opened: &OpenProject, range: Option<(f64, f64)>) -> Result<ChangeCurve> {
    let mut curve = ChangeCurve::load(&opened.layout().change_curve())
        .with_context(|| "读取 change_curve.json 失败 —— 步 1 可能还没跑".to_string())?;
    if let Some((start, end)) = range {
        if end < start {
            anyhow::bail!("区间不合法:start={start} > end={end}");
        }
        curve.changes = curve
            .changes
            .iter()
            .filter(|c| c.t >= start && c.t <= end)
            .copied()
            .collect();
    }
    Ok(curve)
}

/// 取变化密集区清单。
pub fn list_regions(opened: &OpenProject) -> Result<DenseRegions> {
    DenseRegions::load(&opened.layout().dense_regions())
        .with_context(|| "读取 dense_regions.json 失败 —— 步 1 可能还没跑".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    #[test]
    fn shot_detail_carries_the_window_of_transcript_around_it() {
        let fake = Fake::new("materials-detail")
            .with_transcript(&[(0.0, 2.0, "开场"), (8.0, 12.0, "这里说的是样本")])
            .with_shot(1, 9.0);
        let opened = fake.open();

        let detail = shot_detail(&opened, ShotId::new(1).unwrap(), 6.0).unwrap();
        assert_eq!(detail.shot.t, 9.0);
        assert_eq!(detail.mime, "image/jpeg");
        assert_eq!(detail.window_start, 3.0, "3.0 = 9.0 - 6.0,不该被夹到 0");
        assert_eq!(detail.window_end, 15.0);
        // 只有落在窗口里的那段该出现
        assert!(detail.transcript.contains("样本"), "{}", detail.transcript);
        assert!(!detail.transcript.contains("开场"), "{}", detail.transcript);
    }

    #[test]
    fn shot_detail_without_transcript_is_still_usable() {
        let fake = Fake::new("materials-notranscript").with_shot(1, 9.0);
        let opened = fake.open();
        let detail = shot_detail(&opened, ShotId::new(1).unwrap(), 6.0).unwrap();
        assert_eq!(detail.transcript, "", "没转录不是错误,只是没有文可给");
    }

    #[test]
    fn missing_shot_says_which_id_is_missing() {
        let fake = Fake::new("materials-missing").with_shot(1, 9.0);
        let opened = fake.open();
        let err = shot_detail(&opened, ShotId::new(7).unwrap(), 6.0)
            .unwrap_err()
            .to_string();
        assert!(err.contains("s007"), "{err}");
    }

    #[test]
    fn search_is_case_insensitive_and_trims() {
        let fake = Fake::new("materials-search")
            .with_transcript(&[(0.0, 2.0, "看 PYTHON 的数据变化"), (2.0, 4.0, "再看样本")]);
        let opened = fake.open();

        let hit = transcript_search(&opened, " python ", 10).unwrap();
        assert_eq!(hit.segments.len(), 1);
        assert_eq!(hit.segments[0].id, 1);
        assert_eq!(hit.total_segments, 2, "全量段数不因过滤而变");

        assert!(transcript_search(&opened, "   ", 10).is_err());
    }

    #[test]
    fn curve_range_filters_points() {
        let fake = Fake::new("materials-curve").with_curve(5);
        let opened = fake.open();

        assert_eq!(change_curve(&opened, None).unwrap().changes.len(), 5);
        let cut = change_curve(&opened, Some((1.0, 2.0))).unwrap();
        assert_eq!(cut.changes.len(), 2);
        assert_eq!(cut.changes[0].t, 1.0);
        assert!(change_curve(&opened, Some((3.0, 1.0))).is_err());
    }

    #[test]
    fn fixed_transcript_wins_over_the_raw_one() {
        let fake = Fake::new("materials-fixed")
            .with_transcript(&[(0.0, 2.0, "派森列表")]);
        let opened = fake.open();

        let before = transcript(&opened, None).unwrap();
        assert!(!before.corrected);
        assert_eq!(before.segments[0].text, "派森列表");
        assert!(!before.segments[0].corrected);

        // 写一份纠错稿进去,再读就该拿到纠错后的文本
        let mut fixed = Transcript::load(&opened.layout().transcript()).unwrap();
        fixed.segments[0].corrected_text = Some("Python 列表".into());
        fixed.save(&opened.layout().transcript_fixed()).unwrap();

        let after = transcript(&opened, None).unwrap();
        assert!(after.corrected);
        // **不用调 `effective_text`** —— 视图里 `text` 就是当前有效的那一份。
        //
        // 这条断言以前写的是 `segments[0].effective_text()`:它绿着,却挡不住
        // "调用方忘了调"这个真问题(M4 联调就是这么栽的)。现在规矩在类型里。
        assert_eq!(after.segments[0].text, "Python 列表");
        assert!(after.segments[0].corrected, "该标出这一段被动过");
    }
}
