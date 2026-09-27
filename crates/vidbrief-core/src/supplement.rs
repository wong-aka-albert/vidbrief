//! 补图候选 —— **纯函数:转录 + 截图进,候选清单出**。
//!
//! 设计见 `docs/DESIGN.md` §4.9 与 §6.2 第 ④ 条(`find_supplement_candidates`)。
//!
//! 这里刻意**不做任何判断**:既不判断"用户说『看这里』是不是真有一张图要看",
//! 也不判断该不该补。算法只负责把**可能漏掉的时刻**列出来,补与不补由 agent 看过之后决定
//! (§4.9 原话:"算法只负责列候选,要不要补由 agent 判断")。所以宁可多列 ——
//! 漏掉的那个时刻在视频里已经播完了,agent 再想补也说不清该补哪一秒;
//! 多列的代价只是让 agent 多看一眼。
//!
//! 不碰进程、不碰文件:`Transcript` 与 `Shots` 都是**调用方从盘上读好之后传进来的值**。
//! 这样指代表达清单与 `window` 大小这类判据可以在单测里毫秒级反复调,
//! 不必每轮都建一遍项目、跑一遍 ffmpeg(`feature` / `regions` / `shots` 出于同一理由也是纯的)。
//!
//! # 算法
//!
//! ```text
//! 输入:transcript.segments[](先并成组,见 `SegmentGroup`)、shots.shots[]、window
//!
//! 1) 留用图 = shots 里 status == keep 的那些(drop 的不算)
//! 2) 每一**组**取中点 t = (group.start + group.end) / 2
//! 3) 指代性表达:该组**整组现读法**里含 DEICTIC_PHRASES 里任一条?
//!    —— 不含就丢掉,**不再看"附近有没有图"**(见下)
//! 4) 附近无图:没有留用图满足 |t - shot.t| ≤ window?(只用来标 trigger = both)
//! 5) 按 t 升序 → 从 1 起连续编号
//! ```
//!
//! ## 四个需要解释的决定
//!
//! **为什么取段中点**:段是"一句话"的粒度,而画面在这句话里的任何时刻出现都算对得上。
//! 中点是对"这一句在讲哪一帧"最中立的猜测(取 `start` 或 `end` 会系统性地偏早或偏晚)。
//! 它只是**建议值** —— agent 收到候选后完全可以自己换个时刻抽帧。
//!
//! **为什么以「组」为单位**(2026-09-27,[260927-15]):whisper 按停顿切段,一段常常
//! 只有三五个字,断句断在半个词上 —— 「大家看」在一段、「这张图」在下一段的话,
//! **逐段扫会一条都扫不到**。而 agent 判断"要不要补"靠的正是那句完整的话。
//!
//! **为什么"附近没图"不再单独成候选**(2026-09-27,[260927-16]):它曾经是个判据,
//! 结果**四份真实视频里 95~99% 的段都成了候选**(样本 A 270/277、样本 C
//! 2235/2269、样本 D 307/322、样本 E 688/702),而 agent 一条都没补
//! (**3700 条候选,0 条 accepted**)—— 讲授型视频里"离上一张图超过 3 秒"几乎恒真,
//! 它筛出来的不是"缺图",是"时间过去了"。代价却全在 agent 头上:2235 条候选逐条表态
//! 把上下文顶到 491k、400 撞墙。现在它只做**信息**(`trigger = both` 表示"不但嘴
//! 上指了、附近还没图",更该看一眼),不再单独构成候选。
//!
//! **为什么 `window` 是闭区间**:`|t - shot.t| <= window` 与 `<` 在真实数据上差不了几帧,
//! 但闭区间有确定的语义 ——"正好差 `window` 秒"算附近。这样文档里写的 `±3s` 与代码判据
//! 完全一致,不会出现"照文档算该命中、跑起来却没命中"的错位(边界值最容易吵架)。
//!
//! **为什么"附近有图"只认留用图**:被 `drop` 掉的图**不会出现在最终产物里**。
//! 若拿它当"这里已经有图了",就会把真正缺图的那段永久静音 —— 而那段画面再也补不回来。
//! 所以判据只看 `status == keep`。
//!
//! **为什么读 `Segment::effective_text()` 而不是 `text`**:纠错的成果只写在
//! 结论里,直接读 `text` 等于拿 ASR 的错字去匹配关键词(「如图」被听成
//! 「入图」就漏了)。`effective_text()` 有纠错用纠错、没有才回落到原文,是唯一正确的入口。

use std::cmp::Ordering;

use vidbrief_project::{
    Shot, Shots, SupplementRequest, SupplementRequests, SupplementStatus, Transcript, Trigger,
};

/// 指代性表达。命中任一条即认为"嘴上提到了画面上的东西"。
///
/// 中文没有词边界,所以这里用朴素子串匹配 —— 宁可多列几条候选(agent 还要自己判断),
/// 也不要漏。要加就往下加,不要删。
///
/// **为什么拆得这么细**:`看这里` / `这里显示` / `这边` 在语义上是同一类手指动作,
/// 但子串匹配只认字面,拆开列出来才能一眼看出"要加新说法该往哪一类里加"。
/// 反过来说,**没列 `这个`**:它在中文口语里出现得太密(`这个东西`、`这个时候`),
/// 单靠它列出来的候选会淹掉真正的指代,得不偿失。
pub const DEICTIC_PHRASES: &[&str] = &[
    // ① 让人看向某处 —— 最典型的"嘴上指了一下画面"
    "看这里",
    "看这儿",
    "看这块",
    "这边",
    "这里",
    "这儿",
    "大家看",
    "各位看",
    "看屏幕",
    "看演示",
    "屏幕上",
    "画面上",
    "屏幕里",
    "这个位置",
    "这一块",
    // ② 直接说到"图"
    "这个图",
    "这张图",
    "这个图形",
    "这张图形",
    // 「示意图」是**实测补进来的**(2026-09-27):那份被 agent 逐条看过的候选里,
    // 唯一一条真指代而词表漏掉的正是它(「我们先看一下这个示意图」指向开场那张手绘图)。
    // 加上它,四份材料合计只多出 1 条候选 —— 精确,不泛。
    "示意图",
    "图中",
    "图上",
    "图里",
    "如图",
    "如下图",
    "上图",
    "下图",
    "图表",
    // ③ 指某一段(演示常常是"这一段从头演一遍")
    "这一段",
    "这段",
    "这一节",
    // ④ 说的就是画面上的东西 —— 这类词几乎一定要配图才讲得清
    "这个形态",
    "形态",
    "这段代码",
    // ⑤ 显示 / 标注 / 圈画 / 缩放 / 切换这类"操作画面"的动作
    "这里显示",
    "显示",
    "标注",
    "画一下",
    "画个",
    "画一条",
    "框出来",
    "圈出来",
    "放大",
    "缩小",
    "切换",
    "切到",
    "调出来",
];

/// 列出补图候选。**纯函数。**
///
/// `window` 是"附近有图"的判定半径(秒),与 `docs/DESIGN.md` §4.9 里
/// `supplement_requests.json.window` 是同一个数(默认 3.0)。
///
/// 产出的每条候选 `status` 恒为 `Pending`、`shot_id` 与 `note` 恒为 `None` ——
/// 算法只列候选,填这两样是 agent 的事。
pub fn find_candidates(transcript: &Transcript, shots: &Shots, window: f64) -> SupplementRequests {
    let mut requests = SupplementRequests::new(window);

    // 用 `Shots::usable()` 而不是自己写 `status == Keep`:让"什么叫留用"只有一处定义,
    // 将来真出现了第三种状态也不会漏改这里。
    let kept: Vec<&Shot> = shots.usable().collect();

    // 单位是**组**(一口气说完的一小段,见 `SegmentGroup`):断句断在半个词上的时候,
    // 逐段扫会连"大家看 / 这张图"这种最典型的指代都扫不到。
    //
    // 组通常已按时间升序,但编号要求是"按 t 升序连续"。若依赖上游顺序,
    // 上游哪天不守约(id 乱序 / 区间重叠)就会悄悄编出错号,而且不会报错 —— 显式排一次。
    let mut groups: Vec<_> = transcript.groups().into_iter().collect();
    groups.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(Ordering::Equal)
            // 同一时刻的两组按首段 id 定序,使结果与输入顺序无关
            .then(a.from.cmp(&b.from))
    });

    for group in groups {
        let text = group.text(transcript);
        // **只说"附近没图"不再打扰 agent** —— 见模块文档里那条实测
        // (3700 条候选、0 条 accepted,代价却全在上下文上)。
        if !has_deictic_phrase(&text) {
            continue;
        }

        let t = (group.start + group.end) / 2.0;
        let nearest = nearest_kept(&kept, t);
        let gap = nearest.map(|(_, distance)| distance);
        let no_shot_nearby = match gap {
            Some(distance) => distance > window,
            // 一张留用图都没有:整段视频都是"附近无图"
            None => true,
        };

        requests.requests.push(SupplementRequest {
            id: 0, // 排完序统一发号(与 regions 的做法一致)
            t,
            // 组的**首段** —— 段号仍然是引用材料的那把钥匙(组的区间是 from..=to)
            segment_id: group.from,
            // 整组的现读法,不截断:agent 正是靠这句话判断该不该补,
            // 它也是"为什么会被列出来"的唯一凭据(见 `SupplementRequest::text` 的注释)。
            text,
            trigger: if no_shot_nearby {
                Trigger::Both
            } else {
                Trigger::DeicticPhrase
            },
            nearest_shot: nearest.map(|(shot, _)| shot.id),
            gap,
            status: SupplementStatus::Pending,
            shot_id: None,
            note: None,
        });
    }

    for (i, request) in requests.requests.iter_mut().enumerate() {
        request.id = i as u32 + 1;
    }
    requests
}

/// 文本里是否含指代性表达。
///
/// 用 `str::contains`,**不做任何字节切片** —— 中文一个字三字节,切在中间直接 panic
/// (AGENTS.md §1.1)。这里也根本不需要切:判据是"含不含",不是"哪一段"。
fn has_deictic_phrase(text: &str) -> bool {
    DEICTIC_PHRASES.iter().any(|phrase| text.contains(*phrase))
}

/// 离 `t` 最近的**留用**截图,及其距离(秒)。没有任何留用图时为 `None`。
///
/// 距离一样近时(比如 `t` 正好落在两张图正中间)取 `t` 靠前的那张。不是因为它更"对",
/// 而是为了让结果**可重复**:否则 `nearest_shot` 会随 `shots.shots` 的数组顺序漂移,
/// 同一份项目跑两次给出不同候选,就没法拿它当回归基线了。
fn nearest_kept<'a>(kept: &[&'a Shot], t: f64) -> Option<(&'a Shot, f64)> {
    kept.iter()
        .map(|shot| (*shot, (t - shot.t).abs()))
        .min_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(Ordering::Equal)
                .then(a.0.t.partial_cmp(&b.0.t).unwrap_or(Ordering::Equal))
                // 同一时刻的多张图再按 id 定序,把"完全平手"也钉死
                .then(a.0.id.cmp(&b.0.id))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::{RelPath, Segment, ShotId, ShotOrigin, ShotStatus};

    /// 默认判定半径,DESIGN §4.9 给的就是 3.0。
    const W: f64 = 3.0;

    /// 造一份转录。参数是 `(id, start, end, text)`,**顺序照传入的来**
    /// (有一条用例故意倒着传,验证编号不依赖输入顺序)。
    fn transcript(segments: &[(u32, f64, f64, &str)]) -> Transcript {
        Transcript {
            source: RelPath::new("source/video.mp4").unwrap(),
            model: "ggml-medium".into(),
            language: "zh".into(),
            duration: 600.0,
            generated_at: vidbrief_project::now(),
            segments: segments
                .iter()
                .map(|(id, start, end, text)| Segment {
                    id: *id,
                    start: *start,
                    end: *end,
                    text: (*text).to_string(),
                    conclusion: None,
                    checked_at: None,
                })
                .collect(),
        }
    }

    /// 造一份纠错过的转录:单段,`text` 是 ASR 原文,`corrected` 是 agent 的结论。
    fn corrected_transcript(text: &str, corrected: &str) -> Transcript {
        let mut t = transcript(&[(1, 0.0, 10.0, text)]);
        t.segments[0].conclusion = Some(vidbrief_project::Conclusion::Corrected {
            text: corrected.to_string(),
            kind: vidbrief_project::CorrectionType::Other,
            note: "测试".into(),
            reason: "测试".into(),
        });
        t
    }

    /// 造一张截图(`origin` 固定用算法产的密集区,与真实产物同形)。
    fn shot(id: u32, t: f64, status: ShotStatus) -> Shot {
        let sid = ShotId::new(id).unwrap();
        let origin = ShotOrigin::DenseRegion;
        Shot {
            id: sid,
            t,
            file: Shot::default_rel_path(origin, sid, t).unwrap(),
            origin,
            region_id: None,
            status,
            reason: None,
            created_at: vidbrief_project::now(),
            check: None,
        }
    }

    fn shots(list: Vec<Shot>) -> Shots {
        let mut all = Shots::new();
        all.shots = list;
        all
    }

    /// 候选列表里唯一那条(用例大多只有一条,取出来省得反复写 `[0]`)。
    fn only(reqs: &SupplementRequests) -> &SupplementRequest {
        assert_eq!(reqs.requests.len(), 1, "本该只有一条候选:{:#?}", reqs.requests);
        &reqs.requests[0]
    }

    #[test]
    fn a_deictic_phrase_alone_is_a_candidate() {
        // 段中点 t = 5.0,恰好有张留用图在 5.0 → 附近有图,只因"嘴上指了画面"入选
        let t = transcript(&[(1, 0.0, 10.0, "我们来看这个形态")]);
        let s = shots(vec![shot(1, 5.0, ShotStatus::Keep)]);
        let reqs = find_candidates(&t, &s, W);

        let r = only(&reqs);
        assert_eq!(r.trigger, Trigger::DeicticPhrase);
        assert_eq!(r.t, 5.0, "取段中点");
        assert_eq!(r.segment_id, 1);
        assert_eq!(r.nearest_shot, Some(ShotId::new(1).unwrap()), "附近有图,该给出最近的一张");
        assert_eq!(r.gap, Some(0.0));
        assert_eq!(reqs.window, W, "window 要原样落到结果里");
    }

    #[test]
    fn no_shot_nearby_alone_is_no_longer_a_candidate() {
        // 「附近没图」单独不再成候选(2026-09-27,[260927-16]):实测四份视频 95~99% 的段
        // 都命中它、而 agent 一条都没补 —— 它筛出来的是"时间过去了",不是"缺图"。
        let t = transcript(&[(1, 0.0, 10.0, "今天先讲一下基本面")]);
        let s = shots(vec![shot(1, 100.0, ShotStatus::Keep)]);
        assert!(
            find_candidates(&t, &s, W).requests.is_empty(),
            "嘴上没指画面,就不该拿'附近没图'去打扰 agent"
        );

        // 但它没白算:指了画面、附近又确实没图 → trigger = both,而且距离摆在候选上
        let t = transcript(&[(1, 0.0, 10.0, "今天先讲一下基本面,大家看这里")]);
        let reqs = find_candidates(&t, &s, W);
        let r = only(&reqs);
        assert_eq!(r.trigger, Trigger::Both);
        assert_eq!(r.nearest_shot, Some(ShotId::new(1).unwrap()), "远处那张仍是'最近的一张'");
        assert_eq!(r.gap, Some(95.0));
    }

    #[test]
    fn both_triggers_are_reported_as_both() {
        let t = transcript(&[(1, 0.0, 10.0, "大家看这里")]);
        let s = shots(vec![shot(1, 100.0, ShotStatus::Keep)]);
        let reqs = find_candidates(&t, &s, W);

        assert_eq!(only(&reqs).trigger, Trigger::Both);
    }

    #[test]
    fn a_plain_segment_with_a_nearby_shot_is_not_a_candidate() {
        // 没提画面,附近也有图 → 没有任何理由麻烦 agent
        let t = transcript(&[(1, 0.0, 10.0, "今天先讲一下基本面")]);
        let s = shots(vec![shot(1, 5.0, ShotStatus::Keep)]);
        assert!(
            find_candidates(&t, &s, W).requests.is_empty(),
            "两条都不命中时不该出现在结果里"
        );
    }

    #[test]
    fn no_shots_at_all_means_no_nearest_and_every_shot_nearby_is_unknown() {
        let t = transcript(&[
            (1, 0.0, 10.0, "今天先讲一下基本面,大家看这里"),
            (2, 10.0, 20.0, "再说说后面的安排,如图"),
        ]);
        let reqs = find_candidates(&t, &shots(vec![]), W);

        assert_eq!(reqs.requests.len(), 2);
        for r in &reqs.requests {
            // 一张图都没有 → "附近无图"恒真,于是两条都是 both
            assert_eq!(r.trigger, Trigger::Both);
            assert_eq!(r.nearest_shot, None);
            assert_eq!(r.gap, None, "没有图就没有距离可言");
        }
    }

    #[test]
    fn an_empty_transcript_yields_an_empty_list() {
        // 转录为空是合法输入(比如纯音乐视频),不该 panic
        let reqs = find_candidates(&transcript(&[]), &shots(vec![]), W);
        assert!(reqs.requests.is_empty());
        assert_eq!(reqs.window, W);
    }

    #[test]
    fn a_shot_exactly_window_away_still_counts_as_nearby() {
        // 边界:闭区间 —— 正好差 window 秒算"附近",所以这条只因指代入选,
        // 而不是同时命中 no_shot_nearby
        let t = transcript(&[(1, 0.0, 10.0, "大家看这里")]);
        let s = shots(vec![shot(1, 8.0, ShotStatus::Keep)]); // |5.0 - 8.0| == 3.0 == window
        let reqs = find_candidates(&t, &s, W);
        let r = only(&reqs);
        assert_eq!(r.gap, Some(3.0));
        assert_eq!(r.trigger, Trigger::DeicticPhrase, "正好 window 秒要算附近(闭区间)");

        // 只要多一点点就越界,判据确实是"闭"而不是"开"
        let s = shots(vec![shot(1, 8.1, ShotStatus::Keep)]);
        assert_eq!(
            only(&find_candidates(&t, &s, W)).trigger,
            Trigger::Both,
            "超出 window 一点点就该算附近无图"
        );
    }

    #[test]
    fn a_dropped_shot_does_not_count_as_nearby() {
        // drop 掉的图不会出现在最终产物里,拿它当"这里已经有图"会把缺口永久盖住:
        // 它既不该压掉候选,也不该被当成"最近的那一张"。
        let t = transcript(&[(1, 0.0, 10.0, "大家看这里的代码")]);
        let dropped = shots(vec![shot(1, 5.0, ShotStatus::Drop)]);

        let reqs = find_candidates(&t, &dropped, W);
        let r = only(&reqs);
        assert_eq!(r.trigger, Trigger::Both, "被 drop 的图不算附近有图");
        assert_eq!(r.nearest_shot, None, "一张留用图都没有");

        // 有留用图时,最近的那张也绝不能是 drop 掉的那张
        let mixed = shots(vec![
            shot(1, 5.0, ShotStatus::Drop),
            shot(2, 20.0, ShotStatus::Keep),
        ]);
        let mixed_reqs = find_candidates(&t, &mixed, W);
        let r = only(&mixed_reqs);
        assert_eq!(r.nearest_shot, Some(ShotId::new(2).unwrap()));
        assert_eq!(r.gap, Some(15.0));
    }

    #[test]
    fn ids_are_continuous_in_time_order_even_if_segments_arrive_shuffled() {
        // 故意倒着传、且 id 与时间不一致 —— 编号必须按 t 升序重排
        let t = transcript(&[
            (3, 20.0, 30.0, "这一段是第三句,看这里"),
            (1, 0.0, 10.0, "这一段是第一句,看这里"),
            (2, 10.0, 20.0, "这一段是第二句,看这里"),
        ]);
        let reqs = find_candidates(&t, &shots(vec![]), W);

        let ids: Vec<u32> = reqs.requests.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "编号从 1 起连续");
        let times: Vec<f64> = reqs.requests.iter().map(|r| r.t).collect();
        assert_eq!(times, vec![5.0, 15.0, 25.0], "按 t 升序");
        let seg_ids: Vec<u32> = reqs.requests.iter().map(|r| r.segment_id).collect();
        assert_eq!(seg_ids, vec![1, 2, 3]);
    }

    /// **候选的单位是「组」**(2026-09-27,[260927-16]):whisper 把一句话切碎时,
    /// 指代的一半可能在上一段、另一半在下一段 —— 逐段扫一条都扫不到。
    #[test]
    fn a_candidate_covers_its_whole_group() {
        let t = transcript(&[
            (1, 0.0, 2.0, "好那我们"),        // 组里的一段,不含指代
            (2, 2.0, 5.0, "大家看这张图"),    // 指代在这里
            (3, 5.0, 8.0, "上面画的这条线"),  // 组里的一段
        ]);

        let reqs = find_candidates(&t, &shots(vec![]), W);
        let r = only(&reqs);
        assert_eq!(r.segment_id, 1, "段号落在**组首**");
        assert_eq!(r.t, 4.0, "建议时刻取组中点");
        assert_eq!(
            r.text, "好那我们大家看这张图上面画的这条线",
            "text 是**整组**的现读法,不是命中的那一段"
        );
    }

    /// 指代被切断在两段之间也算 —— 这正是逐段扫会漏、按组扫不会漏的那种输入。
    #[test]
    fn a_phrase_split_across_two_segments_still_counts() {
        // 「调出来」被切在中间:两段各自都不含任何指代词,合起来才是
        let t = transcript(&[(1, 0.0, 3.0, "好我们把它调"), (2, 3.0, 6.0, "出来对比一下")]);
        assert!(!has_deictic_phrase("好我们把它调"), "前提:前后半截各自都不命中");
        assert!(!has_deictic_phrase("出来对比一下"), "前提:前后半截各自都不命中");

        let reqs = find_candidates(&t, &shots(vec![]), W);
        assert_eq!(only(&reqs).text, "好我们把它调出来对比一下");
    }

    #[test]
    fn text_comes_from_effective_text_and_is_not_truncated() {
        // 关键词只在纠错后的文本里 —— 若直接读 `text` 就会漏掉这条候选
        let t = corrected_transcript("我们来看入图", "我们来看如图的这个指标");
        let s = shots(vec![shot(1, 5.0, ShotStatus::Keep)]);
        let first = find_candidates(&t, &s, W);
        assert_eq!(
            only(&first).trigger,
            Trigger::DeicticPhrase,
            "要用 effective_text() 判关键词"
        );

        // 反过来:原文里的"这里"已被纠错改掉,`text` 落盘的必须是纠错后的原文
        let t = corrected_transcript("看这里的数字", "读一下屏幕上的数字");
        let second = find_candidates(&t, &s, W);
        let r = only(&second);
        assert_eq!(r.text, "读一下屏幕上的数字", "text 落盘的是有效文本原文,不截断");
        assert_eq!(
            r.trigger,
            Trigger::DeicticPhrase,
            "纠错后的文本里有「屏幕上」,仍是指代"
        );

        let long = "看这里".to_string() + &"很长的一句话".repeat(50);
        let t = corrected_transcript("随便", &long);
        let third = find_candidates(&t, &s, W);
        assert_eq!(only(&third).text, long, "text 必须是整段原文,不做任何截断");
    }

    #[test]
    fn nearest_prefers_the_earlier_shot_on_a_tie() {
        // t = 5.0 落在两张图正中间 → 取 t 较小的那张,且与数组顺序无关
        let t = transcript(&[(1, 0.0, 10.0, "大家看这里的两张图")]);
        let forward = shots(vec![
            shot(1, 3.0, ShotStatus::Keep),
            shot(2, 7.0, ShotStatus::Keep),
        ]);
        let backward = shots(vec![
            shot(2, 7.0, ShotStatus::Keep),
            shot(1, 3.0, ShotStatus::Keep),
        ]);
        for s in [forward, backward] {
            let reqs = find_candidates(&t, &s, W);
            let r = only(&reqs);
            assert_eq!(r.nearest_shot, Some(ShotId::new(1).unwrap()), "平手取 t 较小的");
            assert_eq!(r.gap, Some(2.0));
        }
    }

    #[test]
    fn defaults_are_pending_and_unassigned() {
        // 算法只列候选:status 恒为 pending,shot_id / note 恒为空
        let t = transcript(&[(1, 0.0, 10.0, "看屏幕,这里显示的是结果")]);
        let s = shots(vec![shot(1, 100.0, ShotStatus::Keep)]);
        let reqs = find_candidates(&t, &s, W);

        let r = only(&reqs);
        assert_eq!(r.status, SupplementStatus::Pending);
        assert_eq!(r.shot_id, None);
        assert_eq!(r.note, None);
        assert_eq!(reqs.pending().count(), 1);
    }

    #[test]
    fn deictic_matching_is_substring_based() {
        // 中文没有词边界:关键词嵌在长句里也算命中
        assert!(has_deictic_phrase("好我们切到主图"));
        assert!(has_deictic_phrase("大家看这里的成交量"));
        assert!(!has_deictic_phrase("今天先讲一下基本面"));
        // 清单里逐条都要能命中自己(防止手滑写错字)
        for phrase in DEICTIC_PHRASES {
            assert!(has_deictic_phrase(phrase), "清单里的 `{phrase}` 匹配不到自己");
        }
    }
}
