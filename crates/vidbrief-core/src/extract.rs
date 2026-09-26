//! 精确抽帧:按时刻取**一帧**存成 JPEG。
//!
//! 用于步 1 预筛选截图,以及步 2/3 的补图(`extract_shot` / `request_shot`)。
//! 设计见 `docs/DESIGN.md` §5.1 ③:宽度 ≥1280px、JPEG q85。

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::ffmpeg::{self, arg};

/// 截图质量参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShotQuality {
    /// 目标宽度上限。**不放大** —— 源视频比它窄时保持原宽。
    pub max_width: u32,
    /// JPEG 质量(ffmpeg 的 `-q:v`,2 最好、31 最差)。
    pub jpeg_q: u8,
}

impl Default for ShotQuality {
    fn default() -> Self {
        // DESIGN 说的是 q85;ffmpeg 用 2..31 的反向刻度,q≈85 大致对应 -q:v 3
        Self {
            max_width: 1280,
            jpeg_q: 3,
        }
    }
}

/// 取 `video` 在时刻 `t` 的那一帧,返回 RGB24 原始像素。
///
/// 与 [`crate::curve::build_curve`] 的抽帧**必须落在同一帧上** ——
/// 两边都按"第一个 `pts ≥ t` 的帧"取值,而 `build_curve` 的采样表达式保证
/// 每帧的时间戳正好是整数秒。这条一致性由
/// `tests/pipeline_consistency.rs` 逐字节钉住。
pub fn frame_at(video: &Path, t: f64, width: u32, height: u32) -> Result<Vec<u8>> {
    if !t.is_finite() || t < 0.0 {
        bail!("抽帧时刻不合法:{t}");
    }
    if !video.is_file() {
        bail!("源视频不存在:{}", video.display());
    }

    let args = vec![
        arg("-v"),
        arg("error"),
        // `-ss` 放在 `-i` 之前:先快速定位到关键帧再精确解码。ffmpeg 默认会丢帧直到目标时刻,
        // 所以拿到的是"第一个 pts ≥ t"的帧 —— 与 build_curve 的语义一致
        arg("-ss"),
        arg(format!("{t:.6}")),
        arg("-i"),
        video.as_os_str().to_os_string(),
        arg("-frames:v"),
        arg("1"),
        arg("-vf"),
        arg(format!("scale={width}:{height}")),
        arg("-f"),
        arg("rawvideo"),
        arg("-pix_fmt"),
        arg("rgb24"),
        arg("-"),
    ];

    let mut out = ffmpeg::run_ffmpeg_capture(&args)
        .with_context(|| format!("读取帧失败:t={t:.3}s {}", video.display()))?;

    let need = (width as usize) * (height as usize) * 3;
    if out.len() < need {
        bail!(
            "只读到 {} 字节,不足一帧({need} 字节):t={t:.3}s 可能超出视频末尾",
            out.len()
        );
    }
    out.truncate(need);
    Ok(out)
}

/// **这一帧是不是纯色**(切黑 / 片尾淡出 / 纯色占位)。
///
/// 抽帧之前先花一次小解码看一眼。用途:纯色帧**照样截下来,但会被预标成弃用**
/// (见 `pipeline::extract_shots`)—— 省的是 agent 的注意力,不是材料:
/// 实测 16 分钟的视频末尾切黑,算法当关键帧截了一张,agent 得为它花一次调用然后写
/// 「画面无法确认」(技术债表里那条"纯色帧没有检测",第三次撞见同一现象)。
///
/// **为什么不是"干脆不截"**(实测改的口径):`fixtures/test_sample.mp4` 的三个切换点
/// 就是纯色帧 —— 不截的话"切换点必须命中"这条 M1 验收标准当场作废。
/// **一次画面切换是事件,黑屏也是那个事件的证据**,丢掉图等于丢掉事件。
///
/// 判据是纯函数([`FrameFeature::is_flat`]),这里只负责把像素拿回来 ——
/// 采样宽度比正式截图小得多(统计量不需要分辨率,而解码是按秒定位的,不按像素数)。
///
/// **只给步 1 的自动截图用**。`extract_shot`(点名要某一刻)不拦:那是明确点的菜,
/// 给出来由 agent 自己判 —— 悄悄地不给,比给一张黑图更坏。
pub fn flatness_at(video: &Path, t: f64) -> Result<Flatness> {
    const SAMPLE_WIDTH: u32 = 320;
    // 16:9 只是给 ffmpeg 一个尺寸;统计量对高度不敏感(逐像素扫,按实际像素数归一)
    const SAMPLE_HEIGHT: u32 = 180;

    let pixels = frame_at(video, t, SAMPLE_WIDTH, SAMPLE_HEIGHT)?;
    let feature = crate::feature::FrameFeature::from_rgb24(&pixels, SAMPLE_WIDTH, SAMPLE_HEIGHT);
    Ok(Flatness {
        luma_stddev: feature.luma_stddev(),
        dominant_share: feature.dominant_share(),
        flat: feature.is_flat(),
    })
}

/// 纯色体检的结果。数字都带出来,是为了让"为什么没截这一张"能写进日志。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Flatness {
    /// 亮度标准差(0–255 刻度)。
    pub luma_stddev: f64,
    /// 最挤的那个通道 bin 的占比(0–1)。
    pub dominant_share: f64,
    /// 是不是纯色(判据见 [`FrameFeature::is_flat`])。
    pub flat: bool,
}

/// 抽 `video` 在时刻 `t` 的那一帧,写到 `out`(JPEG)。
pub fn extract_frame(video: &Path, t: f64, out: &Path, quality: ShotQuality) -> Result<()> {
    if !t.is_finite() || t < 0.0 {
        bail!("抽帧时刻不合法:{t}");
    }
    if !video.is_file() {
        bail!("源视频不存在:{}", video.display());
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败:{}", parent.display()))?;
    }

    // `scale=min(宽, iw)` 用表达式避免把窄视频放大
    let vf = format!("scale='min({},iw)':-2", quality.max_width);

    let args = vec![
        arg("-v"),
        arg("error"),
        // `-ss` 放在 `-i` 之前:先快速定位再精确解码,比解码整段快得多
        arg("-ss"),
        arg(format!("{t:.6}")),
        arg("-i"),
        video.as_os_str().to_os_string(),
        arg("-frames:v"),
        arg("1"),
        arg("-vf"),
        arg(vf),
        arg("-q:v"),
        arg(quality.jpeg_q.to_string()),
        arg("-y"),
        out.as_os_str().to_os_string(),
    ];

    ffmpeg::run_ffmpeg(&args)
        .with_context(|| format!("抽帧失败:t={t:.1}s → {}", out.display()))?;

    if !out.is_file() {
        bail!(
            "ffmpeg 报告成功但没有产出文件(t={t:.1}s):{}",
            out.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_quality_matches_the_design() {
        let q = ShotQuality::default();
        assert_eq!(q.max_width, 1280, "DESIGN §5.1 ③ 要求宽度 ≥1280px");
        assert!(q.jpeg_q <= 5, "JPEG 质量应当偏高,实际 q={}", q.jpeg_q);
    }

    #[test]
    fn rejects_bad_time() {
        let dir = std::env::temp_dir();
        let out = dir.join("vidbrief-never.jpg");
        assert!(extract_frame(Path::new("x.mp4"), -1.0, &out, ShotQuality::default()).is_err());
        assert!(extract_frame(Path::new("x.mp4"), f64::NAN, &out, ShotQuality::default()).is_err());
    }

    #[test]
    fn rejects_missing_video_before_calling_ffmpeg() {
        let dir = std::env::temp_dir();
        let err = extract_frame(
            Path::new("no/such/video.mp4"),
            1.0,
            &dir.join("vidbrief-never.jpg"),
            ShotQuality::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("不存在"), "{err}");
    }
}
