//! 存结论:D 组工具(`record_shot_check` / `apply_corrections` / `write_note` /
//! `write_summary`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具不判断对错,只搬运与保存。**
//! 这里是"agent 的结论落成文件"的唯一入口。
//!
//! 步 4(报告)不在这里:`report.md` / `report.html` 都是**同一份报告数据的投影**,
//! 落数据、渲染、开页面归 `crate::report`(见 devlog [260926-07] 的"数据为源")。

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use vidbrief_project::{
    Conclusion, Confidence, CorrectionType, RelPath, ShotCheck, Transcript, without_punctuation,
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
    /// **照抄**的那句转录原文 —— 这条结论凭的就是它。
    ///
    /// 工具拿它去转录里找:找得到 → 落在这一段上,连同段号存下来;找不到 → 拒收。
    /// 详见 [`vidbrief_project::Evidence`]。
    pub evidence_quote: Option<String>,
}

/// 把一张图的判断结论写回 `shots.json`。
///
/// 写回的是**全量**替换:agent 复核过一遍再写一次,应以最后一次为准。
///
/// # 证据是**找出来的**,不是报上来的
///
/// `evidence_quote` 给的是**照抄的原文**,段号由这里去转录里搜 —— 搜不到就拒收。
/// 让模型自己报段号的话,它就会报错段号,而那又是一处没法核对的自述:
/// 我们要的不是"它说引了第几段",是"**它引的那句话真的在材料里**"。
pub fn record_shot_check(
    opened: &mut OpenProject,
    id: vidbrief_project::ShotId,
    input: &ShotCheckInput,
) -> Result<ShotCheck> {
    // 读的是"内容 + 状态"合起来的那一份(见 `materials` 的模块文档)
    let mut shots = crate::materials::shots(opened)?;
    let shot = shots
        .get(id)
        .with_context(|| format!("项目里没有截图 {id}"))?
        .clone();

    let transcript = crate::materials::effective_transcript(opened)
        .ok()
        .map(|(t, _, _)| t);

    let neighbor_text = match &input.neighbor_text {
        Some(text) => Some(text.clone()),
        None => transcript.as_ref().map(|t| {
            t.render_range(
                (shot.t - crate::NEIGHBOR_WINDOW).max(0.0),
                shot.t + crate::NEIGHBOR_WINDOW,
            )
        })
        .filter(|s| !s.is_empty()),
    };

    let evidence = match &input.evidence_quote {
        None => None,
        Some(quote) => {
            let quote = quote.trim();
            if quote.is_empty() {
                bail!("`evidence_quote` 给了空的 —— 要么照抄一句转录原文,要么整个不给");
            }
            let transcript = transcript.as_ref().with_context(|| {
                "要给证据,得先有转录 —— 这个项目还没有可读的转录".to_string()
            })?;
            // **逐字**在一**组**里找(见 `Transcript::locate_quote`)。落下来的段号是
            // 引文**起点**那一段 —— 它就是留给人事后回去核对的定位。
            let Some(hit) = transcript.locate_quote(quote) else {
                bail!(
                    "这段文在转录里找不到:`{quote}`\n\
                     \n\
                     证据必须是**照抄**的一句原文(从 `get_transcript` 里原样搬过来)。\
                     一句话可以横跨几个碎段(whisper 按停顿切)—— 但不能跨组:\
                     跨组就是两句不同的话了。\n\
                     找不到就说明它不是转录里的话 —— 那这条结论就没有凭据,不能这么写。"
                );
            };
            Some(vidbrief_project::Evidence {
                segment: hit.segment,
                quote: quote.to_string(),
            })
        }
    };

    let check = ShotCheck {
        r#match: input.r#match,
        audio_visual_sync: input.audio_visual_sync,
        type_read: input.type_read.clone(),
        neighbor_text,
        issues: input.issues.clone(),
        confidence: input.confidence,
        evidence,
        checked_at: vidbrief_project::now(),
    };

    shots
        .get_mut(id)
        .expect("上面刚查到过,这里必然还在")
        .check = Some(check.clone());
    // 结论进状态(明文那份跟着重写成投影)—— **只此一条路**,
    // 手改 `shots.json` 里那几栏骗不过门(见 devlog [260925-30] 第 2 条)
    crate::materials::save_shots(opened, &shots)?;
    Ok(check)
}

// ── 转录纠错 ─────────────────────────────────────────────────────────

/// `apply_corrections` 的一条结论。
#[derive(Debug, Clone)]
pub struct CorrectionInput {
    /// 转录段 id(`transcript.json` 里的序号)。
    pub segment_id: u32,
    /// 结论。**和落盘的那条是同一个类型** —— 工具层不用自己拼,
    /// 也就拼不出"说要改、改后文本空着"这种自相矛盾的组合。
    pub conclusion: Conclusion,
}

/// 「这一段我审过」的一个**区间**(闭区间:两端都算)。
///
/// # 为什么要有它
///
/// whisper 按停顿切段,切出 1.8 秒一段是常事(实测 70 分钟的视频切出 **2269 段**)。
/// 而门的判据是「每一段都有核验状态」,表态的唯一口子是 `reviewed` —— 于是模型每轮
/// 要吐两千多个数字,实测一次 2.7k token,**对判断毫无贡献**,还把上下文顶穿了
/// (2026-09-27 五份重跑:491k 输入 / 524k 上限,400 撞墙)。
///
/// 区间是同一件事的紧凑写法:读完一片、整片没问题,报一个区间就够。
/// 它**只表达「看过」**,不表达任何结论 —— 真要改字仍然逐条走 `corrections`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewedRange {
    /// 起点(含)。**按段 id**(与 `transcript.json` 里的号一致,从 1 起)。
    pub from: u32,
    /// 终点(含)。
    pub to: u32,
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
    /// 全稿里**改了正文**的段数(累计)。
    pub corrected: usize,
    /// 全稿里**只加了标点 / 断句**的段数(累计,字一个没动)。
    pub punctuated: usize,
    /// 全稿里**决定不改**的段数(累计)。
    pub kept: usize,
    /// 全稿里"存疑但未改"的段数(累计)。
    pub uncertain: usize,
    /// 全稿里**核验过**的段数(累计)。含改过的、决定不改的、存疑的、和没话说的。
    pub reviewed: usize,
}

/// 把 agent 的纠错写进 `02_calibrate/transcript.fixed.json`,并刷新对照表。
///
/// **原始转录永不就地改写**(`docs/schema/README.md`)。本函数读原文、写副本 ——
/// 这样"agent 到底改了什么"永远可查;就地改的话原文就消失了,核对纠错是否合理就没了依据。
///
/// 可以反复调用:每次都在**现有纠错稿**上继续改,所以 agent 可以分批发现、分批写回。
///
/// # `reviewed` / `reviewed_ranges`:审过但没话说的那些段
///
/// `items` 只装"**确实有话说**"的段(改了哪儿 / 哪儿没把握)。可校准里绝大多数段是
/// **审了、没问题** —— 没有地方记它们的话,"审过没问题"和"压根没审"就分不开,
/// 门也就拦不住"跳过了 300 段"。
///
/// 所以这两个入参是"这一段我看过了,不用改",和 `items` 一起提交(它们本来就是
/// 同一趟活:读完一批 → 挑出有问题的 → 其余照过)。改过的段自动算审过,不用重复列。
///
/// 两种写法**取并集**,混用随意:
///
/// - `reviewed: [2, 3, 4]` —— 逐段点名(几段、几十段时好写);
/// - `reviewed_ranges: [{from: 5, to: 300}]` —— 整片区间(whisper 把一句话切碎时
///   常有几百段的片段,逐段报 id 纯属浪费输出 —— 见 [`ReviewedRange`])。
pub fn apply_corrections(
    opened: &mut OpenProject,
    items: &[CorrectionInput],
    reviewed: &[u32],
    ranges: &[ReviewedRange],
) -> Result<CorrectionOutcome> {
    if items.is_empty() && reviewed.is_empty() && ranges.is_empty() {
        bail!("没有要写的东西 —— `corrections`、`reviewed`、`reviewed_ranges` 至少给一个");
    }
    let layout = opened.layout().clone();
    let raw = Transcript::load(&layout.transcript()).with_context(|| {
        format!(
            "读取原始转录失败:{} —— 步 1 可能还没做转录",
            layout.transcript().display()
        )
    })?;

    // 上一批的结论在**状态**里,这里读原文叠一份出来继续改
    //(以前是读上一份纠错稿,现在那份只是给人看的投影 —— 读它等于拿明文当判据)
    let mut fixed = raw.clone();
    opened.state().apply_to_transcript(&mut fixed);

    // 先整体校验一遍再动手:半路失败会留下"改了一半"的文件,比整批拒绝更难收拾
    let known: std::collections::HashSet<u32> = fixed.segments.iter().map(|s| s.id).collect();
    let mut unknown: Vec<u32> = Vec::new();
    for item in items {
        if !known.contains(&item.segment_id) {
            unknown.push(item.segment_id);
        }
    }
    for id in reviewed {
        if !known.contains(id) {
            unknown.push(*id);
        }
    }
    for range in ranges {
        // 空区间先挡:不然它悄悄"什么都没报",调用方还以为审过了
        if range.from > range.to {
            bail!(
                "区间 {}–{} 是空的(起点比终点大)。\
                 闭区间两端都算,所以 [{}, {}] 才表示这两段。",
                range.from,
                range.to,
                range.to,
                range.from
            );
        }
        // **越界要在展开之前挡掉**:一个手滑的 `to`(比如 99999)会让下面转上十万圈。
        // 先看两端在不在 —— 在,就说明这一段落在已有的段号范围里,展开才是有界的。
        let mut edges_ok = true;
        for edge in [range.from, range.to] {
            if !known.contains(&edge) {
                unknown.push(edge);
                edges_ok = false;
            }
        }
        if edges_ok {
            // 段 id 是从 1 起连续的,但这里**不假设**它:逐个核,缺一个就整批拒
            unknown.extend((range.from..=range.to).filter(|id| !known.contains(id)));
        }
    }
    if !unknown.is_empty() {
        unknown.sort_unstable();
        unknown.dedup();
        let shown: Vec<String> = unknown.iter().take(8).map(u32::to_string).collect();
        let tail = if unknown.len() > shown.len() {
            format!(" 等 {} 个", unknown.len())
        } else {
            String::new()
        };
        bail!(
            "这些转录段不存在:{}{tail}(共 {} 段,id 从 1 起)",
            shown.join("、"),
            fixed.segments.len()
        );
    }

    // **每一段的毛病都过一遍,一次列全** —— 不是碰到第一条就退。
    //
    // 实测(2026-09-27,样本 D 那次重跑):模型一次报 26 条,里面 8 条各有各的毛病
    // (5 条 `corrected` 没给 `text`、4 条该 `uncertain` 却写了 `corrected`…),
    // 还两次把同一段报了两遍。工具当时只点名**第一条**,整批被拒 → 它把 26 条原样重发
    // → 又被拒(这次是 `uncertain` 没写 `note`)→ 再重发才过。**三趟的 token 全花在
    // "把同一份内容再说一次"上**,而每条毛病本来一句话就说清了。
    let mut trouble = check_items(items, &fixed);
    if !trouble.is_empty() {
        trouble.sort();
        trouble.dedup();
        bail!(
            "这批结论里有 {} 处要改,一次都列在这儿了(**改完再交一次**即可,别一条一条试):\n\
             \n\
             {}\n\
             \n\
             四种 `outcome` 各自要带什么(一次说清):\n\
             - `corrected` —— 必给 `text`(**改后的整句**,不是只写改动的那个词);\
             `type` 选填;`text` 跟原文一字不差会被拒(要保留原句就换 `kept`),\
             跟原文只差标点也会被拒(那一档叫 `punctuated`)\n\
             - `punctuated` —— 必给 `text`(**加了标点之后的整段**);\
             只许往原文里插标点:去掉标点之后必须跟原文一字不差(动了字就用 `corrected`);\
             这一档不给 `note` / `reason`\n\
             - `kept` —— 必给 `note`(为什么决定不改)\n\
             - `uncertain` —— 必给 `note`(疑似是什么写法)\n\
             除 `punctuated` 外都要给 `reason`(依据:画面文字 / 上下文 / 术语表)。\n\
             **一段只能有一条结论**:同一段有几处改动,合并进同一个 `text`。\n\
             `reviewed` / `reviewed_ranges` 报的是**审过、没问题**的那些段,和上面这四个无关。",
            trouble.len(),
            trouble
                .iter()
                .map(|t| format!("  - {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    // "审过"的戳盖在两处:改过的段(改了当然就是审过)+ 调用方点名/圈区间的那批。
    // **先收成一个集合再盖章**:片段几百段时,逐段去 `contains` 一遍是 O(段×报的),
    // 2269 段 × 2269 个 id 就是五百万次比较 —— 换成人手写就是"看着能跑、实测卡住"。
    let mut touched: std::collections::HashSet<u32> = reviewed.iter().copied().collect();
    for range in ranges {
        touched.extend(range.from..=range.to);
    }
    for item in items {
        touched.insert(item.segment_id);
    }

    for item in items {
        let index = fixed
            .segments
            .iter()
            .position(|s| s.id == item.segment_id)
            .expect("上面刚校验过");
        let original = fixed.segments[index].text.clone();
        fixed.segments[index].conclusion = Some(polish(&item.conclusion, &original));
    }

    let now = vidbrief_project::now();
    for segment in &mut fixed.segments {
        if touched.contains(&segment.id) {
            segment.checked_at = Some(now);
        }
    }

    // 结论进**状态**(`state.bin`);明文那份 `transcript.fixed.json` 退回投影。
    // 顺序要紧:先收状态再写投影,两者同出一源,不会各说各话。
    opened.state_mut().absorb_transcript(&fixed);
    crate::materials::save_transcript_projection(opened, &fixed)?;
    opened.save()?;

    let diff = render_diff(&raw, &fixed);
    let stats = fixed.stats();
    vidbrief_project::write_atomic_str(&layout.transcript_diff(), &diff)
        .context("写入 transcript.diff.md 失败")?;

    Ok(CorrectionOutcome {
        fixed_path: layout.transcript_fixed(),
        diff_path: layout.transcript_diff(),
        total_segments: fixed.segments.len(),
        applied: items.len(),
        corrected: stats.corrected,
        punctuated: stats.punctuated,
        kept: stats.kept,
        uncertain: stats.uncertain,
        reviewed: fixed
            .segments
            .iter()
            .filter(|s| s.checked_at.is_some())
            .count(),
    })
}

/// **把这一批结论里所有不对劲的地方一次查出来**(不落盘、不改数据:调用方拿着这份清单
/// 一次改完再交一遍)。
///
/// 查的四样都是实测踩过的坑:
/// - **"改了"却没给改后的整句** —— 模型最容易漏的一条:它把改动写进 `reason`
///   (「蹦发 → 爆发」)却忘了 `text` 要的是**整句**;
/// - **"改了"但文本和原文一字不差** —— 那不叫改。想保留原句就用"决定不改"
///   (第五轮那份对照表里,27 段"改过"里有 11 段的正文一个字没动);
/// - **`kept` / `uncertain` 却不说为什么 / 疑似是什么** —— 这条记录就没有信息量了;
/// - **同一段报了两次** —— 一段只有一条结论,报两条时后一条会静默盖掉前一条
///   (实测模型想把两处改动分两条报,结果只落了一条)。
///
/// 段号不存在的**不在这里报**:那一条由调用方那边统一核(`这些转录段不存在`),
/// 两处都报会让人以为材料有问题。
fn check_items(items: &[CorrectionInput], fixed: &Transcript) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for item in items {
        if !seen.insert(item.segment_id) {
            out.push(format!(
                "第 {} 段报了两次 —— 一段只能有一条结论,把这一段的几处改动**合并进同一个 `text`**",
                item.segment_id
            ));
            continue;
        }
        let Some(segment) = fixed.segments.iter().find(|s| s.id == item.segment_id) else {
            continue;
        };
        let id = item.segment_id;
        // `reason` 是事后复核的唯一线索 —— 除断句外都得写。schema 那边它现在是**选填**
        // (断句不用给),所以"写没写"这条判在工具这边,和别的毛病一起一次列全。
        if !matches!(&item.conclusion, Conclusion::Punctuated { .. })
            && item.conclusion.reason().trim().is_empty()
        {
            out.push(format!(
                "第 {id} 段没给 `reason`(依据:画面文字 / 上下文 / 术语表)—— \
                 除 `punctuated` 外,每一种结论都要写"
            ));
        }
        match &item.conclusion {
            Conclusion::Corrected { text, .. } => {
                if text.trim().is_empty() {
                    out.push(format!(
                        "第 {id} 段说改了(`outcome = corrected`),却没给 `text` —— \
                         要的是**改后的整句**(不是只写改动的那个词:那是 `reason` 里的东西);\
                         决定不改就换成 `outcome = kept`"
                    ));
                } else if text == &segment.text {
                    out.push(format!(
                        "第 {id} 段的 `text` 和原文一字不差 —— 那不叫改;\
                         要保留原句就换成 `outcome = kept`(并写清为什么决定不改)"
                    ));
                } else if without_punctuation(text) == without_punctuation(&segment.text) {
                    out.push(format!(
                        "第 {id} 段的 `text` 跟原文**只差标点**(字一个没动)—— \
                         这一档有专门的 `outcome = punctuated`,账上要跟'改了字'分开;\
                         换成它,并把 `note` / `reason` 去掉"
                    ));
                }
            }
            Conclusion::Punctuated { text } => {
                if text.trim().is_empty() {
                    out.push(format!(
                        "第 {id} 段说加了断句(`outcome = punctuated`),却没给 `text` —— \
                         要的是**加了标点之后的整段**(不是只给标点)"
                    ));
                } else if text == &segment.text {
                    out.push(format!(
                        "第 {id} 段标了 `punctuated`,可 `text` 跟原文一字不差 —— 一个字都没加。\
                         这一段要是没问题,报进 `reviewed` / `reviewed_ranges` 就够,不用单列一条"
                    ));
                } else if without_punctuation(text) != without_punctuation(&segment.text) {
                    out.push(format!(
                        "第 {id} 段标的是「只加标点」(`outcome = punctuated`),可正文里的**字**动了 —— {}。\
                         改字要用 `outcome = corrected`(那才记 `type` / `note`);\
                         `punctuated` 只许往原文里插标点",
                        first_difference(&segment.text, text)
                    ));
                }
            }
            Conclusion::Kept { note, .. } => {
                if note.trim().is_empty() {
                    out.push(format!(
                        "第 {id} 段决定不改(`outcome = kept`),却没写 `note`(为什么决定不改)"
                    ));
                }
            }
            Conclusion::Uncertain { note, .. } => {
                if note.trim().is_empty() {
                    out.push(format!(
                        "第 {id} 段标了存疑(`outcome = uncertain`),却没写 `note`(疑似是什么写法)"
                    ));
                }
            }
        }
    }
    out
}

/// 第一处**字**不一样的地方,写成一句能照着改的话。
///
/// 只在"标的是只加标点、实际动了字"这条上用到 —— 光说"不一样"没用,
/// 得让它看见是哪一段的哪几个字。比对在**去了标点的两份串**上做,
/// 否则"你加的那个逗号"会被当成第一处不同。
///
/// 全程按字符走:中文一个字 3 字节,按字节下标切会 panic(`AGENTS.md` §1.1)。
fn first_difference(old: &str, new: &str) -> String {
    let a: Vec<char> = without_punctuation(old).chars().collect();
    let b: Vec<char> = without_punctuation(new).chars().collect();
    let at = a
        .iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()));

    let window = |chars: &[char]| -> String {
        let lo = at.saturating_sub(6);
        let hi = (at + 7).min(chars.len());
        let mut out: String = chars[lo..hi].iter().collect();
        if lo > 0 {
            out.insert(0, '…');
        }
        if hi < chars.len() {
            out.push('…');
        }
        out
    };
    format!(
        "第 {} 个字起:原文「{}」,你给的「{}」",
        at + 1,
        window(&a),
        window(&b)
    )
}

/// 把一条结论校准到可以落盘 —— **只补能自动补的**。
///
/// 校验已经在 [`check_items`] 那一趟里做过了(一次列全);走到这里的东西都是合规的,
/// 所以这个函数不再返回错误,只负责:理由/说明两头空白掐掉、`note` 没给就用
/// [`changed_span`] 自动掐出来的改动片段顶上。
fn polish(conclusion: &Conclusion, original: &str) -> Conclusion {
    match conclusion {
        Conclusion::Corrected {
            text,
            kind,
            note,
            reason,
        } => Conclusion::Corrected {
            text: text.clone(),
            kind: *kind,
            note: match note.trim() {
                "" => changed_span(original, text),
                given => given.to_string(),
            },
            reason: reason.trim().to_string(),
        },
        Conclusion::Punctuated { text } => Conclusion::Punctuated {
            text: text.trim().to_string(),
        },
        Conclusion::Kept { note, reason } => Conclusion::Kept {
            note: note.trim().to_string(),
            reason: reason.trim().to_string(),
        },
        Conclusion::Uncertain { note, reason } => Conclusion::Uncertain {
            note: note.trim().to_string(),
            reason: reason.trim().to_string(),
        },
    }
}

/// 掐掉公共前后缀,只留真正改动的那一小段,给结论的 `note` 用。
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

    // 数数只有一处口径 —— 见 `Transcript::stats`。这几个数**加起来等于总段数**,
    // 不会出现"改过 27 + 存疑未改 0 ≠ 合计动过 40"这种事(第五轮实测)。
    let stats = fixed.stats();
    out.push_str(&format!(
        "- 全稿 {} 段:**改了 {} 段**、**加了断句 {} 段**(字一个没动)、决定不改 {} 段、\
         存疑未改 {} 段 —— 合计动过 {} 段\n\n",
        stats.total,
        stats.corrected,
        stats.punctuated,
        stats.kept,
        stats.uncertain,
        stats.touched()
    ));

    if stats.touched() == 0 {
        out.push_str("还没有任何结论。\n");
        return out;
    }

    if stats.rewritten() > 0 {
        out.push_str("## 动了字的\n\n");
        out.push_str("| # | 时刻 | 原文 | 改后 | 类型 | 说明 | 依据 |\n");
        out.push_str("|---|------|------|------|------|------|------|\n");
        for seg in &fixed.segments {
            let Some(conclusion) = &seg.conclusion else {
                continue;
            };
            if matches!(conclusion, Conclusion::Punctuated { .. }) {
                continue; // 断句另开一节:混在一起会把这几行淹掉
            }
            let original = raw
                .segments
                .iter()
                .find(|s| s.id == seg.id)
                .map(|s| s.text.as_str())
                .unwrap_or(&seg.text);
            let after = match conclusion {
                Conclusion::Corrected { text, .. } => cell(text),
                Conclusion::Punctuated { .. } => unreachable!("上面已经跳过"),
                Conclusion::Kept { .. } => "(决定不改,保留原句)".to_string(),
                Conclusion::Uncertain { .. } => "(存疑未改)".to_string(),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                seg.id,
                clock(seg.start),
                cell(original),
                after,
                kind_name(conclusion),
                cell(conclusion.note()),
                cell(conclusion.reason()),
            ));
        }
        out.push('\n');
    }

    // 断句:几百行"原文 → 只是多了逗号"的对照,贴上来只会把上面那张表淹掉。
    // 全量本来就是**纠错稿本身**(逐段可读),这里给个头 20 条让人看一眼手感,并说清这一点。
    let punctuated: Vec<&vidbrief_project::Segment> = fixed
        .segments
        .iter()
        .filter(|s| s.is_punctuated())
        .collect();
    if !punctuated.is_empty() {
        out.push_str(&format!(
            "## 只加了断句的({} 段)\n\n\
             标点加在哪儿,**读纠错稿最清楚**(`{}`,逐段可读)。\
             这里只列前 {} 条,免得把这份对照表撑成几百行。\n\n",
            punctuated.len(),
            vidbrief_project::layout::TRANSCRIPT_FIXED_FILE,
            PUNCTUATED_SHOWN.min(punctuated.len()),
        ));
        out.push_str("| # | 时刻 | 原文 | 加了标点之后 |\n");
        out.push_str("|---|------|------|------|\n");
        for seg in punctuated.iter().take(PUNCTUATED_SHOWN) {
            let original = raw
                .segments
                .iter()
                .find(|s| s.id == seg.id)
                .map(|s| s.text.as_str())
                .unwrap_or(&seg.text);
            out.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                seg.id,
                clock(seg.start),
                cell(original),
                cell(seg.effective_text()),
            ));
        }
    }
    out
}

/// 对照表里断句那一节列多少条(全量在纠错稿里)。
const PUNCTUATED_SHOWN: usize = 20;

/// Markdown 表格单元格:竖线会把列拆掉,换行会把行拆掉,都得先拍平。
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\n', '\r'], " ")
}

fn kind_name(conclusion: &Conclusion) -> &'static str {
    match conclusion.kind() {
        Some(CorrectionType::Homophone) => "同音字",
        Some(CorrectionType::Terminology) => "术语",
        Some(CorrectionType::Number) => "数字",
        Some(CorrectionType::Other) => "其它",
        None => conclusion.label(),
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

/// `write_note` 只许写这两个目录 —— 它们是"笔记"的地盘。
const NOTE_DIRS: [&str; 2] = [
    vidbrief_project::layout::CALIBRATE_DIR,
    vidbrief_project::layout::SUMMARY_DIR,
];

/// **有专属工具的落点**:`write_note` 一律不许碰,哪怕它们就在 [`NOTE_DIRS`] 里面。
///
/// 为什么光查目录不够:这两个文件正好落在允许的两个目录里,而它们偏偏是最不该手写的 ——
/// `summary.md` 要过步 2 的审核门、写完还要标上步 3 的进度;`transcript.diff.md`
/// 是给人核对纠错的**证据**,得像纠错稿一样由 `apply_corrections` 刷。手写它们不是
/// "格式不对",是**门和账一起失效**。
///
/// 这不是假想的:M4 第三轮真 agent 联调里,agent 就是在审核门关着的情况下
/// 用 `write_note` 把 `summary.md` 整个重写掉的 —— 它没打算绕门,它只是想改个文件。
/// 一道"顺手就能绕过去、而且绕的人自己都不知道"的门不算门。
///
/// 每项是 `(路径, 该走的工具, 绕过它连带失效的东西)`。
const OWNED_BY_TOOLS: [(&str, &str, &str); 2] = [
    (
        vidbrief_project::layout::SUMMARY_FILE,
        "write_summary",
        "步 2 的审核门和步 3 的进度",
    ),
    (
        vidbrief_project::layout::TRANSCRIPT_DIFF_FILE,
        "apply_corrections",
        "这份证据与纠错稿的一致",
    ),
];

/// 写一份 Markdown 到项目里(`write_note`)。
///
/// `rel` 必须是项目根相对路径且落在允许的目录里 —— 路径校验有三道:
/// [`RelPath`] 保证不越出项目根(挡目录穿越),这里再挡掉 [`OWNED_BY_TOOLS`]
/// (有专属工具的文件),最后限一层"只能写哪几个目录",免得 agent 顺手把
/// `project.json` 覆盖掉。
///
/// [`RelPath`] 不做归一化(`.` / `..` / 反斜杠 / 空段全被它拒了),所以 `as_str()`
/// 已经是规范形式,**精确串比就是严的** —— 不需要再考虑"换个写法绕过去"。
pub fn write_note(
    opened: &mut OpenProject,
    rel: &RelPath,
    md: &str,
    mode: WriteMode,
    count_round: bool,
) -> Result<PathBuf> {
    let path = rel.as_str();

    if let Some((_, tool, also)) = OWNED_BY_TOOLS.iter().find(|(f, _, _)| *f == path) {
        bail!(
            "`{path}` 有自己的工具(`{tool}`),不能走 write_note —— \
             它写这个文件的同时还管着{also},绕过它,那些也就一起失效了"
        );
    }

    // 比的是**路径段**而不是字符串:不然 `02_calibrate_extra/` 会从
    // `02_calibrate` 这个前缀底下溜过去。
    let in_note_dir = NOTE_DIRS
        .iter()
        .any(|dir| path.strip_prefix(dir).is_some_and(|rest| rest.starts_with('/')));
    if !in_note_dir {
        bail!(
            "只能写在 {NOTE_DIRS:?} 下面,不允许写 `{path}` —— 材料文件有自己的工具,别绕过它们"
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
        opened.state_mut().progress.calibrate.bump();
        opened.save()?;
    }

    Ok(full)
}

/// 交上来的一段总结(见 [`write_summary`])。
#[derive(Debug, Clone, Default)]
pub struct SectionInput {
    /// 这一节讲的是哪一刻(秒)。**不给 = 总述**(不上时间轴,只进右侧面板)。
    pub t: Option<f64>,
    /// 这一段的名字(页面上那条的标题;改稿时按它认段)。
    pub title: String,
    /// 正文(markdown)。
    pub md: String,
    /// 待核验的点(看不清但结论靠它的数字……)。没有就空着。
    pub verify: Vec<String>,
}

/// 落一段之后回什么。
#[derive(Debug, Clone)]
pub struct SummaryOutcome {
    /// 分段数据落点(`03_summary/sections.json`)—— 报告页从它组装。
    pub sections_path: PathBuf,
    /// 拼好的文章(`03_summary/summary.md`)。
    pub article_path: PathBuf,
    /// 现在一共几段。
    pub sections: usize,
    /// 其中上时间轴的几段(其余是总述)。
    pub anchored: usize,
    /// 这一次是新加的一段,还是改写了同名的那段。
    pub added: bool,
}

/// 交**一段**总结(一小节)—— 分段提交,写完一段落一段。
///
/// # 为什么是分段(2026-09-27 改)
///
/// 从前是一次交整篇 markdown(实测一次 15KB)。两个后果:①一发很大、断了要重来;
/// ②**报告那一步没有结构可用** —— 报告页要往时间轴上挂点,而一篇散文里没有
/// "哪一段对应哪一刻",于是步 4 只能回头再问模型要一遍结构化数据(又一发十几 KB)。
/// 同一份内容写两遍,还逼着模型当打字机。
///
/// 现在:**写的时候就把结构带上**。一段 = 一个小节(讲哪一刻、什么标题、正文),
/// 写完提交;报告只是把它摆到页面上。
///
/// # 认段
///
/// 按**标题**认:同名 = 改写那一段(原地,顺序不变),新标题 = 追加。
/// 为什么不用序号:改稿时说「把《两种方案》那段改一下」比「把第 7 段改一下」稳。
///
/// # 门与底账
///
/// **先过步 2 的门**(`review::gate_calibrate`);每次落盘都刷新 `summary.md`
/// (拼好的那篇文章,人读它)与它的**内容底账** —— 步 3→4 那道门问的就是
/// "出报告之前,这份总结还是工具写出去的那一份吗"。
pub fn write_summary(opened: &mut OpenProject, input: &SectionInput) -> Result<SummaryOutcome> {
    crate::review::gate_calibrate(opened)?;

    let title = input.title.trim();
    if title.is_empty() {
        bail!("这一段没给标题 —— 它就是页面上那条的名字,也是以后改稿时认它的凭据");
    }
    let md = input.md.trim();
    if md.is_empty() {
        bail!("第「{title}」段没给正文 —— 一段空话交上去,页面上就是一片空白");
    }
    if let Some(t) = input.t {
        if !t.is_finite() || t < 0.0 {
            bail!("第「{title}」段的时刻 `t` 不合法:{t}");
        }
        let duration = opened.meta().duration;
        if duration > 0.0 && t > duration + 1.0 {
            bail!(
                "第「{title}」段的时刻 {t} 落在视频之外(整支 {duration:.1}s)——\
                 报告页上那个圆点会飘在时间轴外头。\n\
                 想写总述那种不挂时刻的段,就别给 `t`。"
            );
        }
    }

    let path = opened.layout().summary_sections();
    let mut sections = if path.is_file() {
        vidbrief_project::Sections::load(&path)
            .with_context(|| format!("读 {} 失败", path.display()))?
    } else {
        vidbrief_project::Sections::new()
    };
    let added = sections.upsert(vidbrief_project::Section {
        t: input.t,
        title: title.to_string(),
        md: md.to_string(),
        verify: input
            .verify
            .iter()
            .map(|one| one.trim().to_string())
            .filter(|one| !one.is_empty())
            .collect(),
    });
    sections
        .save(&path)
        .with_context(|| format!("写 {} 失败", path.display()))?;

    // `summary.md` 仍是"给人读的那篇文章":总述在前、其余按时刻 —— 每次落段都重拼
    let article = sections.to_markdown();
    let md_path = opened.layout().summary_md();
    vidbrief_project::write_atomic_str(&md_path, &article).context("写入 summary.md 失败")?;
    opened.state_mut().note_content(
        vidbrief_project::layout::SUMMARY_FILE,
        vidbrief_project::doc_print(&article),
    );
    opened.state_mut().progress.summarize.mark_done();
    opened.save()?;

    Ok(SummaryOutcome {
        sections_path: path,
        article_path: md_path,
        sections: sections.sections.len(),
        anchored: sections.anchored().len(),
        added,
    })
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

    /// **"改成跟原文一样"不是改。** 想保留原句就用 `kept` —— 这一条挡掉的正是
    /// 第五轮里那 11 段"结论说要改、正文一个字没动"的空操作。
    #[test]
    fn a_no_op_correction_is_refused() {
        let fake = Fake::new("concl-noop").with_transcript(&[(0.0, 2.0, "派森列表")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Corrected {
                    text: "派森列表".into(), // 和原文一字不差
                    kind: CorrectionType::Other,
                    note: String::new(),
                    reason: "测试".into(),
                },
            }],
            &[], &[])
        .unwrap_err()
        .to_string();
        assert!(err.contains("一字不差"), "{err}");
        assert!(err.contains("kept"), "要告诉它改用哪个:{err}");

        // 换成"决定不改"就收,而且正文一个字不动
        apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Kept {
                    note: "上一轮那处改写是错的".into(),
                    reason: "原句通顺".into(),
                },
            }],
            &[], &[])
        .unwrap();
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "派森列表");
        assert_eq!(fixed.stats().corrected, 0);
        assert_eq!(fixed.stats().kept, 1);
    }

    /// 一份纠错稿里四种结论各来一条,表头的四个数**加起来等于总段数**。
    #[test]
    fn the_diff_header_adds_up() {
        let fake = Fake::new("concl-header").with_transcript(&[
            (0.0, 2.0, "派森列表"),
            (2.0, 4.0, "元祖笔记"),
            (4.0, 6.0, "原句通顺"),
            (6.0, 8.0, "各位好"),
        ]);
        let mut opened = fake.open();

        apply_corrections(
            &mut opened,
            &[
                CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Corrected {
                        text: "Python 列表".into(),
                        kind: CorrectionType::Terminology,
                        note: String::new(),
                        reason: "画面文字".into(),
                    },
                },
                CorrectionInput {
                    segment_id: 2,
                    conclusion: Conclusion::Kept {
                        note: "撤销上一轮的改写".into(),
                        reason: "原句通顺".into(),
                    },
                },
                CorrectionInput {
                    segment_id: 3,
                    conclusion: Conclusion::Uncertain {
                        note: "「原句通顺」疑为「原句通顺,不用改」".into(),
                        reason: "同音,没把握".into(),
                    },
                },
            ],
            &[4], &[])
        .unwrap();

        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        let s = fixed.stats();
        assert_eq!((s.corrected, s.kept, s.uncertain, s.clean, s.unchecked), (1, 1, 1, 1, 0));
        assert_eq!(s.touched(), 3);

        let diff = std::fs::read_to_string(fake.layout().transcript_diff()).unwrap();
        let header = diff.lines().find(|l| l.contains("全稿")).unwrap().to_string();
        assert!(header.contains("改了 1 段"), "{header}");
        assert!(header.contains("决定不改 1 段"), "{header}");
        assert!(header.contains("存疑未改 1 段"), "{header}");
        assert!(header.contains("合计动过 3 段"), "{header}");
        // 三个数加起来就是"动过"那个数 —— 第五轮那次 27+0≠40 正是缺了这一条
        assert!(header.contains("全稿 4 段"), "{header}");
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
            &mut opened,
            &[
                CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Corrected {
                        text: "Python 列表".into(),
                        kind: CorrectionType::Terminology,
                        note: String::new(),
                        reason: "截图画面写着「Python 列表」".into(),
                    },
                },
                CorrectionInput {
                    segment_id: 2,
                    conclusion: Conclusion::Uncertain {
                        note: "疑似「元组」".into(),
                        reason: "上下文在讲培训课程".into(),
                    },
                },
            ],
            &[], &[])
        .unwrap();

        assert_eq!(outcome.applied, 2);
        assert_eq!(outcome.corrected, 1);
        assert_eq!(outcome.kept, 0);
        assert_eq!(outcome.uncertain, 1);
        assert_eq!(outcome.total_segments, 2);

        // 原文不动
        let raw = Transcript::load(&fake.layout().transcript()).unwrap();
        assert_eq!(raw.segments[0].text, "派森列表");
        assert!(raw.segments[0].conclusion.is_none());

        // 副本改了
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "Python 列表");
        assert_eq!(fixed.segments[1].effective_text(), "元祖笔记");
        assert!(fixed.segments[1].is_uncertain());

        // 对照表里两条都在,且标出了"未改,存疑"
        let diff = std::fs::read_to_string(fake.layout().transcript_diff()).unwrap();
        assert!(diff.contains("派森列表"), "{diff}");
        assert!(diff.contains("Python 列表"), "{diff}");
        assert!(diff.contains("(存疑未改)"), "{diff}");
        assert!(diff.contains("派森 → Python "), "自动掐出来的改动片段:{diff}");
    }

    #[test]
    fn corrections_accumulate_across_calls() {
        let fake = Fake::new("concl-accum").with_transcript(&[(0.0, 2.0, "甲"), (2.0, 4.0, "乙")]);
        let mut opened = fake.open();

        apply_corrections(
            &mut opened,
            &[CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Corrected {
                        text: "甲甲".into(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "第一次".into(),
                    },
                }],
            &[], &[])
        .unwrap();
        let second = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                    segment_id: 2,
                    conclusion: Conclusion::Corrected {
                        text: "乙乙".into(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "第二次".into(),
                    },
                }],
            &[], &[])
        .unwrap();

        assert_eq!(second.corrected, 2, "第二次不该把第一次的结论抹掉");
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "甲甲");
        assert_eq!(fixed.segments[1].effective_text(), "乙乙");
    }

    #[test]
    fn unknown_segment_ids_write_nothing() {
        let fake = Fake::new("concl-unknown").with_transcript(&[(0.0, 2.0, "甲")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[
                CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Corrected {
                        text: "甲甲".into(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "好的那条".into(),
                    },
                },
                CorrectionInput {
                    segment_id: 99,
                    conclusion: Conclusion::Corrected {
                        text: "无中生有".into(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "坏的那条".into(),
                    },
                },
            ],
            &[], &[])
        .unwrap_err()
        .to_string();

        assert!(err.contains("99"), "{err}");
        assert!(
            !fake.layout().transcript_fixed().exists(),
            "整批拒绝时不该落下半个文件"
        );
    }

    // ── `reviewed_ranges`:整片报「审过」(2026-09-27,清账十六) ──────────

    /// 三段的区间报上去,那三段就是"审过、没问题";区间外的照旧是"没审"。
    ///
    /// 这条要钉的是**它真的等价于逐段点名**,不是"另记了一笔区间账" ——
    /// 门的判据只有每段的 `checked_at`,区间必须展开到段上(见 `ReviewedRange`)。
    #[test]
    fn a_reviewed_range_marks_every_segment_in_it() {
        let fake = Fake::new("concl-range").with_transcript(&[
            (0.0, 2.0, "甲"),
            (2.0, 4.0, "乙"),
            (4.0, 6.0, "丙"),
            (6.0, 8.0, "丁"),
            (8.0, 10.0, "戊"),
        ]);
        let mut opened = fake.open();

        let outcome = apply_corrections(
            &mut opened,
            &[],
            &[],
            &[ReviewedRange { from: 2, to: 4 }],
        )
        .unwrap();

        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.reviewed, 3);
        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        let s = fixed.stats();
        assert_eq!((s.clean, s.unchecked), (3, 2));
        assert_eq!(fixed.segments[1].checked_at.is_some(), true);
        assert_eq!(fixed.segments[0].checked_at.is_none(), true);
        assert_eq!(fixed.segments[4].checked_at.is_none(), true);
        // 区间只表态「看过」,一个字都不改
        assert_eq!(fixed.segments[1].effective_text(), "乙");
        assert_eq!(s.corrected + s.kept + s.uncertain, 0);
    }

    /// 逐段点名与区间**取并集**,而且重叠、越段都不算错 —— 分批报时天然会重叠。
    #[test]
    fn ids_and_ranges_unite_and_may_overlap() {
        let fake = Fake::new("concl-range-mix")
            .with_transcript(&[(0.0, 2.0, "甲"), (2.0, 4.0, "乙"), (4.0, 6.0, "丙"), (6.0, 8.0, "丁")]);
        let mut opened = fake.open();

        apply_corrections(
            &mut opened,
            &[],
            &[1],
            &[
                ReviewedRange { from: 3, to: 4 },
                ReviewedRange { from: 2, to: 3 }, // 与上一条重叠,也接上了点名的 1
            ],
        )
        .unwrap();

        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.stats().unchecked, 0, "四条都该被点到");
    }

    /// 空区间(`[5, 4]`)整批拒 —— 悄悄"什么都没报"的话,调用方还以为审过了。
    #[test]
    fn an_empty_range_is_refused() {
        let fake = Fake::new("concl-range-empty").with_transcript(&[(0.0, 2.0, "甲"), (2.0, 4.0, "乙")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[],
            &[],
            &[ReviewedRange { from: 5, to: 4 }],
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("空的"), "{err}");
        assert!(err.contains("[4, 5]"), "要顺手告诉它怎么写:{err}");
        assert!(!fake.layout().transcript_fixed().exists());
    }

    /// 越界的区间整批拒 —— 而且**不许先展开再判**:一个手滑的 `to` 不该让工具转上十万圈。
    #[test]
    fn a_range_past_the_end_is_refused() {
        let fake = Fake::new("concl-range-over").with_transcript(&[(0.0, 2.0, "甲"), (2.0, 4.0, "乙")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[],
            &[],
            &[ReviewedRange { from: 1, to: 4_000_000_000 }],
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("4000000000"), "{err}");
        assert!(err.contains("这些转录段不存在"), "{err}");
        assert!(!fake.layout().transcript_fixed().exists());
    }

    /// **一批里所有毛病一次列全** —— 不是碰到第一条就退。
    ///
    /// 实测(2026-09-27,样本 D 重跑):模型一次报 26 条、其中 8 条各有各的毛病,
    /// 而工具只点名第一条 → 它把 26 条**原样重发** → 又被拒(这次是 `uncertain` 没写 `note`)
    /// → 再重发才过。三趟的 token 全花在"把同一份内容再说一次"上。
    #[test]
    fn every_problem_in_the_batch_is_reported_at_once() {
        let fake = Fake::new("concl-all-at-once").with_transcript(&[
            (0.0, 2.0, "甲"),
            (2.0, 4.0, "乙"),
            (4.0, 6.0, "丙"),
            (6.0, 8.0, "丁"),
            (8.0, 10.0, "戊"),
            (10.0, 12.0, "己"),
        ]);
        let mut opened = fake.open();
        let err = apply_corrections(
            &mut opened,
            &[
                // ① 说改了,却只把改动写进 `reason`
                CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Corrected {
                        text: String::new(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "蹦发 → 爆发".into(),
                    },
                },
                // ② 决定不改,却不写为什么
                CorrectionInput {
                    segment_id: 2,
                    conclusion: Conclusion::Kept {
                        note: String::new(),
                        reason: "原句通顺".into(),
                    },
                },
                // ③ 标了存疑,却不写疑似写法
                CorrectionInput {
                    segment_id: 3,
                    conclusion: Conclusion::Uncertain {
                        note: String::new(),
                        reason: "听不清".into(),
                    },
                },
                // ④ 同一段报了两次:后一条会静默盖掉前一条
                CorrectionInput {
                    segment_id: 4,
                    conclusion: Conclusion::Kept {
                        note: "不改".into(),
                        reason: "原句通顺".into(),
                    },
                },
                CorrectionInput {
                    segment_id: 4,
                    conclusion: Conclusion::Kept {
                        note: "第二次".into(),
                        reason: "同上".into(),
                    },
                },
                // ⑤ 标着"只加标点",实际把字改动了
                CorrectionInput {
                    segment_id: 5,
                    conclusion: Conclusion::Punctuated {
                        text: "戊戊。".into(),
                    },
                },
                // ⑥ 走 `corrected`,却跟原文只差标点
                CorrectionInput {
                    segment_id: 6,
                    conclusion: Conclusion::Corrected {
                        text: "己。".into(),
                        kind: CorrectionType::Other,
                        note: String::new(),
                        reason: "断句".into(),
                    },
                },
            ],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();

        for (who, what) in [
            ("第 1 段", "却没给 `text`"),
            ("第 2 段", "却没写 `note`"),
            ("第 3 段", "却没写 `note`"),
            ("第 4 段", "报了两次"),
            ("第 5 段", "字**动了"),
            ("第 6 段", "只差标点"),
        ] {
            assert!(err.contains(who) && err.contains(what), "{who}{what} 没报出来:\n{err}");
        }
        // 四种 outcome 的规矩也一并说清 —— 一次交完,省得再来回
        assert!(err.contains("一次说清"), "{err}");
        assert!(err.contains("改完再交一次"), "{err}");
        assert!(err.contains("punctuated"), "断句那一档的规矩要说清:{err}");
        assert!(
            !fake.layout().transcript_fixed().exists(),
            "整批拒绝时不该落下半个文件"
        );
    }

    #[test]
    fn uncertain_without_note_is_refused() {
        let fake = Fake::new("concl-nonote").with_transcript(&[(0.0, 2.0, "甲")]);
        let mut opened = fake.open();
        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                    segment_id: 1,
                    conclusion: Conclusion::Uncertain {
                        note: String::new(),
                        reason: "说不上来".into(),
                    },
                }],
            &[], &[])
        .unwrap_err()
        .to_string();
        assert!(err.contains("note"), "{err}");
    }

    // ── 只加标点 / 断句(2026-09-27,清账三十八)──────────────────────────

    /// **断句是单独一档**:字一个没动,但它算"动过"、算"审过"。
    #[test]
    fn punctuation_is_recorded_apart_from_corrections() {
        let fake = Fake::new("concl-punct").with_transcript(&[
            (0.0, 2.0, "今天我们来讲一个列表"),
            (2.0, 4.0, "之所以把它放在一开始"),
        ]);
        let mut opened = fake.open();

        let outcome = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Punctuated {
                    text: "今天我们来讲一个列表。".into(),
                },
            }],
            &[2],
            &[],
        )
        .unwrap();

        assert_eq!(
            (outcome.punctuated, outcome.corrected),
            (1, 0),
            "断句不该混进'改了'"
        );
        assert_eq!(outcome.reviewed, 2, "加了断句的段当然算审过");

        let fixed = Transcript::load(&fake.layout().transcript_fixed()).unwrap();
        assert_eq!(fixed.segments[0].effective_text(), "今天我们来讲一个列表。");
        assert_eq!(fixed.segments[0].text, "今天我们来讲一个列表", "原文一个字不动");
        assert!(fixed.segments[0].is_punctuated());
        let s = fixed.stats();
        assert_eq!((s.punctuated, s.corrected, s.clean), (1, 0, 1));

        // 对照表:断句另开一节,并说清全量在纠错稿里(不把表撑成几百行)
        let diff = std::fs::read_to_string(fake.layout().transcript_diff()).unwrap();
        assert!(diff.contains("只加了断句的(1 段)"), "{diff}");
        assert!(diff.contains("加了断句 1 段"), "{diff}");
    }

    /// 标点里**顺手改了个字** → 拒,并指到具体是第几个字。
    #[test]
    fn punctuation_that_sneaks_in_a_character_is_refused() {
        let fake = Fake::new("concl-punct-char").with_transcript(&[(0.0, 2.0, "今天我们来讲一个列表")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                // 把「列表」改成了「例子」 —— 那不叫断句
                conclusion: Conclusion::Punctuated {
                    text: "今天我们来讲一个例子。".into(),
                },
            }],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("第 9 个字起"), "要指出是哪儿动了:\n{err}");
        assert!(err.contains("corrected"), "要告诉它改用哪一个:\n{err}");
        assert!(
            !fake.layout().transcript_fixed().exists(),
            "整批拒绝时不该落下半个文件"
        );
    }

    /// 没加任何标点 → 拒(没问题的段报 `reviewed` 就够,不该单列一条)。
    #[test]
    fn punctuation_that_adds_nothing_is_refused() {
        let fake = Fake::new("concl-punct-noop").with_transcript(&[(0.0, 2.0, "各位好")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Punctuated {
                    text: "各位好".into(),
                },
            }],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("一字不差"), "{err}");
        assert!(err.contains("reviewed"), "要告诉它该报进哪里:{err}");
    }

    /// `corrected` 跟原文**只差标点** → 拒,并点名该换成 `punctuated`。
    #[test]
    fn a_correction_that_only_adds_punctuation_is_refused() {
        let fake = Fake::new("concl-punct-as-corrected").with_transcript(&[(0.0, 2.0, "各位好")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Corrected {
                    text: "各位好,".into(),
                    kind: CorrectionType::Other,
                    note: String::new(),
                    reason: "断句".into(),
                },
            }],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("只差标点"), "{err}");
        assert!(err.contains("punctuated"), "要告诉它改用哪一个:{err}");
    }

    /// `reason` 现在是选填(断句不用给)—— "其余三种必须写"这条就落在工具这边。
    #[test]
    fn a_missing_reason_is_reported() {
        let fake = Fake::new("concl-noreason").with_transcript(&[(0.0, 2.0, "甲")]);
        let mut opened = fake.open();

        let err = apply_corrections(
            &mut opened,
            &[CorrectionInput {
                segment_id: 1,
                conclusion: Conclusion::Kept {
                    note: "不改".into(),
                    reason: String::new(),
                },
            }],
            &[],
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("没给 `reason`"), "{err}");
    }

    #[test]
    fn record_shot_check_fills_neighbor_text_by_itself() {
        let fake = Fake::new("concl-shot")
            .with_transcript(&[(8.0, 12.0, "这里是 Python 列表结构")])
            .with_shot(1, 9.0);
        let mut opened = fake.open();

        let check = record_shot_check(
            &mut opened,
            vidbrief_project::ShotId::new(1).unwrap(),
            &ShotCheckInput {
                r#match: true,
                audio_visual_sync: true,
                type_read: "Python 列表 结构".into(),
                issues: vec![],
                confidence: Confidence::High,
                neighbor_text: None,
                evidence_quote: None,
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

        // 落进**状态**了(明文那份跟着重写成投影,读回来的也是它)
        let shots = crate::materials::shots(&opened).unwrap();
        assert!(shots.get(vidbrief_project::ShotId::new(1).unwrap()).unwrap().check.is_some());
        assert!(
            fake.state().shot(vidbrief_project::ShotId::new(1).unwrap()).unwrap().check.is_some(),
            "结论必须在状态里 —— 明文那几栏只是投影"
        );
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

    /// **有专属工具的文件,写在允许的目录里也不行。**
    ///
    /// 这条曾经漏掉:判据是"路径开头是不是那两个目录",而 `summary.md` 与
    /// `transcript.diff.md` 正好落在里面 —— 于是 agent 能在审核门关着的时候
    /// 用 `write_note` 把 `summary.md` 整个重写掉(M4 第三轮真 agent 联调实测,
    /// 而且它自己都没意识到绕了门)。报错必须点出**该走哪个工具**,否则调用方
    /// 只会觉得"这里不让写",转头再找个别的写入口。
    #[test]
    fn write_note_refuses_files_that_have_their_own_tool() {
        let fake = Fake::new("concl-note-owned");
        let mut opened = fake.open();

        for (path, tool) in [
            (
                vidbrief_project::layout::SUMMARY_FILE,
                "write_summary",
            ),
            (
                vidbrief_project::layout::TRANSCRIPT_DIFF_FILE,
                "apply_corrections",
            ),
        ] {
            let err = write_note(
                &mut opened,
                &rel(path).unwrap(),
                "# 手写的\n",
                WriteMode::Overwrite,
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(tool), "{path} 该告诉它走哪个工具:{err}");
            assert!(
                !fake.layout().rel(path).is_file(),
                "{path} 被拒了却还是写下去了"
            );
        }

        // 同一个目录里**没有**专属工具的那些照写不误 —— 别把整片地都封了
        for ok in [
            vidbrief_project::layout::CROSS_VALIDATE_FILE,
            vidbrief_project::layout::AGENT_LOG_FILE,
            "03_summary/notes/案例一.md",
        ] {
            write_note(&mut opened, &rel(ok).unwrap(), "笔记", WriteMode::Overwrite, false)
                .unwrap_or_else(|e| panic!("{ok} 该能写:{e:#}"));
        }
    }

    /// 目录判据比的是**路径段**,不是字符串前缀。
    ///
    /// 不然 `02_calibrate_extra/`(以及任何以这两个名字开头的兄弟目录)会从
    /// 前缀底下溜过去。
    #[test]
    fn write_note_matches_whole_path_segments_not_just_a_prefix() {
        let fake = Fake::new("concl-note-prefix");
        let mut opened = fake.open();

        for bad in ["02_calibrate_extra/x.md", "03_summary_backup/x.md"] {
            let err = write_note(
                &mut opened,
                &rel(bad).unwrap(),
                "x",
                WriteMode::Overwrite,
                false,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("只能写在"), "{bad} 靠前缀溜进去了:{err}");
        }
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
        assert_eq!(fake.state().progress.calibrate.rounds, 2);
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
        assert_eq!(fake.state().progress.calibrate.rounds, 0);
    }

    /// 步 2 的活干完(图核过、转录报过审)—— 写总结那道门的前置条件。
    ///
    /// 走的是**正经工具**(`record_shot_check` / `apply_corrections`),不是直接改状态:
    /// 门读的就是工具写下的那一份(见 `testkit` 里那几个助手的说明)。
    fn calibrated(tag: &str) -> Fake {
        let fake = Fake::new(tag)
            .with_transcript(&[(0.0, 2.0, "各位好"), (2.0, 4.0, "接着讲")])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        record_shot_check(
            &mut opened,
            vidbrief_project::ShotId::new(1).unwrap(),
            &ShotCheckInput {
                r#match: true,
                audio_visual_sync: true,
                type_read: "画面内容".into(),
                issues: Vec::new(),
                confidence: Confidence::High,
                neighbor_text: None,
                evidence_quote: Some("各位好".into()),
            },
        )
        .unwrap();
        apply_corrections(&mut opened, &[], &[1, 2], &[]).unwrap();
        fake
    }

    /// 一段总结的输入(标题 + 正文,可选锚点)。
    fn a_section(t: Option<f64>, title: &str, md: &str) -> SectionInput {
        SectionInput {
            t,
            title: title.into(),
            md: md.into(),
            verify: Vec::new(),
        }
    }

    /// 落一段总结 —— 拿它的结果说事时不必每次都写一遍 `unwrap`。
    fn write_one(opened: &mut OpenProject, input: SectionInput) -> SummaryOutcome {
        write_summary(opened, &input).unwrap_or_else(|e| panic!("落分段失败:{e:#}"))
    }

    #[test]
    fn summary_marks_its_step_and_leaves_a_ledger() {
        let fake = Fake::new("concl-steps")
            .with_transcript(&[(0.0, 2.0, "各位好")])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();

        // 步 2 的门:这一步的活没干完就不许写总结。
        // 这里那张图还没核验,所以挡的是"还没核验完"那一档 —— 不需要任何人出面。
        let blocked = write_summary(&mut opened, &a_section(None, "总述", "# 总结"))
            .unwrap_err()
            .to_string();
        assert!(blocked.contains("还没核验完"), "{blocked}");
        assert!(
            !fake.layout().summary_md().is_file(),
            "被挡住的时候**不该留下半成品**"
        );
        assert!(
            !fake.layout().summary_sections().is_file(),
            "分段数据也一样:被挡住就一个字节都不落"
        );

        // 把活干完 —— 走正经工具写回结论,**不用签字**,门自己就开
        record_shot_check(
            &mut opened,
            vidbrief_project::ShotId::new(1).unwrap(),
            &ShotCheckInput {
                r#match: true,
                audio_visual_sync: true,
                type_read: "画面内容".into(),
                issues: Vec::new(),
                confidence: Confidence::High,
                neighbor_text: None,
                evidence_quote: Some("各位好".into()),
            },
        )
        .unwrap();
        // 转录那一段也报一声"审过了" —— 门分不出"看过没问题"和"压根没看"
        apply_corrections(&mut opened, &[], &[1], &[]).unwrap();
        write_one(&mut opened, a_section(None, "总述", "# 总结"));

        let state = fake.state();
        assert!(state.progress.summarize.done);
        assert!(fake.layout().summary_md().is_file());
        assert!(
            fake.layout().summary_sections().is_file(),
            "分段数据要落 —— 报告页从它组装"
        );
        // 底账记的是**这一份正文**(拼好的那篇文章)—— 步 3→4 那道门就靠它对账
        assert_eq!(
            state.content.get(vidbrief_project::layout::SUMMARY_FILE),
            Some(&vidbrief_project::doc_print("# 总结\n")),
            "写总结要顺手记下它的内容底账"
        );
    }

    /// 分段落盘:一次交一段;同名 = 改写那一段,新标题 = 追加。
    ///
    /// 守的是"认段按标题"这条规矩(报告页按它认条):改稿时说「把《切到解释器主窗口》
    /// 那段改一下」比说「把第 7 段改一下」稳 —— 人记标题,不记序号。
    #[test]
    fn sections_land_one_at_a_time_and_the_same_title_rewrites_that_one() {
        let fake = calibrated("concl-sections");
        let mut opened = fake.open();

        let first = write_one(
            &mut opened,
            SectionInput {
                t: Some(2.0),
                title: "切到解释器主窗口".into(),
                md: "讲者从大纲切到解释器主窗口。".into(),
                verify: vec!["  画面左下角的版本号看不清,核一下 s001  ".into()],
            },
        );
        assert!(first.added, "第一次是新加的一段");
        assert_eq!((first.sections, first.anchored), (1, 1), "这一段上时间轴");
        assert!(first.sections_path.is_file(), "分段文件要落");
        assert!(
            first.article_path.is_file(),
            "给人读的那篇文章也要跟着重拼"
        );

        let second = write_one(&mut opened, a_section(None, "总述", "整支讲一件事。"));
        assert_eq!(
            (second.sections, second.anchored),
            (2, 1),
            "总述算一段,但不上时间轴"
        );

        // 同名再落一遍 = 改写那一段(段数不动)
        let again = write_one(
            &mut opened,
            a_section(Some(2.0), "切到解释器主窗口", "改过一遍的正文。"),
        );
        assert!(!again.added, "同名该是改写,不是新增");
        assert_eq!(again.sections, 2, "段数不该变");

        let sections =
            vidbrief_project::Sections::load(&fake.layout().summary_sections()).unwrap();
        let one = sections
            .sections
            .iter()
            .find(|s| s.title == "切到解释器主窗口")
            .expect("那一段该在");
        assert_eq!(one.md, "改过一遍的正文。");
        assert!(
            one.verify.is_empty(),
            "改写是整段替换:上一次的待核验跟着走"
        );

        // `summary.md` 是给人读的那一篇:总述在前,其余按时刻
        let article = std::fs::read_to_string(fake.layout().summary_md()).unwrap();
        let overview = article.find("整支讲一件事").expect("总述要在");
        let anchored = article.find("改过一遍的正文").expect("锚点段要在");
        assert!(overview < anchored, "文章顺序该是 总述 → 时刻序:{article}");
    }

    /// **渲染不出来的形状问题挡在写入口**:标题或正文空着、时刻不合法或落在视频外。
    ///
    /// 从前这几条判在 `build_report` 那一侧(组装报告时才发现)。2026-09-27 改成分段之后,
    /// 一段的形状在**写的时候**就定下了 —— 挡在这儿,坏数据根本进不了 `sections.json`,
    /// 报告那一侧也就不必再判一遍。
    #[test]
    fn a_section_without_a_title_body_or_a_sane_anchor_is_refused() {
        let fake = calibrated("concl-section-bad");
        let mut opened = fake.open();

        let bad: [(SectionInput, &str); 4] = [
            (a_section(Some(2.0), "  ", "正文"), "没给标题"),
            (a_section(Some(2.0), "标题", "   "), "没给正文"),
            (a_section(Some(f64::NAN), "标题", "正文"), "不合法"),
            (a_section(Some(10_000.0), "标题", "正文"), "落在视频之外"),
        ];
        for (input, needle) in bad {
            let err = write_summary(&mut opened, &input).unwrap_err().to_string();
            assert!(err.contains(needle), "该被拦下的是「{needle}」:{err}");
            assert!(
                !fake.layout().summary_sections().is_file(),
                "整批退回时不该落下半个分段文件"
            );
            assert!(
                !fake.layout().summary_md().is_file(),
                "给人读的那篇文章也不该落"
            );
        }
    }

    #[test]
    fn clock_matches_the_timeline_format() {
        assert_eq!(clock(0.0), "00:00.0");
        assert_eq!(clock(188.6), "03:08.6");
        assert_eq!(clock(985.43), "16:25.4");
        assert_eq!(clock(-1.0), "00:00.0");
    }
}
