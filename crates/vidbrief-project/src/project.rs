//! `project.json` —— 项目的**身份**:它是谁、从哪来、叫什么。
//!
//! 格式见 `docs/schema/project.schema.json` 与 `docs/DESIGN.md` §4.2。
//!
//! # 这里面**没有**状态
//!
//! 进度、核验戳、看图结论、拍板记录曾经都在这个文件里,现在全挪进了
//! [`crate::state`] 的 `state.bin`(非明文,见 devlog [260925-30] 第 2 条)。
//!
//! 为什么挪:这个文件明文、谁都能改,而模型手里就拿着宿主给的文件工具。
//! 第五轮联调实测:它把 `report.done` 改成 `true`、`at` 编成了**未来时刻**
//! (当时 19:54,写的是 19:58)—— 而这几栏正是"这一步做完了没有"的判据。
//! **明文状态 = 门是纸糊的。**
//!
//! 现在这里只剩**推不出来、也不驱动门**的东西:身份、来处、术语表。
//! 想改进度、想记拍板,都得走工具(`vidbrief-ops`),它们写的是 `state.bin`。
//!
//! # 与进度有关的两个词,别再混
//!
//! - `progress`(现在在 `state.bin` 里)记"做过什么",给 `vidbrief status` 展示;
//! - `review`(也在 `state.bin` 里)记"用户看过并点头了",**是门**。
//!
//! M0 时这里写的是"进度不是审核门,'通过了没有'纯靠对话"。M4 联调推翻了它:
//! 两轮跑下来,**"靠对话"等于没有门** —— 一个一张图都没校准的项目照样能写总结、
//! 把步 3 标成完成。详见 devlog [260925-21]。

use std::collections::BTreeMap;
use std::fmt;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::ids::ProjectId;
use crate::relpath::RelPath;
use crate::source::SourceLink;

/// 当前 schema 版本。任一项目文件结构发生不兼容变更时 +1。
///
/// # v1 → v2:状态挪出明文([260925-30] 第 2 条)
///
/// v1 的 `project.json` 里带着 `rev` / `progress` / `review`,现在它们住
/// `state.bin`(见 [`crate::state`])。这是**不兼容**变更:老文件按新结构读,
/// 会因为多了三个字段被 `deny_unknown_fields` 拒掉 —— 而那正是我们想要的信号:
/// 让 [`Project::load_v1`] 接住它,由 `vidbrief upgrade` 显式迁一次,
/// 而不是在读取时悄悄猜。
pub const SCHEMA_VERSION: u32 = 2;

/// schema 版本标记。**反序列化即校验**:版本不符直接报错,不做静默兼容。
///
/// 放在类型系统里而不是靠调用方记得检查,是因为"读到不认识的项目文件却继续跑"
/// 比"读不出来"危险得多 —— 前者会静默写坏数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SchemaVersion;

impl Serialize for SchemaVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(SCHEMA_VERSION)
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let found = u32::deserialize(deserializer)?;
        if found != SCHEMA_VERSION {
            return Err(de::Error::custom(format!(
                "项目 schema_version 为 {found},本工具支持 {SCHEMA_VERSION};请用对应版本的工具打开,或手工迁移"
            )));
        }
        Ok(Self)
    }
}

impl fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{SCHEMA_VERSION}")
    }
}

/// 源视频的来处。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    /// 项目内位置(通常是 `source/video.<ext>`)。
    pub path: RelPath,
    /// 源视频的原始绝对路径。**纯信息性,不用于访问文件** —— 项目搬走后它多半已经失效,
    /// 保留只是为了让人知道这个项目是从哪来的。
    pub original: String,
    /// 源视频以什么方式进入项目。`copy` 才是自包含的;硬链接/软链接的项目依赖原文件还在。
    pub link: SourceLink,
}

/// 单步进度。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepProgress {
    pub done: bool,
    pub at: Option<DateTime<FixedOffset>>,
}

impl StepProgress {
    /// 标记为已完成并记录时刻。
    pub fn mark_done(&mut self) {
        self.done = true;
        self.at = Some(crate::now());
    }
}

/// 校准进度。**没有 `done`** —— 校准是迭代,不是一次性任务。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibrateProgress {
    /// 迭代轮数。每完成一轮"取材料 → 判断 → 写回"由 agent +1。
    pub rounds: u32,
    pub last_at: Option<DateTime<FixedOffset>>,
}

impl CalibrateProgress {
    /// 记一轮校准。
    pub fn bump(&mut self) {
        self.rounds += 1;
        self.last_at = Some(crate::now());
    }
}

/// 四步进度。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub preprocess: StepProgress,
    pub calibrate: CalibrateProgress,
    pub summarize: StepProgress,
    pub report: StepProgress,
}

/// 确认当时数下来的四项。
///
/// 四项全是服务端自己数得出来的东西 —— 不依赖 agent 自报,也不依赖"谁签的"
/// (那拿不到:MCP 握手里只有客户端名,没有模型名)。
///
/// ⚠️ **名字很像,口径要分清**(M4 第四轮实测就是在这儿混起来的):
/// - `needing_review` 是**门口径** —— "**模型自己说它定不了吗**"(`confidence: low`);
/// - `list_shots` 里那个同名计数是**视图口径** —— "这张图值不值得人看一眼"
///   (把握低 / 报了问题 / 图文不符)。它已改叫 `needs_look`,因为两个问题不一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCounts {
    /// 还没核验的截图张数。
    pub unchecked: usize,
    /// 模型核验过、但标了"定不了"、等用户拍板的截图张数。
    pub needing_review: usize,
    /// 还没处理过的补图候选条数。
    pub supplement_pending: usize,
    /// 标了"存疑但没改"、等人裁决的转录段数。
    pub uncertain: usize,
}

/// 确认当时的**材料内容**,逐条一个短摘要(见 [`crate::digest`])。
///
/// # 为什么要有它 —— 四个计数不够
///
/// [`ReviewCounts`] 只表达得了"**数量**变了",而校准里最常发生的那类改动是**改字**:
/// 把转录里听错的词改对。改字不动任何一项计数。
///
/// M4 第三轮真 agent 联调实测到了这个洞:用户签字时转录改了 4 段,之后 agent 又改到
/// 10 段,四项计数**一模一样**,门照开、总结照写 —— 用户签的那版和写总结用的那版
/// 根本不是同一份材料。计数是"拿量长度的尺子量温度"。
///
/// 所以这里存内容本身。任何一条对不上,这次确认就作废。
///
/// 计数不扔:摘要判"算不算数",计数负责把话**说给人听** ——
/// 「待裁决 7 张」人读得懂,`a1b2c3d4` 读不懂。
///
/// # 只收"被动过的",不收全部
///
/// 转录 322 段全存进来,`project.json` 会从 1KB 涨到 10KB,而那是个要给人看的文件。
/// 没收的那些段不会变 —— **原始转录永不就地改写**(`transcript.rs` 钉着这条),
/// 没被动过的段没有会变的字段。真被就地改了,那是"绕过工具把材料改坏",
/// 该由 `vidbrief status` 的内容体检去发现,不是这道门的活。
///
/// (`BTreeMap` 而不是 `HashMap`:键有序,序列化才是稳定的 ——
/// `project.json` 的示例文件是**逐字节**比对的。)
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDigests {
    /// 转录里**被动过的段**:改过的(读起来跟原文不一样),或标了存疑的。
    /// 键是段 id。
    ///
    /// 改完又改回原文的段**不在里面** —— 对读的人来说它跟没改过一样,
    /// 门不该为它拦一次。
    pub segments: BTreeMap<String, String>,
    /// 每张截图一个。键是截图 id,如 `s003`。
    pub shots: BTreeMap<String, String>,
    /// 每条补图候选一个。键是候选 id(数字,但和上面一样按字符串存,
    /// 免得 JSON 对象键的三套写法各写各的)。
    pub supplements: BTreeMap<String, String>,
}

/// 一次确认。
///
/// **没有"谁签的"这个字段** —— 拿不到。服务端能观察到的只有:什么时候、当时什么状态。
/// 名字也不用 `approved_by` 之类:一个填不出来的字段,迟早被填成假的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub approved_at: DateTime<FixedOffset>,
    /// 确认当时的四项计数。用来把话说清楚,不单独作判据(判据是 [`Self::digests`])。
    pub counts: ReviewCounts,
    /// 确认当时的材料内容。**这一条变了,这次确认就作废。**
    ///
    /// `None` = 这次确认签在"逐条记摘要"这版之前,当时只记了计数 —— 那时
    /// **没法判断材料后来有没有被改过**。门认得出这个区别,会说"查不了,再确认一次",
    /// 而不是假装材料变了。
    ///
    /// 为什么要 `Option` 而不是"空表就当没有":**空表是有意义的** ——
    /// 一个刚建好、什么都还没跑的项目,材料本来就是空的。
    /// 两者分不开的话,那种项目一签字就再也过不去门了(测试当场抓到了这一条)。
    #[serde(default)]
    pub digests: Option<MaterialDigests>,
    /// **带着未处理项通过**时的说明(正常通过是 `None`)。
    /// 事后要能回答"当初为什么放行"。
    pub note: Option<String>,
}

/// 各步的确认。`None` = 还没被用户确认过。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    /// 步 2(校准)。**这是道门**:没确认过、或者确认之后东西又变了,
    /// 就不许进步 3。
    pub calibrate: Option<Approval>,
}

/// `project.json`。
///
/// 字段顺序即为写盘顺序,不要随意调整(测试里与示例文件逐字节比对)。
///
/// **这里只装身份** —— 状态在 [`crate::state::ProjectState`] 里(见模块文档)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema_version: SchemaVersion,
    pub id: ProjectId,
    /// 人类可读名称,随便写(可含中文)。
    pub name: String,
    pub created_at: DateTime<FixedOffset>,
    pub source: SourceRef,
    /// 术语表;**默认 `None`(关闭)**。需要时由用户提供一份正确写法清单,
    /// 供 agent 在转录纠错时对照(优先级最低,见 §6.2 ②)。
    pub glossary: Option<Vec<String>>,
}

impl Project {
    /// 新建项目(尚未落盘)。
    pub fn new(
        id: ProjectId,
        name: impl Into<String>,
        source_path: RelPath,
        original_path: impl Into<String>,
        link: SourceLink,
    ) -> Self {
        Self {
            schema_version: SchemaVersion,
            id,
            name: name.into(),
            created_at: crate::now(),
            source: SourceRef {
                path: source_path,
                original: original_path.into(),
                link,
            },
            glossary: None,
        }
    }

    /// 写盘。
    ///
    /// **不再有 `rev` 这件事了** —— 修订号跟着状态走(`state.bin`),
    /// 因为"改了几次"是状态,不是身份。
    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }

    /// 从磁盘读。
    pub fn load(path: &std::path::Path) -> Result<Self> {
        crate::json::read(path)
    }

    /// 只看 `schema_version` 这一个字段,用来把话说清楚(不解析整份文件)。
    ///
    /// 用途:老项目(版本对不上)与新项目要给的提示**完全不一样** ——
    /// "换个版本的工具打开"和"这份文件坏了"是两条去路,不能含糊成一句。
    pub fn peek_version(path: &std::path::Path) -> Result<u32> {
        #[derive(Deserialize)]
        struct VersionOnly {
            schema_version: u32,
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取失败:{}", path.display()))?;
        let v: VersionOnly = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("解析 {} 失败:{e}", path.display()))?;
        Ok(v.schema_version)
    }

    /// 读**旧格式(v1)**的 `project.json` —— 只给 `vidbrief upgrade` 用。
    ///
    /// 返回(v1 里那份"身份 + 状态"):身份部分照现在读,`rev`/`progress`/`review`
    /// 原样交出去,由升级那边收进 `state.bin`。
    pub fn load_v1(path: &std::path::Path) -> Result<(Self, LegacyStateV1)> {
        let legacy: ProjectV1 = crate::json::read(path)
            .with_context(|| format!("按旧格式(v1)读取 {} 失败", path.display()))?;
        Ok((
            Self {
                schema_version: SchemaVersion,
                id: legacy.id,
                name: legacy.name,
                created_at: legacy.created_at,
                source: legacy.source,
                glossary: legacy.glossary,
            },
            LegacyStateV1 {
                rev: legacy.rev,
                progress: legacy.progress,
                review: legacy.review,
            },
        ))
    }
}

/// v1 的 `project.json` —— **只用来读,不会再写出去**。
///
/// 留着它的理由和 [`crate::transcript`] 里那份老格式读取器一样:
/// 已经写出来的项目不能因为一次重构就打不开。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectV1 {
    #[allow(dead_code)]
    schema_version: u32,
    rev: u64,
    id: ProjectId,
    name: String,
    created_at: DateTime<FixedOffset>,
    source: SourceRef,
    glossary: Option<Vec<String>>,
    progress: Progress,
    /// 老文件里可能没有这一段(缺省 = 谁都没确认过),照 v1 当年的默认来。
    #[serde(default)]
    review: Review,
}

/// v1 里**那部分现在归状态的字段**。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyStateV1 {
    pub rev: u64,
    pub progress: Progress,
    pub review: Review,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_enforced_on_read() {
        let text = r#"{"schema_version": 3, "id":"a", "name":"a",
            "created_at":"2026-09-24T21:00:00+08:00",
            "source":{"path":"source/video.mp4","original":"x","link":"copy"},
            "glossary":null}"#;
        let err = serde_json::from_str::<Project>(text).unwrap_err().to_string();
        assert!(err.contains("schema_version"), "{err}");
    }

    /// v1 的 `project.json`(带 `rev` / `progress` / `review`)**读不进新结构** ——
    /// 这正是升级要接住的信号:让它显式迁一次,而不是读取时悄悄猜。
    #[test]
    fn a_v1_file_does_not_sneak_into_the_new_shape() {
        let text = r#"{"schema_version": 1, "rev":5, "id":"a", "name":"a",
            "created_at":"2026-09-24T21:00:00+08:00",
            "source":{"path":"source/video.mp4","original":"x","link":"copy"},
            "glossary":null,
            "progress":{"preprocess":{"done":true,"at":null},
                        "calibrate":{"rounds":1,"last_at":null},
                        "summarize":{"done":false,"at":null},
                        "report":{"done":false,"at":null}}}"#;
        let err = serde_json::from_str::<Project>(text).unwrap_err().to_string();
        assert!(err.contains("unknown field") || err.contains("schema_version"), "{err}");

        // 而升级那条路认得它,而且把状态那几栏**原样交出来**
        let dir = std::env::temp_dir().join(format!("vidbrief-proj-v1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");
        std::fs::write(&path, text).unwrap();
        let (project, legacy) = Project::load_v1(&path).unwrap();
        assert_eq!(legacy.rev, 5);
        assert!(legacy.progress.preprocess.done);
        assert_eq!(legacy.progress.calibrate.rounds, 1);
        assert_eq!(project.id.as_ref(), "a");
        assert_eq!(project.schema_version, SchemaVersion, "读完就是当前版本");
        assert_eq!(Project::peek_version(&path).unwrap(), 1, "原文里的版本仍是 1");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn glossary_round_trips_as_array() {
        let mut p = Project::new(
            ProjectId::new("demo").unwrap(),
            "样例",
            RelPath::new("source/video.mp4").unwrap(),
            "D:/videos/demo.mp4",
            SourceLink::Copy,
        );
        p.glossary = Some(vec!["样本".into(), "PYTHON".into()]);
        let text = crate::json::to_pretty_string(&p).unwrap();
        let back: Project = serde_json::from_str(&text).unwrap();
        assert_eq!(back.glossary.as_ref().unwrap().len(), 2);
        assert_eq!(back, p);
    }

    #[test]
    fn new_project_starts_clean() {
        let p = Project::new(
            ProjectId::new("demo").unwrap(),
            "样例",
            RelPath::new("source/video.mp4").unwrap(),
            "D:/videos/demo.mp4",
            SourceLink::Copy,
        );
        assert_eq!(p.schema_version, SchemaVersion);
        assert!(p.glossary.is_none());
        // 状态那几栏已经不在这个文件里了 —— 想改进度得去 state.bin
        let text = crate::json::to_pretty_string(&p).unwrap();
        for gone in ["\"rev\"", "\"progress\"", "\"review\""] {
            assert!(!text.contains(gone), "project.json 里不该再有 {gone}:{text}");
        }
    }

    #[test]
    fn save_round_trips_without_touching_any_counter() {
        let dir = std::env::temp_dir().join(format!("vidbrief-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");

        let p = Project::new(
            ProjectId::new("demo").unwrap(),
            "样例",
            RelPath::new("source/video.mp4").unwrap(),
            "D:/videos/demo.mp4",
            SourceLink::Copy,
        );
        p.save(&path).unwrap();
        p.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), p);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
