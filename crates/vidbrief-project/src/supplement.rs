//! `02_calibrate/supplement_requests.json` —— 补图候选。
//!
//! 格式见 `docs/schema/supplement_requests.schema.json`,设计见 `docs/DESIGN.md` §6.2 子任务④。
//!
//! 算法只负责**列候选**(纯文本判据 + 时间距离),**要不要补由 agent 判断**:
//! 用户说"看这里"不代表真有个图要看,agent 收到候选后仍要自行决定。

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::ids::ShotId;

/// 为什么被列为候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// 该段含指代性表达(看这里 / 这边显示 / 大家看屏幕 / 这个图 / 这一段 / 形态…)。
    DeicticPhrase,
    /// 该处附近(±`window` 秒)没有任何截图。
    NoShotNearby,
    /// 两条都命中。
    Both,
}

/// agent 的判断结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupplementStatus {
    /// 还没看。**默认值** —— 算法只负责列出来。
    Pending,
    /// 确实要补,已调 `extract_shot`。
    Accepted,
    /// 不需要。
    Rejected,
}

/// 一条补图候选。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupplementRequest {
    /// 项目内唯一,从 1 起。
    pub id: u32,
    /// 建议补图的时刻(通常取该转录段的中点)。
    pub t: f64,
    /// 触发本候选的转录段 id。
    pub segment_id: u32,
    /// 该段转录原文 —— 让人一眼看出为什么它被列出来,不用回查别处。
    pub text: String,
    pub trigger: Trigger,
    /// 现有截图里离 `t` 最近的一张;项目里还没有截图时为 `None`。
    pub nearest_shot: Option<ShotId>,
    /// `t` 与 `nearest_shot` 的距离(秒)。`nearest_shot` 为 `None` 时也为 `None`。
    pub gap: Option<f64>,
    pub status: SupplementStatus,
    /// `status` 为 `accepted` 时补出的截图 id。
    pub shot_id: Option<ShotId>,
    /// agent 接受 / 拒绝的理由。
    pub note: Option<String>,
}

impl SupplementRequest {
    /// 还是 `pending`,没被 agent 看过。
    pub fn is_pending(&self) -> bool {
        self.status == SupplementStatus::Pending
    }
}

/// 补图候选清单。允许为空。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupplementRequests {
    pub generated_at: DateTime<FixedOffset>,
    /// "附近有图"的判定半径(秒),默认 3.0。
    pub window: f64,
    /// 按 `t` 升序。
    pub requests: Vec<SupplementRequest>,
}

impl SupplementRequests {
    pub fn new(window: f64) -> Self {
        Self {
            generated_at: crate::now(),
            window,
            requests: Vec::new(),
        }
    }

    /// 还没被处理的候选。
    pub fn pending(&self) -> impl Iterator<Item = &SupplementRequest> {
        self.requests.iter().filter(|r| r.is_pending())
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut SupplementRequest> {
        self.requests.iter_mut().find(|r| r.id == id)
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

    #[test]
    fn wire_names() {
        assert_eq!(
            serde_json::to_string(&Trigger::DeicticPhrase).unwrap(),
            "\"deictic_phrase\""
        );
        assert_eq!(
            serde_json::to_string(&Trigger::NoShotNearby).unwrap(),
            "\"no_shot_nearby\""
        );
        assert_eq!(
            serde_json::to_string(&SupplementStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&SupplementStatus::Accepted).unwrap(),
            "\"accepted\""
        );
    }

    #[test]
    fn pending_filter_and_update() {
        let mut reqs = SupplementRequests::new(3.0);
        reqs.requests.push(SupplementRequest {
            id: 1,
            t: 18.8,
            segment_id: 7,
            text: "画面停住了大家可以把结论抄下来".into(),
            trigger: Trigger::NoShotNearby,
            nearest_shot: Some(ShotId::new(3).unwrap()),
            gap: Some(3.2),
            status: SupplementStatus::Pending,
            shot_id: None,
            note: None,
        });
        assert_eq!(reqs.pending().count(), 1);

        {
            let r = reqs.get_mut(1).unwrap();
            r.status = SupplementStatus::Accepted;
            r.shot_id = Some(ShotId::new(4).unwrap());
            r.note = Some("补一张便于读数".into());
        }
        assert_eq!(reqs.pending().count(), 0);

        let text = crate::json::to_pretty_string(&reqs).unwrap();
        let back: SupplementRequests = serde_json::from_str(&text).unwrap();
        assert_eq!(back, reqs);
    }
}
