//! 差帧图:按固定帧率抽帧,逐帧打分,产出 [`ChangeCurve`]。
//!
//! 设计见 `docs/DESIGN.md` §5.1 ①。**内存是有意控制的**:985 秒的视频按 1fps 采样,
//! 每帧 480×270×3 ≈ 380 KB,全留下就是 380 MB。差帧只需要相邻两帧,
//! 所以这里流式读、算完特征就丢像素,峰值内存 = 两个特征向量。

use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use vidbrief_project::{ChangeCurve, ChangePoint, RelPath, VideoMeta};

use crate::feature::{self, FrameFeature};
use crate::ffmpeg::{self, arg};

/// 抽帧参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurveOptions {
    /// 采样帧率(每秒取几个点)。
    pub fps: f64,
    /// 参与打分的缩小后宽度。
    pub width: u32,
    /// 参与打分的缩小后高度。
    pub height: u32,
}

impl Default for CurveOptions {
    fn default() -> Self {
        Self {
            fps: vidbrief_project::curve::DEFAULT_FPS_SAMPLED,
            width: vidbrief_project::curve::DEFAULT_WIDTH,
            height: 270,
        }
    }
}

impl CurveOptions {
    /// 按源视频的宽高比推出缩小后的尺寸。
    ///
    /// **为什么把高也显式算出来、而不是交给 ffmpeg 的 `scale=480:-2`**:
    /// 那样我们得猜 ffmpeg 会取哪个高度,猜错一个像素整条 rawvideo 流就全错位了
    /// (后面每一帧都错,而且不会报错,只会得到一堆垃圾分数)。
    /// 显式给两个数就没得猜 —— 代价只是宽高比可能有 0.4% 的偏差,对差帧毫无影响。
    pub fn for_video(meta: &VideoMeta) -> Self {
        let d = Self::default();
        match (meta.width, meta.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => Self {
                height: even_height(d.width, w, h),
                ..d
            },
            _ => d,
        }
    }

    /// 每帧的字节数(rgb24)。
    pub fn frame_bytes(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 3
    }
}

/// 按宽高比缩到 `out_w`,并**向上取到偶数**(H.264 系要求偶数,ffmpeg 的 `-2` 也是这个意思)。
fn even_height(out_w: u32, src_w: u32, src_h: u32) -> u32 {
    let h = (out_w as f64 * src_h as f64 / src_w as f64).round().max(2.0) as u32;
    h + (h % 2)
}

/// 抽帧 + 打分,产出差帧图。
///
/// `rel` 是视频在项目内的位置(写进结果里)。
pub fn build_curve(
    video: &Path,
    rel: RelPath,
    duration: f64,
    opts: CurveOptions,
) -> Result<ChangeCurve> {
    if opts.fps <= 0.0 || !opts.fps.is_finite() {
        bail!("采样帧率不合法:{}", opts.fps);
    }
    let frame_bytes = opts.frame_bytes();

    let args = vec![
        arg("-v"),
        arg("error"),
        arg("-i"),
        video.as_os_str().to_os_string(),
        arg("-vf"),
        arg(format!(
            "fps={},scale={}:{}",
            opts.fps, opts.width, opts.height
        )),
        arg("-an"), // 不要音频
        arg("-sn"), // 不要字幕
        arg("-f"),
        arg("rawvideo"),
        arg("-pix_fmt"),
        arg("rgb24"),
        arg("-"),
    ];

    let mut child = ffmpeg::spawn_ffmpeg(&args)?;
    let mut stdout = BufReader::new(child.stdout.take().context("ffmpeg 的 stdout 没拿到")?);

    // stderr 交给另一个线程读干,否则管道写满会把 ffmpeg 卡死(典型死锁)
    let stderr = child.stderr.take().context("ffmpeg 的 stderr 没拿到")?;
    let drain = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut text);
        text
    });

    let mut changes: Vec<ChangePoint> = Vec::new();
    let mut prev: Option<FrameFeature> = None;
    let mut buf = vec![0u8; frame_bytes];
    let mut index: u64 = 0;

    loop {
        match fill(&mut stdout, &mut buf) {
            Ok(true) => {}
            // 正常的流末尾,或最后一帧不完整 —— 两种都当结束
            Ok(false) => break,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e).with_context(|| format!("读取 {} 的抽帧输出失败", video.display()));
            }
        }

        let current = FrameFeature::from_rgb24(&buf, opts.width, opts.height);
        let score = match &prev {
            // 第一帧没有上一帧可比,score 恒为 0(约定见 docs/schema/README.md §1.7)
            None => 0.0,
            Some(p) => feature::score(p, &current),
        };
        changes.push(ChangePoint {
            t: index as f64 / opts.fps,
            score,
        });
        prev = Some(current);
        index += 1;
    }

    let status = child.wait().context("等待 ffmpeg 退出失败")?;
    let stderr_text = drain.join().unwrap_or_default();

    if changes.is_empty() {
        bail!(
            "ffmpeg 没有产出任何帧({}):\n{}",
            video.display(),
            stderr_text.trim()
        );
    }
    if !status.success() {
        // 已经拿到帧就不当致命错误(比如视频尾部有坏帧),但要让人知道
        eprintln!(
            "警告:ffmpeg 以 {} 退出,已取到 {} 帧;stderr 末尾:\n{}",
            status,
            changes.len(),
            stderr_text.trim()
        );
    }

    Ok(ChangeCurve {
        source: rel,
        duration,
        fps_sampled: opts.fps,
        width: opts.width,
        height: opts.height,
        metric: vidbrief_project::curve::DEFAULT_METRIC.to_string(),
        generated_at: vidbrief_project::now(),
        changes,
    })
}

/// 尽量填满缓冲区。返回 `false` = 流提前结束(残缺帧按丢弃处理)。
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_height_rounds_up_to_even() {
        // 640x360 → 480x270
        assert_eq!(even_height(480, 640, 360), 270);
        // 1920x1080 → 480x270
        assert_eq!(even_height(480, 1920, 1080), 270);
        // 会算出奇数时向上取偶
        assert_eq!(even_height(480, 1000, 333) % 2, 0);
        // 极端比例也不为 0
        assert!(even_height(480, 10000, 1) >= 2);
    }

    #[test]
    fn options_follow_the_source_aspect() {
        let mut meta = sample_meta();
        meta.width = Some(1280);
        meta.height = Some(720);
        let o = CurveOptions::for_video(&meta);
        assert_eq!((o.width, o.height), (480, 270));
        assert_eq!(o.frame_bytes(), 480 * 270 * 3);

        // 探测不到尺寸时退回默认
        meta.width = None;
        meta.height = None;
        assert_eq!(CurveOptions::for_video(&meta).height, 270);
    }

    #[test]
    fn rejects_bad_fps() {
        let err = build_curve(
            Path::new("whatever.mp4"),
            RelPath::new("source/video.mp4").unwrap(),
            1.0,
            CurveOptions { fps: 0.0, ..Default::default() },
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("采样帧率"), "{err}");
    }

    fn sample_meta() -> VideoMeta {
        VideoMeta {
            path: RelPath::new("source/video.mp4").unwrap(),
            duration: 60.0,
            width: Some(640),
            height: Some(360),
            fps: Some(30.0),
            codec: Some("h264".into()),
            size_bytes: 1,
            sha256: "0".repeat(64),
            probed_at: vidbrief_project::now(),
            prober: "test".into(),
        }
    }
}
