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

/// 一条看图结论**对不上材料**的地方。
///
/// 门查的只有机械的两样 —— **图还在不在、文是不是真在转录里** ——
/// 因为那是工具唯一能自己核的东西(画面上写了什么,它没有眼睛)。
/// 但它把"一句自述"变成了"一句自述 + 一处能查的凭据":
/// 从前你只能再问模型一遍,而那是同一张嘴。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadEvidence {
    /// 哪张图。
    pub shot: String,
    /// 哪里对不上。
    pub why: String,
}

/// 查这一条结论的凭据还在不在。
///
/// 三样,全是机械的:
/// 1. **没有凭据** → 说"补上",不说它对或错(摘要这套之前写的结论就是这样);
/// 2. **图没了** → 结论是关于一张不存在的图;
/// 3. **文对不上** → 它引的那句话,转录里没有。
///
/// 第 3 条按**逐字包含**判,而且判的是**当初引的那一段**:文中途被人改过也算对不上 ——
/// 结论是照着旧文下的,材料变了它就该重看一遍。
fn bad_evidence(opened: &OpenProject, shots: &vidbrief_project::Shots, transcript: Option<&vidbrief_project::Transcript>) -> Vec<BadEvidence> {
    let layout = opened.layout();
    let mut bad = Vec::new();

    for shot in &shots.shots {
        if shot.status != ShotStatus::Keep {
            continue;
        }
        let Some(check) = &shot.check else { continue };
        let id = shot.id.to_string();

        // 图还在不在 —— 结论是"关于这张图"的,图没了它就没有对象
        if !layout.rel(shot.file.as_str()).is_file() {
            bad.push(BadEvidence {
                shot: id.clone(),
                why: format!("图不在了({})", shot.file.as_str()),
            });
            continue;
        }

        let Some(evidence) = &check.evidence else {
            bad.push(BadEvidence {
                shot: id.clone(),
                why: "没有凭据 —— 得给一句照抄的转录原文".to_string(),
            });
            continue;
        };

        let hit = transcript.and_then(|t| t.segments.iter().find(|s| s.id == evidence.segment));
        match hit {
            None => bad.push(BadEvidence {
                shot: id.clone(),
                why: format!("凭据指向的转录段 seg {} 不在了", evidence.segment),
            }),
            Some(segment) if !segment.effective_text().contains(evidence.quote.trim()) => {
                bad.push(BadEvidence {
                    shot: id.clone(),
                    why: format!(
                        "凭据说凭的是 seg {} 里的「{}」,可那一段现在不是这么写的",
                        evidence.segment,
                        evidence.quote.trim()
                    ),
                })
            }
            Some(_) => {}
        }
    }

    bad
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
///
/// **但"缺文件"和"读不动"是两件事。** 前者按空算,后者**报错**:
/// 一份读不动的清单不是"没有待办",而是"不知道有没有待办" ——
/// 门在这两种情况下要做的判断正好相反(前者没得拦,后者必须拦)。
/// M4 第五轮实测:`.ok()` 把一份读坏的候选清单当成了"没有候选",门一路放行。
pub fn state(opened: &OpenProject) -> Result<ReviewState> {
    let shots = crate::materials::shots_optional(opened)?.unwrap_or_default();
    let supplements = crate::materials::supplements(opened)?;
    // 有纠错稿就用纠错稿 —— 核验痕迹盖在它上面。
    let transcript = crate::materials::effective_transcript_optional(opened)?.map(|(t, _, _)| t);

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
/// 缺文件一律按"空"算,和 [`state`] 一致:没跑过前处理的项目不是错误。
/// **读不动照旧报错** —— 摘要算不出来,就不能拿它当"没有变化"。
pub fn digests(opened: &OpenProject) -> Result<MaterialDigests> {
    let shots = crate::materials::shots_optional(opened)?.unwrap_or_default();
    let supplements = crate::materials::supplements(opened)?;
    let transcript = crate::materials::effective_transcript_optional(opened)?.map(|(t, _, _)| t);

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
    let evidence = check.evidence.as_ref();
    short(&[
        status,
        if check.r#match { "match" } else { "mismatch" },
        if check.audio_visual_sync { "sync" } else { "async" },
        &check.type_read,
        &check.issues.join("\n"),
        confidence,
        // 凭据也算材料:它变了,用户当初看到的那条结论就不是同一条了
        evidence.map(|e| e.quote.as_str()).unwrap_or(""),
        &evidence.map(|e| e.segment.to_string()).unwrap_or_default(),
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
    opened.state_mut().review.calibrate = Some(approval.clone());
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
    // ⓪ **材料还是工具写的那一份吗**
    //
    // 第①条是**从明文文件里数**"还有哪些没核验"的,所以文件里没有的行它看不见 ——
    // 把没审的段/图/候选从文件里拿掉,活就被藏起来了(用户问的"它改了不还得改回去吗":
    // **改**是这样,**拿掉**不是)。所以开口之前先拿**内容底账**对一遍。
    //
    // 改流程时别把这步挪到后面:它对不上时说的话与①②③都不一样,
    // 而且它的去路是"重跑产生它的那一步",不是"接着审"。
    // (例外「状态里一条底账都没有时不对账」写在 `content_mismatches` 里 ——
    //  门与体检共用那一处判断,免得两边各写一份、迟早分叉。)
    let stale = crate::materials::content_mismatches(opened)?;
    if !stale.is_empty() {
        let lines: Vec<String> = stale.iter().map(|s| format!("  - {s}")).collect();
        bail!(
            "**材料不是工具写出去的那一份了**,不能开始写总结。\n\
             \n\
             {}\n\
             \n\
             门数的'还有哪些没核验'是从这几份文件里数的 —— 行被拿掉了,活就看不见了,\n\
             所以开口之前先跟**内容底账**对一遍(底账记在状态里,改不动)。\n\
             \n\
             **两条路,选一条:**\n\
             ① 重跑产生它的那一步,把工具算出来的那份找回来:\n\
               `preprocess`(转录与截图;它会报出作废了几条旧结论)、\n\
               `find_supplement_candidates`(补图候选,要显式带上 `rebuild`)。\n\
             ② 让用户确认这份改动是他要的,然后跑一次\n\
               `vidbrief upgrade <项目目录> --accept-material`\n\
               —— 那是**显式**把现在这份当成新底账(它会报出重记了哪几份、原来几条现在几条)。\n\
             \n\
             在这之前别拿宿主的文件工具去'改回去':改哪一份、改什么,上面已经写清了。",
            lines.join("\n")
        );
    }

    let now = state(opened)?;

    // ① 活还没干完 —— 让模型接着审
    //
    // 报的每一项都要**点出能消掉它的那一步**(M4 第五轮的直接教训:门数着候选,
    // 而当时没有任何工具能改候选的状态 —— 模型撞三轮过不去,最后自己改文件)。
    if !now.unchecked.is_empty() {
        bail!(
            "校准还没核验完,不能开始写总结。\n\
             \n\
             还没核验的:{}\n\
             \n\
             接着审。三类各有各的入口,别漏:\n\
             - **截图**:`get_shot` 看过之后用 `record_shot_check` 记结论;\n\
             - **转录段**:审过、没问题的也要报上来(`apply_corrections` 的 `reviewed`),\
             不报等于没审 —— 门分不出'看过没问题'和'压根没看';\n\
             - **补图候选**:每一条都要结论,补或不补都用 `disposition_supplements` 报,\
             并给一句理由。",
            now.unchecked.describe().unwrap_or_default()
        );
    }

    // ② 有结论,但**凭据对不上材料** —— 那也是"活没干完",而且是模型自己修得了的
    let shots = crate::materials::shots_optional(opened)?.unwrap_or_default();
    let transcript = crate::materials::effective_transcript_optional(opened)?.map(|(t, _, _)| t);
    let bad = bad_evidence(opened, &shots, transcript.as_ref());
    if !bad.is_empty() {
        let lines: Vec<String> = bad
            .iter()
            .map(|b| format!("  - {}:{}", b.shot, b.why))
            .collect();
        bail!(
            "有看图结论的**凭据对不上材料**,不能开始写总结。\n\
             \n\
             {}\n\
             \n\
             重看一眼那张图,然后给一句**照抄**的转录原文(`evidence_quote`)。\n\
             画面上写了什么工具核不了 —— 它只能核'你引的那句话真的在材料里'。",
            lines.join("\n")
        );
    }

    // ③ 剩下的是"模型定不了、等人拍板"的
    if !now.needs_user.is_empty() {
        // 已经有用户拍板(或模型在授权下自己拍的板)覆盖着这一批 → 放行。
        // 判据是内容摘要对得上:那之后材料没再动过,这一批还是当初那一批。
        let signed = opened
            .state()
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
             **两条路,选一条:**\n\
             ① 问过他,把他的话落到条目上 —— 转录用 `apply_corrections` 改定,\
             截图用 `record_shot_check` 重写结论;他说'先留着 / 我也不确定 / 别管了'也是一句话,\
             把它放进 `approve` 的 `note`(`approve` 记的是'用户在这一步拍过板',\
             **包括他决定先这样**)。落定之后接着往下走。\n\
             ② 保持存疑、就此收手 —— 什么都不动,把这几条在对话里说清,结束这一轮等他。\n\
             \n\
             **问过的不要再问一遍。** 材料上没有'问过'的痕迹,只有你手上的对话有 ——\
             先看一眼自己是不是已经问过、答过了。",
            now.needs_user.describe().unwrap_or_default()
        );
    }

    // ④ 全核验完、没人等着、凭据也都对得上 —— 直接过
    Ok(())
}

/// 步 3 的门:**出报告之前,总结得是工具写出去的那一份。**
///
/// 与 [`gate_calibrate`] 是**同一套机制**,只是快照对象换成了 `summary.md`
/// (指纹用 [`vidbrief_project::doc_print`],账记在同一张内容底账上 ——
/// 见 `ProjectState::content`)。它挡的是两种"跳步":
///
/// 1. **步 3 没走过工具**:`summary.md` 不在,或者它不是 `write_summary` 落的
///    (进度里那一步还空着)。M4 第三轮真 agent 联调就是这个形状 ——
///    审核门关着,它拿宿主的文件工具把 `summary.md` 整个写了出来,然后接着往下做。
/// 2. **写完之后又被绕过工具改过**:底账对不上。改一个字、换一版、把文件拿掉,
///    都在里面。门要的是"**用户看到的那一版,就是出报告用的那一版**"。
///
/// 去路只有一条,而且很短:**把定稿交回 `write_summary` 再落一遍**
/// (内容一字不改也行)。它不是惩罚,它是这条路上唯一的记账口。
///
/// # 这里为什么没有"等人拍板"那一档
///
/// 步 2 的那道门有三档(未核验 / 待拍板 / 放行),因为校准里"没把握"是模型自己
/// 说得出来的状态。总结不同:**它没有"没把握"这回事** —— 一版总结要么是这个说法,
/// 要么是另一个说法,判它是模型自己的活。
///
/// 要不要写完停下来等用户点头,是模型照用户那句交代决定的(`DESIGN` §6.1.1):
/// 用户说"每个节点找我确认",它自然会停;说"全程托管只看结果",就一路做下去。
/// **门在这里只保证一件事:出报告用的那份总结,和用户在对话里看到的那份是同一份。**
pub fn gate_summarize(opened: &OpenProject) -> Result<()> {
    let path = opened.layout().summary_md();
    let md = std::fs::read_to_string(&path)
        .ok()
        .filter(|text| !text.trim().is_empty());

    // ① 步 3 的产出不在(没写过 / 被拿掉 / 是空的)
    let Some(md) = md else {
        bail!(
            "还没有总结,不能出报告。\n\
             \n\
             报告的两级内容里,**全局总结**就是这一步的产物(`{}`),\
             没有它就没有可导的东西。\n\
             \n\
             先写总结:`write_summary`(命令行 `vidbrief summary <项目> --md <文件|->`)。",
            vidbrief_project::layout::SUMMARY_FILE
        );
    };

    // ② 它不是工具写出去的那一份
    let now = vidbrief_project::doc_print(&md);
    let stale = match opened.state().content.get(vidbrief_project::layout::SUMMARY_FILE) {
        None => Some("没有底账 —— 这份总结不是经 `write_summary` 落盘的".to_string()),
        Some(then) if then.digest != now.digest => Some(format!(
            "内容跟工具写出去的那次对不上(那次 {} 行,现在 {} 行)",
            then.items, now.items
        )),
        Some(_) => None,
    };
    if let Some(why) = stale {
        bail!(
            "这份总结不是工具写出去的那一份了,不能出报告。\n\
             \n\
             `{}`:{why}\n\
             \n\
             出报告用的那份总结,得和用户在对话里看到的那份**是同一份** ——\
             中间被谁换过一版,报告就成了一份没人看过的稿子。\n\
             \n\
             **去路:把现在这份正文交回 `write_summary` 再落一遍**\
             (命令行 `vidbrief summary <项目> --md <这份文件>`,内容一字不改也可以)。\
             它就是这条路上唯一的记账口 —— 落完这一遍,底账就对上现在这份了。",
            vidbrief_project::layout::SUMMARY_FILE
        );
    }

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

    /// 一条"看起来像真 agent 写的"看图结论。
    ///
    /// **默认就带一条 `issues`,也默认 `match = true`** —— 真 agent 就是这么写的:
    /// 「画面无任何标题文字,本节标题无法从画面上核实」这类**观察**。
    /// 把它写成默认,是因为"`issues` 非空"恰恰是这段代码最容易被判错的那种输入。
    fn a_check(
        confidence: vidbrief_project::Confidence,
        matches: bool,
    ) -> vidbrief_project::ShotCheck {
        vidbrief_project::ShotCheck {
            r#match: matches,
            audio_visual_sync: true,
            type_read: "画面内容".into(),
            neighbor_text: None,
            issues: vec!["画面无标题文字,本节标题无法从画面上核实".into()],
            confidence,
            evidence: Some(vidbrief_project::Evidence {
                segment: 1,
                quote: "各位好".into(),
            }),
            checked_at: vidbrief_project::now(),
        }
    }

    /// 给一张图写回结论。
    ///
    /// **写的是状态,而且是手上这个已打开的项目的那一份** —— 门读的就是它。
    /// (明文那几栏只是投影:拿它造场景,测的就不是门了,而且是绿着骗人的那种。)
    fn check_shot(
        opened: &mut crate::project::OpenProject,
        id: u32,
        confidence: vidbrief_project::Confidence,
    ) {
        check_shot_with(opened, id, confidence, true);
    }

    /// 同上,但可以指定 `match`。`match = false` 是"画面与口播对不上"这种**发现**。
    fn check_shot_with(
        opened: &mut crate::project::OpenProject,
        id: u32,
        confidence: vidbrief_project::Confidence,
        matches: bool,
    ) {
        opened
            .state_mut()
            .shot_mut(vidbrief_project::ShotId::new(id).unwrap())
            .check = Some(a_check(confidence, matches));
    }

    fn high() -> vidbrief_project::Confidence {
        vidbrief_project::Confidence::High
    }
    fn low() -> vidbrief_project::Confidence {
        vidbrief_project::Confidence::Low
    }

    /// 报告这些转录段审过了。
    fn review_segments(opened: &mut crate::project::OpenProject, ids: &[u32]) {
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
        let mut opened = fake.open();

        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, high());
        review_segments(&mut opened, &[1, 2]);

        gate_calibrate(&opened).expect("活干完了就该过");
        assert!(
            opened.state().review.calibrate.is_none(),
            "这一档走完,不该留下任何签字"
        );

        // 补一张新图 → 它还没被看过 → 门又拦上(而且是"活没干完"那一档)
        //
        // 夹具是**直接写文件**的(不走 `extract_shot`),所以这里要把已经核验的那份先落盘、
        // 再重新打开 —— 真项目里补图走工具,底账与状态是同一趟写进去的。
        opened.save().unwrap();
        fake.add_shot(3, 9.0, vidbrief_project::ShotOrigin::AgentRequest);
        let opened = fake.open();
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("s003"), "{err}");
        assert!(!err.contains("approve"), "{err}");
    }

    /// **模型定不了的项 → 拦,而且点出是哪一条。**
    #[test]
    fn pending_user_items_shut_the_gate_and_are_named() {
        let fake = two_shots("review-needs-user");
        let mut opened = fake.open();

        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, low()); // 这一张模型自己说没把握
        review_segments(&mut opened, &[1, 2]);

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
        let mut opened = fake.open();
        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, high());

        crate::conclusions::apply_corrections(
            &mut opened,
            &[crate::conclusions::CorrectionInput {
                segment_id: 1,
                conclusion: vidbrief_project::Conclusion::Uncertain {
                    note: "元祖 → 元组?".into(),
                    reason: "同音,没把握".into(),
                },
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
        check_shot(&mut opened, 1, low());
        check_shot(&mut opened, 2, high());
        review_segments(&mut opened, &[1, 2]);

        assert!(gate_calibrate(&opened).is_err(), "先得真被挡住");

        approve(&mut opened, Some("用户说这张我知道,就这样过".into())).unwrap();
        gate_calibrate(&opened).expect("拍板了,这一批就放过");

        // 又冒出来一条要拍板的
        check_shot(&mut opened, 2, low());
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
        fake.set_shot_status(2, vidbrief_project::ShotStatus::Drop);
        let mut opened = fake.open();
        check_shot(&mut opened, 1, high());
        review_segments(&mut opened, &[1, 2]);

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
        let mut opened = fake.open();
        // 满满一条 issues,而且判了 match=false —— 都是有话要说,不是定不了
        check_shot_with(&mut opened, 1, high(), false);
        check_shot_with(&mut opened, 2, high(), false);
        review_segments(&mut opened, &[1, 2]);

        gate_calibrate(&opened).expect("模型说它有把握,门就该放行 —— 哪怕它记了一堆发现");

        // 反过来:模型自己说没把握,哪怕一句 issues 都没有,也要等人
        let fake2 = two_shots("review-undecided");
        let mut one = a_check(low(), true);
        one.issues.clear(); // 一句 issues 都没有,只是它自己说没把握
        fake2.set_shot_check(1, Some(one));
        let mut opened2 = fake2.open();
        check_shot(&mut opened2, 2, high());
        review_segments(&mut opened2, &[1, 2]);

        let err = gate_calibrate(&opened2).unwrap_err().to_string();
        assert!(err.contains("等人拍板"), "{err}");
        assert!(err.contains("s001"), "{err}");
    }

    /// **凭据对不上材料 → 拦,而且说清是哪一条、哪里对不上。**
    ///
    /// 这是「结论必须带能拿去对的东西」那条的落地。三种对不上:
    /// 没凭据 / 段里那句话已经不是这么写的 / 引的段不在了。
    #[test]
    fn evidence_is_checked_against_the_material() {
        let fake = two_shots("review-evidence");
        let mut opened = fake.open();
        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, high());
        review_segments(&mut opened, &[1, 2]);

        gate_calibrate(&opened).expect("凭据对得上就该过");

        fn set_evidence(opened: &mut crate::project::OpenProject, ev: Option<vidbrief_project::Evidence>) {
            let mut check = a_check(high(), true);
            check.evidence = ev;
            opened.state_mut().shot_mut(vidbrief_project::ShotId::FIRST).check = Some(check);
        }

        // ① 没凭据
        set_evidence(&mut opened, None);
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("s001"), "{err}");
        assert!(err.contains("没有凭据"), "{err}");

        // ② 引的那句话,材料里没有 —— 也就是"编的"
        set_evidence(
            &mut opened,
            Some(vidbrief_project::Evidence {
                segment: 1,
                quote: "画面右上角写着 Python 列表".into(),
            }),
        );
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("不是这么写的"), "要说清哪里对不上:{err}");

        // ③ 材料后来被改过 —— 结论是照着旧文下的,该重看
        set_evidence(
            &mut opened,
            Some(vidbrief_project::Evidence {
                segment: 2,
                quote: "接着讲".into(),
            }),
        );
        crate::conclusions::apply_corrections(
            &mut opened,
            &[crate::conclusions::CorrectionInput {
                    segment_id: 2,
                    conclusion: vidbrief_project::Conclusion::Corrected {
                        text: "接下来继续讲".into(),
                        kind: vidbrief_project::CorrectionType::Other,
                        note: String::new(),
                        reason: "测试".into(),
                    },
                }],
            &[],
        )
        .unwrap();
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("s001"), "{err}");
        assert!(err.contains("不是这么写的"), "{err}");

        // 把凭据改对,又放行
        set_evidence(
            &mut opened,
            Some(vidbrief_project::Evidence {
                segment: 1,
                quote: "各位好".into(),
            }),
        );
        gate_calibrate(&opened).expect("凭据补对了就该过");
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

    /// **改一个字也发现得了,而且门要把两条去路都点出来。**
    ///
    /// 底账按**内容**算(hash 的是段号/时刻/正文),所以:
    /// - 改正文 → 对不上 → 拦;
    /// - 换个缩进、重排键序 → 对得上 → 不拦(那是同一份内容换个写法)。
    #[test]
    fn editing_the_material_text_is_caught_and_the_ways_out_are_named() {
        let fake = two_shots("review-content-edit");
        let mut opened = fake.open();
        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, high());
        review_segments(&mut opened, &[1, 2]);
        gate_calibrate(&opened).expect("先确认它是干净的");

        // ① 重排格式(同一个内容换个写法)—— **不该**被当成"材料变了"
        let mut shots = vidbrief_project::Shots::load(&fake.layout().shots_json()).unwrap();
        shots.sort_by_time();
        shots.save(&fake.layout().shots_json()).unwrap();
        assert!(
            crate::materials::content_mismatches(&opened).unwrap().is_empty(),
            "缩进/键序变了不算材料变了"
        );

        // ② 改正文一个字 —— 该被发现
        let path = fake.layout().transcript();
        let mut t = vidbrief_project::Transcript::load(&path).unwrap();
        t.segments[0].text = "各位好,我们今天讲列表".into();
        t.save(&path).unwrap();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("不是工具写出去的那一份"), "{err}");
        assert!(err.contains("transcript.json"), "得说清是哪一份:{err}");
        assert!(err.contains("322 条") || err.contains("2 条"), "得说清几条:{err}");
        // 两条去路都要写出来 —— 没有出路的规则不许进门的话术
        assert!(err.contains("preprocess"), "{err}");
        assert!(err.contains("find_supplement_candidates"), "{err}");
        assert!(err.contains("--accept-material"), "{err}");
    }

    /// 工具自己写回去一次,底账就跟着更新 —— 拦完了总得能走过去。
    #[test]
    fn a_tool_write_refreshes_the_ledger() {
        let fake = two_shots("review-ledger-refresh");
        let mut opened = fake.open();
        check_shot(&mut opened, 1, high());
        check_shot(&mut opened, 2, high());
        review_segments(&mut opened, &[1, 2]);

        // 手改一份材料 → 拦
        let path = fake.layout().transcript();
        let mut t = vidbrief_project::Transcript::load(&path).unwrap();
        t.segments[0].text = "改过的正文".into();
        t.save(&path).unwrap();
        assert!(gate_calibrate(&opened).is_err());

        // 走工具给这一段下个结论(工具写的是状态,不动原文)—— 底账没变,还是拦
        crate::conclusions::apply_corrections(
            &mut opened,
            &[crate::conclusions::CorrectionInput {
                segment_id: 2,
                conclusion: vidbrief_project::Conclusion::Kept {
                    note: "原句通顺".into(),
                    reason: "上下文".into(),
                },
            }],
            &[],
        )
        .unwrap();
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("不是工具写出去的那一份"), "{err}");

        // **恢复原文** → 底账又对上了(改动写成结论,原文不动 —— 这才是正路)
        t.segments[0].text = "各位好".into();
        t.save(&path).unwrap();
        gate_calibrate(&opened).expect("原文回来了,底账就对上了");
    }

    /// **手改明文投影,门照旧拦着。**
    ///
    /// 这是"状态改走非明文"要买的那件事:模型手里有宿主给的文件工具,
    /// 明文材料谁都能写 —— 而判据不能是能随手写的东西。
    ///
    /// 这条测试**不碰状态文件**,只改 `shots.json` 里那几栏:门读的是状态,
    /// 所以它连看都不看那几栏一眼。反过来也验一遍:读材料那条路
    /// (`materials::shots`)同样以状态为准,免得模型改完再读回来自己骗自己。
    #[test]
    fn editing_the_plaintext_projection_does_not_open_the_gate() {
        let fake = two_shots("review-forged-plaintext");
        let opened = fake.open();

        // 明文里把两张图都写成"看过了、很有把握"
        let path = fake.layout().shots_json();
        let mut shots = vidbrief_project::Shots::load(&path).unwrap();
        for shot in &mut shots.shots {
            shot.check = Some(a_check(high(), true));
        }
        shots.save(&path).unwrap();

        // 门照旧拦:状态里一张都没核验过
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("还没核验完"), "{err}");
        assert!(err.contains("s001"), "{err}");

        // 读材料那条路也不认那几栏
        let read_back = crate::materials::shots(&opened).unwrap();
        assert!(
            read_back.shots.iter().all(|s| s.check.is_none()),
            "明文那几栏只是投影,读回来也得以状态为准"
        );
        // 而且这不算"材料变了":底账算的是**内容**(哪张图、哪一刻、存在哪),不含结论
        assert!(
            crate::materials::content_mismatches(&opened).unwrap().is_empty(),
            "改投影里的结论,不该被当成材料被人换过"
        );
    }

    /// **把明文的行拿掉,就把活藏起来了。** —— 这条现在**必须红**,它记录的是一个真实的洞。
    ///
    /// 门的第①条数的"还没核验的转录段"是从**明文那份文件里**数出来的:
    /// 文件里没有的段,它当然看不见。于是模型不用审 322 段 ——
    /// 把没审的段从 `transcript.json` 里拿掉,门就以为活干完了。
    ///
    /// 用户问的那句「门控不放过,它改了不还得改回去吗」:**改**是这样(改多了得改回去),
    /// **拿掉**不是 —— 删掉了行,门反而看不见那部分活。这条测试就是把这句话钉死。
    #[test]
    fn removing_rows_from_the_plaintext_must_not_hide_work() {
        // 一个没有截图的项目:把"转录"这一档单独隔离出来看
        let fake = Fake::new("review-deleted-rows")
            .with_transcript(&[(0.0, 2.0, "各位好"), (2.0, 4.0, "接着讲")])
            .with_curve(10);
        let opened = fake.open();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("seg 1"), "本来该拦在这两段上:{err}");

        // 把转录**整份拿掉** —— 门不该因此就当"没有段落要核验"
        std::fs::remove_file(fake.layout().transcript()).unwrap();
        let opened = fake.open();
        match gate_calibrate(&opened) {
            Ok(()) => panic!("转录被整份拿掉了,门却放行了 —— 活被藏起来了"),
            Err(e) => {
                assert!(
                    e.to_string().contains("转录"),
                    "拦住了,但没说清是转录这一档:{e}"
                );
            }
        }
    }

    /// **门和体检必须说同一件事。** —— 这是"规则一多就左脚拌右脚"的探针。
    ///
    /// 规则多了以后,最容易出的不是"某条规则错了",而是**两条规则各说各的**:
    /// 体检说没事、门却拦着(用户拿到的是一句没法照做的话),或者反过来。
    ///
    /// 这一条把几种场面摆一排,对每一个分别问门、问 `vidbrief status`,
    /// 断言"内容底账"这一条两边说法**一致**:门因它拦 ⟺ 体检把它列出来。
    /// 将来谁把判据改成两份,这条就会红。
    #[test]
    fn the_gate_and_the_status_view_agree_about_the_ledger() {
        /// 门是不是因为**底账**拦的(而不是①②③)。
        fn gate_blocked_on_ledger(opened: &crate::project::OpenProject) -> bool {
            match gate_calibrate(opened) {
                Ok(()) => false,
                Err(e) => e.to_string().contains("不是工具写出去的那一份"),
            }
        }
        fn status_lists_it(fake: &Fake) -> bool {
            !crate::inspect::scan_materials(fake.layout()).content_stale.is_empty()
        }

        let checked = |tag: &str| {
            let fake = two_shots(tag);
            let mut opened = fake.open();
            check_shot(&mut opened, 1, high());
            check_shot(&mut opened, 2, high());
            review_segments(&mut opened, &[1, 2]);
            (fake, opened)
        };

        // ① 干净的:谁都不该说话
        let (fake, opened) = checked("agree-clean");
        assert!(!gate_blocked_on_ledger(&opened));
        assert!(!status_lists_it(&fake));

        // ② 转录正文被改:两边都得说
        let (fake, opened) = checked("agree-text-edited");
        let mut t = vidbrief_project::Transcript::load(&fake.layout().transcript()).unwrap();
        t.segments[0].text = "手改".into();
        t.save(&fake.layout().transcript()).unwrap();
        assert!(gate_blocked_on_ledger(&opened), "门该因底账拦");
        assert!(status_lists_it(&fake), "体检也该列出来");

        // ③ 截图清单被改(`t` 是内容):
        let (fake, opened) = checked("agree-shots-edited");
        let mut shots = vidbrief_project::Shots::load(&fake.layout().shots_json()).unwrap();
        shots.shots[0].t = 9.5;
        shots.save(&fake.layout().shots_json()).unwrap();
        assert!(gate_blocked_on_ledger(&opened), "门该因底账拦");
        assert!(status_lists_it(&fake), "体检也该列出来");

        // ④ 材料**整份不在**了(底账里记着它有):
        let (fake, opened) = checked("agree-gone");
        std::fs::remove_file(fake.layout().transcript()).unwrap();
        assert!(gate_blocked_on_ledger(&opened), "拿掉整份更要拦");
        assert!(status_lists_it(&fake), "体检也该列出来");

        // ⑤ **一条底账都没有**(刚建的项目 / 状态刚重起):两边都不该说话 ——
        //    那是"还没记过账",交给第①条按"还没核验"拦
        let (fake, mut opened) = checked("agree-no-ledger");
        opened.state_mut().content.clear();
        opened.save().unwrap();
        assert!(!gate_blocked_on_ledger(&opened), "没底账不是'材料被换了'");
        assert!(!status_lists_it(&fake), "体检也不该列出它");
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

    /// **读不动 ≠ 没有。** 候选清单在、但读不动时,门必须拦住 —— 而且要说清怎么自救。
    ///
    /// 这条是 M4 第五轮实测换来的:那次模型手写了一份清单(状态值全不合法),
    /// `load().ok()` 把它当成了"没有候选",门数出来 0 条、一路放行。
    /// **门不是被骗了,是它的输入被清空了。**
    #[test]
    fn an_unreadable_supplement_list_blocks_instead_of_passing() {
        let fake = Fake::new("review-broken-supplement");
        std::fs::write(fake.layout().supplement_requests(), b"{ not json").unwrap();
        let opened = fake.open();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("supplement_requests.json"), "{err}");
        assert!(err.contains("读不动"), "{err}");
        // 报的每一项都要有一步能做掉它 —— 这里就是"重算一份"
        assert!(err.contains("find_supplement_candidates"), "{err}");
        // 而且不许说成"没有候选"
        assert!(!err.contains("还没有核验的:补图候选 0"), "{err}");
    }

    /// 截图清单读不动:同样按"还没校准"拦,不许当"一张图都没有"。
    #[test]
    fn an_unreadable_shot_list_blocks_instead_of_passing() {
        let fake = Fake::new("review-broken-shots");
        std::fs::write(fake.layout().shots_json(), b"[ broken").unwrap();
        let opened = fake.open();
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("shots.json"), "{err}");
        assert!(err.contains("读不动"), "{err}");
    }

    /// **状态读不动 → 整个项目打不开,而且要把去路说出来。**
    ///
    /// 状态是每一件工具都要用的东西(谁审过、哪张图看过、哪一步拍过板都在里面):
    /// 让它半路才发现,报错就会零散在十几个地方,每个说法还不一样。
    /// 所以报在**打开项目**这一步,而且这句话必须点名怎么自救。
    #[test]
    fn an_unreadable_state_stops_everything_with_a_way_out() {
        let fake = Fake::new("review-broken-state").with_transcript(&[(0.0, 1.0, "各位好")]);
        std::fs::write(fake.layout().state_file(), b"not ours").unwrap();

        let err = crate::project::OpenProject::from_layout(fake.layout().clone())
            .unwrap_err()
            .to_string();
        assert!(err.contains("状态文件读不动"), "{err}");
        assert!(err.contains("upgrade"), "得说清怎么自救:{err}");
        assert!(!err.contains("删"), "不许出现『删』:{err}");
    }

    /// **状态文件不在 → 按"什么都还没核验"算(拦),不许按"没有待办"算(放行)。**
    ///
    /// 这是那次事故的方向问题:读不到时门要往**保守**那头倒。
    /// 拿不到任何便宜 —— 这正是它敢不报错的原因。
    #[test]
    fn a_missing_state_file_reads_as_nothing_verified() {
        let fake = two_shots("review-no-state");
        std::fs::remove_file(fake.layout().state_file()).unwrap();
        let opened = fake.open();
        assert!(
            opened.state_note().is_some_and(|n| n.contains("状态文件")),
            "状态不在要说出来:{:?}",
            opened.state_note()
        );

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("还没核验完"), "该按'都还没核验'拦:{err}");
        assert!(err.contains("s001"), "{err}");
    }

    /// 反向:**文件不在**照旧不是错误 —— 没跑过前处理的项目不该被这道门卡住。
    ///
    /// 两个方向都要钉:混成一件才是那次事故的根。
    #[test]
    fn a_missing_file_is_still_just_an_empty_project() {
        let fake = Fake::new("review-missing-files");
        let opened = fake.open();
        assert!(!fake.layout().shots_json().exists());
        assert!(!fake.layout().supplement_requests().exists());
        let s = state(&opened).unwrap();
        assert!(s.unchecked.is_empty() && s.needs_user.is_empty());
    }

    /// **门报的每一项,都要点出能消掉它的那一步。** 没有出路的规则不许进门的话术。
    ///
    /// 这条是拿一次死循环换来的:M4 第五轮门数着 298 条候选,而当时**没有任何工具
    /// 能改候选的状态** —— 模型撞了三轮过不去,最后自己动手改文件(值还写错了),
    /// 门跟着瞎掉、一路放行。
    #[test]
    fn every_gate_rule_names_the_step_that_clears_it() {
        let fake =
            Fake::new("review-gate-words").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let opened = fake.open();
        let err = gate_calibrate(&opened).unwrap_err().to_string();
        for tool in [
            "record_shot_check",
            "apply_corrections",
            "disposition_supplements",
        ] {
            assert!(err.contains(tool), "门的话里没点出 `{tool}`:{err}");
        }
    }

    /// ③ 等人拍板那一条:**两个出口都要说出来**,而且不许同一件事问两遍。
    #[test]
    fn the_waiting_rule_names_both_exits() {
        let fake = Fake::new("review-two-exits").with_transcript(&[(0.0, 2.0, "这个叫元祖")]);
        let mut opened = fake.open();

        {
            let entry = opened.state_mut().segment_mut(1);
            entry.set_conclusion(Some(vidbrief_project::Conclusion::Uncertain {
                note: "元祖 疑为 元组".into(),
                reason: "系列名".into(),
            }));
            entry.checked_at = Some(vidbrief_project::now());
        }

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("apply_corrections"), "{err}");
        assert!(err.contains("approve"), "{err}");
        assert!(
            err.contains("收手"),
            "第二个出口(保持存疑、就此收手)没说出来:{err}"
        );
        assert!(err.contains("不要再问一遍"), "{err}");

        // 出口②:他说"我不确定,先保留"也是拍板 —— 记下来门就该开
        approve(&mut opened, Some("用户说他也不确定,先保留".into())).unwrap();
        gate_calibrate(&opened).expect("记下他的拍板,门就该开");
    }
}
