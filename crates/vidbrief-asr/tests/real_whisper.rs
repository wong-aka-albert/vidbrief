//! 真实 whisper 链路的端到端测试。
//!
//! **标 `#[ignore]`**:它要拉起 whisper-cli、装载 466MB 的 ggml 模型,跑一次几十秒。
//! 放进常规 `cargo test` 会毁掉反馈环。需要时显式跑:
//!
//! ```bash
//! cargo test -p vidbrief-asr --offline -- --ignored --nocapture
//! ```
//!
//! 输入音频**不入库**:自备一段中文语音(16kHz 单声道 wav)放到
//! `tools/whisper/sample_zh.wav` 即可跑;当年那几份实测凭据就是这么来的。

use std::path::{Path, PathBuf};

use vidbrief_project::{Layout, RelPath, VideoMeta};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

fn whisper_ready() -> bool {
    repo_root().join("tools/whisper/Release/whisper-cli.exe").is_file()
        && repo_root().join("tools/whisper/models").is_dir()
}

fn sample_wav() -> PathBuf {
    repo_root().join("tools/whisper/sample_zh.wav")
}

fn meta_for(duration: f64) -> VideoMeta {
    VideoMeta {
        path: RelPath::new("source/video.wav").unwrap(),
        duration,
        width: None,
        height: None,
        fps: None,
        codec: None,
        size_bytes: 0,
        sha256: "0".repeat(64),
        probed_at: vidbrief_project::now(),
        prober: "test".into(),
    }
}

#[test]
#[ignore = "要拉 whisper-cli 与 466MB 模型,几十秒:cargo test -p vidbrief-asr -- --ignored"]
fn real_whisper_produces_a_usable_transcript() {
    if !whisper_ready() {
        eprintln!(
            "跳过:{} 或 models/ 不存在",
            repo_root().join("tools/whisper/Release").display()
        );
        return;
    }
    let wav = sample_wav();
    assert!(wav.is_file(), "测试音频不见了:{}", wav.display());

    // 模型与二进制都在仓库里 —— 显式指过去,别指望使用者已经设好环境变量
    let cfg = vidbrief_asr::Config {
        bin: Some(repo_root().join("tools/whisper/Release/whisper-cli.exe")),
        model: repo_root().join("tools/whisper/models/ggml-small.bin"),
        language: "zh".into(),
        threads: None,
    };
    assert!(cfg.model.is_file(), "模型不见了:{}", cfg.model.display());

    let tmp = repo_root()
        .join("target")
        .join("vidbrief-test")
        .join(format!("asr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let layout = Layout::new(&tmp);
    layout.create_dirs().unwrap();

    let started = std::time::Instant::now();
    let transcript = vidbrief_asr::transcribe(&wav, &layout, &meta_for(11.0), &cfg)
        .expect("whisper 转录失败")
        .expect("这个 wav 有音轨,不该返回 None");
    eprintln!("whisper 用时 {:?}", started.elapsed());

    // 副产物都落盘了
    for ext in ["srt", "txt", "vtt"] {
        let p = layout.preprocess_dir().join(format!("transcript.{ext}"));
        assert!(p.is_file(), "{ext} 没写出来:{}", p.display());
    }
    assert!(layout.transcript().is_file(), "transcript.json 没写出来");
    // 中间 WAV 不该留在项目里
    assert!(
        !layout.preprocess_dir().join("audio.wav").exists(),
        "中间音频文件应当被清掉"
    );

    // 内容层面
    assert!(!transcript.segments.is_empty(), "一段都没转出来");
    assert!(transcript.is_untouched(), "原始转录不该带纠错标记");
    eprintln!(
        "模型 {} / {} 段 / 首段 {:.1}-{:.1}s:{}",
        transcript.model,
        transcript.segments.len(),
        transcript.segments[0].start,
        transcript.segments[0].end,
        transcript.segments[0].text
    );
    for seg in transcript.segments.iter().take(5) {
        eprintln!("  ({:.1}-{:.1}) {}", seg.start, seg.end, seg.text);
    }

    // 时间戳必须落在音频时长内,且不倒退
    let mut prev_end = -1.0;
    for seg in &transcript.segments {
        assert!(seg.start >= prev_end - 0.001, "段 {} 与前一段重叠", seg.id);
        assert!(seg.end >= seg.start);
        assert!(
            seg.end <= 12.0,
            "段 {} 的结束时间 {} 超出了音频时长",
            seg.id,
            seg.end
        );
        prev_end = seg.end;
    }

    // 中文识别:至少得有一句是中文,否则说明语言参数或模型不对
    let has_cjk = transcript
        .segments
        .iter()
        .any(|s| s.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
    assert!(has_cjk, "没转出任何汉字,检查 -l zh 与模型:\n{:#?}", transcript.segments);

    let _ = std::fs::remove_dir_all(&tmp);
}
