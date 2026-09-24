//! JSON 读写:统一缩进风格 + 原子写。
//!
//! 全项目的 JSON **只有一个写入口**([`write_atomic`]),因此:
//!
//! - 缩进恒为 2 空格、键序恒为结构体字段顺序、`null` 显式写出、文件以 `\n` 结尾;
//! - 读者永远不会看到半截文件(临时文件 + 原子 rename);
//! - `docs/schema/examples/*.json` 就是用这套序列化器产出的,测试里做逐字节比对。

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// 进程内自增,避免同目录下多个临时文件互相覆盖。
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 序列化为"项目规范 JSON":2 空格缩进 + 结尾换行。
pub fn to_pretty_string<T: Serialize>(value: &T) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value).context("序列化 JSON 失败")?;
    text.push('\n');
    Ok(text)
}

/// 读 JSON,出错时把文件路径带上(否则只有一句"expected value at line 1")。
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("读取失败:{}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("解析失败:{}", path.display()))
}

/// 原子写 JSON:先写同目录临时文件并 `sync_all`,再 rename 覆盖目标。
///
/// rename 在同一文件系统内是原子的(Windows 上 Rust 用 `MOVEFILE_REPLACE_EXISTING`),
/// 因此读者要么看到旧内容、要么看到完整新内容,不会读到半截。
pub fn write_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = to_pretty_string(value)?;
    write_atomic_str(path, &text)
}

/// 同 [`write_atomic`],但写入已序列化好的文本(用于 `.md`、`.html` 等)。
pub fn write_atomic_str(path: &Path, text: &str) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("路径没有父目录,无法写入:{}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .with_context(|| format!("文件名不是合法 UTF-8:{}", path.display()))?;
    if name.is_empty() {
        bail!("文件名为空:{}", path.display());
    }

    fs::create_dir_all(parent).with_context(|| format!("创建目录失败:{}", parent.display()))?;

    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{name}.tmp-{}-{seq}", std::process::id()));

    let write_result = (|| -> Result<()> {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("创建临时文件失败:{}", tmp.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("写入失败:{}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("刷盘失败:{}", tmp.display()))?;
        Ok(())
    })();

    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }

    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| {
            format!("原子替换失败:{} → {}", tmp.display(), path.display())
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        name: String,
        maybe: Option<u32>,
        list: Vec<String>,
    }

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vidbrief-json-{tag}-{}-{}",
            std::process::id(),
            TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_canonical_shape() {
        let value = Sample {
            name: "示例".into(),
            maybe: None,
            list: vec![],
        };
        let text = to_pretty_string(&value).unwrap();
        assert_eq!(
            text,
            "{\n  \"name\": \"示例\",\n  \"maybe\": null,\n  \"list\": []\n}\n"
        );
    }

    #[test]
    fn atomic_write_round_trip_and_creates_dirs() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("nested/deep/sample.json");
        let value = Sample {
            name: "视频".into(),
            maybe: Some(7),
            list: vec!["a".into()],
        };
        write_atomic(&path, &value).unwrap();
        let back: Sample = read(&path).unwrap();
        assert_eq!(back, value);

        // 覆盖写:内容被替换,且没有残留临时文件
        let value2 = Sample {
            name: "视频2".into(),
            maybe: None,
            list: vec![],
        };
        write_atomic(&path, &value2).unwrap();
        assert_eq!(read::<Sample>(&path).unwrap(), value2);

        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "残留临时文件:{leftovers:?}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_field_is_rejected() {
        let dir = tmpdir("unknown");
        let path = dir.join("x.json");
        fs::write(&path, r#"{"name":"a","maybe":null,"list":[],"oops":1}"#).unwrap();
        let err = read::<Sample>(&path).unwrap_err();
        // 用 `{:#}` 打印整条因果链,只 to_string() 会丢失底层原因
        let msg = format!("{err:#}");
        assert!(msg.contains("oops"), "错误信息应指出多余字段:{msg}");
        fs::remove_dir_all(&dir).unwrap();
    }
}
