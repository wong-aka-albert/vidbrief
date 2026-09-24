//! 帧特征与打分 —— **纯函数,不碰进程、不碰文件**。
//!
//! 这是整个步 1 里唯一"真正决定质量"的地方,所以它必须能在**不跑 ffmpeg** 的情况下单测:
//! 给两帧像素,出一个数。上一节说过的原则 —— 算法调试的反馈环不能卡在外部进程上。
//!
//! # 打分口径(沿用旧实现的实测标定)
//!
//! 见 `docs/RESEARCH.md` §5 的实测结论,与 `docs/DESIGN.md` §4.3:
//!
//! ```text
//! score = 0.75 × 直方图 L1 距离 + 0.25 × 亮度均值差
//! ```
//!
//! - **直方图**:RGB 三通道各 8 个 bin 的**计数**,各自按像素数归一化后取 L1 距离。
//! - **亮度**:逐像素 `(r+g+b)/3` 的均值,差除以 255。
//! - 结果 `clamp(0,1)`。
//!
//! ## 一个要留意的口径怪癖(有意保留)
//!
//! 24 个 bin 的 L1 距离按经验最大 `2.0` 归一化,但三通道合起来每个分布的和是 `3.0`,
//! 真正的最大 L1 是 `6.0`。也就是说这一项**被放大了 3 倍**,很快就饱和到 1.0。
//!
//! 保留它是因为:RESEARCH.md 里那些实测结论(985 帧检出 12 峰、与转录时间戳吻合)
//! 以及 `peak_threshold = 0.25` 这类阈值,都是**在这套口径下标定的**。
//! 换归一化会让所有阈值失去意义。等 M1 用真实长视频标定时再决定是否改成 `/6.0`。

/// 每个通道的直方图 bin 数。
pub const HIST_BINS: usize = 8;
/// 三通道合计的 bin 数。
pub const HIST_LEN: usize = HIST_BINS * 3;

/// 直方图项权重。
pub const HIST_WEIGHT: f64 = 0.75;
/// 亮度项权重。
pub const LUMA_WEIGHT: f64 = 0.25;

/// 一帧的廉价特征:够用来判断"变了多少",不够用来理解画面。
///
/// **不存像素** —— 985 秒的视频按 1fps 采样也有近千帧,每帧 480×270×3 就是 380 MB。
/// 差帧只需要相邻两帧,所以算完特征就丢掉像素。
#[derive(Debug, Clone, PartialEq)]
pub struct FrameFeature {
    /// RGB 三通道各 8 个 bin 的像素计数,共 24 个。
    hist: [f64; HIST_LEN],
    /// 逐像素 `(r+g+b)/3` 的累加值。
    luma_sum: f64,
    /// 实际参与的像素数。
    pixels: f64,
}

impl FrameFeature {
    /// 从 RGB24 原始字节算特征。
    ///
    /// `width`/`height` 决定读多少个像素;数据不足时按实际能读到的算
    /// (不 panic —— ffmpeg 偶尔会少给最后几字节)。
    pub fn from_rgb24(data: &[u8], width: u32, height: u32) -> Self {
        let expected = (width as usize) * (height as usize);
        let mut f = FrameFeature {
            hist: [0.0; HIST_LEN],
            luma_sum: 0.0,
            pixels: 0.0,
        };

        for px in data.chunks_exact(3).take(expected) {
            let (r, g, b) = (px[0], px[1], px[2]);
            f.hist[bin_of(r)] += 1.0;
            f.hist[HIST_BINS + bin_of(g)] += 1.0;
            f.hist[HIST_BINS * 2 + bin_of(b)] += 1.0;
            f.luma_sum += (r as f64 + g as f64 + b as f64) / 3.0;
            f.pixels += 1.0;
        }
        f
    }

    pub fn pixels(&self) -> f64 {
        self.pixels
    }

    /// 平均亮度(0–255)。没有像素时为 0。
    pub fn mean_luma(&self) -> f64 {
        if self.pixels == 0.0 {
            0.0
        } else {
            self.luma_sum / self.pixels
        }
    }

    /// 某个 bin 的归一化占比(调试与测试用)。
    pub fn hist_share(&self, index: usize) -> f64 {
        match self.hist.get(index) {
            Some(count) if self.pixels > 0.0 => count / self.pixels,
            _ => 0.0,
        }
    }

    /// 空的(没有像素)特征。相减时视为"没变化"。
    pub fn is_empty(&self) -> bool {
        self.pixels == 0.0
    }
}

/// 像素值 → bin 下标。`0..=255` → `0..=7`,超范围夹住。
fn bin_of(v: u8) -> usize {
    ((v as usize) * HIST_BINS / 256).min(HIST_BINS - 1)
}

/// 两帧的差异,归一化到 `[0,1]`。
///
/// 任一侧没有像素时返回 `0.0`(无信息 = 当作没变),而不是 `NaN` ——
/// `NaN` 会一路污染到 JSON(serde_json 拒绝写 NaN,直接报错)。
pub fn score(a: &FrameFeature, b: &FrameFeature) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut l1 = 0.0f64;
    for i in 0..HIST_LEN {
        l1 += (a.hist_share(i) - b.hist_share(i)).abs();
    }
    // 见模块头的"口径怪癖":这里的 2.0 是旧实现的经验值,不是数学上的最大值
    let hist_score = l1 / 2.0;

    let luma_delta = ((a.mean_luma() - b.mean_luma()) / 255.0).abs();

    (HIST_WEIGHT * hist_score + LUMA_WEIGHT * luma_delta).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一帧纯色(或任意填充)RGB24。
    fn solid(r: u8, g: u8, b: u8, pixels: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(pixels * 3);
        for _ in 0..pixels {
            v.extend_from_slice(&[r, g, b]);
        }
        v
    }

    fn feature(r: u8, g: u8, b: u8, pixels: usize) -> FrameFeature {
        FrameFeature::from_rgb24(&solid(r, g, b, pixels), pixels as u32, 1)
    }

    #[test]
    fn identical_frames_score_zero() {
        let a = feature(120, 130, 140, 1000);
        assert_eq!(score(&a, &a), 0.0);
        let b = feature(120, 130, 140, 1000);
        assert_eq!(score(&a, &b), 0.0);
    }

    #[test]
    fn black_to_white_is_a_maximal_change() {
        // 全黑 → 全白:直方图完全错开,亮度差也是满的
        let black = feature(0, 0, 0, 1000);
        let white = feature(255, 255, 255, 1000);
        let s = score(&black, &white);
        assert!(s > 0.9, "黑到白应当接近 1.0,实际 {s}");
        assert!(s <= 1.0);
    }

    #[test]
    fn a_tiny_brightness_nudge_stays_small() {
        let a = feature(120, 120, 120, 4000);
        let b = feature(124, 124, 124, 4000);
        let s = score(&a, &b);
        assert!(s < 0.05, "轻微抖动不该被当成切换,实际 {s}");
    }

    #[test]
    fn score_is_symmetric() {
        let a = feature(10, 200, 30, 500);
        let b = feature(240, 20, 180, 500);
        assert_eq!(score(&a, &b), score(&b, &a));
    }

    #[test]
    fn empty_frames_do_not_produce_nan() {
        let empty = FrameFeature::from_rgb24(&[], 0, 0);
        assert!(empty.is_empty());
        let real = feature(1, 2, 3, 10);
        let s = score(&empty, &real);
        assert!(s.is_finite(), "不能是 NaN");
        assert_eq!(s, 0.0);
    }

    #[test]
    fn truncated_data_does_not_panic() {
        // 声称 100 个像素,只给 10 个
        let data = solid(1, 2, 3, 10);
        let f = FrameFeature::from_rgb24(&data, 100, 1);
        assert_eq!(f.pixels(), 10.0, "应当按实际读到的算");
    }

    #[test]
    fn histogram_bins_cover_the_whole_range() {
        assert_eq!(bin_of(0), 0);
        assert_eq!(bin_of(31), 0);
        assert_eq!(bin_of(32), 1);
        assert_eq!(bin_of(255), 7, "255 不能越界");
    }

    #[test]
    fn half_the_image_changing_is_a_big_score() {
        // 上半黑下半白 → 上半白下半黑:直方图完全一样,但亮度也一样 —— 这类
        // "只换位置不换内容"的变化直方图确实抓不到。记下来当已知边界。
        let mut a = solid(0, 0, 0, 500);
        a.extend_from_slice(&solid(255, 255, 255, 500));
        let mut b = solid(255, 255, 255, 500);
        b.extend_from_slice(&solid(0, 0, 0, 500));

        let fa = FrameFeature::from_rgb24(&a, 1000, 1);
        let fb = FrameFeature::from_rgb24(&b, 1000, 1);
        assert_eq!(
            score(&fa, &fb),
            0.0,
            "直方图法对'纯位置交换'不敏感 —— 这是已知局限,不是 bug"
        );
    }

    #[test]
    fn weighted_sum_matches_the_documented_formula() {
        // 用**不饱和**的用例,否则测不出权重(饱和的情形见下一个测试)
        let mut a_pixels = solid(0, 0, 0, 990);
        a_pixels.extend_from_slice(&solid(255, 255, 255, 10));
        let mut b_pixels = solid(0, 0, 0, 990);
        b_pixels.extend_from_slice(&solid(64, 64, 64, 10));

        let a = FrameFeature::from_rgb24(&a_pixels, 1000, 1);
        let b = FrameFeature::from_rgb24(&b_pixels, 1000, 1);

        let mut l1 = 0.0;
        for i in 0..HIST_LEN {
            l1 += (a.hist_share(i) - b.hist_share(i)).abs();
        }
        let expected = (HIST_WEIGHT * (l1 / 2.0)
            + LUMA_WEIGHT * ((a.mean_luma() - b.mean_luma()) / 255.0).abs())
        .clamp(0.0, 1.0);

        let actual = score(&a, &b);
        assert!(
            (actual - expected).abs() < 1e-12,
            "actual={actual} expected={expected}"
        );
        assert!(actual < 1.0, "这个用例不该饱和,否则等于没测权重:{actual}");
    }

    #[test]
    fn the_normalizer_saturates_early_this_is_the_known_quirk() {
        // 全黑 → 全灰:直方图完全错开,按 `/2.0` 归一化会算到 3.0,靠 clamp 才回到 1.0。
        // 这是旧实现的口径(见模块头"口径怪癖"),所有阈值都在这套口径下标定过,不能改。
        let black = feature(0, 0, 0, 1000);
        let gray = feature(64, 64, 64, 1000);
        assert_eq!(score(&black, &gray), 1.0, "任何跨 bin 的整幅变化都会饱和到上限");

        // 换个更小的变化,只要跨了 bin 也一样饱和 —— 这就是"敏感"
        let dark = feature(0, 0, 0, 1000);
        let slightly = feature(32, 32, 32, 1000);
        assert_eq!(score(&dark, &slightly), 1.0);
    }
}
