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
//! **为什么要记下来**:它决定了"这个项目能不能脱离原文件独立使用"。
//!
//! - `copy` 与 `hardlink`:能。硬链接的**目录项在项目里**,数据同一份;
//!   删掉原文件不影响项目,归档工具读到的也是真实字节。
//! - `symlink`:**不能**。只是个路径引用,原文件一移动就废;而且归档工具对软链接的
//!   处理不一致(有的跟随目标、有的原样保留 → 死链),这个失败是**静默的**。
//!
//! 硬链接唯一要注意的是:项目与原路径**共用同一份数据**,所以**就地修改**原文件会让项目
//! 跟着变。这不是自包含性问题,是内容漂移问题 —— 由 `video.meta.json.sha256` 发现。

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::relpath::RelPath;

/// 源视频是以什么方式进入项目的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLink {
    /// 同一卷上的硬链接(零拷贝)。数据就在项目目录里,只是与原路径共用同一份。
    Hardlink,
    /// 符号链接(零拷贝,需权限)。
    Symlink,
    /// 真实复制。
    Copy,
}

impl SourceLink {
    /// 项目能否脱离原文件独立使用(拷走 / 打包 / 删掉原文件之后)。
    ///
    /// - [`Copy`](Self::Copy):独立副本 ✓
    /// - [`Hardlink`](Self::Hardlink):**也是 ✓**。硬链接的**目录项在项目里**,
    ///   数据同一份;删掉原文件不影响项目,归档工具读到的也是真实字节。
    ///   早先这里写成"只有 Copy 才自包含"是**错的** —— 它把硬链接和软链接混为一谈了。
    /// - [`Symlink`](Self::Symlink):✗ 只是个路径引用,原文件一走就废。
    pub fn is_self_contained(self) -> bool {
        !matches!(self, Self::Symlink)
    }

    /// **原地修改**原文件后,项目里的数据会不会跟着变。
    ///
    /// 复制不会;硬链接和软链接会。这不是自包含性问题,是**内容漂移**问题,
    /// 由 `video.meta.json.sha256` 负责发现(发现比阻止重要 —— 视频很少被就地改,
    /// 真被改了也该让人知道,而不是让工具去猜)。
    pub fn tracks_source_edits(self) -> bool {
        matches!(self, Self::Hardlink | Self::Symlink)
    }

    /// 一句话说清"源视频是怎么进来的"以及**实际后果**。
    ///
    /// 措辞要准:早先这里对硬链接写的是"不**自包含**,要打包请改用复制",
    /// 结果用户读完以为工具复制了一份文件 —— 而它其实一个字节都没拷。
    pub fn describe(self, size_bytes: u64) -> String {
        match self {
            Self::Hardlink => {
                "硬链接 —— 与源文件共用同一份数据,不占额外磁盘(删掉源文件不影响本项目)"
                    .to_string()
            }
            Self::Symlink => "软链接 —— 仅路径引用,源文件移动或删除后本项目即失效".to_string(),
            Self::Copy => format!("复制 —— 独立副本,占用 {}", crate::human_size(size_bytes)),
        }
    }
}

// ── 文件身份:判断"是不是同一份数据" ─────────────────────────────────
//
// 判硬链接关系的**正确**方式是比文件标识,不是比路径:
// 路径规范化在 Windows 上会带上 `\\?\` 前缀,而且会把符号链接解析掉。
//
// **平台上的一处妥协**:Windows 上 `std::os::windows::fs::MetadataExt` 的
// `volume_serial_number` / `file_index` / `number_of_links` 到 Rust 1.96 仍是
// unstable(`windows_by_handle`,issue #63010),所以同一文件判断改用 `same-file` crate
// (Windows 走 `GetFileInformationByHandle`,Unix 走 `dev+ino`)。
// 也因此这里**没有**提供"链接数":拿不到就宁可不给,而不是只在一个平台上给。

/// 路径所在的**卷**:Windows 取根前缀(`E:` / `\\server\share`),Unix 取设备号。
///
/// 用来预判"硬链接能不能建" —— 硬链接**不能跨卷**。
///
/// **先剥 `\\?\` 前缀再取**:`canonicalize()` 在 Windows 上产出的是 `\\?\E:\...`,
/// 它的前缀组件是 `\\?\E:` 而不是 `E:` —— 直接比较会把**同卷判成跨卷**。
/// 相对路径会先转成绝对路径,否则拿不到盘符。
pub fn volume_of(path: &Path) -> Option<String> {
    let simplified = simplify_path(path);
    let abs = std::path::absolute(&simplified).unwrap_or(simplified);

    #[cfg(windows)]
    {
        use std::path::Component;
        match abs.components().next() {
            Some(Component::Prefix(p)) => Some(p.as_os_str().to_string_lossy().into_owned()),
            _ => None,
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Unix 上卷号只能从已存在的文件拿;目录还不存在时判断不出来,返回 None
        std::fs::metadata(&abs).ok().map(|m| m.dev().to_string())
    }
}

/// 两个路径是否指向**同一份数据**。任一路径不存在则为 `false`。
pub fn same_file(a: &Path, b: &Path) -> bool {
    same_file::is_same_file(a, b).unwrap_or(false)
}

/// [`plan`] 的结论:动手**之前**能预判到的东西。
///
/// 存在的意义是"别让用户干等":几百 MB 的复制要好几秒,而这期间如果什么都不说,
/// 用户只会以为卡住了,事后看到一行"复制"也不知道为什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// 预计会用的方式(只是预判,以 [`materialize`] 的实际结果为准)。
    pub predicted: SourceLink,
    /// 素材与目标目录是否在同一个卷。`None` = 判断不出来。
    pub same_volume: Option<bool>,
    /// 预计要复制的字节数。
    pub size_bytes: u64,
}

impl Placement {
    /// 预判会跨卷复制吗。
    pub fn will_copy_across_volumes(&self) -> bool {
        self.same_volume == Some(false)
    }
}

/// 预判 [`materialize`] 会怎么做。**不产生任何副作用。**
///
/// `dst` 的父目录必须已经存在(否则判断不出目标在哪个卷)。
pub fn plan(src: &Path, dst: &Path) -> Result<Placement> {
    let meta = std::fs::metadata(src)
        .with_context(|| format!("读不到源文件属性:{}", src.display()))?;
    let same_volume = match (volume_of(src), dst.parent().and_then(volume_of)) {
        (Some(a), Some(b)) => Some(a == b),
        _ => None,
    };
    Ok(Placement {
        // 跨卷时硬链接**必然**失败(不是"可能"),可以直接预判;其余情况先假设能硬链接
        predicted: if same_volume == Some(false) {
            SourceLink::Copy
        } else {
            SourceLink::Hardlink
        },
        same_volume,
        size_bytes: meta.len(),
    })
}

/// [`materialize`] 的结果:用了哪种方式,以及**为什么没能用更好的方式**。
///
/// 没有 `fallbacks` 的话,"为什么落到复制"就只能靠猜 —— 而用户没有任何办法自查。
/// 这条信息必须由工具给出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materialized {
    pub link: SourceLink,
    /// 回退链上被放弃的尝试及原因(按尝试顺序)。第一选择就成功时为空。
    pub fallbacks: Vec<String>,
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
/// `dst` 已存在(含**悬空的符号链接**)则报错,绝不覆盖。
/// 每一次回退的原因都记进 [`Materialized::fallbacks`]。
pub fn materialize(src: &Path, dst: &Path) -> Result<Materialized> {
    if !src.is_file() {
        bail!("源视频不存在:{}", src.display());
    }
    // 用 symlink_metadata 而不是 exists():后者对**悬空的符号链接**返回 false,
    // 会让我们往一个已经存在的目录项上写
    if std::fs::symlink_metadata(dst).is_ok() {
        bail!("目标已存在,拒绝覆盖:{}", dst.display());
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败:{}", parent.display()))?;
    }

    let mut fallbacks = Vec::new();

    match std::fs::hard_link(src, dst) {
        Ok(()) => {
            return Ok(Materialized {
                link: SourceLink::Hardlink,
                fallbacks,
            });
        }
        Err(e) => {
            // 跨卷是最常见的原因,而且是**必然**失败(不是"权限"或"文件系统不支持"),
            // 所以直接说人话,别把 `os error 17` 丢给用户
            let reason = match plan(src, dst) {
                Ok(p) if p.will_copy_across_volumes() => {
                    "硬链接: 素材与项目不在同一个卷(盘)上,硬链接跨不了卷".to_string()
                }
                _ => format!("硬链接: {e}"),
            };
            fallbacks.push(reason);
        }
    }

    match symlink_file(src, dst) {
        Ok(()) => {
            return Ok(Materialized {
                link: SourceLink::Symlink,
                fallbacks,
            });
        }
        Err(e) => fallbacks.push(format!("软链接: {e}")),
    }

    std::fs::copy(src, dst)
        .with_context(|| format!("复制源视频失败:{} → {}", src.display(), dst.display()))?;
    Ok(Materialized {
        link: SourceLink::Copy,
        fallbacks,
    })
}

/// 分块计算 SHA-256(十六进制小写)。用固定缓冲流式读,避免把整个视频读进内存。
///
/// 这是文件**内容身份**:`video.meta.json` 记一份,健康检查时重算一份用来发现漂移。
pub fn sha256_of(path: &Path) -> Result<String> {
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

        let got = materialize(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"not-really-a-video");

        // 不变式:**回退 ⟺ 有原因**。成功用上第一选择时不该有原因;
        // 退到后面任何一种,就必须能说清为什么(否则用户只能猜)。
        if got.link == SourceLink::Hardlink {
            assert!(got.fallbacks.is_empty(), "硬链接成功却记了回退:{}", got.fallbacks[0]);
        } else {
            assert!(!got.fallbacks.is_empty(), "退到 {:?} 却没给原因", got.link);
        }
        // 同目录下必然同卷,硬链接应当可用
        assert_eq!(got.link, SourceLink::Hardlink, "同卷同目录应能建硬链接");

        // 再放一次要报错,不能悄悄覆盖
        assert!(materialize(&src, &dst).is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn materialize_refuses_to_write_through_a_dangling_symlink() {
        // `exists()` 对悬空符号链接返回 false —— 用它做守卫会漏。
        let dir = tmpdir("dangling");
        let src = dir.join("orig.mp4");
        std::fs::write(&src, b"data").unwrap();
        let dst = dir.join("dangling-link.mp4");
        if symlink_file(&dir.join("nowhere.mp4"), &dst).is_err() {
            // 本机没有建符号链接的权限(Windows 默认如此),这条测不了
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        assert!(!dst.exists(), "前提:悬空链接 exists() 应为 false");

        let err = materialize(&src, &dst).unwrap_err().to_string();
        assert!(err.contains("拒绝覆盖"), "{err}");

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
    fn volume_of_ignores_the_verbatim_prefix() {
        // canonicalize 在 Windows 上产出 `\\?\E:\...`,而用户给的是 `E:\...`。
        // 两者必须被认作同一个卷 —— 早先没剥前缀,把同卷判成了跨卷(真实踩过的 bug)。
        let dir = tmpdir("vol");
        let plain = dir.join("a.txt");
        std::fs::write(&plain, b"x").unwrap();
        let canonical = plain.canonicalize().unwrap();

        assert_eq!(
            volume_of(&plain),
            volume_of(&canonical),
            "同一路径的普通形式与 canonical 形式必须算同一个卷"
        );
        assert!(volume_of(&plain).is_some(), "应当能判断出卷");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn plan_does_not_call_a_same_volume_pair_cross_volume() {
        let dir = tmpdir("plan-same");
        let src = dir.join("orig.mp4");
        std::fs::write(&src, b"data").unwrap();
        let dst = dir.join("proj/source/video.mp4");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();

        // 源路径故意用 canonical(带 `\\?\`),目标用普通路径 —— 这是真实调用形态
        let p = plan(&src.canonicalize().unwrap(), &dst).unwrap();
        assert_eq!(p.same_volume, Some(true), "同一目录下必然同卷:{p:?}");
        assert_eq!(p.predicted, SourceLink::Hardlink);
        assert!(!p.will_copy_across_volumes());
        assert_eq!(p.size_bytes, 4);

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
    fn copy_is_no_longer_the_only_self_contained_kind() {
        // 这条曾经写错成"只有 Copy 自包含",把硬链接和软链接混为一谈 ——
        // 硬链接的数据就在项目目录里,删掉原文件照样能用。
        assert!(SourceLink::Copy.is_self_contained());
        assert!(SourceLink::Hardlink.is_self_contained(), "硬链接的项目也是自包含的");
        assert!(!SourceLink::Symlink.is_self_contained(), "软链接只是个路径引用");

        // 但硬链接会跟着原文件的就地修改走,复制不会
        assert!(SourceLink::Hardlink.tracks_source_edits());
        assert!(SourceLink::Symlink.tracks_source_edits());
        assert!(!SourceLink::Copy.tracks_source_edits());

        assert_eq!(
            serde_json::to_string(&SourceLink::Hardlink).unwrap(),
            "\"hardlink\""
        );
    }
}
