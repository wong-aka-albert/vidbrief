//! 变化密集区检测 —— **纯函数:曲线进,区间出**。
//!
//! 设计见 `docs/DESIGN.md` §5.2。刻意不碰文件也不碰进程,原因很实际:
//! 三类判据里有两类(孤立峰、长静止)**用一个 60 秒的测试视频根本触发不了**,
//! 只能靠合成曲线来测。算法必须是纯的才做得到。
//!
//! # 算法
//!
//! ```text
//! 输入: changes[](等间隔的 (t, score)), 参数 params
//!
//! 1) 滑窗累计(居中): mass[i] = Σ score[i-half ..= i+half]
//! 2) 标定阈值:        mass_threshold = max(median(mass) × mass_k, mass_floor)
//! 3) 密集区:          mass 连续超阈值的极大区间 → 回退 half 摊平窗口 → 按"贡献下限"修边
//! 4) 孤立硬切换:      score ≥ peak_threshold,不在任何密集区,且邻域均值低于阈值
//! 5) 长静止打断:      静止超过 static_break 秒后的首次变化
//! 6) 排序 → 合并挨太近的 → 从 1 起编号
//! ```
//!
//! ## 三个需要解释的决定
//!
//! **为什么窗口是"居中"的**:如果窗口往后看(`[i, i+W)`),一个尖峰会让它**之前**的
//! 所有窗口都超阈值 —— 区间整体前移,定位出来是错的。居中窗口的 `mass[i]` 表达的是
//! "i 附近有多热闹",这才是要定位的东西。
//!
//! **为什么还要"修边"**:居中窗口会把区间向两侧摊平 `half`,于是范围比实际活跃范围宽。
//! 修边依据是**贡献下限** = `mass_threshold / 窗口样本数`,含义是"每帧至少要贡献这么多,
//! 才撑得起这个阈值";低于它的边缘帧被裁掉。
//!
//! **静止判据为什么是派生值**:`static_level = peak_threshold × STATIC_RATIO`,没有做成
//! `RegionParams` 的字段 —— 现在没有任何真实数据能标定它,先暴露一个调不动的旋钮
//! 不如从已有参数派生。等用真实长视频标定时再决定是否独立出来。

use vidbrief_project::{
    ChangeCurve, ChangePoint, DenseRegions, Region, RegionParams, RegionReason, RegionStats,
};

/// 静止判据相对 `peak_threshold` 的比例。
///
/// `peak_threshold` 是"这算一次硬切换"的门槛;静止取它的五分之一,即"几乎没变化"。
pub const STATIC_RATIO: f64 = 0.2;

/// 孤立峰的**突出度**要求:分数必须至少是邻域均值的这么多倍。
///
/// 这是 `docs/DESIGN.md` §5.2 "邻域均值**远低**"的可操作定义,数值直接沿用旧实现
/// (`src/main.rs` 的 `PEAK_PROMINENCE = 3.0`)—— 那是实测标定过的数值。
///
/// **为什么光有 `peak_threshold` 不够**:合成回归素材前半段有个每 4 秒一次的小动画,
/// 分数稳定在 0.26–0.28,刚好越过 0.25 的门槛,于是被当成了硬切换。加上突出度之后,
/// 它的分数(0.282)不到邻域均值(0.113)的 3 倍,被正确拒掉;而真正的切换点(1.000
/// 对邻域 0.036)以 27 倍的差距稳稳通过。
pub const PEAK_PROMINENCE: f64 = 3.0;

/// 在差帧图上找出变化密集区。**纯函数。**
pub fn detect(curve: &ChangeCurve, params: &RegionParams) -> DenseRegions {
    let changes = &curve.changes;
    let n = changes.len();
    let step = curve.step().unwrap_or(1.0);

    if n == 0 {
        return DenseRegions::new(*params, RegionStats::new(0, 0.0, params), Vec::new());
    }

    let scores: Vec<f64> = changes.iter().map(|c| c.score).collect();
    let half = half_window(params.window, step);
    let mass = window_mass(&scores, half);
    let mass_median = median(&mass);
    let mass_threshold = params.mass_threshold(mass_median);

    let mut found: Vec<Region> = Vec::new();

    // ── 3) 密集区 ────────────────────────────────────────────────────
    let window_samples = (2 * half + 1) as f64;
    let contribution_floor = mass_threshold / window_samples;
    for (i0, i1) in runs_above(&mass, mass_threshold) {
        let lo = i0.saturating_sub(half);
        let hi = (i1 + half).min(n - 1);
        let (start_idx, end_idx) = trim_to_active(&scores, lo, hi, contribution_floor);

        // **单帧不是"密集区"。** 一次硬切换会让它前后共 9 个窗口都超过阈值(每个窗口里
        // 都有那一个 1.0),于是被误判成"一段热闹"。但按定义,密集区是**持续**活跃的一段;
        // 单点事件交给孤立峰判据,那里还有突出度把关。
        if start_idx >= end_idx {
            continue;
        }
        found.push(make_region(
            RegionReason::HighDensity,
            changes,
            start_idx,
            end_idx,
        ));
    }

    // ── 4/5) 孤立硬切换与长静止打断 ──────────────────────────────────
    //
    // 长静止打断**不是一个独立判据,而是对孤立峰的重新标注**:同一个事件,
    // "长时间静止后的首次变化"比"一次硬切换"信息量大 —— 它说明这是个**章节边界**。
    // 所以先算静态点,再在造峰时用它决定标签。
    let static_level = params.peak_threshold * STATIC_RATIO;
    let static_hits = static_breaks(changes, static_level, params.static_break);

    for i in isolated_peaks(changes, half, params, &found) {
        let reason = if static_hits.contains(&i) {
            RegionReason::LongStaticBreak
        } else {
            RegionReason::IsolatedPeak
        };
        found.push(make_region(reason, changes, i, i));
    }

    // 静止后变化很轻微(慢淡入之类)够不上孤立峰门槛,但仍是个章节起点
    for i in static_hits {
        if found.iter().any(|r| r.peak_t == changes[i].t) {
            continue;
        }
        found.push(make_region(RegionReason::LongStaticBreak, changes, i, i));
    }

    // ── 6) 排序 → 合并 → 编号 ────────────────────────────────────────
    sort_by_time(&mut found);
    let mut found = merge_close(found, params.merge_gap);
    for (i, region) in found.iter_mut().enumerate() {
        region.id = i as u32 + 1;
    }

    DenseRegions::new(
        *params,
        RegionStats::new(n as u32, mass_median, params),
        found,
    )
}

fn sort_by_time(regions: &mut [Region]) {
    regions.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.end.partial_cmp(&b.end).unwrap_or(std::cmp::Ordering::Equal))
    });
}

/// 窗口半宽(样本数)。`W=8s`、间隔 1s → `half=4`(窗口共 9 个样本 ≈ 9 秒)。
fn half_window(window_secs: f64, step: f64) -> usize {
    if step <= 0.0 || !window_secs.is_finite() || window_secs <= 0.0 {
        return 0;
    }
    ((window_secs / step) / 2.0).round().max(0.0) as usize
}

/// 居中滑窗累计。
fn window_mass(scores: &[f64], half: usize) -> Vec<f64> {
    let n = scores.len();
    let mut out = vec![0.0; n];
    for (i, slot) in out.iter_mut().enumerate() {
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(n - 1);
        *slot = scores.get(lo..=hi).map(|s| s.iter().sum()).unwrap_or(0.0);
    }
    out
}

/// `values[i] > threshold` 的极大连续区间(闭区间下标对)。
fn runs_above(values: &[f64], threshold: f64) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    for (i, v) in values.iter().enumerate() {
        if *v > threshold {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            runs.push((s, i - 1));
        }
    }
    if let Some(s) = start {
        runs.push((s, values.len() - 1));
    }
    runs
}

/// 从两端裁掉"贡献不足"的帧。
fn trim_to_active(scores: &[f64], lo: usize, hi: usize, floor: f64) -> (usize, usize) {
    let mut s = lo;
    let mut e = hi;
    while s < e && scores.get(s).copied().unwrap_or(0.0) < floor {
        s += 1;
    }
    while e > s && scores.get(e).copied().unwrap_or(0.0) < floor {
        e -= 1;
    }
    (s, e)
}

/// 用下标区间造一个 region(峰值在区间内取)。
fn make_region(
    reason: RegionReason,
    changes: &[ChangePoint],
    start_idx: usize,
    end_idx: usize,
) -> Region {
    let slice = changes.get(start_idx..=end_idx).unwrap_or(&[]);
    let peak = slice
        .iter()
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
    let start = changes.get(start_idx).map(|c| c.t).unwrap_or(0.0);
    let end = changes.get(end_idx).map(|c| c.t).unwrap_or(start);

    Region {
        id: 0, // 排序后统一编号
        start,
        end,
        mass: slice.iter().map(|c| c.score).sum(),
        peak_t: peak.map(|c| c.t).unwrap_or(start),
        peak_score: peak.map(|c| c.score).unwrap_or(0.0),
        reason,
    }
}

/// 孤立硬切换的下标。
///
/// 三条判据:
///
/// 1. `score ≥ peak_threshold` —— 够格叫"硬切换";
/// 2. `score ≥ 邻域均值 × PEAK_PROMINENCE` —— 邻域**远低**,不是"一直在动的画面里
///    抖了一下"。这是把"远低"变成可计算的关键一条,少了它周期性小动画会集体误报;
/// 3. **不落在任何密集区里** —— 密集区里的切换由密集区自己代表。
///
/// 最后按 `merge_gap` 并掉挨得太近的,只留最高的那个。
fn isolated_peaks(
    changes: &[ChangePoint],
    half: usize,
    params: &RegionParams,
    regions: &[Region],
) -> Vec<usize> {
    let n = changes.len();
    let mut hits: Vec<usize> = Vec::new();

    for (i, point) in changes.iter().enumerate() {
        if point.score < params.peak_threshold {
            continue;
        }
        let lo = i.saturating_sub(half);
        let hi = (i + half).min(n - 1);
        let neighbors: Vec<f64> = (lo..=hi)
            .filter(|j| *j != i)
            .filter_map(|j| changes.get(j).map(|c| c.score))
            .collect();
        if neighbors.is_empty() {
            continue;
        }
        let mean = neighbors.iter().sum::<f64>() / neighbors.len() as f64;
        if point.score < mean * PEAK_PROMINENCE {
            continue; // 周围也没闲着 → 不是孤立切换
        }
        if regions
            .iter()
            .any(|r| point.t >= r.start && point.t <= r.end)
        {
            continue; // 已经在密集区里
        }
        hits.push(i);
    }

    // 并掉挨得太近的(直接用时刻比,不需要换算成样本数)
    let mut kept: Vec<usize> = Vec::new();
    for i in hits {
        match kept.last().copied() {
            Some(last) if changes[i].t - changes[last].t <= params.merge_gap => {
                if changes[i].score > changes[last].score {
                    kept.pop();
                    kept.push(i);
                }
            }
            _ => kept.push(i),
        }
    }
    kept
}

/// 长静止后的首个变化帧的下标。
fn static_breaks(changes: &[ChangePoint], static_level: f64, min_secs: f64) -> Vec<usize> {
    if min_secs <= 0.0 || changes.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, point) in changes.iter().enumerate() {
        if point.score < static_level {
            run_start.get_or_insert(i);
        } else {
            if let Some(start) = run_start.take() {
                let span = point.t - changes.get(start).map(|c| c.t).unwrap_or(point.t);
                if span >= min_secs {
                    out.push(i);
                }
            }
        }
    }
    out
}

/// 把间隔小于 `merge_gap` 的区域并起来(输入须已按时间排序)。
fn merge_close(regions: Vec<Region>, merge_gap: f64) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::new();
    for region in regions {
        match out.last_mut() {
            Some(last) if region.start - last.end < merge_gap => {
                if region.peak_score > last.peak_score {
                    last.peak_t = region.peak_t;
                    last.peak_score = region.peak_score;
                }
                last.end = last.end.max(region.end);
                last.mass += region.mass;
                // 并出来的区间同时具备两类特征时,归到更"强"的那个判据。
                // 密集区优先 —— 它表示"这一段整体都很热闹",信息量更大。
                if region.reason == RegionReason::HighDensity {
                    last.reason = RegionReason::HighDensity;
                }
            }
            _ => out.push(region),
        }
    }
    out
}

/// 中位数。空集返回 0。
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted.get(mid).copied().unwrap_or(0.0)
    } else {
        let a = sorted.get(mid - 1).copied().unwrap_or(0.0);
        let b = sorted.get(mid).copied().unwrap_or(0.0);
        (a + b) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::RelPath;

    /// 造一条合成曲线:`(时刻, 分数)` 列表 → `ChangeCurve`(间隔 1 秒)。
    fn curve(scores: &[f64]) -> ChangeCurve {
        ChangeCurve {
            source: RelPath::new("source/video.mp4").unwrap(),
            duration: scores.len() as f64,
            fps_sampled: 1.0,
            width: 480,
            height: 270,
            metric: vidbrief_project::curve::DEFAULT_METRIC.to_string(),
            generated_at: vidbrief_project::now(),
            changes: scores
                .iter()
                .enumerate()
                .map(|(i, s)| ChangePoint {
                    t: i as f64,
                    score: *s,
                })
                .collect(),
        }
    }

    fn params() -> RegionParams {
        RegionParams::default()
    }

    #[test]
    fn empty_curve_yields_nothing() {
        let regions = detect(&curve(&[]), &params());
        assert!(regions.regions.is_empty());
        assert_eq!(regions.stats.sampled_points, 0);
    }

    #[test]
    fn a_completely_static_video_yields_nothing() {
        // 全 0 曲线:中位数也是 0,阈值取绝对下限,不该误报
        let regions = detect(&curve(&[0.0; 300]), &params());
        assert!(
            regions.regions.is_empty(),
            "静止视频不该产出密集区:{:?}",
            regions.regions
        );
    }

    #[test]
    fn a_single_hard_cut_becomes_one_isolated_peak() {
        let regions = detect(&curve(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.6, 0.0, 0.0, 0.0, 0.0]), &params());
        let hits: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::IsolatedPeak)
            .collect();
        assert_eq!(hits.len(), 1, "应当只有一个孤立峰:{:#?}", regions.regions);
        assert_eq!(hits[0].peak_t, 5.0);
        assert_eq!(hits[0].id, 1, "编号从 1 起");
    }

    #[test]
    fn a_busy_stretch_becomes_a_high_density_region() {
        // 0-9 静止,10-19 一直很热闹,之后又静止
        let mut scores = vec![0.01; 40];
        for s in scores.iter_mut().skip(10).take(10) {
            *s = 0.3;
        }
        let regions = detect(&curve(&scores), &params());
        let dense: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::HighDensity)
            .collect();
        assert_eq!(dense.len(), 1, "应当只有一段密集区:{:#?}", regions.regions);
        let r = dense[0];
        assert!(
            r.start <= 10.0 && r.end >= 19.0,
            "区间应当覆盖 10..19,实际 {}..{}",
            r.start,
            r.end
        );
        // 修边不该把范围摊得太宽
        assert!(r.start >= 6.0 && r.end <= 23.0, "修边后仍过宽:{}..{}", r.start, r.end);
    }

    #[test]
    fn a_cut_inside_a_busy_stretch_is_not_reported_separately() {
        // 硬切换嵌在密集区里 —— 不该再单独出一个孤立峰
        let mut scores = vec![0.01; 40];
        for s in scores.iter_mut().skip(10).take(10) {
            *s = 0.3;
        }
        scores[14] = 0.9;
        let regions = detect(&curve(&scores), &params());
        let peaks = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::IsolatedPeak)
            .count();
        assert_eq!(peaks, 0, "密集区里的切换不该再报孤立峰:{:#?}", regions.regions);
    }

    #[test]
    fn a_long_static_stretch_marks_the_next_change() {
        // 需要 > static_break(默认 120s)的静止之后才有变化
        let mut scores = vec![0.0; 200];
        scores[180] = 0.4; // 180 秒静止后的首次变化
        let regions = detect(&curve(&scores), &params());
        let breaks: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::LongStaticBreak)
            .collect();
        assert_eq!(breaks.len(), 1, "应有一个章节起点:{:#?}", regions.regions);
        assert_eq!(breaks[0].peak_t, 180.0);
    }

    #[test]
    fn a_short_pause_is_not_a_static_break() {
        let mut scores = vec![0.0; 60];
        scores[30] = 0.4; // 只有 30 秒静止,没到 120 秒
        let regions = detect(&curve(&scores), &params());
        assert!(
            !regions
                .regions
                .iter()
                .any(|r| r.reason == RegionReason::LongStaticBreak),
            "{:#?}",
            regions.regions
        );
    }

    #[test]
    fn nearby_peaks_are_merged_by_merge_gap() {
        let mut scores = vec![0.0; 30];
        scores[10] = 0.6;
        scores[12] = 0.7; // 相隔 2 秒 < merge_gap(4s)
        scores[25] = 0.5;
        let regions = detect(&curve(&scores), &params());
        let peaks: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::IsolatedPeak)
            .collect();
        assert_eq!(peaks.len(), 2, "近的两个该并成一个:{:#?}", regions.regions);
        assert_eq!(peaks[0].peak_t, 12.0, "并了之后保留分数更高的那个");
    }

    #[test]
    fn regions_are_numbered_from_one_in_time_order() {
        let mut scores = vec![0.0; 60];
        scores[50] = 0.6;
        scores[10] = 0.6;
        scores[30] = 0.6;
        let regions = detect(&curve(&scores), &params());
        assert!(!regions.regions.is_empty());
        let ids: Vec<u32> = regions.regions.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            (1..=regions.regions.len() as u32).collect::<Vec<_>>(),
            "编号应当从 1 连续递增"
        );
        let starts: Vec<f64> = regions.regions.iter().map(|r| r.start).collect();
        let mut sorted = starts.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(starts, sorted, "应当按时间升序");
    }

    /// 复刻回归素材(`fixtures/test_sample.mp4`)前半段的真实形态:
    /// 每 4 秒一次"两帧小动画"(0.256 / 0.282),其余帧带轻微噪声(0.017 / 0.030)。
    ///
    /// 写合成用例时**必须带上噪声** —— 少了它邻域均值会低到让突出度判据失效,
    /// 测出来是假的(第一版就是这么写错的)。
    fn flickery_prefix(n: usize) -> Vec<f64> {
        let mut scores: Vec<f64> = (0..n)
            .map(|i| if i % 2 == 0 { 0.017 } else { 0.030 })
            .collect();
        for t in [3, 7, 11, 15] {
            if let Some(s) = scores.get_mut(t) {
                *s = 0.256;
            }
            if let Some(s) = scores.get_mut(t + 1) {
                *s = 0.282;
            }
        }
        scores
    }

    #[test]
    fn a_lone_spike_is_not_a_dense_region() {
        // 一次硬切换会让它前后共 9 个窗口都超过阈值(每个窗口里都有那一个 1.0),
        // 于是很容易被误判成"一段热闹"。按定义,密集区要**持续**活跃。
        let mut scores = vec![0.0; 60];
        scores[30] = 1.0;
        let regions = detect(&curve(&scores), &params());
        assert_eq!(regions.regions.len(), 1, "{:#?}", regions.regions);
        assert_eq!(
            regions.regions[0].reason,
            RegionReason::IsolatedPeak,
            "单帧事件该是孤立峰,不是密集区"
        );
    }

    #[test]
    fn a_periodic_flicker_is_not_reported_as_a_hard_cut() {
        // 0.282 越过了 peak_threshold(0.25),但邻域均值约 0.09 ——
        // 0.282 < 0.09 × 3.0,突出度不够。没有这条判据它们会被逐个报成硬切换
        // (真实回归素材上就是这么误报的:多出一个 t=8 的"切换点")。
        let regions = detect(&curve(&flickery_prefix(60)), &params());
        let peaks: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.reason == RegionReason::IsolatedPeak)
            .collect();
        assert!(
            peaks.is_empty(),
            "周期性小动画不该被当成硬切换:{:#?}",
            regions.regions
        );
    }

    #[test]
    fn a_real_cut_inside_a_busy_stretch_still_gets_its_own_shot() {
        // 真切换(1.0)紧挨着一段闪烁 —— 它会被吸进密集区,但**密集区的代表帧取峰值**,
        // 所以那帧照样会被抽出来。这里断言的是"有没有那一帧",不是"它被贴了什么标签"。
        let mut scores = flickery_prefix(60);
        scores[20] = 1.0;
        for s in scores.iter_mut().skip(21) {
            *s = 0.0; // 切换之后画面稳定下来
        }
        let curve = curve(&scores);
        let regions = detect(&curve, &params());
        let times: Vec<f64> = crate::shots::plan(&regions, &curve)
            .iter()
            .map(|p| p.t)
            .collect();

        assert!(
            times.iter().any(|t| (t - 20.0).abs() < 0.001),
            "真切换那一帧必须被抽出来,实际 {times:?}\n区域:{:#?}",
            regions.regions
        );
        // 闪烁本身不该逐个产帧(否则 60 秒能产出十几个"关键帧")
        assert!(
            times.len() <= 3,
            "闪烁不该被拆成一堆关键帧:{times:?}"
        );
    }

    #[test]
    fn prominence_needs_the_neighbourhood_to_be_quiet() {
        // 钉住突出度公式:同样的分数,邻域越吵越该被拒。
        // 注意边界是"小于"才拒 —— 0.30 对邻域 0.10 恰好 3.0 倍,是**过**的。
        let build = |background: f64| {
            let mut s = vec![background; 30];
            s[15] = 0.30;
            s
        };
        let count = |scores: &[f64]| {
            detect(&curve(scores), &params())
                .regions
                .iter()
                .filter(|r| r.reason == RegionReason::IsolatedPeak)
                .count()
        };

        assert_eq!(count(&build(0.0)), 1, "安静邻域里的 0.30 应当算硬切换");
        assert_eq!(count(&build(0.11)), 0, "0.30 对邻域 0.11 不到 3 倍,该被拒");
        assert_eq!(count(&build(0.10)), 1, "恰好 3.0 倍是边界,取\"过\"");
    }

    #[test]
    fn stats_are_self_consistent() {
        let mut scores = vec![0.01; 40];
        for s in scores.iter_mut().skip(10).take(10) {
            *s = 0.3;
        }
        let regions = detect(&curve(&scores), &params());
        assert_eq!(regions.stats.sampled_points, 40);
        assert_eq!(
            regions.stats.mass_threshold,
            regions.params.mass_threshold(regions.stats.mass_median),
            "落盘的阈值必须与公式一致"
        );
    }

    #[test]
    fn window_halving() {
        assert_eq!(half_window(8.0, 1.0), 4);
        assert_eq!(half_window(8.0, 0.5), 8);
        assert_eq!(half_window(0.0, 1.0), 0);
        assert_eq!(half_window(8.0, 0.0), 0);
    }

    #[test]
    fn median_of_even_and_odd_counts() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), 2.5);
        assert_eq!(median(&[]), 0.0);
    }

    #[test]
    fn merge_keeps_the_stronger_reason() {
        let a = Region {
            id: 0,
            start: 0.0,
            end: 1.0,
            mass: 1.0,
            peak_t: 0.0,
            peak_score: 0.5,
            reason: RegionReason::IsolatedPeak,
        };
        let b = Region { start: 1.5, reason: RegionReason::HighDensity, ..a };
        let merged = merge_close(vec![a, b], 4.0);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].reason, RegionReason::HighDensity);
    }
}
