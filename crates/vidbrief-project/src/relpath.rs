//! 项目根相对路径。

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// 项目根相对路径(以 `/` 分隔的 POSIX 风格相对路径)。
///
/// **构造即校验**,不合法就构造不出来,因此内部永远不会出现"看起来能用、一 join 就出界"的路径。
/// 合法示例:`01_preprocess/shots/s007_0479.0.jpg`。
///
/// 被拒绝的形态(约定见 `docs/schema/README.md` §1.3):
///
/// | 形态 | 例 | 为什么拒绝 |
/// |------|----|-----------|
/// | 绝对路径 | `/tmp/x.jpg` | 项目要能整体搬走、压缩、发给别人 |
/// | 盘符 / ADS | `C:/x.jpg` | 跨平台失败;`:` 在 Windows 文件名里本就非法 |
/// | 反斜杠 | `01_preprocess\shots\a.jpg` | 反斜杠在 Unix 上只是普通字符,混用必然出错 |
/// | `.` / `..` 段 | `02_calibrate/../01_preprocess/a.json` | 目录穿越 |
/// | 空段 | `a//b`、`a/` | 同上 |
/// | 以点开头的段 | `.git/config` | 防止误写仓库/系统文件 |
/// | 控制字符 | `a\u{0}b` | 路径注入 |
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    /// 校验并构造。不合法返回错误(错误信息里带上违规的原因与原文)。
    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        Self::validate(&raw)?;
        Ok(Self(raw))
    }

    fn validate(raw: &str) -> Result<()> {
        if raw.is_empty() {
            bail!("路径为空");
        }
        if raw.len() > 1024 {
            bail!("路径过长({} 字节):{raw}", raw.len());
        }
        if raw.starts_with('/') {
            bail!("不允许绝对路径:{raw}");
        }
        if raw.ends_with('/') {
            bail!("不允许以 / 结尾(本类型只表示文件):{raw}");
        }
        if raw.contains('\\') {
            bail!("不允许反斜杠,请用 / 分隔:{raw}");
        }
        if raw.contains(':') {
            bail!("不允许冒号(盘符或 NTFS 数据流):{raw}");
        }
        if raw.contains("//") {
            bail!("不允许空路径段:{raw}");
        }
        if raw.chars().any(char::is_control) {
            bail!("路径含控制字符:{raw:?}");
        }
        for seg in raw.split('/') {
            if seg == "." || seg == ".." {
                bail!("不允许 . 或 .. 路径段:{raw}");
            }
            if seg.starts_with('.') {
                bail!("不允许以点开头的路径段:`{seg}`");
            }
        }
        Ok(())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 拼到项目根上,得到可用的绝对路径。
    pub fn to_path(&self, root: &Path) -> PathBuf {
        root.join(Path::new(self.as_str()))
    }

    /// 追加一段(会重新校验)。`tail` 里若含 `/` 表示追加多级。
    pub fn join(&self, tail: &str) -> Result<Self> {
        Self::new(format!("{}/{tail}", self.0))
    }

    /// 最后一段(文件名)。
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(self.as_str())
    }

    /// 去掉最后一段。
    pub fn parent(&self) -> Option<Self> {
        self.0.rsplit_once('/').map(|(head, _)| Self(head.to_string()))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RelPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> Self {
        p.0
    }
}

impl TryFrom<String> for RelPath {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_paths() {
        for p in [
            "project.json",
            "source/video.mp4",
            "01_preprocess/shots/s001_0009.0.jpg",
            "02_calibrate/shots_extra/s004_0018.8.jpg",
            "03_summary/notes/第一个案例.md",
        ] {
            assert!(RelPath::new(p).is_ok(), "应接受:{p}");
        }
    }

    #[test]
    fn rejects_unsafe_paths() {
        for p in [
            "",
            "/tmp/x.jpg",
            "C:/x.jpg",
            "01_preprocess\\shots\\a.jpg",
            "a//b",
            "a/",
            "..",
            "../x",
            "a/../../b",
            ".",
            "./x",
            ".git/config",
            "a/.hidden",
            "a/b\u{0}c",
        ] {
            assert!(RelPath::new(p).is_err(), "应拒绝:{p:?}");
        }
    }

    #[test]
    fn join_and_parent_round_trip() {
        let dir = RelPath::new("01_preprocess/shots").unwrap();
        let file = dir.join("s001_0009.0.jpg").unwrap();
        assert_eq!(file.as_str(), "01_preprocess/shots/s001_0009.0.jpg");
        assert_eq!(file.file_name(), "s001_0009.0.jpg");
        assert_eq!(file.parent().unwrap(), dir);
        assert_eq!(RelPath::new("project.json").unwrap().parent(), None);
    }

    #[test]
    fn rejects_join_that_escapes() {
        let dir = RelPath::new("01_preprocess").unwrap();
        assert!(dir.join("../secret").is_err());
    }

    #[test]
    fn serde_round_trip() {
        let p = RelPath::new("source/video.meta.json").unwrap();
        let text = serde_json::to_string(&p).unwrap();
        assert_eq!(text, "\"source/video.meta.json\"");
        let back: RelPath = serde_json::from_str(&text).unwrap();
        assert_eq!(back, p);

        // 反序列化同样要过校验
        assert!(serde_json::from_str::<RelPath>("\"../x\"").is_err());
    }
}
