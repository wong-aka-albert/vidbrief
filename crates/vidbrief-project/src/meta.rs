//! `source/video.meta.json` —— 源视频技术元数据。
//!
//! 格式见 `docs/schema/video_meta.schema.json`。建项目时用 `ffprobe` 探一次。
//!
//! 注意:这里**只探测,不转码**。抽帧、抽音等真正的 ffmpeg 调用属于 `vidbrief-core`
//! (见 `docs/DESIGN.md` §10.3);M1 若发现进程调用逻辑重复,再把公共部分提到 core。

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::relpath::RelPath;

/// 打分口径之外的探测结论。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoMeta {
    /// 视频在项目内的位置,应与 `project.json.source.path` 一致。
    pub path: RelPath,
    /// 时长(秒)。探不到就是硬错误 —— 后面所有算法都要用它定时间轴。
    pub duration: f64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// 平均帧率,允许小数(如 29.97)。ffprobe 报不出时为 `None`。
    pub fps: Option<f64>,
    pub codec: Option<String>,
    pub size_bytes: u64,
    /// 文件内容哈希。用于判断源视频是否被换过(路径相同不代表内容相同)。
    pub sha256: String,
    pub probed_at: DateTime<FixedOffset>,
    /// 探测工具与版本,如 `ffprobe N-125875-g5d4d3bdc61`。
    pub prober: String,
}

impl VideoMeta {
    /// 用 `ffprobe` 探测 `video`(绝对路径),按 `rel` 记录它在项目内的位置。
    pub fn probe(video: &Path, rel: RelPath) -> Result<Self> {
        if !video.is_file() {
            bail!("视频文件不存在:{}", video.display());
        }

        let info = ffprobe_json(video)?;
        let stream = first_video_stream(&info)?;

        let duration = format_duration(&info)
            .or_else(|| json_f64(stream, "duration"))
            .with_context(|| {
                format!(
                    "ffprobe 未能给出时长,无法建项目:{}",
                    video.display()
                )
            })?;
        if !duration.is_finite() || duration <= 0.0 {
            bail!("ffprobe 报出的时长不合法({duration}):{}", video.display());
        }

        let fps = json_str(stream, "avg_frame_rate")
            .and_then(parse_rational)
            .or_else(|| json_str(stream, "r_frame_rate").and_then(parse_rational));

        let size_bytes = std::fs::metadata(video)
            .with_context(|| format!("读取文件属性失败:{}", video.display()))?
            .len();

        Ok(Self {
            path: rel,
            duration,
            width: json_u32(stream, "width"),
            height: json_u32(stream, "height"),
            fps,
            codec: json_str(stream, "codec_name").map(str::to_string),
            size_bytes,
            sha256: sha256_file(video)?,
            probed_at: crate::now(),
            prober: ffprobe_version(),
        })
    }

    pub fn load(path: &Path) -> Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }
}

// ── ffprobe 交互 ─────────────────────────────────────────────────────

fn ffprobe_json(video: &Path) -> Result<serde_json::Value> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(video)
        .output()
        .context("无法执行 ffprobe —— 请确认它已安装并在 PATH 中")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ffprobe 探测失败({}):{}", output.status, stderr.trim());
    }
    serde_json::from_slice(&output.stdout).context("ffprobe 输出不是合法 JSON")
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

/// 首行形如 `ffprobe version N-125875-g5d4d3bdc61 Copyright (c) ...`,
/// 取成 `ffprobe N-125875-g5d4d3bdc61`;认不出来就原样返回首行。
fn ffprobe_version() -> String {
    let first_line = Command::new("ffprobe")
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(str::to_string));

    match first_line {
        Some(line) => {
            let mut words = line.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some(tool), Some(kw), Some(ver)) if kw == "version" => format!("{tool} {ver}"),
                _ => line,
            }
        }
        None => "ffprobe (版本未知)".to_string(),
    }
}

/// 分块计算 SHA-256。用固定缓冲流式读,避免把整个视频读进内存。
fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("打开失败:{}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("读取失败:{}", path.display()))?;
        if n == 0 {
            break;
        }
        // 字节切片(不是字符串切片),n 由 read 保证不越界
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
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
    fn sha256_matches_known_vector() {
        let dir = std::env::temp_dir().join(format!("vidbrief-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        // FIPS 180-4 中 "abc" 的标准测试向量
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn probe_rejects_missing_file() {
        let err = VideoMeta::probe(
            Path::new("no/such/video.mp4"),
            RelPath::new("source/video.mp4").unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("不存在"), "{err}");
    }
}
