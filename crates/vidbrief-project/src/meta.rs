//! `source/video.meta.json` —— 源视频技术元数据。
//!
//! 格式见 `docs/schema/video_meta.schema.json`。
//!
//! **这里只有类型与读写,没有探测。** 探测要拉起 `ffprobe`,那是进程能力,
//! 属于 `vidbrief-core`(见 [`vidbrief_core::probe`])—— `project` 是契约层(叶子),
//! 它一旦开始依赖外部进程,依赖方向就烂了。
//!
//! M0 时探测曾经长在这个文件里(图省事),M1 搬走了。

use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::relpath::RelPath;

/// 源视频的探测结论。
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
    pub fn load(path: &Path) -> Result<Self> {
        crate::json::read(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::json::write_atomic(path, self)
    }

    /// 画面尺寸(两个都有才算数)。
    pub fn resolution(&self) -> Option<(u32, u32)> {
        match (self.width, self.height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> VideoMeta {
        VideoMeta {
            path: RelPath::new("source/video.mp4").unwrap(),
            duration: 60.0,
            width: Some(640),
            height: Some(360),
            fps: Some(30.0),
            codec: Some("h264".into()),
            size_bytes: 1024,
            sha256: "a".repeat(64),
            probed_at: crate::now(),
            prober: "ffprobe test".into(),
        }
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("vidbrief-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video.meta.json");

        let m = meta();
        m.save(&path).unwrap();
        assert_eq!(VideoMeta::load(&path).unwrap(), m);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolution_needs_both_sides() {
        assert_eq!(meta().resolution(), Some((640, 360)));
        let mut m = meta();
        m.height = None;
        assert_eq!(m.resolution(), None);
    }
}
