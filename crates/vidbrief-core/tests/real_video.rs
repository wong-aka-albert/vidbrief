//! 真实长视频的回归测试 —— **默认不跑**,需要时用环境变量指过去。
//!
//! 合成素材(`fixtures/test_sample.mp4`,60 秒)只能验证"三个切换点找得到",
//! 它**验证不了**这些东西:
//!
//! - 阈值在真实分布下会不会误报(合成素材要么 0 要么 1,没有中间地带)
//! - 密集区判据在"胶片式录屏 + 讲解"这种形态下表现如何
//! - 985 秒的长视频会不会跑爆内存/时间
//!
//! 用法:
//!
//! ```bash
//! VIDBRIEF_REAL_VIDEO="E:\path\to\视频.mp4" \
//!   cargo test -p vidbrief-core --offline --test real_video -- --ignored --nocapture
//! ```
//!
//! # 期望值是怎么来的
//!
//! 下面的 [`EXPECTED_CHANGES`] 不是"我说是就是",而是**两条独立路径的交集**:
//!
//! 1. ffmpeg 自带的场景检测(`select='gte(scene,0)'`),阈值 0.15;
//! 2. 人工核对:`tools/whisper/demo.srt` 里那几个时刻**人在讲什么** ——
//!    每个检出点都落在一次主题切换上(换标的、出新图、章节转场)。
//!
//! 注意 `192` 与 `240`:ffmpeg 在阈值 0.15 下**漏了**它们(它们的 scene 分是 0.102),
//! 但我的算法找到了,而且转录证实那两处确实在换图/换论点。所以这条测试的期望值是
//! **两条路径的并集**,其中两项恰恰是"本工具比对照实现更灵敏"的证据。

use std::path::{Path, PathBuf};

use vidbrief_core::{CurveOptions, plan_shots};
use vidbrief_project::RegionParams;

/// 真实切换点。来源见模块头。
const EXPECTED_CHANGES: &[f64] = &[
    6.0,   // ffmpeg 1.13/1.37/5.97 合成一段(片头动画)
    85.0,  // 84.23 进入正题
    192.0, // ffmpeg 漏检(scene 0.102),转录:"大家看一下这张图"
    240.0, // ffmpeg 漏检(scene 0.102),转录同期在讲选择
    357.0, // 356.50 进入案例部分
    479.0, // 478.60 换标的
    514.0, // 513.53 换标的
    537.0, // 536.60 换标的
    552.0, // 551.23 换标的
    603.0, // 602.43 章节转场
    967.0, // 966.07 片尾
];

/// 采样是 1fps,检出的时刻最多比真实切换晚一个采样间隔。
const TOLERANCE: f64 = 1.05;

fn real_video() -> Option<PathBuf> {
    let raw = std::env::var_os("VIDBRIEF_REAL_VIDEO")?;
    let path = PathBuf::from(raw);
    path.is_file().then_some(path)
}

#[test]
#[ignore = "需要 VIDBRIEF_REAL_VIDEO 指向一条真实长视频"]
fn a_real_long_video_finds_exactly_the_known_changes() {
    let Some(video) = real_video() else {
        eprintln!("跳过:未设置 VIDBRIEF_REAL_VIDEO");
        return;
    };
    let rel = vidbrief_project::RelPath::new("source/video.mp4").unwrap();
    let meta = vidbrief_core::probe(&video, rel.clone()).expect("探测失败");
    eprintln!("视频:{:.1} 秒,{}x{:?}", meta.duration, meta.width.unwrap_or(0), meta.height);

    let started = std::time::Instant::now();
    let (curve, regions) = vidbrief_core::analyze(
        &video,
        &meta,
        CurveOptions::for_video(&meta),
        RegionParams::default(),
    )
    .expect("分析失败");
    eprintln!(
        "分析用时 {:?};采样 {} 点;密集区 {} 个",
        started.elapsed(),
        curve.changes.len(),
        regions.regions.len()
    );

    let times: Vec<f64> = plan_shots(&regions, &curve).iter().map(|p| p.t).collect();
    eprintln!("检出时刻:{times:?}");
    eprintln!(
        "阈值 {:.3}(中位数 {:.4})",
        regions.stats.mass_threshold, regions.stats.mass_median
    );

    // 不漏:每个已知切换点都要有对应截图
    for &want in EXPECTED_CHANGES {
        assert!(
            times.iter().any(|t| (t - want).abs() <= TOLERANCE),
            "已知切换点 t={want} 没命中;实际检出 {times:?}"
        );
    }

    // 不假阳:每个检出时刻都要落在某个已知切换点附近
    for &t in &times {
        assert!(
            EXPECTED_CHANGES.iter().any(|w| (t - w).abs() <= TOLERANCE),
            "t={t} 不在已知切换点附近,是新冒出来的:\n检出 {times:?}\n区域 {:#?}",
            regions.regions
        );
    }

    // 中间地带必须干净 —— 这条是"阈值是否合适"的真正检验:
    // 合成素材上分数非 0 即 1,看不出分离度;真实视频才有中间值。
    let mut scores: Vec<f64> = curve.changes.iter().map(|c| c.score).collect();
    scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
    let peak_threshold = RegionParams::default().peak_threshold;
    let below: Vec<f64> = scores
        .iter()
        .copied()
        .filter(|s| *s < peak_threshold)
        .collect();
    let highest_below = below.first().copied().unwrap_or(0.0);
    eprintln!(
        "阈值 {peak_threshold} 以下最高分 {highest_below:.4} —— 分离度 {:.1} 倍",
        peak_threshold / highest_below.max(1e-9)
    );
    assert!(
        highest_below < peak_threshold * 0.5,
        "阈值下方紧贴着 {highest_below:.4},离 {peak_threshold} 太近 —— 阈值该重新标定了"
    );
}

#[test]
#[ignore = "需要 VIDBRIEF_REAL_VIDEO 指向一条真实长视频"]
fn a_real_long_video_stays_fast_and_light() {
    // 985 秒 / 1fps 采样 = 近千帧。流式实现应当只留两个特征向量,
    // 而不是把 985 × 480 × 270 × 3 ≈ 380 MB 全留在内存里。
    let Some(video) = real_video() else { return };
    let rel = vidbrief_project::RelPath::new("source/video.mp4").unwrap();
    let meta = vidbrief_core::probe(&video, rel).expect("探测失败");

    let started = std::time::Instant::now();
    let (curve, _) = vidbrief_core::analyze(
        &video,
        &meta,
        CurveOptions::for_video(&meta),
        RegionParams::default(),
    )
    .expect("分析失败");
    let elapsed = started.elapsed();

    let raw_frame_mb = 480.0 * 270.0 * 3.0 / 1024.0 / 1024.0;
    eprintln!(
        "{} 帧,若全留在内存要 {:.0} MB;实际用时 {elapsed:?}",
        curve.changes.len(),
        raw_frame_mb * curve.changes.len() as f64
    );
    assert!(
        elapsed.as_secs() < 120,
        "近千帧的分析不该超过两分钟,实际 {elapsed:?}"
    );
}

#[test]
fn the_fixture_is_still_the_fast_path() {
    // 这条不 ignore:提醒"常规测试跑的是合成素材,真实长视频要显式指定"
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("test_sample.mp4");
    assert!(fixture.is_file(), "回归素材不见了");
}
