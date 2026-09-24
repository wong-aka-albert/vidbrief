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
