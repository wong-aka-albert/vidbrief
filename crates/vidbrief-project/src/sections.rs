//! 总结的**分段**(`03_summary/sections.json`)—— 步 3 的结构化产物。
//!
//! # 为什么要有它(2026-09-27 改)
//!
//! 从前步 3 只落一篇散文(`summary.md`),里面**没有"哪一段对应哪一刻"的结构**。
//! 而报告页要往时间轴上挂点 —— 于是步 4 只能**回头再问模型要一遍结构化数据**
//! (一次十几 KB 的 `topics[]`):同一份内容被写两遍(一遍散文、一遍 JSON),
//! 还顺带逼着模型当打字机(照抄原话、报段号)。实测那正是弱模型翻车的地方。
//!
//! 现在:**结构在写的时候就有**。一段 = 一个小节(讲哪一刻、什么标题、正文是什么),
//! 写一段提交一段;报告只是把它摆到页面上 —— **渲染不再需要模型给任何数据**。
//!
//! # 段的两态
//!
//! | 有 `t` | 报告页上是什么 |
//! |--------|----------------|
//! | 有 | 时间轴上的一**条**:圆点、浮层解析、关联的图与转录段 |
//! | 没有 | **总述**(对整支视频的总起/收束)—— 只在右侧面板里,不上时间轴 |
//!
//! 证据(那一刻说了什么、画面上是什么)由工具按 `t` 就近挂,模型不用给。

use serde::{Deserialize, Serialize};

/// 一段总结。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// 锚点:这一节讲的是视频里的哪一刻(秒)。
    ///
    /// **不给 = 不上时间轴**(总述那种)。给的话它同时决定报告页上圆点的位置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<f64>,
    /// 这一段叫什么。**也是它在页面上的标题** —— 一句话,让人一眼知道这条讲什么。
    pub title: String,
    /// 正文(markdown)。
    pub md: String,
    /// **待核验**:读的人该回头核一下的点(看不清但结论靠它的数字……)。
    ///
    /// 与正文分开列,是因为页面标题栏要能一句"还有 N 处待核验" ——
    /// 埋在正文里读的人看不出来。没有就空着。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verify: Vec<String>,
}

impl Section {
    /// 新的一段(标题 + 正文,没有锚点)。
    pub fn new(title: impl Into<String>, md: impl Into<String>) -> Self {
        Self {
            t: None,
            title: title.into(),
            md: md.into(),
            verify: Vec::new(),
        }
    }

    /// 带上锚点。
    pub fn at(mut self, t: f64) -> Self {
        self.t = Some(t);
        self
    }
}

/// 一个项目的全部分段(**按提交顺序**存)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sections {
    pub schema_version: u32,
    pub sections: Vec<Section>,
}

impl Default for Sections {
    fn default() -> Self {
        Self::new()
    }
}

/// 当前 schema 版本(加一栏就加一)。
pub const SECTIONS_SCHEMA_VERSION: u32 = 1;

impl Sections {
    pub fn new() -> Self {
        Self {
            schema_version: SECTIONS_SCHEMA_VERSION,
            sections: Vec::new(),
        }
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        crate::json::write_atomic(path, self)
    }

    /// 按标题认段:**同名 = 改写那一段**(原地,顺序不变),没有就追加。
    ///
    /// 为什么按标题认而不是按序号:改一稿时说"把《两种方案》那段改一下"
    /// 比"把第 7 段改一下"稳 —— 人记标题,不记序号;重排也不会错位。
    pub fn upsert(&mut self, section: Section) -> bool {
        if let Some(existing) = self
            .sections
            .iter_mut()
            .find(|s| s.title == section.title)
        {
            *existing = section;
            return false;
        }
        self.sections.push(section);
        true
    }

    /// 上时间轴的那些,按时刻排好。
    pub fn anchored(&self) -> Vec<&Section> {
        let mut picked: Vec<&Section> = self.sections.iter().filter(|s| s.t.is_some()).collect();
        picked.sort_by(|a, b| {
            a.t.unwrap_or(0.0)
                .total_cmp(&b.t.unwrap_or(0.0))
        });
        picked
    }

    /// 没上时间轴的那些(总述)。
    pub fn unanchored(&self) -> Vec<&Section> {
        self.sections.iter().filter(|s| s.t.is_none()).collect()
    }

    /// 拼成一篇读得下去的文章:**总述在前,其余按时刻**
    /// (人读的顺序,与报告页的顺序一致)。
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for section in self.unanchored() {
            out.push_str(&section.md);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        for section in self.anchored() {
            out.push_str(&format!("## {}\n\n", section.title));
            out.push_str(&section.md);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.trim_end().to_string() + "\n"
    }

    /// 空的段(标题或正文空白)会被挡在写入口,这里只做"有没有东西"的判断。
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sections {
        let mut sections = Sections::new();
        sections.upsert(Section::new("总述", "整支视频讲一件事。"));
        sections.upsert(Section::new("这一段是什么", "……").at(85.0));
        sections.upsert(Section::new("案例一", "……").at(357.0));
        sections
    }

    /// 同名 = 改写那一段,顺序不动;新标题 = 追加。
    #[test]
    fn the_same_title_rewrites_that_section_in_place() {
        let mut sections = sample();
        let replaced = sections.upsert(Section::new("这一段是什么", "改过的正文").at(90.0));
        assert!(!replaced, "同名该是改写,不是新增");
        assert_eq!(sections.sections.len(), 3, "段数不该变");
        assert_eq!(sections.sections[1].title, "这一段是什么");
        assert_eq!(sections.sections[1].md, "改过的正文");
        assert_eq!(sections.sections[1].t, Some(90.0), "锚点跟着改");

        let added = sections.upsert(Section::new("收尾", "……").at(891.0));
        assert!(added, "新标题该追加");
        assert_eq!(sections.sections.len(), 4);
    }

    /// 文章顺序:**总述在前,其余按时刻** —— 与人读的顺序一致,与报告页也一致。
    #[test]
    fn the_article_puts_the_overview_first_and_the_rest_by_time() {
        let mut sections = Sections::new();
        sections.upsert(Section::new("案例二", "第二个案例。").at(500.0));
        sections.upsert(Section::new("总述", "先看整体。"));
        sections.upsert(Section::new("案例一", "第一个案例。").at(100.0));

        let md = sections.to_markdown();
        let overview = md.find("先看整体").expect("总述要在");
        let first = md.find("第一个案例").expect("案例一要在");
        let second = md.find("第二个案例").expect("案例二要在");
        assert!(overview < first && first < second, "顺序该是 总述 → 时刻序:{md}");
        assert!(md.contains("## 案例一"), "锚点段带小标题:{md}");
    }

    /// 上时间轴的按时刻排好,不上的单独一列 —— 报告页两边各取所需。
    #[test]
    fn anchored_and_unanchored_split_cleanly() {
        let sections = sample();
        assert_eq!(sections.anchored().len(), 2);
        assert_eq!(sections.anchored()[0].title, "这一段是什么");
        assert_eq!(sections.unanchored().len(), 1);
        assert_eq!(sections.unanchored()[0].title, "总述");
    }
}
