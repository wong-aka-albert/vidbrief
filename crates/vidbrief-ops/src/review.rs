//! 用户确认 —— **审核门**。
//!
//! 设计见 `docs/DESIGN.md` §6.3 与 devlog [260925-21]。
//!
//! # 门是什么
//!
//! `progress` 记"做过什么",这里记"**用户看过并点头了**"。后者是门:
//! 校准没被确认过、或者确认之后材料又变了,`write_summary` 就不放行。
//!
//! 为什么要这么一道门:M4 联调跑了两轮,发现"靠对话把关"等于没有把关 ——
//! **一个一张图都没校准的项目,可以直接写总结、把步 3 标成完成。**
//! 门把这个隐性遗漏变成一个**显式动作**:要么把东西处理干净,要么让用户点头。
//!
//! # 门不验"是谁签的"
//!
//! 验不了:MCP 握手只给客户端名(`clientInfo`),拿不到模型名;而"让模型自报"
//! 就是让它编 —— 那正是这套东西要防的事。所以 `Approval` 里**没有** `approved_by`。
//!
//! 服务端能观察到的只有三样:什么时候、当时什么状态、有没有带说明。
//! 门靠"状态有没有变过"来判断这次确认还算不算数 —— 这是**防漏掉,不是防伪造**:
//! 一个必须显式调用 `approve` 才能过去的关口,本身就是一道提醒。

use anyhow::{Result, bail};
use vidbrief_project::ReviewCounts;

use crate::project::OpenProject;

/// 数一下当前的材料状态。
///
/// 四项**全是服务端自己数得出来的** —— 数不出来的东西不进这道门。
/// 缺文件一律按 0 算:没跑过前处理的项目 = "什么都没有",不是错误。
pub fn counts(opened: &OpenProject) -> Result<ReviewCounts> {
    let shots = crate::materials::shots(opened).unwrap_or_default();
    let supplement_pending = vidbrief_project::SupplementRequests::load(
        &opened.layout().supplement_requests(),
    )
    .map(|r| r.requests.iter().filter(|x| x.is_pending()).count())
    .unwrap_or(0);
    let uncertain = crate::materials::effective_transcript(opened)
        .map(|(t, _, _)| t.segments.iter().filter(|s| s.is_uncertain()).count())
        .unwrap_or(0);

    Ok(ReviewCounts {
        unchecked: shots.unchecked().count(),
        needing_review: shots.needing_review().count(),
        supplement_pending,
        uncertain,
    })
}

/// 记下"用户确认了这一步"。
///
/// `note` 是**带着未处理项通过**时的说明;正常通过给 `None`。
/// 服务端不拦"还有 3 张没校准就通过"——那是用户的权力,但要留下理由。
pub fn approve(opened: &mut OpenProject, note: Option<String>) -> Result<vidbrief_project::Approval> {
    let counts = counts(opened)?;
    let approval = vidbrief_project::Approval {
        approved_at: vidbrief_project::now(),
        counts,
        note: note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
    };
    opened.project_mut().review.calibrate = Some(approval.clone());
    opened.save()?;
    Ok(approval)
}

/// 步 2 的门:**校准没被确认过、或者确认之后东西又变了,就别进步 3。**
///
/// 报错要说清**是哪一项变了、从多少变成多少** —— 笼统一句"状态已变"
/// 等于让调用方去猜,而它猜不出来。
pub fn gate_calibrate(opened: &OpenProject) -> Result<()> {
    let now = counts(opened)?;
    let Some(approved) = opened.project().review.calibrate.as_ref() else {
        bail!(
            "校准还没经过用户确认,不能开始写总结。\n\
             \n\
             当前状态:{}\n\
             \n\
             把上面的东西给用户看(render_timeline 的链接),他点头之后再调 `approve`。\
             如果他就想这样过,让他说一句为什么,把这句话放进 `approve` 的 `note`。",
            describe(&now)
        );
    };

    if approved.counts == now {
        return Ok(());
    }

    bail!(
        "用户确认之后材料又变过了,这次确认已经作废,不能直接写总结。\n\
         \n\
         他确认时:{}\n\
         现在:    {}\n\
         \n\
         {}。\n\
         \n\
         把变化给他看一眼(render_timeline),再让他确认一次。",
        describe(&approved.counts),
        describe(&now),
        diff(&approved.counts, &now)
    )
}

/// 四项一句话。给人看的,不是给机器解析的。
fn describe(c: &ReviewCounts) -> String {
    format!(
        "未校准 {} 张 / 待裁决 {} 张 / 未处理补图候选 {} 条 / 存疑转录 {} 段",
        c.unchecked, c.needing_review, c.supplement_pending, c.uncertain
    )
}

/// 哪几项变了。**只说变了的那几项** —— 四行全列一遍,读的人还得自己找。
fn diff(before: &ReviewCounts, after: &ReviewCounts) -> String {
    let pairs = [
        ("未校准", before.unchecked, after.unchecked),
        ("待裁决", before.needing_review, after.needing_review),
        ("未处理补图候选", before.supplement_pending, after.supplement_pending),
        ("存疑转录", before.uncertain, after.uncertain),
    ];
    let changed: Vec<String> = pairs
        .iter()
        .filter(|(_, a, b)| a != b)
        .map(|(name, a, b)| format!("{name}从 {a} 变成了 {b}"))
        .collect();
    changed.join("、")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    #[test]
    fn counts_start_at_zero_and_track_the_three_sources() {
        let fake = Fake::new("review-counts")
            .with_transcript(&[(0.0, 2.0, "各位好"), (2.0, 4.0, "接着讲")])
            .with_curve(10)
            .with_shot(1, 3.0)
            .with_shot(2, 6.0);
        let opened = fake.open();

        let c = counts(&opened).unwrap();
        assert_eq!(c.unchecked, 2, "两张都没校准");
        assert_eq!(c.needing_review, 0, "没检查过就不算'待裁决'");
        assert_eq!(c.supplement_pending, 0);
        assert_eq!(c.uncertain, 0);
    }

    #[test]
    fn the_gate_is_shut_until_someone_approves() {
        let fake = Fake::new("review-gate-shut").with_curve(5).with_shot(1, 2.0);
        let opened = fake.open();

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("还没经过用户确认"), "{err}");
        assert!(err.contains("approve"), "该告诉它下一步调什么:{err}");
        assert!(err.contains("未校准 1 张"), "该说清现在的状态:{err}");
    }

    #[test]
    fn approving_opens_the_gate() {
        let fake = Fake::new("review-gate-open").with_curve(5).with_shot(1, 2.0);
        let mut opened = fake.open();
        approve(&mut opened, None).unwrap();
        gate_calibrate(&opened).unwrap();
    }

    /// **改了东西,确认就作废** —— 这是这道门全部的意义所在。
    ///
    /// 没有这条,用户确认的是"第 2 轮",而 agent 拿着第 3 轮去写总结,
    /// 门就成了摆设。
    #[test]
    fn changing_anything_after_approval_shuts_the_gate_again() {
        let fake = Fake::new("review-invalidate").with_curve(10).with_shot(1, 2.0);
        let mut opened = fake.open();
        approve(&mut opened, None).unwrap();
        gate_calibrate(&opened).unwrap();

        // 补一张图 —— 材料变了(不走 `extract_shot`:那会真去调 ffmpeg)
        fake.add_shot(2, 5.0, vidbrief_project::ShotOrigin::AgentRequest);

        let err = gate_calibrate(&opened).unwrap_err().to_string();
        assert!(err.contains("又变过了"), "{err}");
        assert!(
            err.contains("未校准从 1 变成了 2"),
            "要说清是哪一项变了:{err}"
        );
    }

    #[test]
    fn a_note_is_kept_and_blank_notes_are_dropped() {
        let fake = Fake::new("review-note").with_curve(5).with_shot(1, 2.0);
        let mut opened = fake.open();

        let a = approve(&mut opened, Some("  用户说先这样  ".into())).unwrap();
        assert_eq!(a.note.as_deref(), Some("用户说先这样"), "两头空白要掐掉");

        let a = approve(&mut opened, Some("   ".into())).unwrap();
        assert_eq!(a.note, None, "空说明等于没说明");
    }
}
