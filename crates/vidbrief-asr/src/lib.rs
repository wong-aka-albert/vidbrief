//! 语音转文字(步 1 的第二个产出)。
//!
//! 设计见 `docs/DESIGN.md` §5.1 ②。三步:
//!
//! 1. `ffmpeg` 把音轨抽成 16kHz 单声道 WAV
//! 2. `whisper-cli` 跑出 SRT/TXT/VTT
//! 3. 解析 SRT → [`Transcript`]
//!
//! **本模块不做任何纠错。** 中文专有名词必错(实测「Python」→「派森」、「达摩院」→
//! 「打墨院」),那是 agent 在步 2 的活(§6.2 ②)。这里只负责**如实搬运**。
//!
//! 没有音轨的视频(如合成回归素材)不是错误 —— 差帧图照样有效,只是转录为空。
//! 这种情况返回 `Ok(None)`,由调用方决定怎么呈现。

pub mod srt;
pub mod whisper;

use std::path::Path;

use anyhow::{Context, Result, bail};
use vidbrief_project::{Layout, Segment, Transcript, VideoMeta};

pub use srt::RawSegment;
pub use whisper::Config;

/// 转码出来的 WAV 参数(whisper.cpp 只认 16kHz 单声道 PCM)。
pub const SAMPLE_RATE: u32 = 16_000;

/// 抽音 + 转录 + 解析,并把 `transcript.srt/.txt/.vtt` 与 `transcript.json` 落盘。
///
/// - 源视频没有音轨 → `Ok(None)`(不是错误)
/// - `layout` 决定三个副产物与 `transcript.json` 的位置
pub fn transcribe(
    video: &Path,
    layout: &Layout,
    meta: &VideoMeta,
    cfg: &Config,
) -> Result<Option<Transcript>> {
    if !has_audio(video)? {
        return Ok(None);
    }

    let wav = layout.preprocess_dir().join("audio.wav");
    extract_audio(video, &wav)?;

    let prefix = layout.preprocess_dir().join("transcript");
    whisper::run(cfg, &wav, &prefix)
        .with_context(|| format!("whisper 转录失败:{}", video.display()))?;

    // WAV 是中间产物,转完就没用了 —— 一万秒的视频它是几百 MB,不该留在项目里
    let _ = std::fs::remove_file(&wav);

    let srt_path = whisper::with_extension(&prefix, "srt");
    let text = std::fs::read_to_string(&srt_path)
        .with_context(|| format!("读取失败:{}", srt_path.display()))?;

    let transcript = from_srt(&text, meta, cfg)?;
    transcript
        .save(&layout.transcript())
        .context("写入 transcript.json 失败")?;
    Ok(Some(transcript))
}

/// 把 SRT 转成结构化转录。
pub fn from_srt(text: &str, meta: &VideoMeta, cfg: &Config) -> Result<Transcript> {
    let raws = srt::parse(text)?;
    Ok(Transcript {
        source: meta.path.clone(),
        model: cfg.model_label(),
        language: cfg.language.clone(),
        duration: meta.duration,
        generated_at: vidbrief_project::now(),
        segments: raws
            .into_iter()
            .map(|r| Segment {
                id: r.index,
                start: r.start,
                end: r.end,
                text: r.text,
                // 原始转录**永不就地改写**;这两个字段留给 agent 在步 2 填
                corrected_text: None,
                correction: None,
            })
            .collect(),
    })
}

/// 用 ffmpeg 抽音轨到 16kHz 单声道 WAV。
pub fn extract_audio(video: &Path, out: &Path) -> Result<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败:{}", parent.display()))?;
    }
    use vidbrief_core::ffmpeg::arg;

    let args = vec![
        arg("-v"),
        arg("error"),
        arg("-i"),
        video.as_os_str().to_os_string(),
        arg("-vn"), // 不要视频
        arg("-ar"),
        arg(SAMPLE_RATE.to_string()),
        arg("-ac"),
        arg("1"),
        arg("-c:a"),
        arg("pcm_s16le"),
        arg("-y"),
        out.as_os_str().to_os_string(),
    ];
    vidbrief_core::ffmpeg::run_ffmpeg(&args)
        .with_context(|| format!("抽取音轨失败:{}", video.display()))?;
    if !out.is_file() {
        bail!("ffmpeg 报告成功但没有产出音频:{}", out.display());
    }
    Ok(())
}

/// 视频里有没有音轨。
pub fn has_audio(video: &Path) -> Result<bool> {
    let info = vidbrief_core::ffmpeg::ffprobe_json(video)?;
    Ok(vidbrief_core::has_audio_stream(&info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::RelPath;

    fn meta() -> VideoMeta {
        VideoMeta {
            path: RelPath::new("source/video.mp4").unwrap(),
            duration: 10.0,
            width: Some(640),
            height: Some(360),
            fps: Some(30.0),
            codec: Some("h264".into()),
            size_bytes: 1,
            sha256: "0".repeat(64),
            probed_at: vidbrief_project::now(),
            prober: "test".into(),
        }
    }

    fn cfg() -> Config {
        Config {
            bin: None,
            model: std::path::PathBuf::from("tools/whisper/models/ggml-small.bin"),
            language: "zh".into(),
            threads: None,
        }
    }

    #[test]
    fn srt_becomes_a_transcript_with_no_corrections() {
        let text = "1\n00:00:00,000 --> 00:00:02,000\n各位好\n\n2\n00:00:02,000 --> 00:00:06,000\n我们今天这期视频\n";
        let t = from_srt(text, &meta(), &cfg()).unwrap();

        assert_eq!(t.model, "ggml-small");
        assert_eq!(t.language, "zh");
        assert_eq!(t.duration, 10.0, "时长取自 video.meta,不是从字幕推");
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0].text, "各位好");

        // 原始产出必须是"干净的":纠错字段全空,否则"agent 改了什么"就查不出来了
        assert!(t.is_untouched(), "刚转出来的转录不该带任何纠错标记");
        for seg in &t.segments {
            assert!(seg.corrected_text.is_none());
            assert!(seg.correction.is_none());
            assert_eq!(seg.effective_text(), seg.text);
        }
    }

    #[test]
    fn segment_ids_follow_the_srt_numbers() {
        let text = "7\n00:00:00,000 --> 00:00:01,000\n甲\n";
        let t = from_srt(text, &meta(), &cfg()).unwrap();
        assert_eq!(t.segments[0].id, 7, "id 应当沿用 SRT 序号");
    }

    #[test]
    fn garbage_srt_is_an_error_not_a_silent_empty_transcript() {
        assert!(from_srt("什么都没有", &meta(), &cfg()).is_err());
    }
}
