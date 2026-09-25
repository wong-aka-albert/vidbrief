//! 用户确认 —— **审核门**。
//!
//! 设计见 `docs/DESIGN.md` §6.3 与 devlog [260925-21]。
//!
//! # 门是什么
//!
//! `progress` 记"做过什么",这里记"**用户看过并点头了**"。后者是门:
//! 校准没被确认过、或者确认之后材料又变了,`write_summary` 就不放行。
//!
//! 为什么要这么一道门:M4 联调跑了两轮,发现"靠对话把关"等于没有把关 ——
//! **一个一张图都没校准的项目,可以直接写总结、把步 3 标成完成。**
//! 门把这个隐性遗漏变成一个**显式动作**:要么把东西处理干净,要么让用户点头。
//!
//! # 门不验"是谁签的"
//!
//! 验不了:MCP 握手只给客户端名(`clientInfo`),拿不到模型名;而"让模型自报"
//! 就是让它编 —— 那正是这套东西要防的事。所以 `Approval` 里**没有** `approved_by`。
//!
//! 服务端能观察到的只有三样:什么时候、当时什么状态、有没有带说明。
//! 门靠"状态有没有变过"来判断这次确认还算不算数 —— 这是**防漏掉,不是防伪造**:
//! 一个必须显式调用 `approve` 才能过去的关口,本身就是一道提醒。
//!
//! # 判据是内容摘要,不是计数
//!
//! `counts` 记的四项只表达得了"**数量**变了",而校准里最常发生的那类改动是
//! **改字**(把转录里听错的词改对)—— 一项计数都不动。
//!
//! 这是 M4 第三轮真 agent 联调实测出来的洞:用户签字时转录改了 4 段,之后 agent 又改到
//! 10 段,四项计数**一模一样**,门照开、总结照写。等于拿量长度的尺子量温度。
//!
//! 所以 `Approval` 里多了一份[逐条的内容摘要](vidbrief_project::MaterialDigests):
//! 任何一条对不上,这次确认就作废,而且报错能**说出是哪一条**。
//! 计数留着 —— 它负责把话说给人听。
//!
//! 粒度是"每条被动过的材料一个短摘要",不是"整份材料一个哈希":
//! 300 多段转录里改了一段,人得知道是**哪一段**才回得去核对。
//! 反过来,摘要只算**材料**(转录读到什么、图判定成什么),不算笔记、不算时间戳 ——
//! agent 补一段笔记、或拿同样的结论重记一遍,不该让人重签一次。

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use vidbrief_project::digest::short;
use vidbrief_project::{
    Confidence, MaterialDigests, ReviewCounts, Segment, Shot, ShotStatus, SupplementRequest,
    SupplementStatus,
};

use crate::project::OpenProject;

/// 按来源分的一批待处理条目。
///
/// 存的是**给人看的名**(`s003` / `seg 12` / `#1`),不是内部 id ——
/// 报错里要能直接说"截图 s003、转录 seg 12",不该让读的人再去查一次表。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingItems {
    pub shots: Vec<String>,
    pub segments: Vec<String>,
    pub supplements: Vec<String>,
}

impl PendingItems {
    pub fn is_empty(&self) -> bool {
        self.shots.is_empty() && self.segments.is_empty() && self.supplements.is_empty()
    }

    /// 报错里那半句:`截图 3 张(s003、s007、s009)、转录 41 段(seg 12、seg 13、…)`。
    /// 一条都没有时返回 `None`。
    fn describe(&self) -> Option<String> {
        /// 最多列几个。一轮审几十段是常事,全堆进去读的人只会跳过去。
        const CAP: usize = 5;

        let one = |label: &str, unit: &str, ids: &[String]| -> Option<String> {
            if ids.is_empty() {
                return None;
            }
            let shown: Vec<&str> = ids.iter().take(CAP).map(String::as_str).collect();
            let tail = if ids.len() > CAP {
                format!(" 等 {} {unit}", ids.len())
            } else {
                String::new()
            };
            Some(format!("{label} {} {unit}({}{tail})", ids.len(), shown.join("、")))
        };

        let parts: Vec<String> = [
            one("截图", "张", &self.shots),
            one("转录", "段", &self.segments),
            one("补图候选", "条", &self.supplements),
        ]
        .into_iter()
        .flatten()
        .collect();

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("、"))
        }
    }
}

/// 这一步的核验清单 —— **门的判据**。
///
/// 每一项只有三种状态:未核验 / 已核验 / 核验过但要用户拍板。
/// 门只数前两种,而且**两种拦法完全不同**:
///
/// - 有**未核验** → 拦,让模型接着审。**不打扰人** —— 这是"活还没干完"。
/// - 有**待拍板** → 拦,告诉它这些得等人定。
///
/// 全是"已核验"就直接放行 —— **不需要任何人签字**。这就是"全程托管只看结果"
/// 那一档能自动跑下去的原因:门管的是"有没有跳过步骤",不是"有没有人点头"。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReviewState {
    pub unchecked: PendingItems,
    pub needs_user: PendingItems,
}

/// 数一下这一步的三态。
///
/// **没有第四种状态,所以模型不会卡死**:未核验就接着审,待拍板就去问用户
/// (有授权时自己定),全核验完就过。
///
/// 缺文件一律按"空"算:没跑过前处理的项目不是错误。反过来,
/// 刚转录完、一段都没核验的项目会如实报"322 段未核验" —— 那就是实情。
pub fn state(opened: &OpenProject) -> Result<ReviewState> {
    let shots = crate::materials::shots(opened).unwrap_or_default();
    let supplements =
        vidbrief_project::SupplementRequests::load(&opened.layout().supplement_requests()).ok();
    // 有纠错稿就用纠错稿 —— 核验痕迹盖在它上面。
    let transcript = crate::materials::effective_transcript(opened)
        .ok()
        .map(|(t, _, _)| t);

    let mut out = ReviewState::default();

    for shot in &shots.shots {
        // 判 `drop` 的图不用核验 —— 它不进总结,没人关心画面上是什么。
        if shot.status != ShotStatus::Keep {
            continue;
        }
        match &shot.check {
            None => out.unchecked.shots.push(shot.id.to_string()),
            // 「要人拍板」的判据只有一个:**模型自己说它没把握**(`confidence: low`)。
            //
            // **不用 [`vidbrief_project::ShotCheck::needs_review`]** —— 那个函数答的是另一个
            // 问题:"这张图值不值得人看一眼"(把握低 / 报了问题 / 图文不符),它是**视图**的
            // 入口条件。拿它当门的判据会出人命:`issues` 非空的意思是"**我有话要说**",
            // 不是"我定不了" —— 而模型如实写「画面上没有标题文字,本节标题无法核实」
            // 正是 instructions 要求它做的。**诚实反而过不去门。**
            //
            // M4 第四轮真 agent 实测到了这个死结:7 张卡住的图里**没有一张是把握低**,
            // 全是反复写进 `issues` 的观察,而模型给它们**全打了 `high`** ——
            // 判据和模型自己的判断完全脱节,它只剩"签字放行"一条路,
            // 而门自己的报错里写着「别把发现改成没发现」。
            Some(check) if check.confidence == Confidence::Low => {
                out.needs_user.shots.push(shot.id.to_string())
            }
            Some(_) => {}
        }
    }

    if let Some(t) = &transcript {
        for segment in &t.segments {
            match segment.review() {
                vidbrief_project::SegmentReview::Unchecked => {
                    out.unchecked.segments.push(format!("seg {}", segment.id));
                }
                vidbrief_project::SegmentReview::NeedsUser => {
                    out.needs_user.segments.push(format!("seg {}", segment.id));
                }
                vidbrief_project::SegmentReview::Checked => {}
            }
        }
    }

    if let Some(r) = &supplements {
        for request in &r.requests {
            // 补图候选是"模型自己定得了"的:要不要补,它看上下文就能拿主意。
            // 所以没处理的算"未核验",不算"等人拍板"。
            if request.is_pending() {
                out.unchecked.supplements.push(format!("#{}", request.id));
            }
        }
    }

    Ok(out)
}

/// 数一下当前的材料状态。
///
/// 四项**全是服务端自己数得出来的** —— 数不出来的东西不进这道门。
/// 它是 [`state`] 的一个投影:**门不看它**,留着是为了把话说给人听 ——
/// 「待裁决 7 张」人读得懂,一串 `seg 12、seg 13…` 读着累。
pub fn counts(opened: &OpenProject) -> Result<ReviewCounts> {
    let s = state(opened)?;
    Ok(ReviewCounts {
        unchecked: s.unchecked.shots.len(),
        needing_review: s.needs_user.shots.len(),
        supplement_pending: s.unchecked.supplements.len(),
        uncertain: s.needs_user.segments.len(),
    })
}

/// 算一下当前材料的**内容**摘要。**这才是门的判据。**
///
/// 和 [`counts`] 读同一批文件,回答的是另一个问题:计数问"有几项",
/// 这里问"是哪几份"。改一个字不动计数,却会动这里 —— 这就是它存在的理由。
///
/// 缺文件一律按"空"算,和 [`counts`] 一致:没跑过前处理的项目不是错误。
pub fn digests(opened: &OpenProject) -> Result<MaterialDigests> {
    let shots = crate::materials::shots(opened).unwrap_or_default();
    let supplements =
        vidbrief_project::SupplementRequests::load(&opened.layout().supplement_requests()).ok();
    let transcript = crate::materials::effective_transcript(opened)
        .ok()
        .map(|(t, _, _)| t);

    Ok(MaterialDigests {
        segments: transcript
            .as_ref()
            .map(|t| t.segments.iter().filter_map(segment_digest).collect())
            .unwrap_or_default(),
        shots: shots
            .shots
            .iter()
            .map(|s| (s.id.to_string(), shot_digest(s)))
            .collect(),
        supplements: supplements
            .as_ref()
            .map(|r| {
                r.requests
                    .iter()
                    .map(|x| (x.id.to_string(), supplement_digest(x)))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// 一段转录的"材料":**它现在读作什么**。
///
/// 基准是**原始转录** —— 它永不就地改写,所以"读起来跟原文一样"就等于没变:
///
/// - 没被动过的段:读的就是原文 → 不进表;
/// - 改过、**又改回原文**的段:读起来还是原文 → 也不进表。改回去等于没改,
///   门不该为它拦一次。这正是计数做不到的事(4 段 → 10 段 → 4 段,它只会一路喊"变了");
/// - 标了**存疑**的段:文本没变,但"这段等人裁决"是个新状态,进表。
///
/// **表的键不是"有没有 `correction` 记录"** —— 那是过程,不是材料。
/// 一段被改了两遍最后改回原文的转录,它的 `correction` 还在,
/// 可用户读到的跟没改过的时候一模一样。
fn segment_digest(s: &Segment) -> Option<(String, String)> {
    if s.is_uncertain() {
        return Some((s.id.to_string(), short(&[s.effective_text(), "uncertain"])));
    }
    if s.effective_text() == s.text {
        return None;
    }
    Some((s.id.to_string(), short(&[s.effective_text(), "corrected"])))
}

/// 一张截图的"材料":留用还是丢弃,加上当时的看图结论。
///
/// **不收 `checked_at`** —— 同一张图用同样的结论重记一遍,材料没变,
/// 不该让用户重签一次。`neighbor_text` 同理:它是留档,内容来自那份永不就地改写的原始转录。
///
/// 收进来的这几项,正好覆盖 `needs_review()` 依赖的三个条件
/// (把握低 / 报了问题 / 图文不符),所以计数变了摘要一定跟着变。
fn shot_digest(s: &Shot) -> String {
    let status = match s.status {
        ShotStatus::Keep => "keep",
        ShotStatus::Drop => "drop",
    };
    let Some(check) = &s.check else {
        return short(&[status]);
    };
    let confidence = match check.confidence {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    };
    short(&[
        status,
        if check.r#match { "match" } else { "mismatch" },
        if check.audio_visual_sync { "sync" } else { "async" },
        &check.type_read,
        &check.issues.join("\n"),
        confidence,
    ])
}

/// 一条补图候选的"材料":处理了没有、处理成什么。
///
/// **不收 `note`**(agent 接受/拒绝的理由)—— 同 [`segment_digest`]:理由不是材料。
fn supplement_digest(r: &SupplementRequest) -> String {
    let status = match r.status {
        SupplementStatus::Pending => "pending",
        SupplementStatus::Accepted => "accepted",
        SupplementStatus::Rejected => "rejected",
    };
    short(&[
        status,
        r.shot_id.map(|s| s.to_string()).as_deref().unwrap_or(""),
    ])
}

/// 记下"用户确认了这一步"。
///
/// `note` 是**带着未处理项通过**时的说明;正常通过给 `None`。
/// 服务端不拦"还有 3 张没校准就通过"——那是用户的权力,但要留下理由。
///
/// 记的是**当时的计数加当时的内容摘要**:计数给人看,摘要当判据(见 [`gate_calibrate`])。
pub fn approve(opened: &mut OpenProject, note: Option<String>) -> Result<vidbrief_project::Approval> {
    let approval = vidbrief_project::Approval {
        approved_at: vidbrief_project::now(),
        counts: counts(opened)?,
        digests: Some(digests(opened)?),
        note: note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
    };
    opened.project_mut().review.calibrate = Some(approval.clone());
    opened.save()?;
    Ok(approval)
}

/// 步 2 的门:**校准这一步还有没有没核验完的、或者有没有在等人拍板的。**
///
/// # 它管的是"跳过步骤",不是"有没有人点头"
///
/// 三种核验状态([`ReviewState`])决定三件事:
///
/// 1. **有未核验的 → 拦,让模型接着审。** 不打扰人,也不打断 agent loop ——
///    我们只给反馈,它自己接着干。这一条防的是"跳过了 300 段还想过"。
/// 2. **有要用户拍板的 → 拦,告诉它等的是哪些项。** 模型可以自己去问用户,
///    问完把那些条目改定;用户要是说"就这样过",那句话进 `approve` 的 `note`。
/// 3. **全核验完、也没人等着 → 放行,不需要任何人签字。**
///
/// 第 3 条是关键:门不该给用户添麻烦。**要不要在节点上停下来问用户,是模型
/// 照着用户最初那句交代决定的**(判断归 agent),不是这道门替他决定的 ——
/// 用户说了"每个节点找我确认",模型自然会在全绿之后仍然停一停。
pub fn gate_calibrate(opened: &OpenProject) -> Result<()> {
    let now = state(opened)?;

    // ① 活还没干完 —— 让模型接着审
    if !now.unchecked.is_empty() {
        bail!(
            "校准还没核验完,不能开始写总结。\n\
             \n\
             还没核验的:{}\n\
             \n\
             接着审。**审过、没问题的段也要报上来**(`apply_corrections` 的 `reviewed`),\
             不报等于没审 —— 门分不出'看过没问题'和'压根没看',只能当没看。",
            now.unchecked.describe().unwrap_or_default()
        );
    }

    // ② 剩下的是"模型定不了、等人拍板"的
    if !now.needs_user.is_empty() {
        // 已经有用户拍板(或模型在授权下自己拍的板)覆盖着这一批 → 放行。
        // 判据是内容摘要对得上:那之后材料没再动过,这一批还是当初那一批。
        let signed = opened
            .project()
            .review
            .calibrate
            .as_ref()
            .and_then(|a| a.digests.as_ref());
        let now_digests = digests(opened)?;
        if signed == Some(&now_digests) {
            return Ok(());
        }

        // 上次拍板过、但材料又动过 —— 说清**动了哪几条**,
        // 免得模型把用户已经拍过板的那些又拿去问一遍。
        let since = match signed {
            Some(old) => format!(
                "\n他上次拍板过一批,但那之后材料又动过:\n{}\n",
                moved_items(old, &now_digests)
            ),
            None => String::new(),
        };

        bail!(
            "校准的核验都做完了,但有要用户拍板的项,不能开始写总结。\n\
             \n\
             等人拍板:{}\n\
             {since}\
             \n\
             去问用户,然后把他的决定**落到那些条目上**(截图用 `record_shot_check` 重写结论,\
             转录用 `apply_corrections` 改定或去掉存疑标记)。\
             他要是说'这些我知道,就这样过',把那句话放进 `approve` 的 `note` —— \
             别把发现改成没发现。",
            now.needs_user.describe().unwrap_or_default()
        );
    }

    // ③ 全核验完、没人等着 —— 直接过
    Ok(())
}

/// 材料比上次拍板时动了哪些**条目**。
///
/// 只逐条比,不比计数 —— 编号变了的那种事("从 7 张变成 6 张")对读的人没用,
/// 有用的是"**哪一条**动了":他要回去核对的就是那一条。
/// 调用方只在摘要对不上时才进来,所以至少有一类会报出东西来。
fn moved_items(before_items: &MaterialDigests, after_items: &MaterialDigests) -> String {
    let mut parts: Vec<String> = Vec::new();

    let rows = [
        (
            "转录",
            "seg ",
            ("改了", "又改了", "改回原文了"),
            &before_items.segments,
            &after_items.segments,
        ),
        (
            "截图",
            "",
            ("是新加的", "的结论变了", "没了"),
            &before_items.shots,
            &after_items.shots,
        ),
        (
            "补图候选",
            "#",
            ("是新加的", "的结果变了", "没了"),
            &before_items.supplements,
            &after_items.supplements,
        ),
    ];
    for (label, prefix, wording, before, after) in rows {
        if let Some(line) = items_changed(label, prefix, wording, before, after) {
            parts.push(line);
        }
    }

    parts.join("\n")
}

/// 一类材料里哪几条对不上。`None` = 这一条都没变。
///
/// 三种情况分开说,因为**用户接下来要做的事不一样**:动过的要去核对改成什么了,
/// 新出现的是"你签的时候还没有",没了的是"签完之后它不见了"。
///
/// 三种各自的**说法由调用方给**:"是新加的"对截图说得通,对转录段说不通
/// (段一直在,是它多了一条结论、改了)。措辞硬编码在这里,就会写出
/// "seg 2 是新的"这种话 —— 用户会以为第 2 段是新冒出来的。
fn items_changed(
    label: &str,
    prefix: &str,
    (appeared, changed, gone): (&str, &str, &str),
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Option<String> {
    /// 最多报几条。一轮校准改几十段是常事,把 50 个 id 堆进报错里,
    /// 读的人只会把整段跳过去 —— 那还不如只说个"变了"。
    const CAP: usize = 5;

    let mut moved = Vec::new();
    let mut fresh = Vec::new();
    let mut lost = Vec::new();
    for (id, old) in before {
        match after.get(id) {
            Some(new) if new != old => moved.push(id.clone()),
            None => lost.push(id.clone()),
            Some(_) => {}
        }
    }
    for id in after.keys() {
        if !before.contains_key(id) {
            fresh.push(id.clone());
        }
    }

    let list = |ids: &[String]| -> String {
        let shown: Vec<String> = ids
            .iter()
            .take(CAP)
            .map(|id| format!("{prefix}{id}"))
            .collect();
        match ids.len() {
            0 => String::new(),
            n if n > CAP => format!("{} 等 {n} 条", shown.join("、")),
            _ => shown.join("、"),
        }
    };

    let mut clauses = Vec::new();
    if !moved.is_empty() {
        clauses.push(format!("{} {changed}", list(&moved)));
    }
    if !fresh.is_empty() {
        clauses.push(format!("{} {appeared}", list(&fresh)));
    }
    if !lost.is_empty() {
        clauses.push(format!("{} {gone}", list(&lost)));
    }
    if clauses.is_empty() {
        return None;
    }
    Some(format!("{label}:{}", clauses.join(",")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    /// 给一张图写回结论。
    ///
    /// 不走 `record_shot_check`:那要构造一堆上下文,而这里测的是门。
    ///
    /// **默认就带一条 `issues`,也默认 `match = true`** —— 真 agent 就是这么写的:
    /// 「画面无任何标题文字,本节标题无法从画面上核实」这类**观察**。
    /// 把它写成默认,是因为"`issues` 非空"恰恰是这段代码最容易被判错的那种输入。
    fn check_shot(fake: &Fake, id: u32, confidence: vidbrief_project::Confidence) {
        check_shot_with(fake, id, confidence, true);
    }

    /// 同上,但可以指定 `match`。`match = false` 是"画面与口播对不上"这种**发现**。
    fn check_shot_with(
        fake: &Fake,
        id: u32,
        confidence: vidbrief_project::Confidence,
        matches: bool,
    ) {
        let path = fake.layout().shots_json();
        let mut shots = vidbrief_project::Shots::load(&path).unwrap();
        let shot = shots
            .get_mut(vidbrief_project::ShotId::new(id).unwrap())
            .unwrap();
        shot.check = Some(vidbrief_project::ShotCheck {
            r#match: matches,
            audio_visual_sync: true,
            type_read: "画面内容".into(),
            neighbor_text: None,
            issues: vec!["画面无标题文字,本节标题无法从画面上核实".into()],
            confidence,
            checked_at: vidbrief_project::now(),
        });
        shots.save(&path).unwrap();
    }

    fn high() -> vidbrief_project::Confidence {
        vidbrief_project::Confidence::High
    }
    fn low() -> vidbrief_project::Confidence {
        vidbrief_project::Confidence::Low
    }

    /// 报告这些转录段审过了。
    fn review_segments(opened: &crate::project::OpenProject, ids: &[u32]) {
        crate::conclusions::apply_corrections(opened, &[], ids).unwrap();
    }

    fn two_shots(tag: &str) -> Fake {
        Fake::new(tag)
            .with_transcript(&[(0.0, 2.0, "各位好"), (2.0, 4.0, "接着讲")])
            .with_curve(10)
            .with_shot(1, 2.0)
            .with_shot(2, 6.0)
    }

    /// 刚建好的项目:图没核验、转录没核验 —— 全是"活没干完"。
    #[test]
    fn states_start_at_unchecked() {
        let fake = two_shots("review-states");
        let opened = fake.open();

        let s = state(&opened).unwrap();
        assert_eq!(s.unchecked.shots, vec!["s001", "s002"]);
        assert_eq!(s.unchecked.segments, vec!["seg 1", "seg 2"]);
        assert!(s.needs_user.is_empty(), "还没审,谈不上等人拍板:{}", describe(&s));
    }

    /// **活没干完就拦,而且不把人扯进来。**
    ///
    /// 这是门的第一档,也是它最主要的一档:拦住"跳过了没干完的活就想进下一步"。
    /// 报错里不能出现 `approve` —— 这一档是模型自己的事,用户不该被打扰。
    #[test]
    fn the_gate_is_shut_while_work_remains() {
        let fake = two_shots("review-work");
        let opened = fake.open();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("还没核验完"), "{err}");
        assert!(err.contains("s001"), "要说清差哪一条:{err}");
        assert!(err.contains("seg 1"), "{err}");
        assert!(!err.contains("approve"), "这一档不该把人扯进来:{err}");
        assert!(!err.contains("等人拍板"), "两档的措辞不能混:{err}");
    }

    /// **全核验完就放行 —— 不需要任何人签字。**
    ///
    /// 这是"全程托管只看结果"那一档能自动跑下去的原因。门管的是"有没有跳步",
    /// 不是"有没有人点头";要不要在节点上停下来问用户,是模型照用户的交代决定的。
    #[test]
    fn finishing_the_work_opens_the_gate_without_anyone_signing() {
        let fake = two_shots("review-autopilot");
        let opened = fake.open();

        check_shot(&fake, 1, high());
        check_shot(&fake, 2, high());
        review_segments(&opened, &[1, 2]);

        gate_calibrate(&opened).expect("活干完了就该过");
        assert!(
            opened.project().review.calibrate.is_none(),
            "这一档走完,不该留下任何签字"
        );

        // 补一张新图 → 它还没被看过 → 门又拦上(而且是"活没干完"那一档)
        fake.add_shot(3, 9.0, vidbrief_project::ShotOrigin::AgentRequest);
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("s003"), "{err}");
        assert!(!err.contains("approve"), "{err}");
    }

    /// **模型定不了的项 → 拦,而且点出是哪一条。**
    #[test]
    fn pending_user_items_shut_the_gate_and_are_named() {
        let fake = two_shots("review-needs-user");
        let opened = fake.open();

        check_shot(&fake, 1, high());
        check_shot(&fake, 2, low()); // 这一张模型自己说没把握
        review_segments(&opened, &[1, 2]);

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("等人拍板"), "{err}");
        assert!(err.contains("s002"), "要说清等的是哪一条:{err}");
        assert!(err.contains("approve"), "该告诉它有'就这样过'这条路:{err}");
        assert!(!err.contains("还没核验完"), "两档的措辞不能混:{err}");
    }

    /// **存疑的转录段也是"等人拍板"** —— 那个类型的文档原话就是
    /// "agent 认为这里可能有问题却不敢断言",它本身就是模型在说"我定不了"。
    #[test]
    fn uncertain_segments_count_as_waiting_for_the_user() {
        let fake = two_shots("review-uncertain");
        let opened = fake.open();
        check_shot(&fake, 1, high());
        check_shot(&fake, 2, high());

        crate::conclusions::apply_corrections(
            &opened,
            &[crate::conclusions::CorrectionInput {
                segment_id: 1,
                corrected_text: None, // 不改,只标存疑
                kind: vidbrief_project::CorrectionType::Uncertain,
                note: Some("元祖 → 元组?".into()),
                reason: "同音,没把握".into(),
            }],
            &[2],
        )
        .unwrap();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("等人拍板"), "{err}");
        assert!(err.contains("seg 1"), "{err}");
    }

    /// 拍板过的那一批放行;**但拍板之后材料又动过,那次拍板就不覆盖新冒出来的项。**
    ///
    /// 没有这一条,"用户说这些我知道"会变成一张永久通行证。
    #[test]
    fn a_waiver_covers_only_the_batch_that_was_pending() {
        let fake = two_shots("review-waiver");
        let mut opened = fake.open();
        check_shot(&fake, 1, low());
        check_shot(&fake, 2, high());
        review_segments(&opened, &[1, 2]);

        assert!(gate_calibrate(&opened).is_err(), "先得真被挡住");

        approve(&mut opened, Some("用户说这张我知道,就这样过".into())).unwrap();
        gate_calibrate(&opened).expect("拍板了,这一批就放过");

        // 又冒出来一条要拍板的
        check_shot(&fake, 2, low());
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("s002"), "新冒出来的那条要点出来:{err}");
        assert!(
            err.contains("又动过"),
            "还要说清上次拍板已经不覆盖现在了:{err}"
        );

        // 再拍一次板,又能过
        approve(&mut opened, None).unwrap();
        gate_calibrate(&opened).expect("重新拍板之后该放过");
    }

    /// 判 `drop` 的图不用核验 —— 它不进总结,没人关心画面上是什么。
    #[test]
    fn dropped_shots_do_not_need_checking() {
        let fake = two_shots("review-dropped");
        {
            let path = fake.layout().shots_json();
            let mut shots = vidbrief_project::Shots::load(&path).unwrap();
            shots
                .get_mut(vidbrief_project::ShotId::new(2).unwrap())
                .unwrap()
                .status = vidbrief_project::ShotStatus::Drop;
            shots.save(&path).unwrap();
        }
        let opened = fake.open();
        check_shot(&fake, 1, high());
        review_segments(&opened, &[1, 2]);

        gate_calibrate(&opened).expect("判了 drop 的图不该挡着");
    }

    /// **「有话要说」不等于「定不了」。**
    ///
    /// 这是 M4 第四轮真 agent 联调逼出来的:那一轮 7 张卡在"要拍板"的图里
    /// **没有一张是把握低**,全是模型如实写下的观察(「画面无标题文字,本节标题
    /// 无法从画面上核实」),而它给它们**全打了 `high`**。
    ///
    /// 旧的判据是 `needs_review()`(把握低 **或 issues 非空** 或 match=false)——
    /// 拿它当门口径,等于**诚实反而过不去门**:模型照 instructions 的要求如实写
    /// 「看不出来」,就永远到不了"已核验";要过门只剩两条路,把观察抹掉,或者签字。
    /// 而门自己的报错里写着「别把发现改成没发现」。它选了签字。
    ///
    /// 所以:**判据只有一个 —— 模型自己说它没把握**(`confidence: low`)。
    /// `issues` 与 `match` 照常记录、照常显示,只是不再驱动门。
    #[test]
    fn findings_do_not_make_a_shot_need_the_user() {
        let fake = two_shots("review-findings");
        let opened = fake.open();
        // 满满一条 issues,而且判了 match=false —— 都是有话要说,不是定不了
        check_shot_with(&fake, 1, high(), false);
        check_shot_with(&fake, 2, high(), false);
        review_segments(&opened, &[1, 2]);

        gate_calibrate(&opened).expect("模型说它有把握,门就该放行 —— 哪怕它记了一堆发现");

        // 反过来:模型自己说没把握,哪怕一句 issues 都没有,也要等人
        let fake2 = two_shots("review-undecided");
        {
            let path = fake2.layout().shots_json();
            let mut shots = vidbrief_project::Shots::load(&path).unwrap();
            shots
                .get_mut(vidbrief_project::ShotId::new(1).unwrap())
                .unwrap()
                .check = Some(vidbrief_project::ShotCheck {
                r#match: true,
                audio_visual_sync: true,
                type_read: "画面内容".into(),
                neighbor_text: None,
                issues: Vec::new(),
                confidence: low(),
                checked_at: vidbrief_project::now(),
            });
            shots.save(&path).unwrap();
        }
        let opened2 = fake2.open();
        check_shot(&fake2, 2, high());
        review_segments(&opened2, &[1, 2]);

        let err = gate_calibrate(&opened2).unwrap_err().to_string();
        assert!(err.contains("等人拍板"), "{err}");
        assert!(err.contains("s001"), "{err}");
    }

    #[test]
    fn a_note_is_kept_and_blank_notes_are_dropped() {
        let fake = Fake::new("review-note").with_curve(5).with_shot(1, 2.0);
        let mut opened = fake.open();

        let a = approve(&mut opened, Some("  用户说先这样  ".into())).unwrap();
        assert_eq!(a.note.as_deref(), Some("用户说先这样"), "两头空白要掐掉");

        let a = approve(&mut opened, Some("   ".into())).unwrap();
        assert_eq!(a.note, None, "空说明等于没说明");
    }

    /// 空项目:材料本来就是空的,三态全是 0 —— 门直接放行。
    ///
    /// 这条曾经翻过车:第一版拿"摘要是不是空表"当"有没有摘要",于是空项目
    /// 一签字就再也过不去门。现在没有这个问题了 —— 门根本不看签字。
    #[test]
    fn an_empty_project_passes() {
        let fake = Fake::new("review-empty");
        let opened = fake.open();
        gate_calibrate(&opened).expect("什么都没有的项目,没有活要干,也没有人要等");
    }

    fn describe(s: &ReviewState) -> String {
        format!("{:?}", s.unchecked)
    }
}
