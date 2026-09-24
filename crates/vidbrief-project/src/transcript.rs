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

/// 纠错类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionType {
    /// 同音错字(实测:「达摩院」→「打墨院」)。
    Homophone,
    /// 术语 / 专有名词(实测:「Python」→「派森」)。**这是中文 ASR 最高频的错误类型**。
    Terminology,
    /// 数字 / 单位 / 时间。
    Number,
    /// **存疑但没改**:agent 认为这里可能有问题却不敢断言。
    /// 此时 `corrected_text` 必须保持 `None`,疑似写法只写在 `note` 里,留给用户在审查节点裁决。
    Uncertain,
    Other,
}

/// 纠错说明。只在**确实有话说**时出现 —— 没问题的段落 `correction` 为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Correction {
    #[serde(rename = "type")]
    pub kind: CorrectionType,
    /// 改动内容,如 `派森 → Python`;`uncertain` 时写疑似正确写法。
    pub note: String,
    /// 判断依据(画面文字 / 上下文 / 术语表)。
    pub reason: String,
}

/// 一段转录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// 等于 SRT 里的序号,从 1 起。
    pub id: u32,
    pub start: f64,
    pub end: f64,
    /// ASR 原始输出,**永不就地改写**。
    pub text: String,
    /// agent 修正后的整句文本;未修改或不改时保持 `None`。
    pub corrected_text: Option<String>,
    pub correction: Option<Correction>,
}

impl Segment {
    /// 该段当前应该被读作文本(有纠错就用纠错后的)。
    ///
    /// 所有下游(视图、总结、报告)都应通过这里取文本,不要自己去读 `text` ——
    /// 否则纠错就白做了。
    pub fn effective_text(&self) -> &str {
        self.corrected_text.as_deref().unwrap_or(&self.text)
    }

    /// 是否被真的改过(区别于"存疑但没改")。
    pub fn is_corrected(&self) -> bool {
        self.corrected_text.is_some()
    }

    /// 是否被标记为存疑但未修改。
    pub fn is_uncertain(&self) -> bool {
        matches!(
            self.correction.as_ref().map(|c| c.kind),
            Some(CorrectionType::Uncertain)
        )
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

    /// 是否所有段都没有任何纠错标记(含"存疑但未改")。
    pub fn is_untouched(&self) -> bool {
        self.segments.iter().all(|s| s.correction.is_none())
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
            corrected_text: None,
            correction: None,
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

    #[test]
    fn effective_text_prefers_correction() {
        let mut t = transcript();
        assert_eq!(t.segments[0].effective_text(), "各位好");
        t.segments[0].corrected_text = Some("各位好,开始".into());
        assert_eq!(t.segments[0].effective_text(), "各位好,开始");
        assert!(t.segments[0].is_corrected());
    }

    #[test]
    fn uncertain_does_not_change_text() {
        let mut t = transcript();
        t.segments[0].correction = Some(Correction {
            kind: CorrectionType::Uncertain,
            note: "疑似漏字".into(),
            reason: "画面标注与语音不一致".into(),
        });
        assert!(t.segments[0].is_uncertain());
        assert!(!t.segments[0].is_corrected());
        assert_eq!(t.segments[0].effective_text(), "各位好");
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
    fn correction_type_wire_names() {
        assert_eq!(
            serde_json::to_string(&CorrectionType::Terminology).unwrap(),
            "\"terminology\""
        );
        let c = Correction {
            kind: CorrectionType::Homophone,
            note: "打墨院 → 达摩院".into(),
            reason: "上下文在讲某机构".into(),
        };
        let text = serde_json::to_string(&c).unwrap();
        assert!(text.contains("\"type\":\"homophone\""), "{text}");
    }

    #[test]
    fn round_trip() {
        let t = transcript();
        let text = crate::json::to_pretty_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&text).unwrap();
        assert_eq!(back, t);
    }
}
