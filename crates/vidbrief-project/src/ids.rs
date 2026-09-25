//! 标识符:项目 id 与截图 id。
//!
//! 规则见 `docs/schema/README.md` §3。两者都"构造即校验",不合法就构造不出来。

use std::fmt;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// 项目 id。同时是 `projects/` 下的**目录名**,因此限制为 ASCII 小写字母/数字/`-`/`_`。
///
/// 约束:`^[a-z0-9][a-z0-9_-]{0,63}$`(1–64 字符)。
///
/// 为什么不用中文名做目录:CLI 参数、URL 路径、日志、跨平台文件系统都会碰到它,
/// 保持 ASCII 能省掉一整类编码问题。人类可读的名字放在 `project.json.name` 里,那里随便写。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectId(String);

impl ProjectId {
    pub const MAX_LEN: usize = 64;

    pub fn new(raw: impl Into<String>) -> Result<Self> {
        let raw = raw.into();
        Self::validate(&raw)?;
        Ok(Self(raw))
    }

    fn validate(raw: &str) -> Result<()> {
        let chars: Vec<char> = raw.chars().collect();
        if chars.is_empty() {
            bail!("项目 id 为空");
        }
        if chars.len() > Self::MAX_LEN {
            bail!("项目 id 过长({} 字符,上限 {}):{raw}", chars.len(), Self::MAX_LEN);
        }
        let first = chars[0];
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            bail!("项目 id 必须以小写字母或数字开头:`{raw}`");
        }
        for c in &chars {
            if !c.is_ascii_lowercase() && !c.is_ascii_digit() && *c != '-' && *c != '_' {
                bail!("项目 id 只允许 a-z 0-9 - _(违规字符 `{c}`):`{raw}`");
            }
        }
        Ok(())
    }

    /// 由视频文件名(不含扩展名)推导一个合法 id。
    ///
    /// 规则:转小写 → 保留 `a-z0-9` → 其余字符折叠成单个 `-` → 去掉首尾 `-` → 截断到 64 字符。
    ///
    /// **纯中文名会净化成空,此时返回 `None`**(绝不硬造一个没意义的 id)——
    /// 调用方应提示用户用 `--name` 显式指定。
    ///
    /// 注意:本函数只做净化,**不查重、也不管重名** —— 位置撞车由建项目那边报错处理,
    /// 工具不替调用方改名。
    pub fn from_video_stem(stem: &str) -> Option<String> {
        let mut out = String::new();
        let mut pending_dash = false;
        for ch in stem.chars() {
            let c = ch.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                if pending_dash && !out.is_empty() {
                    out.push('-');
                }
                pending_dash = false;
                out.push(c);
            } else {
                pending_dash = true;
            }
        }
        // 上面保证 out 全是 ASCII,但仍用 chars() 截断,避免任何按字节切的写法
        if out.chars().count() > Self::MAX_LEN {
            out = out.chars().take(Self::MAX_LEN).collect();
        }
        while out.ends_with('-') {
            out.pop();
        }
        (!out.is_empty()).then_some(out)
    }

}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ProjectId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<ProjectId> for String {
    fn from(id: ProjectId) -> Self {
        id.0
    }
}

impl TryFrom<String> for ProjectId {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::new(value)
    }
}

/// 截图 id,形如 `s007`。
///
/// **项目内单调递增、永不复用**:`update_shot(drop)` 只改 `status` 不回收 id,
/// 补图继续往后发号。这样任何日志、报告、对话里出现过的 `s007`,
/// 永远指同一个时刻的同一张图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ShotId(u32);

impl ShotId {
    /// 第一张图的 id(`s001`)。
    pub const FIRST: ShotId = ShotId(1);

    pub fn new(n: u32) -> Result<Self> {
        if n == 0 {
            bail!("截图 id 从 1 开始(s001),没有 s000");
        }
        Ok(Self(n))
    }

    pub fn get(self) -> u32 {
        self.0
    }

    /// 下一张图的 id。
    pub fn next(self) -> Result<Self> {
        Self::new(self.0 + 1)
    }

    /// 解析 `s007` 形式。
    pub fn parse(raw: &str) -> Result<Self> {
        let digits = raw
            .strip_prefix('s')
            .ok_or_else(|| anyhow::anyhow!("截图 id 必须以 s 开头:`{raw}`"))?;
        if digits.is_empty() {
            bail!("截图 id 缺数字:`{raw}`");
        }
        // 只允许 ASCII 数字,因此下面的 len() 是字节数也是字符数
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            bail!("截图 id 只能含 ASCII 数字:`{raw}`");
        }
        if digits.len() > 9 {
            bail!("截图 id 数字过长:`{raw}`");
        }
        let n: u32 = digits
            .parse()
            .map_err(|e| anyhow::anyhow!("截图 id 数字解析失败 `{raw}`:{e}"))?;
        Self::new(n)
    }
}

impl fmt::Display for ShotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{:03}", self.0)
    }
}

impl From<ShotId> for String {
    fn from(id: ShotId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for ShotId {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_accepts_and_rejects() {
        for ok in ["demo", "demo", "a", "a-2", "x_y", "0abc", "a".repeat(64).as_str()] {
            assert!(ProjectId::new(ok).is_ok(), "应接受:{ok}");
        }
        for bad in [
            "",
            "Demo",           // 大写
            "-demo",          // 以 - 开头
            "_demo",          // 以 _ 开头
            "demo x",         // 空格
            "第一讲",          // 中文
            "demo/2",         // 斜杠
            "demo.2",         // 点
            "a".repeat(65).as_str(), // 过长
        ] {
            assert!(ProjectId::new(bad).is_err(), "应拒绝:{bad:?}");
        }
    }

    #[test]
    fn project_id_from_video_stem() {
        assert_eq!(ProjectId::from_video_stem("Sample4").as_deref(), Some("demo"));
        assert_eq!(ProjectId::from_video_stem("my  video  (final)").as_deref(), Some("my-video-final"));
        assert_eq!(ProjectId::from_video_stem("__init__").as_deref(), Some("init"));
        assert_eq!(ProjectId::from_video_stem("a-").as_deref(), Some("a"));
        // 纯中文 / 纯符号净化不出东西 → None,由调用方提示用户用 --name
        assert_eq!(ProjectId::from_video_stem("第一讲 开场"), None);
        assert_eq!(ProjectId::from_video_stem(""), None);
        assert_eq!(ProjectId::from_video_stem("---"), None);
        // 净化结果非空时必须自身合法
        for stem in ["Sample4", "a-", "--", "中文123abc", "a b c"] {
            if let Some(id) = ProjectId::from_video_stem(stem) {
                assert!(ProjectId::new(id.clone()).is_ok(), "净化结果不合法:`{id}`(来自 `{stem}`)");
            }
        }
    }

    #[test]
    fn shot_id_display_and_parse() {
        assert_eq!(ShotId::FIRST.to_string(), "s001");
        assert_eq!(ShotId::new(7).unwrap().to_string(), "s007");
        assert_eq!(ShotId::new(1000).unwrap().to_string(), "s1000");
        assert_eq!(ShotId::parse("s007").unwrap(), ShotId::new(7).unwrap());
        assert_eq!(ShotId::parse("s1000").unwrap().get(), 1000);
        assert_eq!(ShotId::new(7).unwrap().next().unwrap(), ShotId::new(8).unwrap());
    }

    #[test]
    fn shot_id_rejects() {
        for bad in ["", "007", "s", "s00x", "s-1", "s 1", "s0000000001"] {
            assert!(ShotId::parse(bad).is_err(), "应拒绝:{bad:?}");
        }
        assert!(ShotId::new(0).is_err());
    }

    #[test]
    fn serde_shape_is_string() {
        let id = ShotId::new(7).unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"s007\"");
        let back: ShotId = serde_json::from_str("\"s007\"").unwrap();
        assert_eq!(back, id);

        let pid = ProjectId::new("demo").unwrap();
        assert_eq!(serde_json::to_string(&pid).unwrap(), "\"demo\"");
        assert!(serde_json::from_str::<ProjectId>("\"Demo\"").is_err());
    }
}
