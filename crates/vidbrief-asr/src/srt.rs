//! SRT 解析 —— **纯函数,不碰进程**。
//!
//! whisper.cpp 的 `-osrt` 输出形如:
//!
//! ```text
//! 1
//! 00:00:00,000 --> 00:00:02,000
//! 各位好
//!
//! 2
//! 00:00:02,000 --> 00:00:06,000
//! 我们今天这期视频是一个承上启下的内容
//! ```
//!
//! 这类解析器的价值全在**异常输入**上(空块、缺序号、CRLF、BOM、时间戳畸形),
//! 所以它必须能单独喂字符串测试 —— 不能只靠"跑一遍 whisper 看看"。

use anyhow::{Context, Result, bail};

/// 一条原始字幕段。
#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    /// SRT 里写的序号。缺号时按出现顺序补。
    pub index: u32,
    pub start: f64,
    pub end: f64,
    /// 文本。多行会**直接拼接不插空格** —— whisper.cpp 每个 cue 只输出一行,
    /// 真出现多行也是在切分一个中文长句,插空格反而错。
    pub text: String,
}

/// 解析整个 SRT 文本。
pub fn parse(input: &str) -> Result<Vec<RawSegment>> {
    // 去 BOM,统一换行
    let cleaned = input.strip_prefix('\u{feff}').unwrap_or(input);
    let normalized = cleaned.replace("\r\n", "\n").replace('\r', "\n");

    let mut out = Vec::new();
    let mut auto_index = 1u32;

    for block in normalized.split("\n\n") {
        let lines: Vec<&str> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }

        // 找时间戳行:正常是第一行(前面有可选的序号行)
        let time_pos = lines
            .iter()
            .position(|l| l.contains("-->"))
            .with_context(|| format!("SRT 块里没有时间戳行:{block:?}"))?;

        let (start, end) = parse_time_range(lines[time_pos])?;

        // 序号取时间戳行之前的内容(有就解析,没有就按顺序补)
        let index = lines
            .get(..time_pos)
            .and_then(|head| head.last())
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or_else(|| {
                let i = auto_index;
                auto_index += 1;
                i
            });
        auto_index = auto_index.max(index + 1);

        let text = lines.get(time_pos + 1..).unwrap_or(&[]).join("");
        if text.is_empty() {
            // whisper 偶尔会吐空段(纯静音),跳过而不是当成错误
            continue;
        }

        out.push(RawSegment {
            index,
            start,
            end,
            text,
        });
    }

    if out.is_empty() {
        bail!("SRT 里没有解析出任何字幕段");
    }
    Ok(out)
}

/// 解析 `00:00:02,000 --> 00:00:06,000`。
fn parse_time_range(line: &str) -> Result<(f64, f64)> {
    let (left, right) = line
        .split_once("-->")
        .with_context(|| format!("时间戳行缺 `-->`:{line:?}"))?;
    let start = parse_timestamp(left.trim())?;
    // 右边可能带位置参数(如 `00:00:06,000 align:start`),只取第一个词
    let end_token = right.split_whitespace().next().unwrap_or("");
    let end = parse_timestamp(end_token)?;
    if end < start {
        bail!("时间戳倒挂:{line:?}");
    }
    Ok((start, end))
}

/// 解析 `HH:MM:SS,mmm`(也容忍 `.` 当毫秒分隔符)。
fn parse_timestamp(token: &str) -> Result<f64> {
    let token = token.trim();
    let (clock, millis) = token
        .split_once([',', '.'])
        .map(|(c, m)| (c, m))
        .unwrap_or((token, "0"));

    let parts: Vec<&str> = clock.split(':').collect();
    let (h, m, s) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => ("0", *m, *s),
        _ => bail!("时间戳格式不认识:{token:?}"),
    };

    let hours: f64 = h.trim().parse().with_context(|| format!("小时解析失败:{token:?}"))?;
    let minutes: f64 = m.trim().parse().with_context(|| format!("分钟解析失败:{token:?}"))?;
    let seconds: f64 = s.trim().parse().with_context(|| format!("秒解析失败:{token:?}"))?;
    let ms: f64 = millis
        .trim()
        .parse()
        .with_context(|| format!("毫秒解析失败:{token:?}"))?;

    if ![hours, minutes, seconds, ms].iter().all(|v| v.is_finite()) {
        bail!("时间戳含非有限数:{token:?}");
    }
    Ok(hours * 3600.0 + minutes * 60.0 + seconds + ms / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1\n00:00:00,000 --> 00:00:02,000\n各位好\n\n2\n00:00:02,000 --> 00:00:06,000\n我们今天这期视频\n\n3\n00:00:06,000 --> 00:00:10,000\n对于刚加入的\n";

    #[test]
    fn parses_a_normal_file() {
        let segs = parse(SAMPLE).unwrap();
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].index, 1);
        assert_eq!(segs[0].start, 0.0);
        assert_eq!(segs[0].end, 2.0);
        assert_eq!(segs[0].text, "各位好");
        assert_eq!(segs[1].text, "我们今天这期视频");
        assert_eq!(segs[2].end, 10.0);
    }

    #[test]
    fn parses_the_real_whisper_output_shape() {
        // 真实 whisper.cpp 输出用的是 CRLF 与 , 分隔毫秒
        let text = SAMPLE.replace('\n', "\r\n");
        let segs = parse(&text).unwrap();
        assert_eq!(segs.len(), 3, "CRLF 不该影响解析");
    }

    #[test]
    fn handles_bom_and_missing_trailing_newline() {
        let text = format!("\u{feff}{}", SAMPLE.trim_end());
        let segs = parse(&text).unwrap();
        assert_eq!(segs.len(), 3, "BOM 与结尾无换行都该能处理");
    }

    #[test]
    fn tolerates_missing_index_lines() {
        let text = "00:00:01,000 --> 00:00:02,000\n甲\n\n00:00:03,000 --> 00:00:04,000\n乙\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].index, 1, "缺号时按顺序补");
        assert_eq!(segs[1].index, 2);
    }

    #[test]
    fn tolerates_dot_as_millisecond_separator() {
        let text = "1\n00:00:01.500 --> 00:00:02.250\n甲\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs[0].start, 1.5);
        assert_eq!(segs[0].end, 2.25);
    }

    #[test]
    fn tolerates_extra_trailing_whitespace_and_blank_blocks() {
        let text = "\n\n1\n00:00:00,000 --> 00:00:01,000\n甲\n\n\n\n2\n00:00:01,000 --> 00:00:02,000\n乙\n\n\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn skips_empty_text_cues_instead_of_failing() {
        // 纯静音段 whisper 会吐空文本
        let text = "1\n00:00:00,000 --> 00:00:01,000\n\n\n\n2\n00:00:01,000 --> 00:00:02,000\n甲\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "甲");
    }

    #[test]
    fn joins_multiline_cues_without_inserting_spaces() {
        // 中文长句被切开时,插空格反而错
        let text = "1\n00:00:00,000 --> 00:00:01,000\n前半句\n后半句\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs[0].text, "前半句后半句");
    }

    #[test]
    fn ignores_position_suffix_after_the_timecode() {
        let text = "1\n00:00:00,000 --> 00:00:02,000 align:start position:0%\n甲\n";
        let segs = parse(text).unwrap();
        assert_eq!(segs[0].end, 2.0);
    }

    #[test]
    fn rejects_an_empty_or_garbage_file() {
        assert!(parse("").is_err());
        assert!(parse("这不是字幕").is_err(), "没有时间戳行应当报错");
    }

    #[test]
    fn rejects_backwards_timecodes() {
        let text = "1\n00:00:05,000 --> 00:00:01,000\n甲\n";
        let err = parse(text).unwrap_err().to_string();
        assert!(err.contains("倒挂"), "{err}");
    }

    #[test]
    fn timestamp_parsing_edge_cases() {
        assert_eq!(parse_timestamp("00:00:00,000").unwrap(), 0.0);
        assert_eq!(parse_timestamp("01:02:03,004").unwrap(), 3723.004);
        assert_eq!(parse_timestamp("02:03,500").unwrap(), 123.5, "容忍缺小时位");
        assert!(parse_timestamp("garbage").is_err());
    }
}
