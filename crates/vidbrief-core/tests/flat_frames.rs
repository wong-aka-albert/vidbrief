//! **纯色帧预标弃用**(不是不截) —— 用真的 ffmpeg 生成两段视频来验。
//!
//! 判据分两半:纯函数那半(直方图 + 亮度方差)在 `feature.rs` 的单测里;
//! 另一半是"真的从视频里取到那一帧、真的算出来、真的没落盘" —— 那条路只有 ffmpeg 走得通。
//!
//! 素材是**当场生成**的,不用 `fixtures/`:这里要的就是"纯色"这种极端输入,
//! 而手头那段测试片里没有纯色段(`fixtures/test_sample.mp4` 是三个切换点的合成片)。

use std::path::{Path, PathBuf};

use vidbrief_core::extract::flatness_at;
use vidbrief_core::shots::PlannedShot;
use vidbrief_core::{ShotQuality, extract_shots};
use vidbrief_project::{Layout, ShotOrigin};

/// 用 ffmpeg 生成一段视频。`source` 是 lavfi 的输入描述(如 `color=c=black:s=320x180:d=4`)。
fn make_video(dir: &Path, name: &str, source: &str) -> PathBuf {
    let out = dir.join(name);
    let args: Vec<std::ffi::OsString> = [
        "-v",
        "error",
        "-f",
        "lavfi",
        "-i",
        source,
        "-pix_fmt",
        "yuv420p",
        "-y",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .chain(std::iter::once(out.clone().into_os_string()))
    .collect();
    vidbrief_core::ffmpeg::run_ffmpeg(&args).expect("生成测试视频失败(要 ffmpeg 在 PATH 上)");
    assert!(out.is_file(), "ffmpeg 说成功但没产出:{}", out.display());
    out
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vidbrief-flat-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 抽一张(计划里就一帧),返回 `(登记进清单的图, 抽帧失败的理由)`。
fn try_extract(
    video: &Path,
    layout: &Layout,
    at: f64,
) -> (Vec<vidbrief_project::Shot>, Vec<String>) {
    let planned = vec![PlannedShot {
        t: at,
        origin: ShotOrigin::DenseRegion,
        region_id: Some(1),
    }];
    let (shots, skipped) = extract_shots(
        video,
        layout,
        at + 1.0,
        &planned,
        ShotQuality::default(),
        &mut |_, _| {},
    )
    .expect("抽帧不该报错");
    (
        shots.shots,
        skipped.into_iter().map(|(_, why)| why).collect(),
    )
}

/// 纯色视频:**图还是截下来**(那是事件的证据),但**预标弃用** + 写明为什么。
#[test]
fn a_solid_video_is_kept_as_material_but_pre_dropped() {
    let dir = workdir("black");
    let video = make_video(&dir, "black.mp4", "color=c=black:s=320x180:d=4");
    let layout = Layout::new(&dir);
    layout.create_dirs().expect("建项目骨架");

    let (shots, reasons) = try_extract(&video, &layout, 2.0);
    assert!(reasons.is_empty(), "这不是抽帧失败,不该混进 skipped:{reasons:?}");
    assert_eq!(shots.len(), 1, "图要留着 —— 切黑本身是个事件");
    let shot = &shots[0];
    assert_eq!(
        shot.status,
        vidbrief_project::ShotStatus::Drop,
        "纯色帧该被预标成弃用(免得 agent 为它花一次看图)"
    );
    let why = shot.reason.as_deref().unwrap_or_default();
    assert!(why.contains("纯色帧"), "理由要写清是纯色:{why:?}");
    assert!(why.contains("update_shot"), "还要给出路(想留就改回来):{why:?}");

    // 文件在:清单里那一项不是空指
    let file = layout.rel(shot.file.as_str());
    assert!(file.is_file(), "截下来的图该在:{}", file.display());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 有内容的视频照常截 —— 这条是**误杀护栏**:判据收紧的方向不能把真图吃掉。
#[test]
fn a_real_picture_is_still_captured() {
    let dir = workdir("testsrc");
    let video = make_video(&dir, "pattern.mp4", "testsrc=size=320x180:rate=15:duration=4");
    let layout = Layout::new(&dir);
    layout.create_dirs().expect("建项目骨架");

    let (shots, reasons) = try_extract(&video, &layout, 2.0);
    assert!(reasons.is_empty(), "不该有跳过:{reasons:?}");
    assert_eq!(shots.len(), 1, "有内容的画照常截");
    assert_eq!(
        shots[0].status,
        vidbrief_project::ShotStatus::Keep,
        "有内容的画**不许**被预标弃用(误杀护栏)"
    );
    assert!(shots[0].reason.is_none(), "没被处置的图不该带理由");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 体检本身:纯色帧的标准差接近 0,画面帧离阈值有一倍以上的余量。
#[test]
fn flatness_tells_the_two_apart_with_room_to_spare() {
    let dir = workdir("flatness");
    let black = make_video(&dir, "black.mp4", "color=c=black:s=320x180:d=3");
    let pattern = make_video(&dir, "pattern.mp4", "testsrc=size=320x180:rate=15:duration=3");

    let flat = flatness_at(&black, 1.5).expect("体检黑帧");
    let real = flatness_at(&pattern, 1.5).expect("体检画面帧");
    assert!(flat.flat, "{flat:?}");
    assert!(!real.flat, "{real:?}");
    assert!(
        real.luma_stddev > flat.luma_stddev * 4.0,
        "两者该差得远(不然阈值稍微一动就误杀):{flat:?} vs {real:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
