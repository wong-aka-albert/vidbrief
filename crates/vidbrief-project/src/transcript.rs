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
}
