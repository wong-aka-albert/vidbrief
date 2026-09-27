//! 项目目录与数据结构(材料即接口)。
//!
//! 设计见 `docs/DESIGN.md` §4;格式规范见 `docs/schema/README.md`。
//!
//! 本 crate 是"项目目录里的每个 JSON 文件长什么样"的代码侧权威:
//!
//! | 模块 | 对应文件 |
//! |------|---------|
//! | [`project`] | `project.json` |
//! | [`lock`] | `project.lock` |
//! | [`source`] | 把源视频搬进 `source/`;文件身份(卷/文件 ID/链接数) |
//! | [`health`] | 项目体检:源视频还在不在、链接还可不可用 |
//! | [`meta`] | `source/video.meta.json` |
//! | [`curve`] | `01_preprocess/change_curve.json` |
//! | [`regions`] | `01_preprocess/dense_regions.json` |
//! | [`transcript`] | `01_preprocess/transcript.json`、`02_calibrate/transcript.fixed.json` |
//! | [`shots`] | `01_preprocess/shots.json` |
//! | [`supplement`] | `02_calibrate/supplement_requests.json` |
//!
//! 三个贯穿性约定(详见 `docs/schema/README.md`):
//!
//! 1. **一律 `snake_case`**,Rust 字段名与 JSON 字段名完全相同,不做风格转换。
//! 2. **路径一律用 [`RelPath`]**(项目根相对、`/` 分隔、构造即校验),杜绝目录穿越与跨平台失败。
//! 3. **"未知/不适用"写显式 `null`**,不省略字段;所有结构体都开 `deny_unknown_fields`,
//!    与 JSON Schema 的 `additionalProperties: false` 严格对齐。
//!
//! 待实现(非 M0 范围):写锁的获取/释放逻辑见 `docs/DESIGN.md` §4.7,M3 随 MCP 多会话一起做。

pub mod curve;
pub mod digest;
pub mod health;
pub mod ids;
pub mod json;
pub mod layout;
pub mod lock;
pub mod meta;
pub mod project;
pub mod regions;
pub mod relpath;
pub mod report;
pub mod sections;
pub mod shots;
pub mod source;
pub mod state;
pub mod supplement;
pub mod transcript;

use chrono::{DateTime, FixedOffset, Local, Timelike};

pub use curve::{ChangeCurve, ChangePoint};
pub use health::{Health, check_source_link, verify_content};
pub use ids::{ProjectId, ShotId};
pub use json::{read, to_pretty_string, write_atomic, write_atomic_str};
pub use layout::Layout;
pub use lock::{LockGuard, acquire};
pub use meta::VideoMeta;
pub use project::{
    Approval, CalibrateProgress, LegacyStateV1, MaterialDigests, Progress, Project, Review,
    ReviewCounts, SCHEMA_VERSION, SchemaVersion, SourceRef, StepProgress,
};
pub use regions::{DenseRegions, Region, RegionParams, RegionReason, RegionStats};
pub use relpath::RelPath;
pub use report::{Granularity, Report, ReportPreset, ReportStyle, ReportTopic, TopicSource};
pub use sections::{SECTIONS_SCHEMA_VERSION, Section, Sections};
pub use shots::{Confidence, Evidence, Shot, ShotCheck, ShotOrigin, ShotStatus, Shots};
pub use source::{
    Materialized, Placement, SourceLink, materialize, plan, same_file, sha256_of, simplify_path,
    video_rel_for, volume_of,
};
pub use state::{
    ContentPrint, ProjectState, SegmentState, ShotState, StateFormatError, SupplementState,
    doc_print, frame, shots_print, supplements_print, transcript_print, unframe,
};
pub use supplement::{SupplementRequest, SupplementRequests, SupplementStatus, Trigger};
pub use transcript::{
    Conclusion, CorrectionType, Segment, SegmentReview, SegmentStats, Transcript,
};
/// 当前时刻,带本地 UTC 偏移(如 `2026-09-24T21:00:00+08:00`)。
///
/// 全项目所有"墙上时钟"字段都经由此函数产生,保证格式一致。
///
/// **截到整秒**:这些时间戳是给人读的(进度、校准轨迹、报告),亚秒精度只会让 JSON 变啰嗦。
/// chrono 的默认序列化会自动补足小数位,不截的话会写出 `...:40.393500100+08:00`。
pub fn now() -> DateTime<FixedOffset> {
    let dt = Local::now().fixed_offset();
    dt.with_nanosecond(0).unwrap_or(dt)
}

/// 人读的字节数(`512 B` / `2 KB` / `1.9 MB` / `3.00 GB`)。
///
/// 1024 进制;**只用于展示**,不进 JSON(JSON 里一律是精确的整数字节)。
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.2} GB", b / (KB * KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时间戳的形状是契约的一部分(JSON Schema 里的正则),所以在这里钉死。
    #[test]
    fn now_is_second_precision_rfc3339() {
        let text = serde_json::to_string(&now()).unwrap();
        let inner = text.trim_matches('"');

        assert!(!inner.contains('.'), "时间戳不该有小数秒:{inner}");

        let (date, rest) = inner.split_once('T').expect("缺日期与时间的分隔符 T");
        assert_eq!(date.len(), 10, "日期应为 YYYY-MM-DD:{inner}");
        assert_eq!(date.matches('-').count(), 2, "{inner}");

        let clock = rest.get(..8).expect("时间部分不足 8 字节");
        assert_eq!(clock.matches(':').count(), 2, "{inner}");
        assert!(
            clock.bytes().all(|b| b.is_ascii_digit() || b == b':'),
            "时间部分含非法字符:{inner}"
        );

        let offset = rest.get(8..).expect("缺 UTC 偏移");
        assert!(
            offset == "Z"
                || (offset.len() == 6 && matches!(offset.as_bytes().first(), Some(b'+' | b'-'))),
            "UTC 偏移格式不对(应为 Z 或 ±HH:MM):{inner}"
        );
    }
}
