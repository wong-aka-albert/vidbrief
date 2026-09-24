//! `01_preprocess/shots.json` —— 关键截图清单 + agent 的校准结论。
//!
//! 格式见 `docs/schema/shots.schema.json`,设计见 `docs/DESIGN.md` §4.6 / §6.1。
//!
//! **本文件跨越两个步骤**:步 1 由工具生成(`origin` ∈ {`dense_region`, `isolated_peak`}),
//! 步 2/3 由 agent 增删改(`record_shot_check` / `extract_shot` / `update_shot`)。
//! 因此它同时是"算法产出"和"agent 结论"的载体 —— 这也是把结论直接放在 `check` 字段里、
//! 而不再另立 `shot_checks.json` 的原因:一个事实只有一个落点。

use anyhow::{Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::ids::ShotId;
use crate::layout;
use crate::relpath::RelPath;

/// agent 对自己结论的把握程度。**不确定就用 `Low`,不要猜。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// 这张图从哪来。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotOrigin {
    /// 步 1:变化密集区的代表帧。
    DenseRegion,
    /// 步 1:孤立硬切换的峰值帧。
    IsolatedPeak,
    /// 步 2/3:agent 判断后主动补的图(`extract_shot` / `request_shot`)。
    AgentRequest,
    /// 人工补的图。
    Manual,
}

impl ShotOrigin {
    /// 这类截图默认落在哪个目录(项目根相对)。
    ///
    /// 步 3 的二次补图例外:它按 `03_summary/shots_extra2/` 存放,
    /// 由调用方显式指定路径(`docs/DESIGN.md` §4.1)。
    pub fn default_rel_dir(self) -> &'static str {
        match self {
            Self::DenseRegion | Self::IsolatedPeak => layout::SHOTS_REL_DIR,
            Self::AgentRequest | Self::Manual => layout::SHOTS_EXTRA_REL_DIR,
        }
    }

    /// 是否由步 1 的纯算法产生。
    pub fn is_algorithmic(self) -> bool {
        matches!(self, Self::DenseRegion | Self::IsolatedPeak)
    }
}

/// 是否保留。**`drop` 不回收 id**,只改状态 —— 见 [`ShotId`] 的"永不复用"约定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotStatus {
    Keep,
    Drop,
}

/// agent 的看图结论,由 `record_shot_check` 写入。未校准时整个字段为 `None`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShotCheck {
    /// 画面内容与同期转录是否相符。
    pub r#match: bool,
    /// 画面与语音在时间上是否对得上。
    pub audio_visual_sync: bool,
    /// 从画面读到的内容(对象 / 规格 / 状态 / 数值)。
    /// **画面无法确认时如实写「画面无法确认」**,不要顺着转录编。
    pub type_read: String,
    /// 该时刻前后 ±6s 的转录原文,留档作为判断依据。
    pub neighbor_text: Option<String>,
    /// 发现的问题,每条一句;无问题为空数组。
    pub issues: Vec<String>,
    pub confidence: Confidence,
    pub checked_at: DateTime<FixedOffset>,
}

impl ShotCheck {
    /// 是否需要人来裁决:把握低,或报了问题。
    ///
    /// 这是"审查节点"的入口条件 —— 用它筛出该拿给用户看的那些图。
    pub fn needs_review(&self) -> bool {
        self.confidence == Confidence::Low || !self.issues.is_empty() || !self.r#match
    }
}

/// 一张关键截图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shot {
    pub id: ShotId,
    /// 截取时刻(秒),取该时刻的帧。
    pub t: f64,
    /// 图片文件的项目根相对路径。
    pub file: RelPath,
    pub origin: ShotOrigin,
    /// `origin` 为密集区/孤立峰时指向 `dense_regions.json`,否则为 `None`。
    pub region_id: Option<u32>,
    pub status: ShotStatus,
    /// 补图理由(`origin` 为 `agent_request` / `manual` 时应有值)。
    pub reason: Option<String>,
    pub created_at: DateTime<FixedOffset>,
    pub check: Option<ShotCheck>,
}

impl Shot {
    /// 由 id 与时刻合成文件名,如 `s007_0479.0.jpg`。
    ///
    /// `{:06.1}` 的行为:整数部分补零到至少 4 位(`86.0` → `0086.0`),
    /// 超过 9999 秒时自然变宽(`7200.0`),不截断、不重排 —— 所以长视频也能用,
    /// 只是文件名长度不再统一。
    pub fn file_name(id: ShotId, t: f64) -> Result<String> {
        if !t.is_finite() || t < 0.0 {
            bail!("截图时刻不合法:{t}");
        }
        Ok(format!("{id}_{t:06.1}.jpg"))
    }

    /// 按"来源对应的默认目录"算出完整相对路径。
    pub fn default_rel_path(origin: ShotOrigin, id: ShotId, t: f64) -> Result<RelPath> {
        RelPath::new(origin.default_rel_dir())?.join(&Self::file_name(id, t)?)
    }

    /// 是否还等着 agent 看图。
    pub fn is_unchecked(&self) -> bool {
        self.check.is_none()
    }

    /// 是否该出现在最终产物里(留用 且 未被判定图文不符)。
    pub fn is_usable(&self) -> bool {
        self.status == ShotStatus::Keep
    }
}

/// 截图清单。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shots {
    /// 首次生成时间;后续增删改**不更新**此值。
    pub generated_at: DateTime<FixedOffset>,
    /// 按 `t` 升序。被判定 `drop` 的条目仍留在数组里。
    pub shots: Vec<Shot>,
}

impl Shots {
    pub fn new() -> Self {
        Self {
            generated_at: crate::now(),
            shots: Vec::new(),
        }
    }

    /// 下一张图的 id = 现有最大 id + 1。空清单从 [`ShotId::FIRST`] 开始。
    pub fn next_id(&self) -> Result<ShotId> {
        match self.shots.iter().map(|s| s.id.get()).max() {
            Some(max) => ShotId::new(max + 1),
            None => Ok(ShotId::FIRST),
        }
    }

    pub fn get(&self, id: ShotId) -> Option<&Shot> {
        self.shots.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: ShotId) -> Option<&mut Shot> {
        self.shots.iter_mut().find(|s| s.id == id)
    }

    /// 按时间升序排序(插入补图后调用)。
    pub fn sort_by_time(&mut self) {
        self.shots
            .sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// 还没校准的图 —— 步 2 第一轮要处理的就是这些。
    pub fn unchecked(&self) -> impl Iterator<Item = &Shot> {
        self.shots.iter().filter(|s| s.is_unchecked())
    }

    /// 需要人来裁决的图(把握低 / 报了问题 / 图文不符)。
    pub fn needing_review(&self) -> impl Iterator<Item = &Shot> {
        self.shots
            .iter()
            .filter(|s| s.status == ShotStatus::Keep)
            .filter(|s| s.check.as_ref().is_some_and(ShotCheck::needs_review))
    }

    /// 留用且可用的图。
    pub fn usable(&self) -> impl Iterator<Item = &Shot> {
        self.shots.iter().filter(|s| s.is_usable())
    }

    pub fn load(path: &std::path::Path) -> Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &std::path::Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }
}

impl Default for Shots {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(id: u32, t: f64, origin: ShotOrigin) -> Shot {
        let sid = ShotId::new(id).unwrap();
        Shot {
            id: sid,
            t,
            file: Shot::default_rel_path(origin, sid, t).unwrap(),
            origin,
            region_id: None,
            status: ShotStatus::Keep,
            reason: None,
            created_at: crate::now(),
            check: None,
        }
    }

    #[test]
    fn file_name_matches_design_examples() {
        // docs/DESIGN.md §4.1 与 §5.2 里出现的两个例子
        assert_eq!(
            Shot::file_name(ShotId::new(1).unwrap(), 86.0).unwrap(),
            "s001_0086.0.jpg"
        );
        assert_eq!(
            Shot::file_name(ShotId::new(7).unwrap(), 479.0).unwrap(),
            "s007_0479.0.jpg"
        );
        assert_eq!(
            Shot::file_name(ShotId::new(4).unwrap(), 18.8).unwrap(),
            "s004_0018.8.jpg"
        );
        // 超过 9999 秒自然变宽,不截断
        assert_eq!(
            Shot::file_name(ShotId::new(9).unwrap(), 7200.0).unwrap(),
            "s009_7200.0.jpg"
        );
        // 超过 999 号也自然变宽(shot id 最少 3 位)
        assert_eq!(
            Shot::file_name(ShotId::new(1000).unwrap(), 12.0).unwrap(),
            "s1000_0012.0.jpg"
        );
    }

    #[test]
    fn file_name_rejects_bad_time() {
        let id = ShotId::FIRST;
        assert!(Shot::file_name(id, -1.0).is_err());
        assert!(Shot::file_name(id, f64::NAN).is_err());
        assert!(Shot::file_name(id, f64::INFINITY).is_err());
    }

    #[test]
    fn default_rel_path_follows_origin() {
        let p = Shot::default_rel_path(ShotOrigin::DenseRegion, ShotId::new(1).unwrap(), 9.0).unwrap();
        assert_eq!(p.as_str(), "01_preprocess/shots/s001_0009.0.jpg");

        let p = Shot::default_rel_path(ShotOrigin::AgentRequest, ShotId::new(4).unwrap(), 18.8).unwrap();
        assert_eq!(p.as_str(), "02_calibrate/shots_extra/s004_0018.8.jpg");
    }

    #[test]
    fn origin_and_status_wire_names() {
        assert_eq!(
            serde_json::to_string(&ShotOrigin::DenseRegion).unwrap(),
            "\"dense_region\""
        );
        assert_eq!(
            serde_json::to_string(&ShotOrigin::IsolatedPeak).unwrap(),
            "\"isolated_peak\""
        );
        assert_eq!(
            serde_json::to_string(&ShotOrigin::AgentRequest).unwrap(),
            "\"agent_request\""
        );
        assert_eq!(serde_json::to_string(&ShotStatus::Keep).unwrap(), "\"keep\"");
        assert_eq!(serde_json::to_string(&Confidence::Low).unwrap(), "\"low\"");
    }

    #[test]
    fn check_uses_match_keyword_name() {
        let check = ShotCheck {
            r#match: true,
            audio_visual_sync: false,
            type_read: "Python 列表 结构图".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: Confidence::Low,
            checked_at: crate::now(),
        };
        let text = serde_json::to_string(&check).unwrap();
        assert!(text.contains("\"match\":true"), "{text}");
        assert!(text.contains("\"audio_visual_sync\":false"), "{text}");
        // 反序列化也认这个名字
        let back: ShotCheck = serde_json::from_str(&text).unwrap();
        assert_eq!(back, check);
    }

    #[test]
    fn needs_review_covers_three_cases() {
        let base = ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "x".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: Confidence::High,
            checked_at: crate::now(),
        };
        assert!(!base.needs_review());
        assert!(ShotCheck { confidence: Confidence::Low, ..base.clone() }.needs_review());
        assert!(ShotCheck { issues: vec!["有出入".into()], ..base.clone() }.needs_review());
        assert!(ShotCheck { r#match: false, ..base }.needs_review());
    }

    #[test]
    fn id_allocation_is_monotonic_and_never_reuses() {
        let mut shots = Shots::new();
        assert_eq!(shots.next_id().unwrap(), ShotId::FIRST);

        shots.shots.push(shot(1, 9.0, ShotOrigin::DenseRegion));
        shots.shots.push(shot(2, 15.0, ShotOrigin::DenseRegion));
        assert_eq!(shots.next_id().unwrap(), ShotId::new(3).unwrap());

        // 删掉最后一张也不回收号
        shots.shots[1].status = ShotStatus::Drop;
        assert_eq!(shots.next_id().unwrap(), ShotId::new(3).unwrap());
    }

    #[test]
    fn queries() {
        let mut shots = Shots::new();
        shots.shots.push(shot(2, 15.0, ShotOrigin::DenseRegion));
        shots.shots.push(shot(1, 9.0, ShotOrigin::DenseRegion));
        assert_eq!(shots.unchecked().count(), 2);
        assert_eq!(shots.usable().count(), 2);

        shots.get_mut(ShotId::new(1).unwrap()).unwrap().check = Some(ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "x".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: Confidence::Low,
            checked_at: crate::now(),
        });
        assert_eq!(shots.unchecked().count(), 1);
        assert_eq!(shots.needing_review().count(), 1);

        shots.sort_by_time();
        assert_eq!(shots.shots[0].id, ShotId::new(1).unwrap());
        assert!(shots.get(ShotId::new(2).unwrap()).is_some());
    }

    #[test]
    fn round_trip() {
        let mut shots = Shots::new();
        shots.shots.push(shot(1, 9.0, ShotOrigin::IsolatedPeak));
        let text = crate::json::to_pretty_string(&shots).unwrap();
        let back: Shots = serde_json::from_str(&text).unwrap();
        assert_eq!(back, shots);
    }
}
