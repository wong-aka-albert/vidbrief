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

/// 默认会去找模型的地方(相对**当前工作目录**)。
pub const MODEL_SEARCH: &[&str] = &[
    "tools/whisper/models/ggml-medium.bin",
    "tools/whisper/models/ggml-small.bin",
    "tools/whisper/models/ggml-base.bin",
];

/// 这一趟实际会去哪些路径找模型:**先当前工作目录,再可执行文件所在的那棵树**。
///
/// # 为什么多一组(2026-09-27)
///
/// [`MODEL_SEARCH`] 是相对 cwd 的,而 cwd 是个**会变的东西**:从仓库根跑 CLI 找得到,
/// 从项目目录(`projects/xxx/`)跑就一个都找不到 —— 于是同一个项目、同一条命令,
/// 换了个目录就从"沿用上次那份转录"变成"找不到模型,硬失败"(实测踩过两次)。
///
/// 加的这一组是**跟着二进制走**的:`target/release/vidbrief.exe` 往上退到仓库根,
/// 接上同几条相对路径。装到别处、或者把二进制拷走的情况不受影响(那本来就该用
/// `VIDBRIEF_WHISPER_MODEL` 指路)。
pub fn model_search() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = MODEL_SEARCH.iter().map(PathBuf::from).collect();
    if let Some(root) = tree_root_from_exe() {
        out.extend(MODEL_SEARCH.iter().map(|rel| root.join(rel)));
    }
    out
}

/// 从可执行文件的位置往上找"仓库根"(判据:那底下有 `tools/whisper/models/`)。
///
/// 退几级是因为调用方的位置不止一个:`target/release/vidbrief.exe`(祖父是根)、
/// 测试二进制 `target/debug/deps/xxx.exe`(曾祖父是根)。最多退三级。
fn tree_root_from_exe() -> Option<PathBuf> {
    let mut dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for _ in 0..3 {
        if dir.join("tools/whisper/models").is_dir() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// `whisper-cli` 默认会去找的地方(相对 cwd)。
pub const BIN_SEARCH: &[&str] = &[
    "tools/whisper/Release/whisper-cli.exe",
    "tools/whisper/Release/whisper-cli",
    "tools/whisper/whisper-cli.exe",
];

/// 这一趟实际会用哪个 `whisper-cli`。
///
/// 顺序:`VIDBRIEF_WHISPER_BIN` → [`BIN_SEARCH`](先 cwd,再"跟着二进制走"的那棵树)
/// → `PATH` 上的 `whisper-cli`(返回 `None`,由调用方退回名字)。
///
/// 跟模型那一组(`model_search`)同一个理由:cwd 会变,而**外挂解析不出来会被判成
/// "输入变了"** → 明明能沿用的转录被拉去重算,然后在"跑不起来"上失败(实测踩过)。
pub fn bin_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VIDBRIEF_WHISPER_BIN") {
        return Some(PathBuf::from(p));
    }
    let mut candidates: Vec<PathBuf> = BIN_SEARCH.iter().map(PathBuf::from).collect();
    if let Some(root) = tree_root_from_exe() {
        candidates.extend(BIN_SEARCH.iter().map(|rel| root.join(rel)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

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
        Self::resolve_preferring(None)
    }

    /// 同 [`Config::resolve`],但**环境变量没设时优先 `preferred` 那个模型**
    /// (传项目上次用的模型名,如 `ggml-small`)。
    ///
    /// # 为什么
    ///
    /// 优先级是:环境变量最硬(设了就是用户说了算)→ `preferred` → 约定目录里第一个命中的。
    /// 中间那一档是 2026-09-27 补的:少了它,"这台机器默认挑哪个模型"会**悄悄**决定
    /// 一个项目的材料要不要重做 —— 而候选表第一个是 medium([`MODEL_SEARCH`]),
    /// 一个没人选过的默认值。实测的代价:CLI 少带 `VIDBRIEF_WHISPER_MODEL` 跑了一次
    /// `preprocess`,当场把"沿用"判成"要重转录"(转录一换,核验戳与结论全部作废)。
    ///
    /// 要**换**模型只有一个正门:显式设 `VIDBRIEF_WHISPER_MODEL`。
    pub fn resolve_preferring(preferred: Option<&str>) -> Result<Self> {
        let model = match std::env::var_os("VIDBRIEF_WHISPER_MODEL") {
            Some(p) => PathBuf::from(p),
            None => preferred
                .and_then(model_named)
                .or_else(first_default_model)
                .with_context(|| {
                    format!(
                        "找不到 whisper 模型。请设置环境变量 VIDBRIEF_WHISPER_MODEL 指向 ggml 模型,\
                         或把模型放到以下任一位置:{}",
                        MODEL_SEARCH.join(" / ")
                    )
                })?,
        };
        Ok(Self {
            bin: bin_path(),
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

/// whisper-cli 实际会跑哪一个 —— 给体检看。
///
/// 顺序同 [`bin_path`]:环境变量 → 约定目录(含"跟着二进制走"那棵树)→ `PATH` 上的名字。
pub fn bin_display() -> String {
    match bin_path() {
        Some(p) => p.display().to_string(),
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
    first_default_model().map(|p| p.display().to_string())
}
/// 同 [`model_display`],但**没设环境变量时优先那个名字的模型**(项目上次用的那个)。
///
/// 体检要看的是"**这个项目**下一趟会用哪个模型" —— 不是"这台机器默认会挑哪个"。
pub fn model_display_preferring(preferred: Option<&str>) -> Option<String> {
    if let Some(p) = std::env::var_os("VIDBRIEF_WHISPER_MODEL") {
        return Some(PathBuf::from(p).display().to_string());
    }
    preferred
        .and_then(model_named)
        .or_else(first_default_model)
        .map(|p| p.display().to_string())
}

/// 约定目录里第一个**真的在**的候选(含"跟着二进制走"的那一组,见 [`model_search`])。
///
/// 都不在就是 `None` —— 体检要靠这个 `None` 说清"模型没装",**不要**在这里假装有个默认值。
fn first_default_model() -> Option<PathBuf> {
    model_search().into_iter().find(|p| p.is_file())
}

/// 按**模型名**在约定目录里找模型(`ggml-small` → `tools/whisper/models/ggml-small.bin`)。
///
/// 为什么要有它:环境变量没设时,`resolve` 退到"约定目录里第一个命中的",而候选表里
/// **medium 排第一**([`MODEL_SEARCH`],那是当初拍板的"中文默认 medium")——
/// 于是一个**没人选过的默认值**会悄悄把项目从 small 换成 medium:3 倍时长,
/// 而且转录一换,挂在上面的核验戳与结论**全部作废**(实测踩过:CLI 少带环境变量跑了一下
/// `preprocess`,当场判定"要重转录",随后因为 PATH 上没有 whisper-cli 而失败)。
/// 所以解析时允许把项目上次用的模型名带进来 —— 见 [`Config::resolve_preferring`]。
pub fn model_named(label: &str) -> Option<PathBuf> {
    model_named_in(&model_search(), label)
}

/// [`model_named`] 的可注入版本(测试用临时目录当"约定目录")。
fn model_named_in<S: AsRef<std::path::Path>>(search: &[S], label: &str) -> Option<PathBuf> {
    search
        .iter()
        .map(|p| PathBuf::from(p.as_ref()))
        .find(|p| p.is_file() && p.file_stem().and_then(|s| s.to_str()) == Some(label))
}

/// 跑一次 `--version`,取带 `version` 的那一行;跑不起来 → `None`。
///
/// 为什么要问它:**换了 whisper 二进制,同一段音频转出来的字可能不一样** ——
/// 它得进步 1 的输入指纹(见 `vidbrief-ops::inputs`),否则增量重跑会把旧转录当新的用。
/// 版本行走 stdout(whisper.cpp 的 `load_backend:` 那几行走 stderr),两边都扫一遍更稳。
pub fn version() -> Option<String> {
    let bin = bin_path().unwrap_or_else(|| PathBuf::from("whisper-cli"));
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

    /// 按**模型名**找回约定目录里的那一份 —— 环境变量没设时,项目上次用的模型优先于
    /// "候选表第一个命中的"(那个是 medium,一个没人选过的默认值)。
    #[test]
    fn a_recorded_model_can_be_found_by_name() {
        let dir = std::env::temp_dir().join(format!("vidbrief-whisper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("ggml-small.bin");
        let medium = dir.join("ggml-medium.bin");
        std::fs::write(&small, b"x").unwrap();
        std::fs::write(&medium, b"x").unwrap();

        // 候选表里 medium 在前(那是"没得选时"的默认),但按名字找得到 small
        let search = vec![medium.clone(), small.clone()];
        assert_eq!(model_named_in(&search, "ggml-small"), Some(small.clone()));
        assert_eq!(model_named_in(&search, "ggml-medium"), Some(medium.clone()));
        // 名字对不上 / 文件不在 → 找不到(调用方退回正常解析)
        assert_eq!(model_named_in(&search, "ggml-large"), None);
        std::fs::remove_file(&small).unwrap();
        assert_eq!(model_named_in(&search, "ggml-small"), None);
        std::fs::remove_file(&medium).unwrap();
        let _ = std::fs::remove_dir(&dir);
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
