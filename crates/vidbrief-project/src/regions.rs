//! `01_preprocess/dense_regions.json` —— 变化密集区。
//!
//! 格式见 `docs/schema/dense_regions.schema.json`,算法见 `docs/DESIGN.md` §5.2。
//!
//! 本文件把**参数**与**中间统计量**也一起落盘,原因是密集区阈值必须随视频类型自适应:
//! 出问题时只有靠这两组数字才能复现"当时为什么这样判",并据此调参重跑。

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

/// 可调参数(输入)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionParams {
    /// 滑窗宽度 W(秒)。
    pub window: f64,
    /// 孤立硬切换的 score 阈值。
    pub peak_threshold: f64,
    /// 密集区阈值 = `max(mass_median × mass_k, mass_floor)`。
    pub mass_k: f64,
    /// 密集区阈值的绝对下限,防止静止视频整段被判为密集。
    pub mass_floor: f64,
    /// 相邻密集区间隔小于该值(秒)时合并。
    pub merge_gap: f64,
    /// 长静止判据:静止超过该值(秒)后的首次变化记为章节起点。
    pub static_break: f64,
}

impl Default for RegionParams {
    fn default() -> Self {
        // 数值取自 docs/DESIGN.md §5.2 的初始建议;M1 用真实长视频标定后再改这里。
        Self {
            window: 8.0,
            peak_threshold: 0.25,
            mass_k: 3.0,
            mass_floor: 0.6,
            merge_gap: 4.0,
            static_break: 120.0,
        }
    }
}

impl RegionParams {
    /// 套用公式算出实际生效的密集区阈值。
    pub fn mass_threshold(&self, mass_median: f64) -> f64 {
        (mass_median * self.mass_k).max(self.mass_floor)
    }
}

/// 本次运行算出的中间量(输出)。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionStats {
    /// 参与计算的变化点数量,应与 `change_curve.changes` 等长。
    pub sampled_points: u32,
    /// 滑窗累计值的中位数。
    pub mass_median: f64,
    /// 实际生效的阈值。
    pub mass_threshold: f64,
}

impl RegionStats {
    pub fn new(sampled_points: u32, mass_median: f64, params: &RegionParams) -> Self {
        Self {
            sampled_points,
            mass_median,
            mass_threshold: params.mass_threshold(mass_median),
        }
    }
}

/// 被哪条判据选中。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionReason {
    /// 滑窗累计变化量持续超阈值(信息密集段:连续演示、多图快切)。
    HighDensity,
    /// 单点硬切换,邻域均值远低,且不属于任何密集区(如切场景)。
    IsolatedPeak,
    /// 长时间静止后的首次变化(章节起点)。
    LongStaticBreak,
}

/// 一个变化密集区。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// 项目内唯一,从 1 起按时间顺序编号。
    pub id: u32,
    pub start: f64,
    /// 点事件(孤立峰 / 章节起点)时与 `start` 相等。
    pub end: f64,
    /// 区间内变化量累计(未归一化),用于排序与过滤。
    pub mass: f64,
    /// 区间内 `score` 最大处的时刻 —— **代表帧取这里**。
    pub peak_t: f64,
    pub peak_score: f64,
    pub reason: RegionReason,
}

impl Region {
    /// 时长(秒)。点事件为 0。
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// 是否包含某时刻(闭区间)。
    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t <= self.end
    }
}

/// 密集区清单。允许为空(整段无显著变化的视频)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DenseRegions {
    pub generated_at: DateTime<FixedOffset>,
    pub params: RegionParams,
    pub stats: RegionStats,
    /// 按时间升序。
    pub regions: Vec<Region>,
}

impl DenseRegions {
    /// 用默认参数组装(算法实现在 `vidbrief-core`,M1 补上)。
    pub fn new(params: RegionParams, stats: RegionStats, regions: Vec<Region>) -> Self {
        Self {
            generated_at: crate::now(),
            params,
            stats,
            regions,
        }
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
    fn threshold_formula_takes_max() {
        let p = RegionParams {
            mass_k: 3.0,
            mass_floor: 0.6,
            ..RegionParams::default()
        };
        // 中位数很小 → 下限生效
        assert_eq!(p.mass_threshold(0.1), 0.6);
        assert_eq!(RegionStats::new(25, 0.1, &p).mass_threshold, 0.6);
        // 中位数很大 → 倍数生效
        assert_eq!(p.mass_threshold(1.0), 3.0);
    }

    #[test]
    fn reason_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&RegionReason::HighDensity).unwrap(),
            "\"high_density\""
        );
        assert_eq!(
            serde_json::to_string(&RegionReason::LongStaticBreak).unwrap(),
            "\"long_static_break\""
        );
    }

    #[test]
    fn region_helpers() {
        let r = Region {
            id: 1,
            start: 9.0,
            end: 15.0,
            mass: 2.26,
            peak_t: 9.0,
            peak_score: 0.62,
            reason: RegionReason::HighDensity,
        };
        assert_eq!(r.duration(), 6.0);
        assert!(r.contains(9.0) && r.contains(15.0) && !r.contains(15.1));

        let point = Region { end: 9.0, ..r };
        assert_eq!(point.duration(), 0.0);
    }
}
