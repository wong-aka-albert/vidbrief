//! 源视频探测:`ffprobe` → [`VideoMeta`]。
//!
//! **从 `vidbrief-project::meta` 搬过来的。** 那里原本自带 ffprobe 调用,但 `project`
//! 是契约层(叶子),不该有进程能力;`core` 才是"跟 ffmpeg 打交道"的地方。
//! 类型 [`VideoMeta`] 仍然住在 `project`(那是格式契约),这里只负责**探测**。

use std::path::Path;

use anyhow::{Context, Result, bail};
use vidbrief_project::{RelPath, VideoMeta};

use crate::ffmpeg;

/// 探测 `video`(绝对路径),按 `rel` 记录它在项目内的位置。
pub fn probe(video: &Path, rel: RelPath) -> Result<VideoMeta> {
    if !video.is_file() {
        bail!("视频文件不存在:{}", video.display());
    }

    let info = ffmpeg::ffprobe_json(video)?;
    let stream = first_video_stream(&info)?;

    let duration = format_duration(&info)
        .or_else(|| json_f64(stream, "duration"))
        .with_context(|| format!("ffprobe 未能给出时长,无法建项目:{}", video.display()))?;
    if !duration.is_finite() || duration <= 0.0 {
        bail!("ffprobe 报出的时长不合法({duration}):{}", video.display());
    }

    let fps = json_str(stream, "avg_frame_rate")
        .and_then(parse_rational)
        .or_else(|| json_str(stream, "r_frame_rate").and_then(parse_rational));

    let size_bytes = std::fs::metadata(video)
        .with_context(|| format!("读取文件属性失败:{}", video.display()))?
        .len();

    Ok(VideoMeta {
        path: rel,
        duration,
        width: json_u32(stream, "width"),
        height: json_u32(stream, "height"),
        fps,
        codec: json_str(stream, "codec_name").map(str::to_string),
        size_bytes,
        sha256: vidbrief_project::sha256_of(video)?,
        probed_at: vidbrief_project::now(),
        prober: ffmpeg::ffprobe_version(),
    })
}

/// 第一路视频流。没有视频流 = 这不是一个能处理的文件。
fn first_video_stream(info: &serde_json::Value) -> Result<&serde_json::Value> {
    info.get("streams")
        .and_then(|s| s.as_array())
        .and_then(|streams| {
            streams
                .iter()
                .find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("video"))
        })
        .context("文件里没有视频流")
}

/// 有没有音频流。没有音频的视频照样能出差帧图,只是转录那一步会空着。
pub fn has_audio_stream(info: &serde_json::Value) -> bool {
    info.get("streams")
        .and_then(|s| s.as_array())
        .is_some_and(|streams| {
            streams
                .iter()
                .any(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some("audio"))
        })
}

fn format_duration(info: &serde_json::Value) -> Option<f64> {
    info.get("format")?.get("duration")?.as_str()?.parse().ok()
}

fn json_str<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key)?.as_str()
}

fn json_f64(v: &serde_json::Value, key: &str) -> Option<f64> {
    let raw = v.get(key)?;
    match raw {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn json_u32(v: &serde_json::Value, key: &str) -> Option<u32> {
    let n = v.get(key)?.as_u64()?;
    u32::try_from(n).ok()
}

/// 解析 ffprobe 的有理数帧率,如 `30/1`、`30000/1001`。`0/0` 与畸形输入返回 `None`。
fn parse_rational(raw: &str) -> Option<f64> {
    let (num, den) = raw.split_once('/')?;
    let num: f64 = num.trim().parse().ok()?;
    let den: f64 = den.trim().parse().ok()?;
    if den == 0.0 || num <= 0.0 {
        return None;
    }
    let value = num / den;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_parsing() {
        assert_eq!(parse_rational("30/1"), Some(30.0));
        assert_eq!(parse_rational("30000/1001"), Some(30000.0 / 1001.0));
        assert_eq!(parse_rational("0/0"), None);
        assert_eq!(parse_rational("30"), None);
        assert_eq!(parse_rational("30/0"), None);
        assert_eq!(parse_rational("-1/2"), None);
        assert_eq!(parse_rational("abc/1"), None);
    }

    #[test]
    fn probe_rejects_missing_file() {
        let err = probe(
            Path::new("no/such/video.mp4"),
            RelPath::new("source/video.mp4").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("不存在"), "{err}");
    }
}
