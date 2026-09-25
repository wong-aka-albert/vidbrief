//! 算法动作:E 组里跟截图有关的那几个(补图候选 / 补一张 / 改一张)。
//!
//! 设计见 `docs/DESIGN.md` §6.2 ④。判据的分界线很清楚:
//! **算法只负责列候选与搬运像素,要不要补、这张好不好看,是 agent 的判断。**

use anyhow::{Context, Result};
use vidbrief_project::{Shot, ShotId, ShotOrigin, Shots, ShotStatus, SupplementRequests};

use crate::project::OpenProject;

/// 补图候选的默认窗宽(秒):"附近有图"的判定半径。
pub const DEFAULT_SUPPLEMENT_WINDOW: f64 = 3.0;

/// 默认窗宽。写成函数而不是常量,是为了将来能按视频长度自适应。
pub fn supplement_window() -> f64 {
    DEFAULT_SUPPLEMENT_WINDOW
}

/// 扫一遍转录,列出**补图候选**,落盘并返回。
///
/// 算法只看两件事:段里有没有指代性表达、附近有没有图(`docs/DESIGN.md` §4.9)。
/// `status` 的初值恒为 `pending` —— 用户说"看这里"不代表真有个图要看。
///
/// # 重跑时保留已有结论
///
/// 这个工具会被反复调用(每轮校准一次)。如果每次都从零重写,
/// agent 上一轮"接受/拒绝"的判断就白做了 —— 而它多半是要花 token 才能得出的。
/// 所以按 `segment_id` 对齐:
///
/// - 仍然是候选、且之前有结论的 → **沿用**它的 `status` / `shot_id` / `note`(id 也不变)
/// - 已经不是候选、但**已经有结论**的 → **保留**(那是历史,不该因为"现在不缺图了"就消失)
/// - 已经不是候选、且从没被看过的 → 丢掉(情况变了,没必要再让人判断一遍)
pub fn find_supplement_candidates(
    opened: &OpenProject,
    window: f64,
) -> Result<SupplementRequests> {
    let (transcript, _, _) = crate::materials::effective_transcript(opened)?;
    let shots = crate::materials::shots(opened).unwrap_or_else(|_| Shots::new());

    let fresh = vidbrief_core::find_candidates(&transcript, &shots, window);
    let path = opened.layout().supplement_requests();
    let previous = SupplementRequests::load(&path).ok();

    let merged = match previous {
        None => fresh,
        Some(old) => merge(old, fresh),
    };
    merged
        .save(&path)
        .with_context(|| format!("写入失败:{}", path.display()))?;
    Ok(merged)
}

/// 把新算出来的候选与上一轮的结论合起来(规则见 [`find_supplement_candidates`])。
fn merge(old: SupplementRequests, fresh: SupplementRequests) -> SupplementRequests {
    let mut out = SupplementRequests::new(fresh.window);
    let mut next_id = old.requests.iter().map(|r| r.id).max().unwrap_or(0) + 1;

    for mut item in fresh.requests {
        // 上一轮对**同一段**的判断结论
        let previous = old
            .requests
            .iter()
            .find(|r| r.segment_id == item.segment_id)
            .cloned();
        match previous {
            Some(prev) => {
                // 沿用结论,但 t / text / trigger / gap 这些"算法算出来的"要用新的
                item.id = prev.id;
                item.status = prev.status;
                item.shot_id = prev.shot_id;
                item.note = prev.note;
            }
            None => {
                item.id = next_id;
                next_id += 1;
            }
        }
        out.requests.push(item);
    }

    // 已经被判断过、但这次不再是候选的:留着当历史
    for prev in old.requests {
        if prev.is_pending() {
            continue;
        }
        if !out.requests.iter().any(|r| r.segment_id == prev.segment_id) {
            out.requests.push(prev);
        }
    }

    out.requests.sort_by(|a, b| {
        a.t.partial_cmp(&b.t)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.segment_id.cmp(&b.segment_id))
    });
    out
}

/// 补一张图(`extract_shot` / `request_shot`)。
///
/// `t` 会被夹进视频有效范围 —— 想截 999 秒处的帧而视频只有 60 秒,是常态不是错误。
pub fn extract_shot(
    opened: &OpenProject,
    t: f64,
    reason: &str,
    origin: ShotOrigin,
) -> Result<Shot> {
    if !t.is_finite() {
        anyhow::bail!("截图时刻不合法:{t}");
    }
    let video = opened.require_video()?;
    let layout = opened.layout();
    let path = layout.shots_json();
    let mut shots = Shots::load(&path).context("读取 shots.json 失败")?;

    let t = clamp_time(t, opened.meta().duration);
    let id = shots.next_id()?;
    let file = Shot::default_rel_path(origin, id, t)?;
    let full = layout.rel(file.as_str());

    vidbrief_core::extract_frame(&video, t, &full, vidbrief_core::ShotQuality::default())
        .with_context(|| format!("在 t={t:.1}s 抽帧失败"))?;

    let shot = Shot {
        id,
        t,
        file,
        origin,
        region_id: None,
        status: ShotStatus::Keep,
        reason: Some(reason.to_string()),
        created_at: vidbrief_project::now(),
        check: None,
    };
    shots.shots.push(shot.clone());
    shots.sort_by_time();
    shots.save(&path).context("写入 shots.json 失败")?;
    Ok(shot)
}

/// 改一张现有的图(`update_shot`)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShotAction {
    /// 留用。
    Keep,
    /// 弃用。**不回收 id**(见 `ShotId` 的"永不复用"约定)。
    Drop,
    /// 换个时刻重截:重抽帧 → 换文件名 → 删旧图。
    Retime { t: f64 },
}

pub fn update_shot(opened: &OpenProject, id: ShotId, action: ShotAction) -> Result<Shot> {
    let video = opened.require_video()?;
    let layout = opened.layout();
    let path = layout.shots_json();
    let mut shots = Shots::load(&path).context("读取 shots.json 失败")?;

    let index = shots
        .shots
        .iter()
        .position(|s| s.id == id)
        .with_context(|| format!("项目里没有截图 {id}"))?;

    match action {
        ShotAction::Keep => shots.shots[index].status = ShotStatus::Keep,
        ShotAction::Drop => shots.shots[index].status = ShotStatus::Drop,
        ShotAction::Retime { t } => {
            if !t.is_finite() {
                anyhow::bail!("截图时刻不合法:{t}");
            }
            let t = clamp_time(t, opened.meta().duration);
            let origin = shots.shots[index].origin;
            let new_file = Shot::default_rel_path(origin, id, t)?;
            let new_full = layout.rel(new_file.as_str());

            vidbrief_core::extract_frame(&video, t, &new_full, vidbrief_core::ShotQuality::default())
                .with_context(|| format!("在 t={t:.1}s 重抽帧失败"))?;

            let old_full = layout.rel(shots.shots[index].file.as_str());
            shots.shots[index].t = t;
            shots.shots[index].file = new_file;

            // 旧图删掉。删不掉只是留个孤儿文件,不该让整个操作失败
            if old_full != new_full {
                let _ = std::fs::remove_file(&old_full);
            }
        }
    }

    let shot = shots.shots[index].clone();
    shots.sort_by_time();
    shots.save(&path).context("写入 shots.json 失败")?;
    Ok(shot)
}

/// 把时刻夹进 `[0, duration)` —— 视频最后一帧之后是抽不到的。
///
/// 与 `vidbrief-core::pipeline` 里那份是同一个判据;重复写一遍是因为那边是私有的,
/// 而这里要的语义完全一样(末尾留 0.05 秒余量)。
fn clamp_time(t: f64, duration: f64) -> f64 {
    let upper = (duration - 0.05).max(0.0);
    t.clamp(0.0, upper)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_times_inside_the_video() {
        assert_eq!(clamp_time(-5.0, 60.0), 0.0);
        assert_eq!(clamp_time(30.0, 60.0), 30.0);
        assert!(clamp_time(60.0, 60.0) < 60.0, "末尾要留一点余量");
        assert_eq!(clamp_time(1.0, 0.0), 0.0);
    }

    #[test]
    fn merge_keeps_decisions_and_history() {
        use vidbrief_project::{SupplementRequest, SupplementStatus, Trigger};

        let entry = |id: u32, segment_id: u32, t: f64, status: SupplementStatus| {
            SupplementRequest {
                id,
                t,
                segment_id,
                text: format!("第 {segment_id} 段"),
                trigger: Trigger::DeicticPhrase,
                nearest_shot: None,
                gap: None,
                status,
                shot_id: None,
                note: None,
            }
        };

        let mut old = SupplementRequests::new(3.0);
        old.requests = vec![
            entry(1, 10, 10.0, SupplementStatus::Accepted),
            entry(2, 20, 20.0, SupplementStatus::Pending),
            entry(3, 30, 30.0, SupplementStatus::Rejected),
        ];

        let mut fresh = SupplementRequests::new(3.0);
        fresh.requests = vec![
            entry(1, 10, 10.0, SupplementStatus::Pending), // 仍是候选,该沿用 accepted
            entry(1, 40, 40.0, SupplementStatus::Pending), // 新候选
        ];

        let merged = merge(old, fresh);
        let by_segment = |id: u32| merged.requests.iter().find(|r| r.segment_id == id).unwrap();

        assert_eq!(by_segment(10).status, SupplementStatus::Accepted);
        assert_eq!(by_segment(10).id, 1, "沿用的候选 id 不该变");
        assert_eq!(by_segment(40).status, SupplementStatus::Pending);
        assert_eq!(by_segment(40).id, 4, "新候选接着最大 id 往后排");
        assert_eq!(
            by_segment(30).status,
            SupplementStatus::Rejected,
            "已经判断过、现在不再是候选的,要留作历史"
        );
        assert!(
            merged.requests.iter().all(|r| r.segment_id != 20),
            "没人看过的陈旧候选取掉"
        );

        // 按 t 升序
        let times: Vec<f64> = merged.requests.iter().map(|r| r.t).collect();
        let mut sorted = times.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(times, sorted);
    }
}
