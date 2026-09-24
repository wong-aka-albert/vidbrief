//! 把源视频放进项目:`source/video.<ext>`。
//!
//! 设计见 `docs/DESIGN.md` §4.1 的 `source/video.mp4 # 原视频(或软链接)`。
//!
//! 三种方式按"代价从低到高"依次尝试,记录实际用了哪种:
//!
//! | 方式 | 代价 | 能在什么情况下用 |
//! |------|------|-----------------|
//! | [`SourceLink::Hardlink`] | 零拷贝、零权限 | 同一卷(Windows 上建硬链接不需要管理员) |
//! | [`SourceLink::Symlink`] | 零拷贝 | 需要权限:Windows 要开发者模式或管理员 |
//! | [`SourceLink::Copy`] | 占一份磁盘 | 兜底,永远可用 |
//!
//! **为什么要记下来**:只有 `copy` 的项目是自包含的。硬链接/软链接的项目一旦离开原视频
//! 就废了 —— 报告里能不能引用视频、项目能不能打包发给别人,都取决于这个字段。
//!
//! 注意硬链接的语义:项目和原路径指向**同一个文件**。删掉原文件不影响项目(链接还在),
//! 但**就地修改**原文件会让项目看到新内容 —— 这正是 `video.meta.json.sha256` 存在的意义。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::relpath::RelPath;

/// 源视频是以什么方式进入项目的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLink {
    /// 同一卷上的硬链接(零拷贝)。
    Hardlink,
    /// 符号链接(零拷贝,需权限)。
    Symlink,
    /// 真实复制。**只有这一种项目是自包含的**。
    Copy,
}

impl SourceLink {
    /// 项目是否可以脱离原视频独立存在。
    pub fn is_self_contained(self) -> bool {
        self == Self::Copy
    }
}

/// 源视频在项目内的相对路径:沿用原扩展名,固定文件名叫 `video`。
///
/// 为什么不直接叫 `video.mp4`:源文件可能是 `.mkv` / `.mov` / `.ts`,
/// 改扩展名虽然不影响 ffmpeg(它按内容嗅探),但会误导人。
/// 扩展名不合法时退回 `mp4`。
pub fn video_rel_for(src: &Path) -> RelPath {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "mp4".to_string());
    RelPath::new(format!("source/video.{ext}")).expect("source/video.<ext> 一定是合法相对路径")
}

/// 把 `src` 放到 `dst`(按硬链接 → 软链接 → 复制的顺序尝试)。
///
/// `dst` 已存在则报错;`src` 与 `dst` 是同一个文件时直接返回 [`SourceLink::Copy`](无操作)。
pub fn materialize(src: &Path, dst: &Path) -> Result<SourceLink> {
    if !src.is_file() {
        bail!("源视频不存在:{}", src.display());
    }
    if dst.exists() {
        bail!("目标已存在,拒绝覆盖:{}", dst.display());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败:{}", parent.display()))?;
    }

    if same_file(src, dst) {
        // 源视频本来就在项目里(重复 init),不做任何搬运
        return Ok(SourceLink::Copy);
    }

    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(SourceLink::Hardlink);
    }
    if symlink_file(src, dst).is_ok() {
        return Ok(SourceLink::Symlink);
    }
    std::fs::copy(src, dst).with_context(|| {
        format!(
            "复制源视频失败:{} → {}",
            src.display(),
            dst.display()
        )
    })?;
    Ok(SourceLink::Copy)
}

/// 两个路径是否指向同一个文件。任一路径规范化失败就返回 `false`(宁可多复制一次)。
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// 去掉 Windows 的 `\\?\` 扩展长度前缀(`\\?\UNC\server\share` → `\\server\share`)。
///
/// `canonicalize()` 在 Windows 上**总是**返回 `\\?\` 开头的 verbatim 路径:它能用,
/// 但写进 `project.json.original` 会让人看不懂,也不像用户给的那个路径。
/// 非 Windows 上原样返回。
pub fn simplify_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

#[cfg(unix)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(not(any(unix, windows)))]
fn symlink_file(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "本平台不支持符号链接",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("vidbrief-source-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn video_rel_keeps_extension() {
        assert_eq!(
            video_rel_for(Path::new("D:/videos/demo.mp4")).as_str(),
            "source/video.mp4"
        );
        assert_eq!(
            video_rel_for(Path::new("D:/videos/第一讲.MKV")).as_str(),
            "source/video.mkv"
        );
        // 没扩展名 / 扩展名不合法 → mp4
        assert_eq!(
            video_rel_for(Path::new("D:/videos/demo")).as_str(),
            "source/video.mp4"
        );
        assert_eq!(
            video_rel_for(Path::new("D:/videos/奇怪.名字")).as_str(),
            "source/video.mp4"
        );
    }

    #[test]
    fn materialize_puts_bytes_in_place() {
        let dir = tmpdir("place");
        let src = dir.join("orig.mp4");
        std::fs::write(&src, b"not-really-a-video").unwrap();
        let dst_dir = dir.join("proj/source");
        let dst = dst_dir.join("video.mp4");

        let kind = materialize(&src, &dst).unwrap();
        assert!(
            matches!(kind, SourceLink::Hardlink | SourceLink::Symlink | SourceLink::Copy),
            "{kind:?}"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"not-really-a-video");

        // 再放一次要报错,不能悄悄覆盖
        assert!(materialize(&src, &dst).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn materialize_rejects_missing_source() {
        let dir = tmpdir("missing");
        let err = materialize(&dir.join("nope.mp4"), &dir.join("out.mp4"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("不存在"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn simplify_path_strips_verbatim_prefix() {
        assert_eq!(
            simplify_path(Path::new(r"\\?\E:\Projects\x\a.mp4")),
            PathBuf::from(r"E:\Projects\x\a.mp4")
        );
        assert_eq!(
            simplify_path(Path::new(r"\\?\UNC\server\share\a.mp4")),
            PathBuf::from(r"\\server\share\a.mp4")
        );
        // 没有前缀就原样返回
        assert_eq!(
            simplify_path(Path::new(r"E:\Projects\x\a.mp4")),
            PathBuf::from(r"E:\Projects\x\a.mp4")
        );
        assert_eq!(
            simplify_path(Path::new("/home/u/a.mp4")),
            PathBuf::from("/home/u/a.mp4")
        );
    }

    #[test]
    fn copy_is_the_only_self_contained_kind() {
        assert!(SourceLink::Copy.is_self_contained());
        assert!(!SourceLink::Hardlink.is_self_contained());
        assert!(!SourceLink::Symlink.is_self_contained());
        assert_eq!(
            serde_json::to_string(&SourceLink::Hardlink).unwrap(),
            "\"hardlink\""
        );
    }
}
