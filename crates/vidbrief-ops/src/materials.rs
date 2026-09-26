//! 供材料:C 组工具(`list_shots` / `get_shot` / `get_transcript` / `search_transcript` /
//! `get_change_curve` / `list_regions`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具只搬运与保存,不判断对错**。
//! 这一层的全部职责就是:把 agent 需要看的东西准确地、不多不少地递给它。
//!
//! # 读的是"内容 + 状态",写的时候再拆开
//!
//! 明文材料(`shots.json` / `transcript.fixed.json` / `supplement_requests.json`)里那几栏
//! **状态**(留用还是弃用、看图结论、核验戳、候选处置)**只是投影**:谁都能手改,
//! 所以工具不拿它当判据。真正的家在 `state.bin`(见 [`vidbrief_project::state`])。
//!
//! 于是这一层只做两件事:
//!
//! - **读**:读明文里的内容,再把状态盖上去(交给 `ProjectState::apply_to_*`),交给调用方;
//! - **写**:调用方改完那份合并过的对象再交回来(见 [`save_shots`] / [`save_supplements`] /
//!   [`save_transcript_projection`])—— 内容进明文、状态进 `state.bin`,**同出一源**,
//!   所以不会各说各话。
//!
//! "明文文件 = 状态的投影"这条不变式因此只有一个落点。哪天有人手改了明文,
//! 门照样读状态(骗不过去),而 `vidbrief status` 会把对不上的地方列出来给人看。

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{
    ChangeCurve, DenseRegions, RelPath, Segment, Shot, ShotId, Shots, SupplementRequests,
    Transcript,
};

use crate::inspect::ShotCounts;
use crate::project::OpenProject;

// ── 截图 ─────────────────────────────────────────────────────────────

/// 读截图清单(**内容 + 状态**)。
pub fn shots(opened: &OpenProject) -> Result<Shots> {
    shots_optional(opened)?
        .with_context(|| "读取 shots.json 失败 —— 步 1 可能还没跑完(`vidbrief preprocess`)".to_string())
}

/// 读截图清单,**允许它还不存在**(给门和盘点用)。
///
/// 三态,不是两态:**不在** → `Ok(None)`;**在、读得动** → `Ok(Some)`;**在、读不动** → `Err`。
///
/// 为什么非要分三态(M4 第五轮实测):以前写的是 `shots(opened).unwrap_or_default()`,
/// 读不动就拿一份空清单顶上。而门在"没有截图"和"看不清截图清单"上要做的判断正好相反 ——
/// 前者没什么可拦的,**后者必须拦**。那次一份读不动的材料因此被当成"什么都没有",
/// 门数出来 0 条未核验项,一路放行。**把"看不懂"和"没有"混成一件事,门就瞎了。**
pub fn shots_optional(opened: &OpenProject) -> Result<Option<Shots>> {
    let path = opened.layout().shots_json();
    if !path.is_file() {
        return Ok(None);
    }
    let mut shots = Shots::load(&path).with_context(|| {
        format!(
            "读取 {} 失败 —— 这份截图清单读不动(被改过或坏了)。\
             它不是'没有截图':按'还没校准'处理。\
             重新跑一次 `preprocess` 会照当前视频重算一份。",
            path.display()
        )
    })?;
    opened.state().apply_to_shots(&mut shots);
    Ok(Some(shots))
}

/// 读**补图候选清单**,允许它还不存在(给门和盘点用)。三态同上。
///
/// 读不动时的出路:重新跑一次 `find_supplement_candidates` ——
/// 它会照当前转录与截图重算一份(代价是**已做过的处置会跟着重来**,因为读不动的东西没法沿用)。
pub fn supplements(opened: &OpenProject) -> Result<Option<SupplementRequests>> {
    let path = opened.layout().supplement_requests();
    if !path.is_file() {
        return Ok(None);
    }
    let list = SupplementRequests::load(&path).with_context(|| {
        format!(
            "读取 {} 失败 —— 这份补图候选清单读不动(被改过或坏了)。\
             它不是'没有候选':按'还没审过'处理。\
             重新跑一次 `find_supplement_candidates` 会照当前材料重算一份。",
            path.display()
        )
    })?;
    Ok(Some(list))
}

// ── 落盘:内容进明文,状态进 state.bin ────────────────────────────────

/// 把一份截图清单落盘。
///
/// 三件事,缺一不可:
/// 1. **状态**从这份对象里收进 `state.bin`(它才是有判据的那一份);
/// 2. **内容**写进 `shots.json` —— 连同那几栏一起写出去,当**投影**给人看;
/// 3. 状态文件存盘。
///
/// 因为两边同出一源,投影与状态天然一致;哪天有人手改了明文,下一次写就把它盖回去
/// (对不上的那些由 `ProjectState::mismatches` 报给人看)。
pub fn save_shots(opened: &mut OpenProject, shots: &Shots) -> Result<()> {
    opened.state_mut().absorb_shots(shots);
    let path = opened.layout().shots_json();
    shots
        .save(&path)
        .with_context(|| format!("写入 {} 失败", path.display()))?;
    // 记下**内容**的底账:门的第①条是从明文里数数的,数出来的东西得有个对照
    //(不然"把没审的行拿掉"就能把活藏起来,见 `ProjectState::content`)
    let print = vidbrief_project::shots_print(shots);
    opened
        .state_mut()
        .note_content(vidbrief_project::layout::SHOTS_FILE, print);
    opened.save()
}

/// 把一份补图候选清单落盘(规则同 [`save_shots`])。
pub fn save_supplements(opened: &mut OpenProject, list: &SupplementRequests) -> Result<()> {
    opened.state_mut().absorb_supplements(list);
    let path = opened.layout().supplement_requests();
    list.save(&path)
        .with_context(|| format!("写入 {} 失败", path.display()))?;
    let print = vidbrief_project::supplements_print(list);
    opened
        .state_mut()
        .note_content(vidbrief_project::layout::SUPPLEMENT_REQUESTS_FILE, print);
    opened.save()
}

/// 记下**原始转录**的内容底账。
///
/// 单独一个函数,是因为原始转录不是工具"写"出来的 —— 它是 `preprocess` 里
/// `vidbrief-asr` 直接落的盘(那一层不认识状态)。所以由调用方在它写完之后叫一声。
pub fn note_transcript_content(opened: &mut OpenProject, transcript: &Transcript) -> Result<()> {
    let print = vidbrief_project::transcript_print(transcript);
    opened
        .state_mut()
        .note_content(vidbrief_project::layout::TRANSCRIPT_FILE, print);
    opened.save()
}

/// 三份材料的内容底账**现在**是什么样 —— **门与体检共用这一处判断**。
///
/// 返回"跟底账对不上"的那些说法,空 = 都对得上。
/// 每份材料都按**当前内容**重算一遍指纹 —— 算的是内容(hash 的是段号/时刻/正文),
/// 不是文件字节,所以换个缩进、改一栏状态都不会误报。
///
/// # 一条例外:**状态里一条底账都没有**时,什么都不算对不上
///
/// 那是"**还没记过账**"(刚建的项目、状态刚重起),不是"材料被人换了"。
/// 那种场面交给门的第①条按"还没核验"拦 —— 它说的话(接着去审)才是这时候该做的。
///
/// # 为什么只有这一处
///
/// 门(`review::gate_calibrate`)和体检(`inspect::scan_materials`)都要问这个问题。
/// 各写一份的话迟早分叉,而分叉出来的那一种最难查:
/// 「**体检说没事、门却拦着**」(或者反过来)—— 用户看到的是一句无法照做的话。
/// 所以判据只写在这儿,两边都调它;下面还有一个"手上已经有材料"的版本,
/// 给不想再读一遍文件的调用方。
pub fn content_mismatches(opened: &OpenProject) -> Result<Vec<String>> {
    let transcript = effective_transcript_optional(opened)?.map(|(t, _, _)| t);
    let shots = shots_optional(opened)?;
    let supplements = supplements(opened)?;
    Ok(content_mismatches_of(
        opened.state(),
        transcript.as_ref(),
        shots.as_ref(),
        supplements.as_ref(),
    ))
}

/// 同 [`content_mismatches`],但材料由调用方递进来(它手上本来就有,不必再读一遍文件)。
///
/// 递**合并过**的那一份也没关系:指纹算的是内容,而状态那几栏不在里面。
pub fn content_mismatches_of(
    state: &vidbrief_project::ProjectState,
    transcript: Option<&Transcript>,
    shots: Option<&Shots>,
    supplements: Option<&SupplementRequests>,
) -> Vec<String> {
    if state.content.is_empty() {
        return Vec::new();
    }
    let rows: [(&str, Option<vidbrief_project::ContentPrint>); 3] = [
        (
            vidbrief_project::layout::TRANSCRIPT_FILE,
            transcript.map(vidbrief_project::transcript_print),
        ),
        (
            vidbrief_project::layout::SHOTS_FILE,
            shots.map(vidbrief_project::shots_print),
        ),
        (
            vidbrief_project::layout::SUPPLEMENT_REQUESTS_FILE,
            supplements.map(vidbrief_project::supplements_print),
        ),
    ];
    let mut out = Vec::new();
    for (rel, now) in rows {
        if let Some(line) = state.content_stale(rel, now) {
            out.push(line);
        }
    }
    out
}

/// 把转录的**投影**(`transcript.fixed.json`)写出去。
///
/// 结论本身在状态里;这里写的是一份**给人读的全文**:原文 + 当前结论 ——
/// 核对纠错、看视图都不用先解状态文件。
///
/// `merged` 必须是**原文叠上状态之后**的那一份(调用方手上就有)。
pub fn save_transcript_projection(opened: &OpenProject, merged: &Transcript) -> Result<()> {
    let path = opened.layout().transcript_fixed();
    merged
        .save(&path)
        .with_context(|| format!("写入 {} 失败", path.display()))
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
    match effective_transcript_optional(opened)? {
        Some(v) => Ok(v),
        None => {
            let raw = opened.layout().transcript();
            anyhow::bail!(
                "读取 {} 失败 —— 步 1 可能还没做转录(源视频没有音轨时会跳过)",
                raw.display()
            )
        }
    }
}

/// 同 [`effective_transcript`],但**允许项目还没有转录**。三态同 [`shots_optional`]。
///
/// # 纠错稿不再从文件里读
///
/// 从前这里读的是 `02_calibrate/transcript.fixed.json`;现在**结论在状态里**
/// (`state.bin`),那份文件退回"给人读的投影" —— 于是这里读的是**原文 + 状态**,
/// 现叠一份出来。
///
/// 为什么非改不可:纠错稿是"这一段审过了、那段改了"的唯一载体,而那是门的判据 ——
/// 一份明文、谁都能手写的文件当判据,门就是纸糊的。
///
/// 返回的第二个值是**内容来自哪个文件**(永远是原文那一份),
/// 第三个是"这一份里带没带结论",让读的人知道它不是原文。
pub fn effective_transcript_optional(
    opened: &OpenProject,
) -> Result<Option<(Transcript, PathBuf, bool)>> {
    let layout = opened.layout();
    let raw = layout.transcript();
    if !raw.is_file() {
        return Ok(None);
    }
    let mut t = Transcript::load(&raw)
        .with_context(|| format!("读取 {} 失败 —— 转录取不出来(被改过或坏了)", raw.display()))?;
    opened.state().apply_to_transcript(&mut t);
    let concluded = t.stats().touched() > 0
        || opened.state().segments.values().any(|s| s.checked_at.is_some());
    Ok(Some((t, raw, concluded)))
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
    /// 当前有效的文本(有改动就是改后的那一版)。
    pub text: String,
    /// 对这一段下的结论:`corrected` 改了 / `kept` 决定保留原句 /
    /// `uncertain` 存疑留给人 / `null` 没话说(也可能还没看过)。
    ///
    /// **一个字段说清四种状态**,不给"改没改""存没存疑"各开一个布尔 ——
    /// 两个字段能表达同一件事时,迟早填成两样。
    pub outcome: Option<&'static str>,
    /// 改动内容 / 为什么这么定。
    pub note: Option<String>,
    /// 判断依据。
    pub reason: Option<String>,
}

impl SegmentView {
    fn of(seg: &Segment) -> Self {
        let (outcome, note, reason) = match &seg.conclusion {
            Some(c) => (
                Some(match c {
                    vidbrief_project::Conclusion::Corrected { .. } => "corrected",
                    vidbrief_project::Conclusion::Kept { .. } => "kept",
                    vidbrief_project::Conclusion::Uncertain { .. } => "uncertain",
                }),
                Some(c.note().to_string()),
                Some(c.reason().to_string()),
            ),
            None => (None, None, None),
        };
        Self {
            id: seg.id,
            start: seg.start,
            end: seg.end,
            text: seg.effective_text().to_string(),
            outcome,
            note,
            reason,
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
        let mut opened = fake.open();

        let before = transcript(&opened, None).unwrap();
        assert!(before.segments[0].outcome.is_none());
        assert_eq!(before.segments[0].text, "派森列表");
        assert!(before.segments[0].outcome.is_none());

        // 下一段结论进**状态**,再读就该拿到纠错后的文本
        opened.state_mut().segment_mut(1).set_conclusion(Some(
            vidbrief_project::Conclusion::Corrected {
                text: "Python 列表".into(),
                kind: vidbrief_project::CorrectionType::Terminology,
                note: "派森 → Python".into(),
                reason: "画面标题".into(),
            },
        ));

        let after = transcript(&opened, None).unwrap();
        assert_eq!(after.segments[0].outcome, Some("corrected"));
        // **不用调 `effective_text`** —— 视图里 `text` 就是当前有效的那一份。
        //
        // 这条断言以前写的是 `segments[0].effective_text()`:它绿着,却挡不住
        // "调用方忘了调"这个真问题(M4 联调就是这么栽的)。现在规矩在类型里。
        assert_eq!(after.segments[0].text, "Python 列表");
        assert_eq!(after.segments[0].outcome, Some("corrected"), "该标出这一段被动过");
    }
}
