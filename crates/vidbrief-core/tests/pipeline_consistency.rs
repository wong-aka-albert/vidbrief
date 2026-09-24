//! **流式分析**与**单帧抽取**必须看到同一帧 —— 逐字节钉住。
//!
//! # 为什么需要这条测试
//!
//! 步 1 有两条独立的取帧路径:
//!
//! 1. `build_curve` 用一条 ffmpeg 流水线按秒抽帧、**流式**算分;
//! 2. `extract_frame` / `frame_at` 按时刻**单独**抽一帧。
//!
//! 它们必须落在**同一帧**上,否则"这张截图对应那个分数"就是假的。
//! 而这件事**真的错过**:`build_curve` 原来用 ffmpeg 的 `fps=1` 滤镜,它把输出序号 i
//! 归到区间**末尾**那一帧(25fps 素材上是 t≈i+0.46),而 `extract_frame(t=i)` 取的是
//! t=i 那一帧。于是差半秒。
//!
//! 硬切换正好落在整秒时**看不出来**(两侧都在同一个镜头里,截图仍然是对的),
//! 所以 M1 的验收测试全绿也没抓到它。只有把两条路径的输出**逐字节比**才会暴露。
//!
//! # 怎么测
//!
//! 对若干采样点 i,分别用 `frame_at` 取 `i-1` 与 `i` 两帧,用**纯函数** `score` 算一遍,
//! 必须等于 `build_curve` 流式算出的 `changes[i].score`。
//! 两条路径只要错开一帧,分数就对不上(素材里相邻帧的差异很大)。

use std::path::{Path, PathBuf};

use vidbrief_core::{CurveOptions, FrameFeature};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("test_sample.mp4")
}

#[test]
fn streaming_analysis_and_single_frame_extraction_agree() {
    let video = fixture();
    assert!(video.is_file(), "回归素材不见了:{}", video.display());

    let meta = vidbrief_core::probe(
        &video,
        vidbrief_project::RelPath::new("source/video.mp4").unwrap(),
    )
    .expect("探测失败");
    let opts = CurveOptions::for_video(&meta);

    let curve = vidbrief_core::build_curve(
        &video,
        vidbrief_project::RelPath::new("source/video.mp4").unwrap(),
        meta.duration,
        opts,
    )
    .expect("抽帧失败");

    // 逐点比会跑 60 次 ffmpeg;挑覆盖率足够的一批(静止段、动画段、三个切换点)
    let indices = [1usize, 2, 4, 8, 12, 16, 19, 20, 21, 30, 42, 50, 55];

    let mut checked = 0;
    for &i in &indices {
        let Some(expected) = curve.changes.get(i).map(|c| c.score) else {
            continue;
        };
        let prev = grab(&video, opts, i as f64 - 1.0);
        let cur = grab(&video, opts, i as f64);
        let measured = vidbrief_core::score(&prev, &cur);

        assert!(
            (measured - expected).abs() < 1e-9,
            "采样点 i={i}(t={:.1}s)对不上:\n  流式算的 {expected:.9}\n  单帧算的 {measured:.9}\n\
             两边差了一帧就说明抽帧路径不一致",
            i as f64 / opts.fps
        );
        checked += 1;
    }
    assert!(checked >= 10, "有效比对点太少");
}

#[test]
fn sample_timestamps_are_exact_integer_seconds() {
    // 上一条测的是"两条路径一致";这一条测"记下来的 t 是不是真话"。
    // 采样表达式应当让第 i 个点的帧正好在 t = i/fps。
    let video = fixture();
    let meta = vidbrief_core::probe(
        &video,
        vidbrief_project::RelPath::new("source/video.mp4").unwrap(),
    )
    .unwrap();
    let opts = CurveOptions::for_video(&meta);
    let curve = vidbrief_core::build_curve(
        &video,
        vidbrief_project::RelPath::new("source/video.mp4").unwrap(),
        meta.duration,
        opts,
    )
    .unwrap();

    for (i, point) in curve.changes.iter().enumerate() {
        let expected = i as f64 / opts.fps;
        assert!(
            (point.t - expected).abs() < 1e-9,
            "第 {i} 个采样点记的是 t={},应当是 {expected}",
            point.t
        );
    }
}

fn grab(video: &Path, opts: CurveOptions, t: f64) -> FrameFeature {
    let raw = vidbrief_core::extract::frame_at(video, t, opts.width, opts.height)
        .unwrap_or_else(|e| panic!("取 t={t} 的帧失败:{e:#}"));
    FrameFeature::from_rgb24(&raw, opts.width, opts.height)
}
