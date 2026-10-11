//! 报告数据(`04_report/report.json`)—— 报告渲染器的输入。
//!
//! # 形态
//!
//! 两级内容([260925-29]):**全局总结**(对整支视频的总述)+ **局部知识点**
//! (挂在具体时刻上的解析)。**渲染链是数据为源**([260926-07]):
//! `report.html` 从这份数据渲染,`report.md` 是它的一个**导出视图** ——
//! 所以这份 JSON 是判据,两个输出都是它的投影。交互与浮层位置 Markdown 表达不了,
//! 数据在、样式随便换;这一条是 M6 开工前定下的。
//!
//! # 每条知识点都挂在锚点上
//!
//! `t`(时刻)必给 —— 时间轴的定位与浮层都靠它;`shots` / `segments` 是**关联**
//! (有就渲染出图与转录入口,没有也成立)。这三样 M4 已经铺好:
//! 截图 id、转录段号、时刻。
//!
//! # 来源分两类,不混在一个自由文本里
//!
//! `sources` 把**讲者原话**(转录段 + **照抄**的 quote)与**外部补充**(标题 + URL)
//! 分开记 —— 读的人要一眼分得清哪句是听来的、哪句是找来的([260926-07] 决策二)。
//! `segment` 与 `ShotCheck.evidence` 同款:段号由工具按 `quote` 找出来,
//! 不让模型自己报(它报段号就会报错段号)。

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::ids::ShotId;

/// 一条知识点的来源。**两类分开**,各带各自的凭据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TopicSource {
    /// 讲者原话:照抄的转录原文,以及它落在哪一段。
    Speaker {
        /// 转录段 id(`quote` 在该段的**当前文本**里找得到)。
        segment: u32,
        /// 照抄的一句原文 —— 与 `ShotCheck.evidence` 同一条规矩。
        quote: String,
    },
    /// 外部补充(web search / 知识库):标题 + 出处。**页面上必须能点回去**。
    External {
        /// 这条补充说的大意(一句话)。
        title: String,
        /// 出处链接。
        url: String,
    },
}

/// 一条局部知识点 —— 挂在时刻 `t` 上。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportTopic {
    /// 项目内唯一,从 1 起。
    pub id: u32,
    /// 锚点:视频内时刻(秒)。点它 → 时间轴定位;浮层也挂在这儿。
    pub t: f64,
    /// 一句话标题。
    pub title: String,
    /// 解析正文(markdown):整理上下文逻辑、阐述关键概念与逻辑。
    /// **不是复述转录** —— 复述留给转录本身,这里要的是"读懂了之后的说法"。
    pub body: String,
    /// 关联截图。空数组 = 这条不挂图(合法,渲染时就不出图)。
    pub shots: Vec<ShotId>,
    /// 关联转录段(可点回原文)。
    pub segments: Vec<u32>,
    /// 来源。空数组 = 还没标来源(渲染时会显著提示 —— 没有来源的结论不可核)。
    pub sources: Vec<TopicSource>,
    /// **待核验**:读的人该回头核一下的点。
    ///
    /// 主要接住"**画面看不清、但结论靠它**"的那类东西(版本号、规格、比例):
    /// 这种数字**不许静默略过** —— 略过了结论就少了一块,而读的人不知道少了。
    /// 正确做法是**把结论连同它的不确定一起写出来**(「约 3.12 一带(数字待核验)」),
    /// 并在这儿记一条"核什么、去哪儿核"。写法见 `skill/workflow.md`。
    ///
    /// 空数组 = 这条没有要核的东西(`#[serde(default)]` 是为了读得动这一栏加进来之前的报告)。
    #[serde(default)]
    pub verify: Vec<String>,
}

/// 报告的**风格轮廓**(预设)—— 轮廓决定"怎么填",见 `skill/styles.md`。
///
/// 这些名字是**用户能点的把手**:用户说「换成快扫版」,模型据此换一套轮廓
/// (条数、每条骨架、详略),而不是把语气词换一换。
///
/// 定义**不在代码里**,在 `skill/styles.md`(它同时是 `get_guide(step="styles")`
/// 发给任何宿主的那一份)—— 手艺会变,变了改文档,不用动代码;这里只留一组名字与中文标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportPreset {
    /// 快扫版:看过视频的人扫一眼就想起来(结论先行、少图)。
    Quick,
    /// 教学版:没基础的人要真学会(每条讲透、术语首次出现即解释)。
    Teaching,
    /// 故事版:顺着视频时间线一趟讲下来(连着写、少列表)。
    Story,
    /// 复盘版:拿它回看自己的操作(判断 → 证据 → 结果 → 怎么用)。
    Review,
    /// 汇报版:给没看过视频的人交代(结论 → 影响 → 动作)。
    Exec,
    /// 自定义:用户的要求拼不出上面任何一种(要求写进 `notes`)。
    Custom,
}

impl ReportPreset {
    /// 中文标签(给人看的;线上格式是 `quick` / `teaching` / …)。
    pub fn label(self) -> &'static str {
        match self {
            Self::Quick => "快扫版",
            Self::Teaching => "教学版",
            Self::Story => "故事版",
            Self::Review => "复盘版",
            Self::Exec => "汇报版",
            Self::Custom => "自定义",
        }
    }

    /// 线上写法 —— 与 `serde` 的 rename 一致,也是 `skill/styles.md` 里的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Teaching => "teaching",
            Self::Story => "story",
            Self::Review => "review",
            Self::Exec => "exec",
            Self::Custom => "custom",
        }
    }

    /// 全部预设(测试拿它跟 `skill/styles.md` 对账:文档里少一种,那条测试就红)。
    pub const ALL: [Self; 6] = [
        Self::Quick,
        Self::Teaching,
        Self::Story,
        Self::Review,
        Self::Exec,
        Self::Custom,
    ];
}

/// 报告的口味 —— **一份要照着填的范式**,不是包装饰。
///
/// 用户的提示词里带着他要什么:给谁看、要多细、什么语气、要不要例子。
/// 这些东西**必须有个落点**,否则模型会自己默认一个,
/// 而用户拿到手才发现不是他要的(改起来等于重出一份)。
///
/// 两处刻意的取舍:
///
/// - **轮廓在文档里,名字在代码里**:五种预置风格怎么填(条数、每条骨架、证据密度)
///   写在 `skill/styles.md` —— 工具只记**用户点了哪一种**与这一版实到的两条属性。
///   把轮廓固化进代码,等于让"怎么写"这件事没法改,而它恰恰是最常改的那一样。
/// - **粒度是闭集合、语气是自由文本**:粒度决定**结构与篇幅**(多少条、每条多长),
///   是设计决策,三档够用、也说得清;语气穷举不了 —— 「大白话 / 诙谐 / 讲故事 /
///   行业腔」后面还会长出新说法,列不全就别列。但**必须写出来**:
///   交付时要能一句话说清「这份是按什么做的」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportStyle {
    /// 风格轮廓(用户点的那个名字)。**轮廓管"怎么填"**,见 `skill/styles.md`。
    ///
    /// **新报告一律要有**(工具侧的参数是必填,`check_style` 兜底);这里是 `Option`,
    /// 只为**读得动**这一栏加进来之前落的那批数据 —— 页面少显示一个名字,别的照旧。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<ReportPreset>,
    /// 这一版实到的粒度(条数与篇幅的档位)—— 预置给默认值,用户点名就改。
    pub granularity: Granularity,
    /// 这一版实到的语气(**自由文本**):大白话 / 诙谐 / 讲故事 / 行业腔……
    pub tone: String,
    /// 用户还有别的交代就写这儿(给谁看、要不要例子、要不要结论先行)。没交代就不给。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl ReportStyle {
    /// 一句话说清"这份是按什么做的" —— 页面上的小标签、导出的头一行、交付时那句话,
    /// 都从这儿来(三处一个说法,免得各写各的)。
    pub fn describe(&self) -> String {
        match self.preset {
            Some(preset) => format!(
                "{} · {} · {}",
                preset.label(),
                self.granularity.label(),
                self.tone
            ),
            // 这一栏加进来之前的旧数据:没有轮廓名,别的照说
            None => format!("{} · {}", self.granularity.label(), self.tone),
        }
    }
}

/// 知识粒度 —— **闭集合**:它同时决定条数与篇幅的档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// 概括:只留骨架(几处关键的、串起来的那条主线),每条一两句。
    Brief,
    /// 标准(没交代时的默认):视频里**逐个讲的逐个成条**,每条一段到几段。
    Standard,
    /// 详细:在标准之上把每条讲透 —— 背景、数字、边界条件都写进去。
    Detailed,
}

impl Granularity {
    /// 中文档位名(给人看的;线上格式是 `brief` / `standard` / `detailed`)。
    pub fn label(self) -> &'static str {
        match self {
            Self::Brief => "概括",
            Self::Standard => "标准",
            Self::Detailed => "详细",
        }
    }

    /// 线上写法(`brief` / `standard` / `detailed`)—— 与 `serde` 的 rename 一致。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Standard => "standard",
            Self::Detailed => "detailed",
        }
    }
}

/// 报告数据 —— `04_report/report.json`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// 这份数据是何时产出的(墙上时钟)。
    pub generated_at: DateTime<FixedOffset>,
    /// 这份报告是按什么口味做的。**旧数据可能没有这一栏**(M6 之前产出的报告),
    /// 所以是 `Option` —— 新生成的报告一律要给(工具侧是必填参数)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<ReportStyle>,
    /// 全局总结(markdown):主旨、结构、脉络 —— 整支视频的总述。
    pub summary: String,
    /// 局部知识点,按 `t` 升序。
    pub topics: Vec<ReportTopic>,
}

impl Report {
    /// 空报告(还没有数据时渲染器的兜底)。
    pub fn new() -> Self {
        Self {
            generated_at: crate::now(),
            style: None,
            summary: String::new(),
            topics: Vec::new(),
        }
    }

    /// 从磁盘读(`04_report/report.json`)。
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        crate::json::read(path)
    }

    /// 原子写回。
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        crate::json::write_atomic(path, self)
    }

    /// 按 `t` 升序排好 —— 时间轴要的顺序。
    pub fn sort_by_time(&mut self) {
        self.topics.sort_by(|a, b| {
            a.t.partial_cmp(&b.t)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}
