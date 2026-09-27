//! `transcript.json`(原始)与 `transcript.fixed.json`(纠错后)。
//!
//! 两者**共用同一 schema 与同一 Rust 类型**,见 `docs/schema/transcript.schema.json`。
//!
//! 关键约定:**原始转录永不就地改写**。
//! `01_preprocess/transcript.json` 里 `corrected_text` 与 `correction` 恒为 `None`;
//! agent 的纠错结论写在 `02_calibrate/transcript.fixed.json` 这份全量副本里。
//! 这样"agent 到底改了什么"永远可查 —— 如果就地改,原文就消失了,无法核对纠错是否合理。

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::relpath::RelPath;

/// 纠错的**性质**。只在"改了"这一种结论下才有 —— 它答的是"这算什么错"。
///
/// **没有"存疑"这一档**:存疑是一种**结论**(定不了),不是一种错误的性质。
/// 两者以前混在一个枚举里,于是"存疑但改了"这种自相矛盾的状态写得出来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionType {
    /// 同音错字(实测:「达摩院」→「打墨院」)。
    Homophone,
    /// 术语 / 专有名词(实测:「Python」→「派森」)。**这是中文 ASR 最高频的错误类型**。
    Terminology,
    /// 数字 / 单位 / 时间。
    Number,
    Other,
}

impl CorrectionType {
    /// 从 wire 名解析 —— **只给迁移老文件用**。
    ///
    /// 老文件里多一个 `uncertain`,那个值现在表示结论而不是性质,所以这里不认它
    /// (调用方按 `other` 兜底,而结论那边已经把它读成 `Uncertain` 了)。
    fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "homophone" => Self::Homophone,
            "terminology" => Self::Terminology,
            "number" => Self::Number,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// 一段转录的**结论**。四种,互斥且穷尽;字段是 `None` 表示"看过、没话说"。
///
/// # 为什么是一个枚举,而不是两个自由字段
///
/// 以前是 `corrected_text: Option<String>` + `correction: Option<Correction>` 两栏各填各的,
/// 于是"结论栏说要改、改后那栏空着"这种组合能写进去 —— 而**数数的时候没法算它**:
/// 算"改过"看的是文本栏空不空,算"存疑"看的是类型栏写没写存疑,中间那一档掉在地上。
/// M4 第五轮实测的对照表表头就是这么来的:「改过 27 段、存疑未改 0 段、合计动过 40 段」,
/// 27+0≠40 —— 那 13 段"有结论、正文没动"一个桶都没进。
///
/// 现在**改了的话,新文本就在这个枚举里**,不给它在旁边单开一栏:
/// 「说要改」和「改成什么」没法各说各话,"有结论却没落地"也就构造不出来。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum Conclusion {
    /// 改了。
    Corrected {
        /// 改后的整句。
        text: String,
        /// 这算什么错。
        #[serde(rename = "type")]
        kind: CorrectionType,
        /// 改动内容,如 `派森 → Python`。
        note: String,
        /// 判断依据(画面文字 / 上下文 / 术语表)。
        reason: String,
    },
    /// 看过,**决定保留原句**(比如"上一轮那处改写是错的,恢复原文")。
    Kept { note: String, reason: String },
    /// 看过,但定不了 —— 留给用户裁决。疑似写法写在 `note` 里。
    Uncertain { note: String, reason: String },
}

impl Conclusion {
    /// 改动内容 / 为什么这么定。
    pub fn note(&self) -> &str {
        match self {
            Self::Corrected { note, .. } | Self::Kept { note, .. } | Self::Uncertain { note, .. } => {
                note
            }
        }
    }

    /// 判断依据。
    pub fn reason(&self) -> &str {
        match self {
            Self::Corrected { reason, .. }
            | Self::Kept { reason, .. }
            | Self::Uncertain { reason, .. } => reason,
        }
    }

    /// 错的性质 —— 只有"改了"才有。
    pub fn kind(&self) -> Option<CorrectionType> {
        match self {
            Self::Corrected { kind, .. } => Some(*kind),
            _ => None,
        }
    }

    /// 一句话说法,给对照表和视图用。
    pub fn label(&self) -> &'static str {
        match self {
            Self::Corrected { .. } => "改了",
            Self::Kept { .. } => "决定不改",
            Self::Uncertain { .. } => "存疑未改",
        }
    }
}

/// 一段转录。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// 等于 SRT 里的序号,从 1 起。
    pub id: u32,
    pub start: f64,
    pub end: f64,
    /// ASR 原始输出,**永不就地改写**。
    pub text: String,
    /// 对这一段下的**结论**。`None` = 没话说(也可能压根还没看过 —— 看 `checked_at`)。
    ///
    /// 四种结论见 [`Conclusion`];"改了"的时候**新文本就在它里面**。
    #[serde(default)]
    pub conclusion: Option<Conclusion>,
    /// 模型**核验过这一段**的时刻。`None` = 还没核验。
    ///
    /// # 为什么要逐段记
    ///
    /// 审核门要拦的是"**跳过某一段**"。整份转录记一个"审完了"的标记,
    /// 跳了 300 段也看不出来 —— 那门就等于没设。所以标记落在段上。
    ///
    /// # 它和 `correction` 分工
    ///
    /// `correction` 记的是"**确实有话说**"(改了哪儿 / 哪儿没把握);
    /// 这一个记的是"**我看过这一段了**"。审过、没问题的那一段,前者是 `None`
    /// —— 而这正是它以前和"从没审过"分不开的原因。
    ///
    /// 三种核验结论由 [`Segment::review`] 一处读出,别让调用方各自拼。
    ///
    /// 用时间戳不用布尔:顺带答了"这个结论是哪一轮审的"。
    /// `#[serde(default)]` —— 老文件里没有这一段,读作"还没核验",不是错误。
    #[serde(default)]
    pub checked_at: Option<DateTime<FixedOffset>>,
}

/// 一段转录**当前处在哪种核验状态**。
///
/// 审核门数的就是这三个。它们覆盖了"模型对这一段做过什么"的全部可能 ——
/// 没有第四种,所以门不会卡死:模型永远有下一步动作可做
/// (接着审 / 去问用户 / 自己拍板)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentReview {
    /// 还没核验。**门的第一个拦截条件** —— 让模型接着审,不打扰人。
    Unchecked,
    /// 核验过,没话说(识别没问题),或者已经改定了。
    Checked,
    /// 核验过,但定不了 —— 请用户裁决。**门的第二个拦截条件**。
    ///
    /// 判据就是 `correction.type == uncertain`:那个类型的文档原话是
    /// "agent 认为这里可能有问题却不敢断言"—— **这句话本身就是模型在说"我定不了"**。
    NeedsUser,
}

impl Segment {
    /// 该段当前应该被读作文本(有纠错就用纠错后的)。
    ///
    /// 所有下游(视图、总结、报告)都应通过这里取文本,不要自己去读 `text` ——
    /// 否则纠错就白做了。
    pub fn effective_text(&self) -> &str {
        match &self.conclusion {
            Some(Conclusion::Corrected { text, .. }) => text,
            _ => &self.text,
        }
    }

    /// 是否被真的改过(区别于"决定不改"和"存疑未改")。
    pub fn is_corrected(&self) -> bool {
        matches!(self.conclusion, Some(Conclusion::Corrected { .. }))
    }

    /// 是否被标为存疑但未修改。
    pub fn is_uncertain(&self) -> bool {
        matches!(self.conclusion, Some(Conclusion::Uncertain { .. }))
    }

    /// 这一段**当前处在哪种核验状态**。审核门数的就是它。
    ///
    /// 判据收在这一处,不让每个调用方自己拼 —— 拼法一旦散开,
    /// 迟早出现"门说审完了、视图说没审"这种自相矛盾。
    pub fn review(&self) -> SegmentReview {
        if self.is_uncertain() {
            // 存疑优先:那一边更需要人。
            return SegmentReview::NeedsUser;
        }
        match self.checked_at {
            Some(_) => SegmentReview::Checked,
            None => SegmentReview::Unchecked,
        }
    }

    /// 时长(秒)。
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// 是否包含某时刻(闭区间)。
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t <= self.end
    }
}

/// 读老格式的字段 —— **只用来读,不会再写出去**。
///
/// 为什么留这一手:已经写出来的项目不能变成打不开。旧格式是两个自由字段
/// (`corrected_text` + `correction`),这里并成一条结论;下一次工具写回时,
/// 文件就自己迁移到新格式了。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentWire {
    id: u32,
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    conclusion: Option<Conclusion>,
    /// 旧字段:改后的整句。
    #[serde(default)]
    corrected_text: Option<String>,
    /// 旧字段:纠错说明。
    #[serde(default)]
    correction: Option<LegacyCorrection>,
    #[serde(default)]
    checked_at: Option<DateTime<FixedOffset>>,
}

/// 旧格式的纠错说明。`type` 收成字符串,因为旧值里有一个 `uncertain` ——
/// 它现在表示**结论**(定不了),不再表示性质。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCorrection {
    #[serde(rename = "type")]
    kind: String,
    note: String,
    reason: String,
}

impl<'de> Deserialize<'de> for Segment {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let w = SegmentWire::deserialize(deserializer)?;
        let conclusion = w
            .conclusion
            .or_else(|| migrate_legacy(w.corrected_text, w.correction));
        Ok(Self {
            id: w.id,
            start: w.start,
            end: w.end,
            text: w.text,
            conclusion,
            checked_at: w.checked_at,
        })
    }
}

/// 老格式的两个字段 → 一条结论。
fn migrate_legacy(
    corrected_text: Option<String>,
    correction: Option<LegacyCorrection>,
) -> Option<Conclusion> {
    let (note, reason, kind) = match correction {
        Some(c) => (c.note, c.reason, c.kind),
        None => (String::new(), String::new(), String::new()),
    };
    let note = if note.is_empty() {
        "旧格式未给说明".to_string()
    } else {
        note
    };
    let reason = if reason.is_empty() {
        "旧格式未给依据".to_string()
    } else {
        reason
    };
    match (corrected_text, kind.as_str()) {
        (Some(text), _) => Some(Conclusion::Corrected {
            text,
            kind: CorrectionType::from_wire(&kind).unwrap_or(CorrectionType::Other),
            note,
            reason,
        }),
        (None, "uncertain") => Some(Conclusion::Uncertain { note, reason }),
        // 有结论、正文没动 —— 旧格式里这一档最常见的就是"看过,决定不改"
        (None, "") => None,
        (None, _) => Some(Conclusion::Kept { note, reason }),
    }
}

/// 全稿按状态分区数出来的数。**五类互斥且穷尽,和 = 总段数。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SegmentStats {
    pub total: usize,
    /// 还没核验。
    pub unchecked: usize,
    /// 看过、没话说。
    pub clean: usize,
    /// 改了正文。
    pub corrected: usize,
    /// 决定保留原句。
    pub kept: usize,
    /// 存疑留给人。
    pub uncertain: usize,
}

impl SegmentStats {
    /// 留下过结论的段数(= 改了 + 决定不改 + 存疑)。
    pub fn touched(&self) -> usize {
        self.corrected + self.kept + self.uncertain
    }
}

/// 组的划分规则 —— **只有浅特征,一处语义判断都不许有**。
///
/// 「这两句讲的是同一件事」是**判断**,归 agent(`AGENTS.md` §1 的第三条铁律);
/// 工具只认一件**形状**上的事:**whisper 按停顿把一句话切碎了**。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct GroupRule {
    /// 相邻两段之间静音超过它就断开(秒)。
    pub max_gap_seconds: f64,
    /// 一组最多多长(秒)。到顶就断,哪怕话没说完。
    pub max_seconds: f64,
}

impl Default for GroupRule {
    fn default() -> Self {
        Self {
            max_gap_seconds: 0.35,
            max_seconds: 12.0,
        }
    }
}

/// 一口气说下来的一小段 —— **核验与通读的单位**。
///
/// # 为什么要有这一层
///
/// whisper 按停顿切段:70 分钟的视频切出 **2269 段**,一段 1.8 秒、正文常常只有
/// 三五个字(实测 399 段 ≤4 字)。而审核门要求**每一段都有核验状态** ——
/// 于是模型每轮要吐两千多个数字、把 2269 段逐段读一遍,上下文当场顶穿
/// (2026-09-27 五份重跑:491k 输入 / 524k 上限,400 撞墙)。
///
/// 组把"一口气说完的一小段"合成一个单位:2269 段 → **387 组**,通读要读的行数
/// 降到六分之一,表态也一次报一个区间。
///
/// # 三条边界(写死)
///
/// - **段 id 一个字节都不动**:结论、`evidence.segment`、报告的段芯片、门的逐条报账
///   全按段 id 引用 —— 重编号等于把用户攒下的东西一次性作废。组只是**段的区间**。
/// - **组是核验单位,不是材料**:真值永远是**段**(原文)+ **状态**(结论)。
///   组算错了(把两句并了、把一句拆了)最多让人多点一次名,不污染数据。
/// - **组不落盘**:它是**现算的**(浅特征 + 原始转录),不是一个可以手改的文件。
///   落成文件就会多出一条侧门 —— 「把组改大 = 少核验」,而门要么信它(被绕过去),
///   要么不信它(那文件就只是个摆设)。
///
/// 划界读的是 [`Segment::text`](ASR 原文,永不就地改写)而**不是**改后的读法:
/// 原文不动,组就不会跟着"你刚改的那一句"漂移 —— 模型校准到一半,手上的组号
/// 整个换了一茬,那是没法干活的。
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentGroup {
    /// 从 1 起(每次现算都从 1 数)。
    pub id: u32,
    /// 首段段 id(含)。
    pub from: u32,
    /// 末段段 id(含)。
    pub to: u32,
    pub start: f64,
    pub end: f64,
}

impl SegmentGroup {
    /// 覆盖几段。
    pub fn len(&self) -> u32 {
        self.to.saturating_sub(self.from) + 1
    }

    /// 全部由一段组成(没什么可并的)。
    pub fn is_single(&self) -> bool {
        self.from == self.to
    }

    /// 与 `[start, end]` 有没有重叠 —— 取一段区间的组时用。
    pub fn overlaps(&self, start: f64, end: f64) -> bool {
        self.end >= start && self.start <= end
    }

    /// 组里读作什么(把各段现读法接起来)。
    ///
    /// **不加分隔符**:whisper 的段本来就是半句,中文之间插空格反而读不通。
    /// 要按语义断句得先看懂内容 —— 那是 agent 的活,不是这里的。
    ///
    /// 取段只有一处([`Transcript::segment_slice`]),拼法只有这一处:三处各写一份的话,
    /// "页面上读得到、凭据却找不到"迟早出现。
    pub fn text(&self, transcript: &Transcript) -> String {
        let mut out = String::new();
        for seg in transcript.segment_slice(self).unwrap_or_default() {
            out.push_str(seg.effective_text());
        }
        out
    }

    /// 人读的写法:`seg 12–14`。
    pub fn label(&self) -> String {
        if self.is_single() {
            format!("seg {}", self.from)
        } else {
            format!("seg {}–{}", self.from, self.to)
        }
    }

    /// 这一组读完了没有。**全组都核过才算核过。**
    ///
    /// 判据只有这一处(与 [`Segment::review`] 同一套),不让调用方各拼一份 ——
    /// 拼法一散,迟早出现"门说审完了、界面说没审"。
    pub fn review(&self, transcript: &Transcript) -> GroupReview {
        let mut state = GroupReview::Checked;
        for seg in transcript.segment_slice(self).unwrap_or_default() {
            match seg.review() {
                // 未核验是**最先要干的事**,它压过"等用户拍板"那一档
                SegmentReview::Unchecked => return GroupReview::Unchecked,
                SegmentReview::NeedsUser => state = GroupReview::NeedsUser,
                SegmentReview::Checked => {}
            }
        }
        state
    }
}

/// 一句话落在哪儿([`Transcript::locate_quote`] 的结果)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuoteHit {
    /// 引文**起点**所在的那一段 —— 存进 `evidence.segment` 的就是它。
    pub segment: u32,
    /// 所在的组号。
    pub group: u32,
    /// 那一组的段区间(含)。
    pub from: u32,
    pub to: u32,
}

/// 这一段读完了没有 —— **组的核验状态**。
///
/// 判据是"组内**全部**段都核验过"。不能"组里有一段核过就算整组看过":
/// 那正是「分不出看过没问题」那条顾虑要防的(跳几段不会被发现)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupReview {
    /// 组内还有没核验的段。
    Unchecked,
    /// 核验过,但组内有段在等用户拍板。
    NeedsUser,
    /// 全组核过。
    Checked,
}

/// 一组里的段首尾都要在(按 id 取切片时用)。
fn group_of(segments: &[&Segment], index: usize) -> SegmentGroup {
    let first = segments.first().expect("空组不该被造出来");
    let last = segments.last().expect("空组不该被造出来");
    SegmentGroup {
        id: index as u32 + 1,
        from: first.id,
        to: last.id,
        start: first.start,
        end: last.end,
    }
}

/// 这一段是不是说完了(句末标点)。
///
/// 全角 `!?` 用转义写出来 —— 它们在源码里长得和半角一模一样,一眼看不出差别,
/// 而"哪种标点算句末"正是这里唯一的判据。
fn ends_a_sentence(text: &str) -> bool {
    matches!(
        text.trim_end().chars().next_back(),
        Some('。' | '…' | '\u{ff01}' | '\u{ff1f}' | '!' | '?')
    )
}

/// 结构化转录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub source: RelPath,
    /// ASR 模型标识,如 `ggml-medium`。
    pub model: String,
    /// 语言代码,如 `zh`。
    pub language: String,
    pub duration: f64,
    /// 转录音频的时间。`transcript.fixed.json` **沿用**此值,不因纠错而更新 ——
    /// 它是"什么时候转的",不是"什么时候改的"。
    pub generated_at: DateTime<FixedOffset>,
    /// 按 `start` 升序,区间互不重叠。
    pub segments: Vec<Segment>,
}

impl Transcript {
    /// 找出包含时刻 `t` 的段。
    pub fn segment_at(&self, t: f64) -> Option<&Segment> {
        self.segments.iter().find(|s| s.contains(t))
    }

    /// 按**浅特征**把段并成组 —— 核验与通读的单位(见 [`SegmentGroup`])。
    pub fn groups(&self) -> Vec<SegmentGroup> {
        self.groups_with(GroupRule::default())
    }

    /// 同上,规则自己给。判据只有三个,全是形状上的:
    /// 相邻段之间的静音够短、整组没超时、上一段不是句末。
    ///
    /// 按 **start 升序**切(同一时刻按 id 定序),不看数组里的先后 ——
    /// 文件格式本身承诺"按 `start` 升序、区间互不重叠"(`transcript.rs` 的 `Transcript`),
    /// 但读盘那一层不校验它;哪天上游不守约,这里按位置切就会**悄悄切出错组**,
    /// 而错组不会报错,只会让门少数几组。显式排一次,代价是 n log n 的一次比较。
    pub fn groups_with(&self, rule: GroupRule) -> Vec<SegmentGroup> {
        let mut order: Vec<&Segment> = self.segments.iter().collect();
        order.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });

        let mut out: Vec<SegmentGroup> = Vec::new();
        let mut start = 0usize;
        for i in 1..order.len() {
            let prev = order[i - 1];
            let cur = order[i];
            let contiguous = cur.start - prev.end < rule.max_gap_seconds;
            let within = cur.end - order[start].start <= rule.max_seconds;
            // **句末断开**这一条对没有标点的 ASR 输出不生效(实测那五份里只有 36 段
            // 带句号),留着是给带标点的输出用的 —— 它不是判据的主力,主力是时长上限。
            let sentence_done = ends_a_sentence(&prev.text);
            if !(contiguous && within && !sentence_done) {
                out.push(group_of(&order[start..i], out.len()));
                start = i;
            }
        }
        if start < order.len() {
            out.push(group_of(&order[start..], out.len()));
        }
        out
    }

    /// 这一段落在哪一组里。**给凭据校验用** —— 引文可以跨段,但不许跨组。
    pub fn group_holding(&self, segment_id: u32) -> Option<SegmentGroup> {
        self.groups()
            .into_iter()
            .find(|g| (g.from..=g.to).contains(&segment_id))
    }

    /// 一句话落在**哪一段**上(段号 = 引文**起点**所在的那一段)。
    ///
    /// # 为什么不是「逐段包含」
    ///
    /// 从前的判据是"这句话必须整句落在某一段里"。可 whisper 按停顿切段,
    /// 一句话常常横跨两三个碎段 —— 于是材料里明明有的原话被判成"找不到"
    /// (2026-09-27 实测撞了两次)。现在按**组**找:组是"一口气说完的一小段",
    /// 组内跨段是常态,跨组才是"两句话"。
    ///
    /// 落在哪一段由**拼起来的串里那个起点**决定(不是"第一个能装的段"——
    /// 那样报出来的段号会比真正的起点靠前,而段号正是留给人事后回去核对的定位)。
    ///
    /// 读的是 [`Segment::effective_text`](纠错后的现读法):引文是照着**现在读到的
    /// 材料**抄的,而材料唯一会变的途径就是纠错。
    pub fn locate_quote(&self, quote: &str) -> Option<QuoteHit> {
        let quote = quote.trim();
        if quote.is_empty() {
            return None;
        }
        for group in self.groups() {
            let segs = match self.segment_slice(&group) {
                Some(s) => s,
                None => continue,
            };
            let joined = group.text(self);
            // 每段在拼串里的起点(字节)—— 只用来比大小,**不拿来切片**,
            // 所以中文的多字节在这里不构成裸切片风险(AGENTS §1.1)。
            let mut starts: Vec<(usize, u32)> = Vec::with_capacity(segs.len());
            let mut cursor = 0usize;
            for seg in segs {
                starts.push((cursor, seg.id));
                cursor += seg.effective_text().len();
            }
            let Some(at) = joined.find(quote) else {
                continue;
            };
            let anchor = starts
                .iter()
                .rev()
                .find(|(offset, _)| *offset <= at)
                .map(|(_, id)| *id)
                .unwrap_or(group.from);
            return Some(QuoteHit {
                segment: anchor,
                group: group.id,
                from: group.from,
                to: group.to,
            });
        }
        None
    }

    /// 按组取那几段(按 **id** 找首尾,再按**位置**切)。
    ///
    /// 不假设"段号 = 数组下标":文件里 ids 从 1 起、按时间排,但读盘那一层不校验,
    /// 上游不守约时按下标切会悄悄切错(`groups_with` 那段注释说了同一件事)。
    /// 首尾 id 都在、位置也对得上,才给这一段切片。
    pub fn segment_slice(&self, group: &SegmentGroup) -> Option<&[Segment]> {
        let a = self.segments.iter().position(|s| s.id == group.from)?;
        let b = self.segments.iter().position(|s| s.id == group.to)?;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        Some(&self.segments[lo..=hi])
    }

    /// 取 `[start, end]` 区间内重叠的所有段。
    pub fn segments_in(&self, start: f64, end: f64) -> impl Iterator<Item = &Segment> {
        self.segments
            .iter()
            .filter(move |s| s.end >= start && s.start <= end)
    }

    /// 组装一段人可读的文本,如 `(9.0-11.5) 好我们切到主图了`。
    ///
    /// 用于给 agent 的"同期转录"上下文,以及视图中的悬浮提示。
    pub fn render_range(&self, start: f64, end: f64) -> String {
        let mut out = String::new();
        for seg in self.segments_in(start, end) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!(
                "({:.1}-{:.1}) {}",
                seg.start,
                seg.end,
                seg.effective_text()
            ));
        }
        out
    }

    /// 是否所有段都没有任何结论。
    pub fn is_untouched(&self) -> bool {
        self.segments.iter().all(|s| s.conclusion.is_none())
    }

    /// 数一遍全稿。**口径只有这一处**。
    ///
    /// 对照表表头、报告里的"改了多少段"、视图上的统计,全都读它 ——
    /// 免得各算各的。M4 第五轮实测:同一份材料,表头算出来 27(把"改成跟原文一样"
    /// 也算改过,又漏掉 13 段"有结论没落改"),报告照抄了那个 27,而真改了正文的只有 16 段。
    pub fn stats(&self) -> SegmentStats {
        let mut s = SegmentStats {
            total: self.segments.len(),
            ..Default::default()
        };
        for seg in &self.segments {
            // 五类,互斥且穷尽 —— 每个段只进一个桶,所以下面那条断言是白送的保障
            match (&seg.conclusion, seg.checked_at.is_some()) {
                (Some(Conclusion::Corrected { .. }), _) => s.corrected += 1,
                (Some(Conclusion::Kept { .. }), _) => s.kept += 1,
                (Some(Conclusion::Uncertain { .. }), _) => s.uncertain += 1,
                (None, true) => s.clean += 1,
                (None, false) => s.unchecked += 1,
            }
        }
        debug_assert_eq!(
            s.unchecked + s.clean + s.corrected + s.kept + s.uncertain,
            s.total,
            "分区必须盖满全稿 —— 漏一档就是那次 27+0≠40"
        );
        s
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript() -> Transcript {
        let seg = |id: u32, start: f64, end: f64, text: &str| Segment {
            id,
            start,
            end,
            text: text.to_string(),
            conclusion: None,
            checked_at: None,
        };
        Transcript {
            source: RelPath::new("source/video.mp4").unwrap(),
            model: "ggml-medium".into(),
            language: "zh".into(),
            duration: 24.0,
            generated_at: crate::now(),
            segments: vec![
                seg(1, 0.0, 2.5, "各位好"),
                seg(2, 2.5, 6.0, "今天用一个很短的小样例过一遍流程"),
                seg(3, 6.0, 8.9, "现在是讲解人画面这一段基本没变化"),
            ],
        }
    }

    fn corrected(text: &str) -> Conclusion {
        Conclusion::Corrected {
            text: text.into(),
            kind: CorrectionType::Homophone,
            note: "派森 → Python".into(),
            reason: "画面标题".into(),
        }
    }

    #[test]
    fn effective_text_prefers_correction() {
        let mut t = transcript();
        assert_eq!(t.segments[0].effective_text(), "各位好");
        t.segments[0].conclusion = Some(corrected("各位好,开始"));
        assert_eq!(t.segments[0].effective_text(), "各位好,开始");
        assert!(t.segments[0].is_corrected());
    }

    #[test]
    fn uncertain_does_not_change_text() {
        let mut t = transcript();
        t.segments[0].conclusion = Some(Conclusion::Uncertain {
            note: "疑似漏字".into(),
            reason: "画面标注与语音不一致".into(),
        });
        assert!(t.segments[0].is_uncertain());
        assert!(!t.segments[0].is_corrected());
        assert_eq!(t.segments[0].effective_text(), "各位好");
    }

    /// **五类互斥且穷尽,和必须等于总段数。**
    ///
    /// 这条钉的是那次 27+0≠40:以前"改了多少段"是从两个自由字段推出来的,
    /// 推法一变数就变,还有一整档("有结论、正文没动")掉在地上没人算。
    #[test]
    fn the_partition_covers_every_segment() {
        let mut t = transcript();
        // 1 改了 / 2 决定不改 / 3 存疑,全都没核验戳 vs 有戳,四种结论各来一遍
        t.segments[0].conclusion = Some(corrected("各位好,开始"));
        t.segments[0].checked_at = Some(crate::now());
        t.segments[1].conclusion = Some(Conclusion::Kept {
            note: "上一轮那处改写是错的".into(),
            reason: "原句通顺".into(),
        });
        t.segments[1].checked_at = Some(crate::now());
        t.segments[2].conclusion = Some(Conclusion::Uncertain {
            note: "疑似漏字".into(),
            reason: "听不清".into(),
        });
        t.segments[2].checked_at = Some(crate::now());

        let s = t.stats();
        assert_eq!(s.total, 3);
        assert_eq!(s.corrected, 1);
        assert_eq!(s.kept, 1);
        assert_eq!(s.uncertain, 1);
        assert_eq!(s.clean, 0);
        assert_eq!(s.unchecked, 0);
        assert_eq!(s.touched(), 3, "留下过结论的就是这三段");
        assert_eq!(
            s.unchecked + s.clean + s.corrected + s.kept + s.uncertain,
            s.total
        );

        // 全清空 → 全落在"还没核验"这一个桶里
        for seg in t.segments.iter_mut() {
            seg.conclusion = None;
            seg.checked_at = None;
        }
        assert_eq!(t.stats().unchecked, 3);
        assert_eq!(t.stats().touched(), 0);

        // 只有戳、没有结论 → "看过、没问题"
        for seg in t.segments.iter_mut() {
            seg.checked_at = Some(crate::now());
        }
        let s = t.stats();
        assert_eq!(s.clean, 3);
        assert_eq!(s.unchecked + s.clean + s.corrected + s.kept + s.uncertain, s.total);
    }

    /// "决定不改"和"改了"是两种结论,别混 —— 前者正文一个字没动。
    #[test]
    fn kept_is_not_corrected() {
        let mut t = transcript();
        t.segments[0].conclusion = Some(Conclusion::Kept {
            note: "撤销上一轮的改写".into(),
            reason: "原句通顺".into(),
        });
        assert!(!t.segments[0].is_corrected());
        assert_eq!(t.segments[0].effective_text(), "各位好");
        assert_eq!(t.stats().kept, 1);
        assert_eq!(t.stats().corrected, 0);
    }

    /// 老格式(两个自由字段)读得进来,而且按下面的规则并成一条结论。
    ///
    /// 为什么必须认:工具写出来的项目不能因为一次重构就打不开 ——
    /// 而"打不开"在新规矩下意味着"按还没核验算",用户的活等于白做。
    #[test]
    fn legacy_two_field_segments_still_load() {
        let json = r#"{
          "id": 7, "start": 1.0, "end": 2.0, "text": "派森列表",
          "corrected_text": "Python 列表",
          "correction": {"type": "terminology", "note": "派森 → Python", "reason": "画面标题"},
          "checked_at": "2026-09-24T21:20:00+08:00"
        }"#;
        let seg: Segment = serde_json::from_str(json).unwrap();
        assert!(seg.is_corrected());
        assert_eq!(seg.effective_text(), "Python 列表");
        match seg.conclusion.as_ref().unwrap() {
            Conclusion::Corrected { kind, .. } => assert_eq!(*kind, CorrectionType::Terminology),
            other => panic!("该读成「改了」:{other:?}"),
        }

        // 老格式的 uncertain → 结论"存疑",而且不带文本
        let json = r#"{
          "id": 8, "start": 1.0, "end": 2.0, "text": "元祖类型",
          "corrected_text": null,
          "correction": {"type": "uncertain", "note": "元祖 疑为 元组", "reason": "系列名"},
          "checked_at": "2026-09-24T21:20:00+08:00"
        }"#;
        let seg: Segment = serde_json::from_str(json).unwrap();
        assert!(seg.is_uncertain());
        assert_eq!(seg.effective_text(), "元祖类型", "存疑不改文本");

        // 有结论、正文没动、类型又不是 uncertain → "决定不改"(旧格式里那一档最接近的意思)
        let json = r#"{
          "id": 9, "start": 1.0, "end": 2.0, "text": "原句通顺",
          "corrected_text": null,
          "correction": {"type": "other", "note": "撤销上一轮的改写", "reason": "原句通顺"},
          "checked_at": null
        }"#;
        let seg: Segment = serde_json::from_str(json).unwrap();
        assert!(matches!(seg.conclusion, Some(Conclusion::Kept { .. })));

        // 两栏都空、只有核验戳 → "看过、没问题"
        let json = r#"{
          "id": 10, "start": 1.0, "end": 2.0, "text": "各位好",
          "corrected_text": null, "correction": null,
          "checked_at": "2026-09-24T21:20:00+08:00"
        }"#;
        let seg: Segment = serde_json::from_str(json).unwrap();
        assert!(seg.conclusion.is_none());
        assert_eq!(seg.review(), SegmentReview::Checked);
    }

    #[test]
    fn lookup_and_render() {
        let t = transcript();
        assert_eq!(t.segment_at(3.0).unwrap().id, 2);
        assert!(t.segment_at(23.9).is_none());
        assert_eq!(t.segments_in(2.0, 6.5).count(), 3);

        let rendered = t.render_range(2.0, 6.5);
        assert!(rendered.contains("(2.5-6.0) 今天用一个很短的小样例过一遍流程"), "{rendered}");
        assert!(t.is_untouched());
    }

    #[test]
    fn conclusion_wire_shape() {
        assert_eq!(
            serde_json::to_string(&CorrectionType::Terminology).unwrap(),
            "\"terminology\""
        );
        let text = serde_json::to_string(&corrected("Python 列表")).unwrap();
        assert!(text.contains("\"outcome\":\"corrected\""), "{text}");
        assert!(text.contains("\"type\":\"homophone\""), "{text}");
        // 三种结论的 wire 名
        let kept = serde_json::to_string(&Conclusion::Kept {
            note: "n".into(),
            reason: "r".into(),
        })
        .unwrap();
        assert!(kept.contains("\"outcome\":\"kept\""), "{kept}");
        let uncertain = serde_json::to_string(&Conclusion::Uncertain {
            note: "n".into(),
            reason: "r".into(),
        })
        .unwrap();
        assert!(uncertain.contains("\"outcome\":\"uncertain\""), "{uncertain}");
    }

    #[test]
    fn round_trip() {
        let t = transcript();
        let text = crate::json::to_pretty_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&text).unwrap();
        assert_eq!(back, t);
    }

    // ── 组:核验与通读的单位(2026-09-27,清账十七) ──────────────────────

    /// 连成一气的三句合成一组;中间空一下(静音)就从那儿断开。
    #[test]
    fn a_pause_breaks_the_group() {
        let mut t = transcript();
        // 2 与 3 之间留 3 秒 —— 超过 0.35s 的静音阈值
        t.segments[2].start = 9.0;
        t.segments[2].end = 11.0;

        let groups = t.groups();
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!((groups[0].from, groups[0].to), (1, 2));
        assert_eq!((groups[1].from, groups[1].to), (1 + 2, 3));
        assert_eq!(groups[0].id, 1);
        assert_eq!(groups[1].id, 2);
        assert!(!groups[0].is_single());
        assert!(groups[1].is_single());
        assert_eq!(groups[0].label(), "seg 1–2");
        assert_eq!(groups[1].label(), "seg 3");
    }

    /// 一口气说个不停也要断开:一组最多 12 秒 —— 不然一段安静的长独白会并成一组。
    #[test]
    fn a_long_monologue_is_cut_by_the_duration_cap() {
        let seg = |id: u32, start: f64, end: f64, text: &str| Segment {
            id,
            start,
            end,
            text: text.to_string(),
            conclusion: None,
            checked_at: None,
        };
        // 每段 5 秒、首尾相接,共 20 秒
        let t = Transcript {
            source: RelPath::new("source/video.mp4").unwrap(),
            model: "m".into(),
            language: "zh".into(),
            duration: 20.0,
            generated_at: crate::now(),
            segments: (0..4)
                .map(|i| {
                    let start = i as f64 * 5.0;
                    seg(i + 1, start, start + 5.0, "一直说一直说")
                })
                .collect(),
        };

        let groups = t.groups();
        assert_eq!(groups.len(), 2, "12 秒的上限该把它切成 2 组:{groups:?}");
        assert_eq!((groups[0].from, groups[0].to), (1, 2));
        assert!(groups[0].end - groups[0].start <= 12.0);
        assert_eq!((groups[1].from, groups[1].to), (3, 4));
    }

    /// 句末标点断开(对带标点的 ASR 输出才有用 —— 那五份实测里只有 36 段带句号)。
    #[test]
    fn a_full_stop_ends_the_group() {
        let mut t = transcript();
        t.segments[1].text = "今天用一个很短的小样例过一遍流程。".into();
        let groups = t.groups();
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!((groups[0].from, groups[0].to), (1, 2));
    }

    /// **组跟着原文走,不跟着纠错漂移。**
    ///
    /// 划界读的是 ASR 原文(永不就地改写),所以模型校准到一半、改了几段正文,
    /// 手上的组号还是原来那套 —— 不然它得重新通读一遍才知道自己读到哪儿了。
    #[test]
    fn groups_do_not_drift_when_text_is_corrected() {
        let t = transcript();
        let before = t.groups();

        let mut after = t.clone();
        // 改掉正文、甚至把标点也改了(改后版本结尾多了句号)
        after.segments[0].conclusion = Some(corrected("各位好,我们开始。"));
        after.segments[1].checked_at = Some(crate::now());
        after.segments[2].conclusion = Some(Conclusion::Uncertain {
            note: "疑似漏字".into(),
            reason: "听不清".into(),
        });

        assert_eq!(after.groups(), before, "组不该跟着纠错漂移");
    }

    /// 组的核验状态:**组内全核过才算核过**;有未核验的压过"等用户拍板"。
    #[test]
    fn a_group_is_checked_only_when_all_of_its_segments_are() {
        let mut t = transcript();
        for s in &mut t.segments {
            s.checked_at = Some(crate::now());
        }
        let g = &t.groups()[0];
        assert_eq!(g.review(&t), GroupReview::Checked);

        t.segments[1].checked_at = None;
        assert_eq!(g.review(&t), GroupReview::Unchecked, "剩一段没核就是没核");

        // 未核验那一段补上、另一段标存疑 → 这一组是在等人拍板
        t.segments[1].checked_at = Some(crate::now());
        t.segments[2].conclusion = Some(Conclusion::Uncertain {
            note: "疑似漏字".into(),
            reason: "听不清".into(),
        });
        assert_eq!(g.review(&t), GroupReview::NeedsUser);

        // 两者同时存在时,未核验优先 —— 那是还没干完的活
        t.segments[1].checked_at = None;
        assert_eq!(g.review(&t), GroupReview::Unchecked);
    }

    /// 组覆盖到每一段,一段不漏、不重、id 连续。
    #[test]
    fn the_groups_cover_every_segment_exactly_once() {
        let t = transcript();
        let groups = t.groups();
        // 这一份首尾相接、总共 8.9 秒 —— 并成一组
        assert_eq!(groups.len(), 1, "{groups:?}");
        let flat: Vec<u32> = groups.iter().flat_map(|g| g.from..=g.to).collect();
        let ids: Vec<u32> = t.segments.iter().map(|s| s.id).collect();
        assert_eq!(flat, ids);
        assert_eq!(groups[0].id, 1);

        // 断开之后,两组的区间首尾相接、号也接着数
        let mut split = t.clone();
        split.segments[2].start = 9.0;
        split.segments[2].end = 11.0;
        let groups = split.groups();
        assert_eq!(
            groups.iter().map(|g| g.id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let flat: Vec<u32> = groups.iter().flat_map(|g| g.from..=g.to).collect();
        assert_eq!(flat, ids, "断开也不许漏段");
    }

    /// **一句话跨几个碎段**:照样找得到,段号落在引文的**起点**那一段。
    #[test]
    fn a_quote_may_span_segments_and_anchors_where_it_starts() {
        let t = transcript();
        // 「小样例过一遍流程现在是讲解人画面」横跨第 2、3 两段
        let hit = t.locate_quote("小样例过一遍流程现在是讲解人画面").expect("该找得到");
        assert_eq!(hit.segment, 2, "落在起点那一段:{hit:?}");

        // 整句在一段里 → 还是那一段
        assert_eq!(t.locate_quote("各位好").unwrap().segment, 1);
        // 头尾空白不算数
        assert_eq!(t.locate_quote("  各位好 ").unwrap().segment, 1);
        // 组号也给出来了
        assert_eq!(t.locate_quote("各位好").unwrap().group, 1);

        // 空引文 / 材料里没有的 → 找不到
        assert!(t.locate_quote("   ").is_none());
        assert!(t.locate_quote("这句话不在材料里").is_none());
    }

    /// **跨组不算**:那是两句话,不该拿它当一条凭据。
    #[test]
    fn a_quote_across_two_groups_is_refused() {
        let mut t = transcript();
        // 让 2、3 之间断开 → 「各位好」在组 1,后两句在组 2
        t.segments[2].start = 9.0;
        t.segments[2].end = 11.0;
        assert_eq!(t.groups().len(), 2);

        assert!(
            t.locate_quote("小样例过一遍流程现在是讲解人画面").is_none(),
            "跨组的引文不该被认下"
        );
        // 组里的那半句还认
        assert_eq!(t.locate_quote("现在是讲解人画面").unwrap().segment, 3);
    }

    /// 凭据是照**现读法**抄的:段里的字改过之后,引改后的那句话照样落在那一段上。
    #[test]
    fn a_quote_anchors_against_the_text_you_actually_read() {
        let mut t = transcript();
        t.segments[0].conclusion = Some(corrected("各位好,咱们开始"));
        assert_eq!(t.locate_quote("各位好,咱们开始").unwrap().segment, 1);
    }

    /// 一组落在哪一段上:`group_holding` 反向查。
    #[test]
    fn group_holding_finds_the_group_of_a_segment() {
        let mut t = transcript();
        t.segments[2].start = 9.0;
        t.segments[2].end = 11.0;
        assert_eq!(t.group_holding(1).unwrap().id, 1);
        assert_eq!(t.group_holding(2).unwrap().id, 1, "1、2 是一组");
        assert_eq!(t.group_holding(3).unwrap().id, 2);
        assert!(t.group_holding(99).is_none(), "没有的段号 → 没有的组");
    }

    /// 空转录不炸:一组都没有。
    #[test]
    fn an_empty_transcript_has_no_groups() {
        let mut t = transcript();
        t.segments.clear();
        assert!(t.groups().is_empty());
        assert!(t.locate_quote("各位好").is_none());
    }
}
