//! 由密集区规划"要抽哪几帧" —— **纯函数**。
//!
//! 设计见 `docs/DESIGN.md` §5.2 第 6 步。这里只产出**时刻**,不碰 ffmpeg ——
//! 抽帧是 [`crate::extract`] 的事,两者分开才能在不上 ffmpeg 的情况下测规划逻辑。

use vidbrief_project::{ChangeCurve, DenseRegions, RegionReason, ShotOrigin};

/// 待抽的一帧。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlannedShot {
    pub t: f64,
    pub origin: ShotOrigin,
    /// 算法产物指向 `dense_regions.json` 里的区域;补图为 `None`。
    pub region_id: Option<u32>,
}

/// 同一时刻的容差:相差不到半个采样间隔就算同一帧。
const EPSILON_FACTOR: f64 = 0.5;

/// 规划代表帧。
///
/// - **密集区**:取区间峰值时刻;若区间比窗口还长,再补上两端 —— "必要时两端各补一帧"
///   (`docs/DESIGN.md` §5.2 第 6 步)。密集区常常是"从 A 图切到 B 图再演示"，
///   只取峰值会丢掉开头的样子。
/// - **孤立峰 / 章节起点**:本身就是点事件,只取那一帧。
pub fn plan(regions: &DenseRegions, curve: &ChangeCurve) -> Vec<PlannedShot> {
    let epsilon = curve.step().unwrap_or(1.0) * EPSILON_FACTOR;
    let mut planned: Vec<PlannedShot> = Vec::new();

    for region in &regions.regions {
        let origin = origin_of(region.reason);
        let mut times = vec![region.peak_t];

        let is_dense_and_long =
            region.reason == RegionReason::HighDensity && region.duration() > regions.params.window;
        if is_dense_and_long {
            times.push(region.start);
            times.push(region.end);
        }

        for t in times {
            push_unique(&mut planned, t, origin, Some(region.id), epsilon);
        }
    }

    planned.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    planned
}

fn origin_of(reason: RegionReason) -> ShotOrigin {
    match reason {
        RegionReason::HighDensity => ShotOrigin::DenseRegion,
        // 孤立峰与章节起点在 schema 里都归 isolated_peak
        // (origin 的取值见 docs/schema/README.md §4)
        RegionReason::IsolatedPeak | RegionReason::LongStaticBreak => ShotOrigin::IsolatedPeak,
    }
}

/// 时刻太近就不重复排(密集区峰值与两端常常重合)。
fn push_unique(
    planned: &mut Vec<PlannedShot>,
    t: f64,
    origin: ShotOrigin,
    region_id: Option<u32>,
    epsilon: f64,
) {
    if !t.is_finite() || t < 0.0 {
        return;
    }
    if planned.iter().any(|p| (p.t - t).abs() < epsilon) {
        return;
    }
    planned.push(PlannedShot {
        t,
        origin,
        region_id,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::{
        ChangePoint, DenseRegions, Region, RegionParams, RegionStats, RelPath,
    };

    fn curve(n: usize) -> ChangeCurve {
        ChangeCurve {
            source: RelPath::new("source/video.mp4").unwrap(),
            duration: n as f64,
            fps_sampled: 1.0,
            width: 480,
            height: 270,
            metric: vidbrief_project::curve::DEFAULT_METRIC.to_string(),
            generated_at: vidbrief_project::now(),
            changes: (0..n)
                .map(|i| ChangePoint {
                    t: i as f64,
                    score: 0.0,
                })
                .collect(),
        }
    }

    fn regions(list: Vec<Region>) -> DenseRegions {
        let params = RegionParams::default();
        DenseRegions::new(params, RegionStats::new(100, 0.1, &params), list)
    }

    fn region(id: u32, start: f64, end: f64, peak_t: f64, reason: RegionReason) -> Region {
        Region {
            id,
            start,
            end,
            mass: 1.0,
            peak_t,
            peak_score: 0.5,
            reason,
        }
    }

    #[test]
    fn an_isolated_peak_yields_exactly_one_frame() {
        let r = regions(vec![region(1, 9.0, 9.0, 9.0, RegionReason::IsolatedPeak)]);
        let planned = plan(&r, &curve(30));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].t, 9.0);
        assert_eq!(planned[0].origin, ShotOrigin::IsolatedPeak);
        assert_eq!(planned[0].region_id, Some(1));
    }

    #[test]
    fn a_short_dense_region_yields_only_its_peak() {
        // 区间 9..15 长 6 秒 < window(8s) → 只取峰值
        let r = regions(vec![region(1, 9.0, 15.0, 12.0, RegionReason::HighDensity)]);
        let planned = plan(&r, &curve(30));
        assert_eq!(planned.len(), 1, "{planned:?}");
        assert_eq!(planned[0].t, 12.0);
        assert_eq!(planned[0].origin, ShotOrigin::DenseRegion);
    }

    #[test]
    fn a_long_dense_region_also_takes_both_ends() {
        // 区间 9..25 长 16 秒 > window → 峰值 + 两端
        let r = regions(vec![region(1, 9.0, 25.0, 12.0, RegionReason::HighDensity)]);
        let planned = plan(&r, &curve(40));
        let times: Vec<f64> = planned.iter().map(|p| p.t).collect();
        assert_eq!(times, vec![9.0, 12.0, 25.0], "应当有时间序的峰值与两端");
    }

    #[test]
    fn overlapping_times_are_deduplicated() {
        // 峰值就在区间端点上 → 不该重复排
        let r = regions(vec![region(1, 9.0, 25.0, 9.0, RegionReason::HighDensity)]);
        let planned = plan(&r, &curve(40));
        assert_eq!(planned.len(), 2, "9.0 只该出现一次:{planned:?}");
        assert_eq!(planned[0].t, 9.0);
        assert_eq!(planned[1].t, 25.0);
    }

    #[test]
    fn output_is_sorted_by_time_across_regions() {
        let r = regions(vec![
            region(1, 20.0, 20.0, 20.0, RegionReason::IsolatedPeak),
            region(2, 5.0, 5.0, 5.0, RegionReason::IsolatedPeak),
        ]);
        let planned = plan(&r, &curve(40));
        assert_eq!(planned.iter().map(|p| p.t).collect::<Vec<_>>(), vec![5.0, 20.0]);
    }

    #[test]
    fn static_breaks_map_to_isolated_peak_origin() {
        let r = regions(vec![region(1, 180.0, 180.0, 180.0, RegionReason::LongStaticBreak)]);
        let planned = plan(&r, &curve(200));
        // schema 的 origin 只有四个取值,章节起点归到 isolated_peak
        assert_eq!(planned[0].origin, ShotOrigin::IsolatedPeak);
    }

    #[test]
    fn empty_regions_yield_nothing() {
        assert!(plan(&regions(vec![]), &curve(10)).is_empty());
    }
}
