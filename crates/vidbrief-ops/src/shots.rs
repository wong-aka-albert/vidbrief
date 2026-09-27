//! 算法动作:E 组里跟截图有关的那几个(补图候选 / 补一张 / 改一张)。
//!
//! 设计见 `docs/DESIGN.md` §6.2 ④。判据的分界线很清楚:
//! **算法只负责列候选与搬运像素,要不要补、这张好不好看,是 agent 的判断。**

use anyhow::{Context, Result};
use vidbrief_project::{
    Shot, ShotId, ShotOrigin, ShotStatus, SupplementRequests, SupplementStatus,
};

use crate::project::OpenProject;

/// 补图候选的默认窗宽(秒):"附近有图"的判定半径。
pub const DEFAULT_SUPPLEMENT_WINDOW: f64 = 3.0;

/// 把 `s007` / `7` 都认成截图 id。
///
/// 契约层只认 `s007`(那是文件里、文件名里的写法)。工具层放宽一档:
/// agent 手上常常只有一个从 `list_regions` 或转录里看到的数字,为了一个前缀
/// 把它打回去重来,纯属浪费一轮往返。
///
/// 放在**动作层**而不是各入口各写一份:CLI、MCP、报告数据的校验都要它,
/// 三份实现迟早对同一个输入给出三种说法。
pub fn parse_shot_id(raw: &str) -> Result<ShotId> {
    let text = raw.trim();
    let normalized = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        format!("s{text}")
    } else {
        text.to_string()
    };
    ShotId::parse(&normalized).map_err(Into::into)
}

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
    opened: &mut OpenProject,
    window: f64,
    rebuild: bool,
) -> Result<SupplementRequests> {
    let (transcript, _, _) = crate::materials::effective_transcript(opened)?;
    // 读不动就报错,别拿空清单顶上 —— 拿空清单去算,会把"看不清有哪些图"当成"一张图都没有",
    // 于是把已经补过的位置又报成缺图。
    let shots = crate::materials::shots_optional(opened)?.unwrap_or_default();

    let fresh = vidbrief_core::find_candidates(&transcript, &shots, window);
    // `rebuild` 是给"上一份读不动"留的出路:**显式**说重来,才允许不看旧的那份。
    // 没明说就读不动 → 报错(见 `materials::supplements`),不许悄悄覆盖。
    //
    // **读得动、而且上面还有结论时,`rebuild` 一律拦下**(M5 真 agent 实测):
    // 一次 `rebuild=true` 把 298 条候选的历史处置清成了 pending —— 用户一轮轮攒的
    // "补过哪些、拒过哪些、为什么"一句话就没了,而门随后会按"还没表态"再拦一次,
    // 把 agent 拖回去重做几百条。它不是刷新按钮;要刷新,不带 `rebuild` 就够
    // (会按段号把旧结论合回来)。**读不动**才是它该出现的场合 —— 那时除了重算没有别的路。
    let previous = match crate::materials::supplements(opened) {
        Ok(list) => {
            if rebuild {
                let decided = list
                    .as_ref()
                    .map_or(0, |l| l.requests.iter().filter(|r| !r.is_pending()).count());
                if decided > 0 {
                    anyhow::bail!(
                        "`rebuild` 会把已有的 {decided} 条候选结论**全部丢掉**\
                         (补过哪些、拒过哪些、各自的理由)—— 它是留给「候选清单读不动」的出路,\
                         不是刷新用的。\n\
                         \n\
                         **不带 `rebuild` 再调一次**:它会保留你对同一段的判断,\
                         同时把新算出来的候选合进来。\n\
                         确实一条都不要了,先问过用户 —— 这是他攒下来的记录,不是缓存。"
                    );
                }
                None
            } else {
                list
            }
        }
        // 读不动:只有这时 `rebuild` 才放行(读得动就按上面那条办)
        Err(e) => {
            if rebuild {
                None
            } else {
                return Err(e);
            }
        }
    };

    let merged = match previous {
        None => fresh,
        Some(old) => merge(old, fresh),
    };
    // 上一轮那些处置结论跟着这份清单一起回状态(`save_supplements` 收的是**合并后**的)
    crate::materials::save_supplements(opened, &merged)?;
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
    opened: &mut OpenProject,
    t: f64,
    reason: &str,
    origin: ShotOrigin,
) -> Result<Shot> {
    if !t.is_finite() {
        anyhow::bail!("截图时刻不合法:{t}");
    }
    let video = opened.require_video()?;
    let layout = opened.layout();
    // **清单不在 = 还没有图**,不是错误:步 1 可以用 `--no-shots` 跑,
    // 而"补第一张图"正是那时候该干的事。三态里"不在 → 当空"那条,这里也适用 ——
    // 这个动作要的是"发个号",不是判据。
    let mut shots = crate::materials::shots_optional(opened)?.unwrap_or_default();

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
    crate::materials::save_shots(opened, &shots)?;

    // 补图和"候选的结论"是同一件事的两半:只补不标,门那边还挂着"这条候选没结论",
    // 而模型手上没有工具能把它消掉 —— 它只能再补一张,或者去改文件(M4 第五轮实测)。
    link_candidates_to_shot(opened, t, &shot.id)?;
    Ok(shot)
}

/// 把 `t` 附近(候选自己的判定半径内)**还没有结论**的候选标成"已补",指到这张图上。
///
/// 已经有结论的(不补 / 已补到别的图上)不动 —— 那是agent 有意的裁决,不该被覆盖。
fn link_candidates_to_shot(opened: &mut OpenProject, t: f64, id: &ShotId) -> Result<usize> {
    let Some(mut list) = crate::materials::supplements(opened)? else {
        return Ok(0);
    };
    let window = list.window;
    let mut linked = 0;
    for req in list.requests.iter_mut() {
        if req.is_pending() && (req.t - t).abs() <= window {
            req.status = SupplementStatus::Accepted;
            req.shot_id = Some(id.clone());
            req.note = Some(format!("补了 {id}(t={t:.1}s)"));
            linked += 1;
        }
    }
    if linked > 0 {
        crate::materials::save_supplements(opened, &list)?;
    }
    Ok(linked)
}

/// 对一条补图候选的处置。
#[derive(Debug, Clone, PartialEq)]
pub struct SupplementDecision {
    /// 候选号(`supplement_requests.json` 里的 `id`)。
    pub id: u32,
    pub decision: SupplementDecisionKind,
    /// 一句理由。补 → 进截图的 `reason`;不补 → 存进候选的 `note`。
    /// **必给**:没有理由的处置事后没法抽查。
    pub note: String,
    /// 补的时候可以另指时刻(秒)。不给就用候选自己算出来的那个。
    pub t: Option<f64>,
}

/// 补还是不补。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplementDecisionKind {
    /// 要补 —— 工具去抽帧(`t` 附近已经有留用的图就沿用,不重复抽)。
    NeedShot,
    /// 不用补。
    NoShot,
}

/// 一批处置的结果。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SupplementOutcome {
    /// 这次处置了几条。
    pub handled: usize,
    /// 处置完还剩多少条没结论。**门数的就是这个数。**
    pub pending_left: usize,
    /// 新抽出来的图。
    pub extracted: Vec<String>,
    /// 沿用的已有图(候选举例:`#12 → s013`)。
    pub reused: Vec<String>,
    /// 判"不用补"的候选号。
    pub rejected: Vec<u32>,
}

/// 处置一批补图候选:**逐条给结论** —— 补(`extract_shot`)或者不补。
///
/// # 为什么非要这个入口
///
/// `status` 这一栏只有这个函数会写。在这之前,"补"有出口(`extract_shot`),
/// **"不补"没有** —— 而门数的正是"还有几条 `pending`"。于是模型怎么干都过不去门:
/// M4 第五轮实测,它撞了三轮之后自己动手把整份 JSON 重写了一遍(值还不合法),
/// 门跟着瞎了,一路放行。**门要的动,必须有人能做。**
///
/// # 顺手挡掉重复抽图
///
/// `need_shot` 时先看 `t` 附近(候选自己的判定半径内)有没有**留用的**图:
/// 有就沿用它、把候选指过去,而不是再抽一张。第五轮实测里 s012–s019 就是同 4 个
/// 时刻被抽了两遍 —— 8 张图 4 张废,还多出 4 条 drop。
pub fn disposition_supplements(
    opened: &mut OpenProject,
    items: &[SupplementDecision],
) -> Result<SupplementOutcome> {
    if items.is_empty() {
        anyhow::bail!("没有要处置的候选 —— 一次报一批(`items`)");
    }
    for it in items {
        if it.note.trim().is_empty() {
            anyhow::bail!(
                "候选 #{} 没有给理由 —— 补或是不补都要写一句,否则事后没法抽查",
                it.id
            );
        }
    }

    let list = crate::materials::supplements(opened)?.ok_or_else(|| {
        anyhow::anyhow!("这个项目还没有补图候选清单 —— 先调 `find_supplement_candidates`")
    })?;
    let known: Vec<u32> = list.requests.iter().map(|r| r.id).collect();
    if let Some(bad) = items.iter().find(|it| !known.contains(&it.id)) {
        anyhow::bail!(
            "没有候选 #{}。这份清单里的号是:{}(共 {} 条)",
            bad.id,
            known
                .iter()
                .take(12)
                .map(|i| format!("#{i}"))
                .collect::<Vec<_>>()
                .join("、"),
            known.len()
        );
    }

    let window = list.window;
    let mut outcome = SupplementOutcome {
        handled: items.len(),
        pending_left: 0,
        extracted: Vec::new(),
        reused: Vec::new(),
        rejected: Vec::new(),
    };

    // 这一批的结论先**攒着**,最后再落到**重新读到的那一份**上。
    //
    // 为什么不能边读边写:这一轮里的"要补"会调 `extract_shot`,它顺手把附近还没结论的
    // 候选标掉(`link_candidates_to_shot`)—— 那是**别人**刚写进去的东西。
    // 拿一开始读到的那份快照整份覆盖,就会把那一下盖回去:候选又变回"没结论",
    // 门跟着拦,而调用方完全看不出是谁干的(两条规则各自都对,凑一起就打架)。
    let mut decided: Vec<(u32, Option<ShotId>, String)> = Vec::new();

    for it in items {
        let t = it.t.unwrap_or_else(|| {
            list.requests
                .iter()
                .find(|r| r.id == it.id)
                .map(|r| r.t)
                .unwrap_or(0.0)
        });

        let shot_id = match it.decision {
            SupplementDecisionKind::NoShot => None,
            SupplementDecisionKind::NeedShot => {
                let near = crate::materials::shots_optional(opened)?
                    .unwrap_or_default()
                    .shots
                    .into_iter()
                    .find(|s| s.status == ShotStatus::Keep && (s.t - t).abs() <= window);
                let id = match near {
                    Some(existing) => {
                        outcome.reused.push(format!("#{} → {}", it.id, existing.id));
                        existing.id
                    }
                    None => {
                        let shot = extract_shot(opened, t, &it.note, ShotOrigin::AgentRequest)?;
                        outcome.extracted.push(shot.id.to_string());
                        shot.id
                    }
                };
                Some(id)
            }
        };

        if shot_id.is_none() {
            outcome.rejected.push(it.id);
        }
        decided.push((it.id, shot_id, it.note.trim().to_string()));
    }

    // 重新读一遍(**含**上面 `extract_shot` 顺手标掉的那些),只把这一批的结论盖上去
    let mut latest = crate::materials::supplements(opened)?.ok_or_else(|| {
        anyhow::anyhow!("补图候选清单在处置过程中不见了 —— 这一步先停下,让人看一眼")
    })?;
    for (id, shot_id, note) in decided {
        if let Some(req) = latest.get_mut(id) {
            match shot_id {
                Some(id) => {
                    req.status = SupplementStatus::Accepted;
                    req.shot_id = Some(id);
                }
                None => {
                    req.status = SupplementStatus::Rejected;
                    req.shot_id = None;
                }
            }
            req.note = Some(note);
        }
    }

    outcome.pending_left = latest.pending().count();
    crate::materials::save_supplements(opened, &latest)?;
    Ok(outcome)
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

pub fn update_shot(opened: &mut OpenProject, id: ShotId, action: ShotAction) -> Result<Shot> {
    let video = opened.require_video()?;
    let layout = opened.layout();
    // 同 `extract_shot`:清单不在按"还没有图"算,于是报错会是「一张截图都没有」,
    // 而不是一句"步 1 还没跑" —— 后者在"就是想补第一张"时是误导。
    let mut shots = crate::materials::shots_optional(opened)?.unwrap_or_default();

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
    crate::materials::save_shots(opened, &shots)?;
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
    use crate::testkit::Fake;

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

    // ── 候选的处置入口 ────────────────────────────────────────────────

    /// 门要的动,现在有人能做了:把候选逐条给出结论,门就该开。
    ///
    /// 这条是 M4 第五轮的直接教训 —— 在那之前,门数的 `pending` **没有任何工具会写**,
    /// 模型怎么干都过不去,最后自己动手改文件。
    #[test]
    fn disposals_clear_the_gate() {
        let fake = Fake::new("shots-dispose")
            .with_transcript(&[(0.0, 2.0, "大家看一下这张图"), (2.0, 4.0, "还有这里")]);
        let mut opened = fake.open();

        // 段也报"审过" —— 不然门卡在它们身上,就测不出候选这一条清没清
        //(写进**状态**;手上这个已打开的项目才是门的判据来源)
        for seg in 1..=2 {
            opened.state_mut().segment_mut(seg).checked_at = Some(vidbrief_project::now());
        }

        let list = find_supplement_candidates(&mut opened, 3.0, false).unwrap();
        assert!(!list.requests.is_empty(), "这段转录该出候选");
        let pending = list.pending().count();

        // 处置之前,门是被这批候选挡住的
        let err = crate::review::gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("补图候选"), "{err}");

        let items: Vec<SupplementDecision> = list
            .requests
            .iter()
            .map(|r| SupplementDecision {
                id: r.id,
                decision: SupplementDecisionKind::NoShot,
                note: "旁边就有图,判定半径太紧造成的假缺失".into(),
                t: None,
            })
            .collect();
        let outcome = disposition_supplements(&mut opened, &items).unwrap();

        assert_eq!(outcome.handled, pending);
        assert_eq!(outcome.pending_left, 0, "处置完不该还剩 pending");
        assert_eq!(outcome.rejected.len(), pending);
        assert!(outcome.extracted.is_empty(), "都不补,不该抽图");

        // 结论落在材料上,而且带理由
        let after = crate::materials::supplements(&opened).unwrap().unwrap();
        assert_eq!(after.pending().count(), 0);
        let first = &after.requests[0];
        assert_eq!(first.status, SupplementStatus::Rejected);
        assert!(first.note.as_deref().unwrap_or("").contains("判定半径"));

        crate::review::gate_calibrate(&opened).expect("候选都有结论了,门该开");
    }

    /// 要补的候选:那一刻附近已经有留用的图 → **沿用它**,不再抽一张。
    #[test]
    fn need_shot_reuses_a_shot_at_the_same_moment() {
        let fake = Fake::new("shots-reuse")
            .with_transcript(&[(0.0, 2.0, "大家看一下这张图")])
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        let list = find_supplement_candidates(&mut opened, 3.0, false).unwrap();
        assert!(!list.requests.is_empty());
        let id = list.requests[0].id;

        let outcome = disposition_supplements(
            &mut opened,
            &[SupplementDecision {
                id,
                decision: SupplementDecisionKind::NeedShot,
                note: "这张图就是它在讲的东西".into(),
                t: Some(2.2),
            }],
        )
        .unwrap();

        assert!(outcome.extracted.is_empty(), "不该重复抽图");
        assert_eq!(outcome.reused.len(), 1, "{outcome:?}");
        let after = crate::materials::supplements(&opened).unwrap().unwrap();
        let req = after.requests.iter().find(|r| r.id == id).unwrap();
        assert_eq!(req.status, SupplementStatus::Accepted);
        assert_eq!(req.shot_id.as_ref().map(|s| s.to_string()), Some("s001".into()));
        let shots = crate::materials::shots(&opened).unwrap();
        assert_eq!(shots.shots.len(), 1, "还是那一张,没多出来");
    }

    /// **一批的落盘,不许把别人刚写进去的东西盖回去。**
    ///
    /// 「要补」那条会顺手把**附近还没结论**的候选标上(`link_candidates_to_shot`),
    /// 而它发生在**这一批的落盘之前**。要是最后拿一开始读到的那份快照整份覆盖,
    /// 那一下就被盖回去了 —— 候选又变回"没结论",门跟着拦,而调用方完全看不出是谁干的。
    /// **两条规则各自都对,凑一起就打架。**
    ///
    /// 真复现这件事要真抽帧:`Fake` 里的视频是占位文件,所以这里换上仓库的回归素材。
    /// ops 的测试一般刻意不碰 ffmpeg,这一条是例外 —— 它测的就是"补图时写到同一份材料上"。
    #[test]
    fn a_write_that_lands_mid_batch_is_not_clobbered() {
        let fake = Fake::new("shots-mid-batch")
            // 两段之间有句号 → 断成两组(各成一条候选);距离又够近(1.0 与 3.0),
            // 补出来的图正好落在第二条的判定半径里 —— 这一条测的就是那一下
            .with_transcript(&[(0.0, 2.0, "大家看一下这张图。"), (2.0, 4.0, "还有这里")]);
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("fixtures")
            .join("test_sample.mp4");
        std::fs::copy(&fixture, fake.layout().rel("source/video.mp4")).expect("回归素材该在");

        let opened = &mut fake.open();
        let list = find_supplement_candidates(opened, 3.0, false).unwrap();
        assert_eq!(list.requests.len(), 2, "该有两条候选:{list:?}");
        let first = list.requests[0].id;
        let second = list.requests[1].id;

        // 只报第一条「要补」:真抽一张图出来(顺带把旁边的第二条标掉)
        let outcome = disposition_supplements(
            opened,
            &[SupplementDecision {
                id: first,
                decision: SupplementDecisionKind::NeedShot,
                note: "这里确实要看图".into(),
                t: None,
            }],
        )
        .unwrap();
        assert_eq!(outcome.extracted.len(), 1, "该抽了一张图:{outcome:?}");

        let after = crate::materials::supplements(opened).unwrap().unwrap();
        let survivor = after.requests.iter().find(|r| r.id == second).unwrap();
        assert_eq!(
            survivor.status,
            SupplementStatus::Accepted,
            "补出来的图就在它旁边,该顺手标掉 —— 不许被这一批的落盘盖回去:{after:?}"
        );
        assert_eq!(outcome.pending_left, 0, "落完之后不该还剩没结论的:{outcome:?}");
    }

    /// 号报错了就把**有哪些号**说出来 —— 让调用方当场能改,而不是再问一轮。
    #[test]
    fn an_unknown_candidate_id_lists_the_known_ones() {
        let fake = Fake::new("shots-bad-id")
            .with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        find_supplement_candidates(&mut opened, 3.0, false).unwrap();

        let err = disposition_supplements(
            &mut opened,
            &[SupplementDecision {
                id: 999,
                decision: SupplementDecisionKind::NoShot,
                note: "随便".into(),
                t: None,
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("没有候选 #999"), "{err}");
        assert!(err.contains("#1"), "该把清单里有哪些号报出来:{err}");
    }

    /// 没有理由的处置不收 —— 事后没法抽查。
    #[test]
    fn a_disposition_without_a_reason_is_refused() {
        let fake =
            Fake::new("shots-no-reason").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        find_supplement_candidates(&mut opened, 3.0, false).unwrap();

        let err = disposition_supplements(
            &mut opened,
            &[SupplementDecision {
                id: 1,
                decision: SupplementDecisionKind::NoShot,
                note: "   ".into(),
                t: None,
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("理由"), "{err}");
    }

    /// 读不动的清单:处置**报错**,不许当成"没有候选"或者悄悄覆盖。
    #[test]
    fn disposing_against_an_unreadable_list_is_an_error() {
        let fake =
            Fake::new("shots-broken").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        std::fs::write(fake.layout().supplement_requests(), b"{ broken").unwrap();

        let err = disposition_supplements(
            &mut opened,
            &[SupplementDecision {
                id: 1,
                decision: SupplementDecisionKind::NoShot,
                note: "随便".into(),
                t: None,
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("读不动"), "{err}");
    }

    /// 重算是显式的:不给 `rebuild`,读不动就不许覆盖。
    #[test]
    fn rebuild_regenerates_only_when_asked() {
        let fake =
            Fake::new("shots-rebuild").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        std::fs::write(fake.layout().supplement_requests(), b"{ broken").unwrap();

        let err = find_supplement_candidates(&mut opened, 3.0, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("读不动"), "{err}");

        let fresh = find_supplement_candidates(&mut opened, 3.0, true).expect("显式重算该能过");
        assert!(!fresh.requests.is_empty());
    }

    /// `rebuild` 不是刷新按钮:清单读得动、上面还有结论时**一律拦下**。
    ///
    /// M5 真 agent 实测:一次 `rebuild=true` 把 298 条候选的历史处置清成了 pending ——
    /// 用户一轮轮攒的"补过哪些、拒过哪些、为什么"一句话就没了,而门随后按
    /// "还没表态"再拦一次,把 agent 拖回去把几百条重做一遍。
    #[test]
    fn rebuild_refuses_to_discard_existing_conclusions() {
        let fake =
            Fake::new("shots-rebuild-guard").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        let list = find_supplement_candidates(&mut opened, 3.0, false).unwrap();
        let id = list.requests[0].id;
        disposition_supplements(
            &mut opened,
            &[SupplementDecision {
                id,
                decision: SupplementDecisionKind::NoShot,
                note: "旁边就有图,不用补".into(),
                t: None,
            }],
        )
        .unwrap();

        let err = find_supplement_candidates(&mut opened, 3.0, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("1 条"), "该报出会丢几条:{err}");
        assert!(err.contains("不带 `rebuild`"), "该给去路:{err}");

        // 拦下之后清单**一个字没动** —— 那条结论还在
        let after = crate::materials::supplements(&opened).unwrap().unwrap();
        let kept = after.requests.iter().find(|r| r.id == id).unwrap();
        assert_eq!(kept.status, SupplementStatus::Rejected, "{after:?}");
    }

    /// 一条结论都没有时,`rebuild` 照常 —— 没有可丢的东西,别误伤。
    #[test]
    fn rebuild_is_fine_when_nothing_would_be_lost() {
        let fake =
            Fake::new("shots-rebuild-empty").with_transcript(&[(0.0, 2.0, "大家看一下这张图")]);
        let mut opened = fake.open();
        find_supplement_candidates(&mut opened, 3.0, false).unwrap();

        let fresh = find_supplement_candidates(&mut opened, 3.0, true)
            .expect("全是 pending,没有可丢的,该放行");
        assert_eq!(fresh.requests.len(), 1, "{fresh:?}");
    }
}
