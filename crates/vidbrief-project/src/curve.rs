//! `01_preprocess/change_curve.json` —— 差帧图。
//!
//! **定义**:`差帧` = 相邻两帧画面差异算出的**一个数值**;把每对相邻帧的差异值沿**时间轴**
//! 排成直方图,就是"差帧图"。这是时间线视图顶层"像素直方图"的数据源。
//!
//! 格式见 `docs/schema/change_curve.schema.json`,算法见 `docs/DESIGN.md` §4.3 / §5.1。

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::relpath::RelPath;

/// 默认采样帧率:每秒一个点。
pub const DEFAULT_FPS_SAMPLED: f64 = 1.0;
/// 默认参与打分的缩小宽度(高度按宽高比自动推出)。
pub const DEFAULT_WIDTH: u32 = 480;
/// 默认打分口径:颜色直方图 L1 距离(权重 0.75)+ 亮度均值差(权重 0.25)。
///
/// **换算法时必须改这个字符串**,否则无法分辨磁盘上的旧文件是用哪套口径算的。
pub const DEFAULT_METRIC: &str = "hist_l1_0.75+luma_0.25";

/// 一个采样时刻的变化率。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePoint {
    /// 采样时刻(秒),从 0 起。
    pub t: f64,
    /// 该帧与上一帧的差异,`0` = 完全相同,`1` = 完全不同。首点恒为 0。
    pub score: f64,
}

/// 差帧图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeCurve {
    pub source: RelPath,
    /// 视频时长(秒)。冗余存一份,让视图不必再读 `video.meta.json` 就能定横轴范围。
    pub duration: f64,
    pub fps_sampled: f64,
    pub width: u32,
    pub height: u32,
    pub metric: String,
    pub generated_at: DateTime<FixedOffset>,
    /// 按时间升序。**保留未平滑的原始值**(平滑只发生在视图渲染时)。
    pub changes: Vec<ChangePoint>,
}

impl ChangeCurve {
    /// 变化率最大值,用于视图纵轴归一化。
    pub fn max_score(&self) -> f64 {
        self.changes
            .iter()
            .map(|c| c.score)
            .fold(0.0_f64, f64::max)
    }

    /// 取 `[start, end]` 区间内的点(闭区间)。视图按可视窗口取数据时用。
    pub fn slice(&self, start: f64, end: f64) -> impl Iterator<Item = &ChangePoint> {
        self.changes
            .iter()
            .filter(move |c| c.t >= start && c.t <= end)
    }

    /// 采样间隔(秒)。`fps_sampled` 为 0 时返回 `None`(非法数据,不该出现)。
    pub fn step(&self) -> Option<f64> {
        (self.fps_sampled > 0.0).then(|| 1.0 / self.fps_sampled)
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

    fn curve() -> ChangeCurve {
        ChangeCurve {
            source: RelPath::new("source/video.mp4").unwrap(),
            duration: 4.0,
            fps_sampled: 1.0,
            width: 480,
            height: 270,
            metric: DEFAULT_METRIC.to_string(),
            generated_at: crate::now(),
            changes: vec![
                ChangePoint { t: 0.0, score: 0.0 },
                ChangePoint { t: 1.0, score: 0.5 },
                ChangePoint { t: 2.0, score: 0.25 },
                ChangePoint { t: 3.0, score: 0.1 },
                ChangePoint { t: 4.0, score: 0.0 },
            ],
        }
    }

    #[test]
    fn max_and_slice() {
        let c = curve();
        assert_eq!(c.max_score(), 0.5);
        assert_eq!(c.slice(1.0, 3.0).count(), 3);
        assert_eq!(c.slice(0.0, 0.0).count(), 1);
        assert_eq!(c.slice(10.0, 20.0).count(), 0);
        assert_eq!(c.step(), Some(1.0));
    }

    #[test]
    fn round_trip() {
        let c = curve();
        let text = crate::json::to_pretty_string(&c).unwrap();
        let back: ChangeCurve = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
    }
}
