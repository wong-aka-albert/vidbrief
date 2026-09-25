//! `project.json` —— 项目元数据、各步进度标记、用户确认。
//!
//! 格式见 `docs/schema/project.schema.json` 与 `docs/DESIGN.md` §4.2。
//!
//! **两件事要分清**:
//! - `progress` 记"做过什么",给 `vidbrief status` 和 `list_projects` 展示,**不是门**;
//! - `review` 记"用户看过并点头了",**是门** —— 校准没被确认过、或者确认之后东西又变了,
//!   就不许写总结(见 `vidbrief-ops` 的 `review` 模块)。
//!
//! M0 时这里写的是"进度不是审核门,'通过了没有'纯靠对话"。M4 联调推翻了它:
//! 两轮跑下来,**"靠对话"等于没有门** —— 一个一张图都没校准的项目照样能写总结、
//! 把步 3 标成完成。详见 devlog [260925-21]。

use std::fmt;

use anyhow::{Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use crate::ids::ProjectId;
use crate::relpath::RelPath;
use crate::source::SourceLink;

/// 当前 schema 版本。任一项目文件结构发生不兼容变更时 +1。
pub const SCHEMA_VERSION: u32 = 1;

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

/// 确认当时数下来的四项。**一变就作废**。
///
/// 四项全是服务端自己数得出来的东西 —— 不依赖 agent 自报,也不依赖"谁签的"
/// (那拿不到:MCP 握手里只有客户端名,没有模型名)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewCounts {
    /// 还没校准的截图张数。
    pub unchecked: usize,
    /// 需要人裁决的截图张数(把握低 / 报了问题 / 图文不符)。
    pub needing_review: usize,
    /// 还没处理过的补图候选条数。
    pub supplement_pending: usize,
    /// 标了"存疑但没改"、等人裁决的转录段数。
    pub uncertain: usize,
}

/// 一次确认。
///
/// **没有"谁签的"这个字段** —— 拿不到。服务端能观察到的只有:什么时候、当时什么状态。
/// 名字也不用 `approved_by` 之类:一个填不出来的字段,迟早被填成假的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub approved_at: DateTime<FixedOffset>,
    /// 确认当时的材料状态。之后任何一项变了,这次确认就作废。
    pub counts: ReviewCounts,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema_version: SchemaVersion,
    /// 乐观并发修订号(§4.7)。工具每次成功写入本文件时 +1;
    /// 调用方写回时必须带上读到的值,不匹配说明别人改过了。
    pub rev: u64,
    pub id: ProjectId,
    /// 人类可读名称,随便写(可含中文)。
    pub name: String,
    pub created_at: DateTime<FixedOffset>,
    pub source: SourceRef,
    /// 术语表;**默认 `None`(关闭)**。需要时由用户提供一份正确写法清单,
    /// 供 agent 在转录纠错时对照(优先级最低,见 §6.2 ②)。
    pub glossary: Option<Vec<String>>,
    pub progress: Progress,
    /// 用户对各步的确认 —— **审核门**。
    ///
    /// `progress` 记"做过什么",这里记"用户看过并点头了"。两者是两回事:
    /// 一个项目可以四步都 `done`,却一次都没被人确认过。
    ///
    /// 缺省(老项目文件里没有这一段)视作"谁都没确认过"。
    #[serde(default)]
    pub review: Review,
}

impl Project {
    /// 新建项目(尚未落盘)。`rev` 从 0 起。
    pub fn new(
        id: ProjectId,
        name: impl Into<String>,
        source_path: RelPath,
        original_path: impl Into<String>,
        link: SourceLink,
    ) -> Self {
        Self {
            schema_version: SchemaVersion,
            rev: 0,
            id,
            name: name.into(),
            created_at: crate::now(),
            source: SourceRef {
                path: source_path,
                original: original_path.into(),
                link,
            },
            glossary: None,
            progress: Progress::default(),
            review: Review::default(),
        }
    }

    /// 写盘并把 `rev` +1。
    ///
    /// 全项目**只此一处**修改 `rev`,因此修订号永远是单调的写入计数。
    pub fn save(&mut self, path: &std::path::Path) -> Result<()> {
        self.rev += 1;
        crate::json::write_atomic(path, self)
    }

    /// 从磁盘读。
    pub fn load(path: &std::path::Path) -> Result<Self> {
        crate::json::read(path)
    }

    /// 供乐观并发使用:调用方读到的 `rev` 是否仍是当前值。
    pub fn check_rev(&self, seen: u64) -> Result<()> {
        if self.rev != seen {
            bail!(
                "项目 {} 已被其它会话修改(期望 rev={seen},实际 rev={})",
                self.id,
                self.rev
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_enforced_on_read() {
        let text = r#"{"schema_version": 2, "rev":0, "id":"a", "name":"a",
            "created_at":"2026-09-24T21:00:00+08:00",
            "source":{"path":"source/video.mp4","original":"x","link":"copy"},
            "glossary":null,
            "progress":{"preprocess":{"done":false,"at":null},
                        "calibrate":{"rounds":0,"last_at":null},
                        "summarize":{"done":false,"at":null},
                        "report":{"done":false,"at":null}}}"#;
        let err = serde_json::from_str::<Project>(text).unwrap_err().to_string();
        assert!(err.contains("schema_version"), "{err}");
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
        assert_eq!(p.rev, 0);
        assert_eq!(p.schema_version, SchemaVersion);
        assert!(!p.progress.preprocess.done);
        assert_eq!(p.progress.calibrate.rounds, 0);
        assert!(p.glossary.is_none());
    }

    #[test]
    fn save_bumps_rev_and_check_rev_catches_stale() {
        let dir = std::env::temp_dir().join(format!("vidbrief-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("project.json");

        let mut p = Project::new(
            ProjectId::new("demo").unwrap(),
            "样例",
            RelPath::new("source/video.mp4").unwrap(),
            "D:/videos/demo.mp4",
            SourceLink::Copy,
        );
        p.save(&path).unwrap();
        assert_eq!(p.rev, 1);
        p.save(&path).unwrap();
        assert_eq!(p.rev, 2);

        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.rev, 2);
        assert!(loaded.check_rev(2).is_ok());
        assert!(loaded.check_rev(1).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn progress_helpers_set_time() {
        let mut p = Progress::default();
        p.preprocess.mark_done();
        assert!(p.preprocess.done);
        assert!(p.preprocess.at.is_some());
        p.calibrate.bump();
        p.calibrate.bump();
        assert_eq!(p.calibrate.rounds, 2);
        assert!(p.calibrate.last_at.is_some());
    }
}
