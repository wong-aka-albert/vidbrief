//! `whisper-cli` 的定位与调用。
//!
//! 二进制与模型体积大、不入 git,位置由环境变量或约定目录给出(见 [`Config`])。
//! 具体调用形态来自 `docs/WHISPER_SETUP.md` 的实测记录。

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// whisper 的二进制与模型怎么找。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `whisper-cli` 可执行文件。`None` = 用 PATH 上的 `whisper-cli`。
    pub bin: Option<PathBuf>,
    /// ggml 模型文件。
    pub model: PathBuf,
    /// 语言代码,如 `zh`。
    pub language: String,
    /// 线程数。`None` = 让 whisper 自己决定。
    pub threads: Option<u32>,
}

/// 默认会去找模型的地方(相对当前工作目录)。
pub const MODEL_SEARCH: &[&str] = &[
    "tools/whisper/models/ggml-medium.bin",
    "tools/whisper/models/ggml-small.bin",
    "tools/whisper/models/ggml-base.bin",
];

impl Config {
    /// 按"环境变量 → 约定目录"的顺序解析配置。
    ///
    /// | 环境变量 | 作用 | 缺省 |
    /// |---------|------|------|
    /// | `VIDBRIEF_WHISPER_BIN` | whisper-cli 路径 | PATH 上的 `whisper-cli` |
    /// | `VIDBRIEF_WHISPER_MODEL` | ggml 模型路径 | 依次找 [`MODEL_SEARCH`] |
    /// | `VIDBRIEF_ASR_LANG` | 语言 | `zh` |
    ///
    /// 找不到模型时给出**可照做**的报错,而不是"文件不存在"。
    pub fn resolve() -> Result<Self> {
        let model = match std::env::var_os("VIDBRIEF_WHISPER_MODEL") {
            Some(p) => PathBuf::from(p),
            None => MODEL_SEARCH
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
                .with_context(|| {
                    format!(
                        "找不到 whisper 模型。请设置环境变量 VIDBRIEF_WHISPER_MODEL 指向 ggml 模型,\
                         或把模型放到以下任一位置:{}",
                        MODEL_SEARCH.join(" / ")
                    )
                })?,
        };
        Ok(Self {
            bin: std::env::var_os("VIDBRIEF_WHISPER_BIN").map(PathBuf::from),
            model,
            language: std::env::var("VIDBRIEF_ASR_LANG").unwrap_or_else(|_| "zh".to_string()),
            threads: None,
        })
    }

    /// 模型名(写进 `transcript.json.model`),如 `ggml-medium`。
    pub fn model_label(&self) -> String {
        self.model
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    fn bin(&self) -> OsString {
        self.bin
            .clone()
            .map(PathBuf::into_os_string)
            .unwrap_or_else(|| OsString::from("whisper-cli"))
    }
}

/// whisper-cli 实际会跑哪一个 —— 给体检看(`VIDBRIEF_WHISPER_BIN` 指的路径,或 `PATH` 上的名字)。
pub fn bin_display() -> String {
    match std::env::var_os("VIDBRIEF_WHISPER_BIN") {
        Some(p) => PathBuf::from(p).display().to_string(),
        None => "whisper-cli(PATH 上找)".to_string(),
    }
}

/// 模型会加载哪一个:`VIDBRIEF_WHISPER_MODEL` 指的,或约定目录里找到的第一个;
/// 都找不到 → `None`。
///
/// 与 [`Config::resolve`] 分开写是**有意**的:体检得能说清"模型没装",
/// 而 `resolve` 在那种情况下直接报错 —— 那恰恰是体检想报告的事实。
pub fn model_display() -> Option<String> {
    if let Some(p) = std::env::var_os("VIDBRIEF_WHISPER_MODEL") {
        return Some(PathBuf::from(p).display().to_string());
    }
    MODEL_SEARCH
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .map(|p| p.display().to_string())
}

/// 跑一次 `--version`,取带 `version` 的那一行;跑不起来 → `None`。
///
/// 为什么要问它:**换了 whisper 二进制,同一段音频转出来的字可能不一样** ——
/// 它得进步 1 的输入指纹(见 `vidbrief-ops::inputs`),否则增量重跑会把旧转录当新的用。
/// 版本行走 stdout(whisper.cpp 的 `load_backend:` 那几行走 stderr),两边都扫一遍更稳。
pub fn version() -> Option<String> {
    let bin = std::env::var_os("VIDBRIEF_WHISPER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("whisper-cli"));
    let out = Command::new(bin).arg("--version").output().ok()?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines()
        .find(|line| line.contains("version"))
        .map(|line| line.trim().to_string())
}

/// 跑 whisper,把结果写到 `<prefix>.srt` / `.txt` / `.vtt`。
///
/// `prefix` 是**不含扩展名的完整路径**(whisper 的 `-of` 语义)。
pub fn run(cfg: &Config, wav: &Path, prefix: &Path) -> Result<()> {
    if !wav.is_file() {
        bail!("音频文件不存在:{}", wav.display());
    }
    if !cfg.model.is_file() {
        bail!("whisper 模型不存在:{}", cfg.model.display());
    }
    if let Some(parent) = prefix.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败:{}", parent.display()))?;
    }

    let bin = cfg.bin();
    let mut args: Vec<OsString> = vec![
        "-m".into(),
        cfg.model.clone().into_os_string(),
        "-l".into(),
        cfg.language.clone().into(),
        "-f".into(),
        wav.as_os_str().to_os_string(),
        // 三种格式一起出:srt 用来解析,txt/vtt 留给人核对
        "-osrt".into(),
        "-otxt".into(),
        "-ovtt".into(),
        "-of".into(),
        prefix.as_os_str().to_os_string(),
    ];
    if let Some(threads) = cfg.threads {
        args.push("-t".into());
        args.push(threads.to_string().into());
    }

    let out = Command::new(&bin)
        .args(&args)
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "无法执行 `{}` —— 请设置环境变量 VIDBRIEF_WHISPER_BIN 指向 whisper-cli,或把它放进 PATH\n底层错误:{e}",
                bin.to_string_lossy()
            )
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "whisper-cli 失败({}):\n{}",
            out.status,
            tail(&stderr)
        );
    }

    let srt = with_extension(prefix, "srt");
    if !srt.is_file() {
        bail!(
            "whisper-cli 报告成功但没写出 {} —— 检查 -of 前缀是否可写",
            srt.display()
        );
    }
    Ok(())
}

/// 给不含扩展名的前缀补扩展名。
pub fn with_extension(prefix: &Path, ext: &str) -> PathBuf {
    let mut p = prefix.as_os_str().to_os_string();
    p.push(".");
    p.push(ext);
    PathBuf::from(p)
}

fn tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(8);
    lines.get(start..).unwrap_or(&[]).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_extension_appends_rather_than_replaces() {
        // whisper 的 `-of` 是"前缀"语义:`-of a/transcript` 会写出 `a/transcript.srt`。
        // 用 `Path::with_extension` 会把点号后面的东西当扩展名替换掉,所以自己拼。
        assert_eq!(
            with_extension(Path::new("a/transcript"), "srt"),
            PathBuf::from("a/transcript.srt")
        );
        // 前缀里本来有点号也要保留
        assert_eq!(
            with_extension(Path::new("a/my.video"), "srt"),
            PathBuf::from("a/my.video.srt")
        );
    }

    #[test]
    fn model_label_is_the_file_stem() {
        let cfg = Config {
            bin: None,
            model: PathBuf::from("tools/whisper/models/ggml-medium.bin"),
            language: "zh".into(),
            threads: None,
        };
        assert_eq!(cfg.model_label(), "ggml-medium");
    }

    #[test]
    fn missing_wav_is_rejected_before_spawning() {
        let cfg = Config {
            bin: None,
            model: PathBuf::from("whatever.bin"),
            language: "zh".into(),
            threads: None,
        };
        let err = run(&cfg, Path::new("no/such.wav"), Path::new("out/transcript"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("音频文件不存在"), "{err}");
    }

    #[test]
    fn missing_model_is_rejected_with_the_path() {
        let cfg = Config {
            bin: None,
            model: PathBuf::from("no/such/model.bin"),
            language: "zh".into(),
            threads: None,
        };
        let dir = std::env::temp_dir();
        std::fs::write(dir.join("vidbrief-asr-test.wav"), b"x").unwrap();
        let err = run(
            &cfg,
            &dir.join("vidbrief-asr-test.wav"),
            &dir.join("vidbrief-asr-out/transcript"),
        )
        .unwrap_err()
        .to_string();
        std::fs::remove_file(dir.join("vidbrief-asr-test.wav")).ok();
        assert!(err.contains("模型不存在"), "{err}");
    }

    #[test]
    fn model_not_found_message_says_what_to_do() {
        // 不真的去改环境变量(进程级、会和别的测试打架),只检查搜索列表非空且有条目
        assert!(!MODEL_SEARCH.is_empty());
        assert!(MODEL_SEARCH.iter().all(|p| p.ends_with(".bin")));
    }

    #[test]
    fn tail_keeps_the_end() {
        let text = (1..=20).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        let t = tail(&text);
        assert_eq!(t.lines().count(), 8);
        assert!(t.ends_with("line20"));
    }
}
