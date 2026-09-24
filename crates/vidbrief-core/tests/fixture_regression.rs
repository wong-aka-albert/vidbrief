//! 验收测试:用 `fixtures/test_sample.mp4` 验证步 1 的算法。
//!
//! 这个视频是**合成的**,切换点是**已知的**:`t = 20 / 30 / 50`(见 `AGENTS.md` 材料索引)。
//! 这是 M1 的验收标准 —— 三个点必须都命中。
//!
//! 会真的跑 ffmpeg(60 秒的片子,几百毫秒),所以标 `#[ignore]` 吗?**不标**:
//! 它是验收标准,不是可选检查。跑不动 ffmpeg 的机器应当知道自己跑不动。

use std::path::{Path, PathBuf};

use vidbrief_core::{CurveOptions, plan_shots};
use vidbrief_project::{RegionParams, RegionReason, ShotOrigin};

/// 已知切换点。合成视频在这三个时刻整幅换画面。
const SWITCH_POINTS: &[f64] = &[20.0, 30.0, 50.0];

/// 命中容差(秒)。采样间隔 1 秒,所以 ±1 秒内就算命中。
const TOLERANCE: f64 = 1.0;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("test_sample.mp4")
}

/// 跑一遍分析与规划(不落盘、不抽图)。
fn analyze() -> (vidbrief_project::ChangeCurve, vidbrief_project::DenseRegions) {
    let video = fixture();
    assert!(video.is_file(), "回归素材不见了:{}", video.display());

    let rel = vidbrief_project::RelPath::new("source/video.mp4").unwrap();
    let meta = vidbrief_core::probe(&video, rel).expect("ffprobe 探测失败");
    assert!(
        (meta.duration - 60.0).abs() < 1.0,
        "素材应为 60 秒,实测 {}",
        meta.duration
    );

    vidbrief_core::analyze(&video, &meta, CurveOptions::for_video(&meta), RegionParams::default())
        .expect("分析失败")
}

#[test]
fn the_three_known_switch_points_are_all_detected() {
    let (curve, regions) = analyze();

    // 采样点数应当与时长对得上
    assert!(
        (curve.changes.len() as f64 - 60.0).abs() <= 2.0,
        "1fps 采样 60 秒应有约 60 个点,实际 {}",
        curve.changes.len()
    );

    let planned = plan_shots(&regions, &curve);
    assert!(!planned.is_empty(), "没有规划出任何截图");

    for &switch in SWITCH_POINTS {
        let hit = planned
            .iter()
            .find(|p| (p.t - switch).abs() <= TOLERANCE);
        assert!(
            hit.is_some(),
            "切换点 t={switch} 没命中。检出的时刻:{:?}\n区域:{:#?}",
            planned.iter().map(|p| p.t).collect::<Vec<_>>(),
            regions.regions
        );
    }
}

#[test]
fn switch_points_show_up_as_score_spikes() {
    // 比"命中截图"更根本的一层:差帧图本身在那三个时刻就得是尖峰
    let (curve, _) = analyze();
    for &switch in SWITCH_POINTS {
        let point = curve
            .changes
            .iter()
            .min_by(|a, b| {
                (a.t - switch)
                    .abs()
                    .partial_cmp(&(b.t - switch).abs())
                    .unwrap()
            })
            .expect("曲线非空");
        assert!(
            point.score >= 0.25,
            "t≈{switch} 的分数只有 {:.3},够不上硬切换门槛(0.25);整条曲线前 5 高:{:?}",
            point.score,
            top_scores(&curve, 5)
        );
    }
}

#[test]
fn only_the_known_switch_points_are_reported() {
    // 这条是**验收的核心**:既要三个都命中(不能漏),也不能多报(不能假阳)。
    // 合成素材前半段有个每 4 秒一次的周期性小动画(0.26–0.28),正是它逼出了
    // "突出度"这条判据 —— 光有 peak_threshold = 0.25 会把它们全报成硬切换。
    let (curve, regions) = analyze();
    let planned = plan_shots(&regions, &curve);
    let times: Vec<f64> = planned.iter().map(|p| p.t).collect();

    // 不漏:三个切换点都有截图
    for &switch in SWITCH_POINTS {
        assert!(
            times.iter().any(|t| (t - switch).abs() <= TOLERANCE),
            "切换点 t={switch} 没命中;实际检出 {times:?}"
        );
    }

    // 不假阳:每个检出的时刻都必须落在某个切换点附近
    for t in &times {
        assert!(
            SWITCH_POINTS.iter().any(|s| (t - s).abs() <= TOLERANCE),
            "t={t} 不是切换点,却是被当成了关键帧。全部检出:{times:?}\n区域:{:#?}",
            regions.regions
        );
    }

    assert_eq!(
        regions.regions.len(),
        SWITCH_POINTS.len(),
        "这个素材只有三个真的变化时刻:{:#?}",
        regions.regions
    );
}

#[test]
fn shot_origins_are_consistent_with_region_reasons() {
    let (curve, regions) = analyze();
    for planned in plan_shots(&regions, &curve) {
        match planned.origin {
            ShotOrigin::DenseRegion => {
                let id = planned.region_id.expect("密集区截图必须带 region_id");
                let region = regions
                    .regions
                    .iter()
                    .find(|r| r.id == id)
                    .unwrap_or_else(|| panic!("指向了不存在的区域 {id}"));
                assert_eq!(region.reason, RegionReason::HighDensity);
            }
            ShotOrigin::IsolatedPeak => {
                let id = planned.region_id.expect("算法产物必须带 region_id");
                let region = regions.regions.iter().find(|r| r.id == id).unwrap();
                assert_ne!(
                    region.reason,
                    RegionReason::HighDensity,
                    "孤立峰不该挂在密集区上"
                );
            }
            other => panic!("步 1 不该产出 {other:?} 这种来源的截图"),
        }
    }
}

#[test]
#[ignore = "诊断用:cargo test -p vidbrief-core -- --ignored --nocapture 看曲线全貌"]
fn dump_curve_for_inspection() {
    let (curve, regions) = analyze();
    println!("采样点 {} 个,步长 {:?}s", curve.changes.len(), curve.step());
    println!("\n时刻  分数");
    for c in &curve.changes {
        let bar = "#".repeat((c.score * 30.0).round() as usize);
        println!("{:>4.1}  {:.3} {bar}", c.t, c.score);
    }
    println!("\n区域 {} 个:", regions.regions.len());
    for r in &regions.regions {
        println!(
            "  #{} {:>5.1}..{:<5.1} peak_t={:<5.1} peak={:.3} mass={:.2} {:?}",
            r.id, r.start, r.end, r.peak_t, r.peak_score, r.mass, r.reason
        );
    }
    println!("\n阈值: median={:.4} threshold={:.4}", regions.stats.mass_median, regions.stats.mass_threshold);
    println!("规划截图:{:?}", plan_shots(&regions, &curve).iter().map(|p| p.t).collect::<Vec<_>>());
}

fn top_scores(curve: &vidbrief_project::ChangeCurve, n: usize) -> Vec<(f64, f64)> {
    let mut all: Vec<(f64, f64)> = curve.changes.iter().map(|c| (c.t, c.score)).collect();
    all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    all.into_iter().take(n).collect()
}
