//! `ffmpeg` / `ffprobe` 子进程调用。
//!
//! **本模块是"拉起外部进程"这件事的唯一入口。** 之前 `ffprobe` 调用长在
//! `vidbrief-project::meta` 里,那是越位 —— `project` 是契约层(叶子),不该有进程能力。
//! M1 把探测搬到 `core`(能力层),依赖方向恢复成 `core → project`。
//!
//! 二进制位置:默认从 `PATH` 找,可用环境变量覆盖(便于指向自带副本):
//!
//! | 环境变量 | 作用 |
//! |---------|------|
//! | `VIDBRIEF_FFMPEG` | ffmpeg 可执行文件路径 |
//! | `VIDBRIEF_FFPROBE` | ffprobe 可执行文件路径 |

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// ffmpeg 可执行文件(默认 `ffmpeg`)。
pub fn ffmpeg_bin() -> OsString {
    std::env::var_os("VIDBRIEF_FFMPEG").unwrap_or_else(|| OsString::from("ffmpeg"))
}

/// ffprobe 可执行文件(默认 `ffprobe`)。
pub fn ffprobe_bin() -> OsString {
    std::env::var_os("VIDBRIEF_FFPROBE").unwrap_or_else(|| OsString::from("ffprobe"))
}

/// 起一个 ffmpeg 进程:stdout 供调用方流式读,stderr 收进管道(失败时用来报错)。
///
/// 调用方负责读干 stdout 再 `wait()`,否则管道写满会让 ffmpeg 卡住。
pub fn spawn_ffmpeg(args: &[OsString]) -> Result<std::process::Child> {
    let bin = ffmpeg_bin();
    let child = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{}\n底层错误:{e}", missing_binary_error(&bin, "ffmpeg")))?;
    Ok(child)
}

/// 起一个 ffmpeg 命令,不关心 stdout(如抽单帧到文件)。
pub fn run_ffmpeg(args: &[OsString]) -> Result<()> {
    let bin = ffmpeg_bin();
    let out = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("{}\n底层错误:{e}", missing_binary_error(&bin, "ffmpeg")))?;
    if !out.status.success() {
        bail!("{} 执行失败({}):\n{}", bin.to_string_lossy(), out.status, tail_of(&out.stderr));
    }
    Ok(())
}

/// 跑一个 ffmpeg 命令,把 stdout 读成字节(用于 rawvideo 取帧)。
pub fn run_ffmpeg_capture(args: &[OsString]) -> Result<Vec<u8>> {
    let bin = ffmpeg_bin();
    let out = Command::new(&bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| anyhow::anyhow!("{}\n底层错误:{e}", missing_binary_error(&bin, "ffmpeg")))?;
    if !out.status.success() {
        bail!("{} 执行失败({}):\n{}", bin.to_string_lossy(), out.status, tail_of(&out.stderr));
    }
    Ok(out.stdout)
}

/// 取 `ffprobe -print_format json` 的结果。
pub fn ffprobe_json(video: &Path) -> Result<serde_json::Value> {
    let bin = ffprobe_bin();
    let out = Command::new(&bin)
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
        .map_err(|e| anyhow::anyhow!("{}\n底层错误:{e}", missing_binary_error(&bin, "ffprobe")))?;
    if !out.status.success() {
        bail!(
            "{} 探测失败({}):\n{}",
            bin.to_string_lossy(),
            out.status,
            tail_of(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("ffprobe 输出不是合法 JSON")
}

/// "找不到可执行文件"时给用户的话 —— 光说"系统找不到指定的文件"没用,得告诉他怎么改。
fn missing_binary_error(bin: &std::ffi::OsStr, which: &str) -> String {
    format!(
        "无法执行 `{}` —— 请确认 {which} 已安装并在 PATH 中,或用环境变量 VIDBRIEF_{} 指定它的路径",
        bin.to_string_lossy(),
        which.to_ascii_uppercase()
    )
}

/// `ffprobe -version` 的第一行,整理成 `ffprobe <版本>`。
pub fn ffprobe_version() -> String {
    let line = Command::new(ffprobe_bin())
        .arg("-version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.lines().next().map(str::to_string));

    match line {
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

/// ffmpeg 的 stderr 常常几十行,只留末尾几行 —— 真正的错误在最后。
fn tail_of(stderr: &[u8]) -> String {
    const KEEP: usize = 8;
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(KEEP);
    lines.get(start..).unwrap_or(&[]).join("\n")
}

/// 把 `&str` / `String` / `&OsStr` 都转成 `OsString`,让参数表写起来不啰嗦。
///
/// 注意:**别把它赋给变量再用**(`let a = arg; a("-v"); a(x.to_string())`)——
/// 那会把泛型参数固定成第一次调用的类型,后面传别的类型就编译不过。
pub fn arg(s: impl Into<OsString>) -> OsString {
    s.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binary_message_tells_you_how_to_fix_it() {
        // 不去动进程级环境变量(会和别的测试打架),直接测错误文案的构造
        let msg = missing_binary_error(&ffmpeg_bin(), "ffmpeg");
        assert!(msg.contains("VIDBRIEF_FFMPEG"), "{msg}");
        assert!(msg.contains("PATH"), "{msg}");

        let msg = missing_binary_error(&ffprobe_bin(), "ffprobe");
        assert!(msg.contains("VIDBRIEF_FFPROBE"), "{msg}");
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let raw = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n";
        let tail = tail_of(raw);
        assert_eq!(tail.lines().count(), 8);
        assert!(tail.ends_with('k'), "{tail}");
    }

    #[test]
    fn tail_of_empty_is_empty() {
        assert_eq!(tail_of(b""), "");
        assert_eq!(tail_of(b"\n\n\n"), "");
    }
}
