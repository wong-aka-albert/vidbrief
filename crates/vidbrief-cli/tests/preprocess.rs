//! `vidbrief preprocess` 的端到端测试。
//!
//! 这是 M1 验收的**落盘版**:真的起进程、真的跑 ffmpeg、真的写出四个文件。
//!
//! 最有价值的一条是 [`real_output_passes_the_schemas`] —— 之前的 schema 校验都在
//! "工具自己构造的假数据"上跑(M0 的 `writer_output_validates_against_the_schemas`),
//! 这里换成**真跑出来的文件**。契约要真的被生产者满足才算数。
//!
//! 转录一律用 `--no-transcribe`:回归素材没有音轨,而且 whisper 动辄几十秒,
//! 不该拖慢常规测试。ASR 有它自己的单测(见 `vidbrief-asr`)。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vidbrief_project::{ChangeCurve, DenseRegions, Layout, Project, Shots};

/// 已知切换点,与 `crates/vidbrief-core/tests/fixture_regression.rs` 一致。
const SWITCH_POINTS: &[f64] = &[20.0, 30.0, 50.0];
const TOLERANCE: f64 = 0.01;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

/// 同 `tests/init.rs`:放仓库 `target/` 下,与素材同卷。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = repo_root()
            .join("target")
            .join("vidbrief-test")
            .join(format!("preprocess-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    /// 素材**副本**(别硬链仓库里的 fixtures —— 硬链接会穿透)。
    fn stage_video(&self) -> PathBuf {
        let name = repo_root()
            .join("fixtures")
            .join("test_sample.mp4")
            .file_name()
            .expect("素材没有文件名")
            .to_owned();
        let dst = self.0.join(name);
        std::fs::copy(repo_root().join("fixtures").join("test_sample.mp4"), &dst)
            .expect("无法复制素材");
        dst
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("无法启动 vidbrief")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// 跑完 init + preprocess,返回 (临时目录, Layout)。
fn prepared(tag: &str) -> (TempDir, Layout) {
    let tmp = TempDir::new(tag);
    let video = tmp.stage_video();

    let out = run_in(&tmp.0, &["init", video.to_str().unwrap()]);
    assert!(out.status.success(), "init 失败:\n{}", stderr(&out));

    let layout = Layout::for_project(tmp.0.join("projects"), "test-sample");
    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-transcribe"]);
    assert!(out.status.success(), "preprocess 失败:\n{}", stderr(&out));
    (tmp, layout)
}

#[test]
fn preprocess_writes_all_four_preprocess_files() {
    let (_tmp, layout) = prepared("files");

    for (name, path) in [
        ("change_curve.json", layout.change_curve()),
        ("dense_regions.json", layout.dense_regions()),
        ("shots.json", layout.shots_json()),
    ] {
        assert!(path.is_file(), "{name} 没写出来:{}", path.display());
    }
    assert!(layout.shots_dir().is_dir(), "截图目录没建出来");

    // 截图数量与清单一致,而且每张都真的落了盘
    let shots: Shots = vidbrief_project::read(&layout.shots_json()).unwrap();
    assert!(!shots.shots.is_empty(), "一张截图都没有");
    for shot in &shots.shots {
        let path = layout.rel(shot.file.as_str());
        assert!(path.is_file(), "清单里有 {} 但文件不在:{}", shot.id, path.display());
        assert!(
            std::fs::metadata(&path).unwrap().len() > 0,
            "{} 是空文件",
            shot.id
        );
    }

    // 没有音轨 → 不该产出 transcript.json
    assert!(
        !layout.transcript().exists(),
        "素材没有音轨,不该写出 transcript.json"
    );
}

#[test]
fn real_output_passes_the_schemas() {
    // **M1 最有价值的一条测试**:M0 只验证过"工具构造的假数据"合 schema,
    // 这里验证**真跑出来的文件**合 schema。契约要真的被生产者满足才算数。
    let (_tmp, layout) = prepared("schema");
    let schema_dir = repo_root().join("docs").join("schema");

    for (file, schema) in [
        (layout.change_curve(), "change_curve.schema.json"),
        (layout.dense_regions(), "dense_regions.schema.json"),
        (layout.shots_json(), "shots.schema.json"),
        (layout.project_json(), "project.schema.json"),
    ] {
        let instance: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        let schema_value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(schema_dir.join(schema)).unwrap()).unwrap();
        let validator = jsonschema::validator_for(&schema_value).unwrap();

        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| format!("  - {e}  (位置: {})", e.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{} 未通过 {} 校验:\n{}",
            file.display(),
            schema,
            errors.join("\n")
        );
    }
}

#[test]
fn the_three_switch_points_survive_all_the_way_to_disk() {
    let (_tmp, layout) = prepared("switches");
    let shots: Shots = vidbrief_project::read(&layout.shots_json()).unwrap();
    let times: Vec<f64> = shots.shots.iter().map(|s| s.t).collect();

    for &switch in SWITCH_POINTS {
        assert!(
            times.iter().any(|t| (t - switch).abs() <= TOLERANCE),
            "切换点 t={switch} 没有对应的截图;实际 {times:?}"
        );
    }
    // 不该有假阳:每个时刻都得落在某个切换点附近
    for t in &times {
        assert!(
            SWITCH_POINTS.iter().any(|s| (t - s).abs() <= TOLERANCE),
            "t={t} 不是切换点却被抽了图:{times:?}"
        );
    }
}

#[test]
fn curve_and_regions_are_internally_consistent() {
    let (_tmp, layout) = prepared("consistent");
    let curve: ChangeCurve = vidbrief_project::read(&layout.change_curve()).unwrap();
    let regions: DenseRegions = vidbrief_project::read(&layout.dense_regions()).unwrap();
    let shots: Shots = vidbrief_project::read(&layout.shots_json()).unwrap();

    assert_eq!(curve.changes.len(), 60, "60 秒 1fps 应有 60 个采样点");
    assert_eq!(
        regions.stats.sampled_points as usize,
        curve.changes.len(),
        "stats 的采样点数必须与曲线一致"
    );
    assert_eq!(
        regions.stats.mass_threshold,
        regions.params.mass_threshold(regions.stats.mass_median)
    );
    assert_eq!(curve.metric, vidbrief_project::curve::DEFAULT_METRIC);

    // 每张图的文件名、时间、来源都要自洽
    for shot in &shots.shots {
        assert_eq!(
            shot.file.file_name(),
            vidbrief_project::Shot::file_name(shot.id, shot.t).unwrap()
        );
        assert!(shot.t <= curve.duration);
        if let Some(rid) = shot.region_id {
            assert!(
                regions.regions.iter().any(|r| r.id == rid),
                "{} 指向了不存在的区域 {rid}",
                shot.id
            );
        }
    }
}

#[test]
fn preprocess_marks_the_step_done_in_project_json() {
    let (_tmp, layout) = prepared("progress");
    let project = Project::load(&layout.project_json()).unwrap();
    assert!(project.progress.preprocess.done, "跑完了就该标记");
    assert!(project.progress.preprocess.at.is_some());
    assert!(project.rev > 1, "写了 project.json 就该推进 rev");
}

#[test]
fn status_reflects_a_finished_preprocess() {
    let (tmp, _layout) = prepared("via-status");
    let out = run_in(&tmp.0, &["status", "test-sample"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("① 前处理   ✓"), "status 该显示前处理已完成:\n{text}");
}

#[test]
fn flags_can_narrow_the_work() {
    let tmp = TempDir::new("flags");
    let video = tmp.stage_video();
    assert!(run_in(&tmp.0, &["init", video.to_str().unwrap()]).status.success());
    let layout = Layout::for_project(tmp.0.join("projects"), "test-sample");

    // 只出差帧图与密集区
    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-shots", "--no-transcribe"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(layout.change_curve().is_file());
    assert!(layout.dense_regions().is_file());
    assert!(!layout.shots_json().is_file(), "--no-shots 不该写截图清单");

    // 换采样参数
    let out = run_in(&tmp.0, &["preprocess", "test-sample", "-f", "2", "-w", "320", "--no-shots", "--no-transcribe"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let curve: ChangeCurve = vidbrief_project::read(&layout.change_curve()).unwrap();
    assert_eq!(curve.fps_sampled, 2.0);
    assert_eq!(curve.width, 320);
    assert!(
        curve.changes.len() > 100,
        "2fps 采样 60 秒应有一百多个点,实际 {}",
        curve.changes.len()
    );
}

/// 转录失败**不算这一步完成**。
///
/// 这条来自一次真实踩坑:whisper 找不到时,`preprocess` 打印了"转录失败"却仍然写了
/// `preprocess.done = true` 并以**退出码 0** 结束 —— 脚本和 agent 会以为这一步做完了。
/// 进度标记是给人看的,标错了就是骗人。想只要视觉部分,请显式 `--no-transcribe`。
#[test]
fn a_failed_transcription_does_not_mark_the_step_done() {
    let tmp = TempDir::new("asr-fail");
    let video = tmp.stage_video();
    assert!(run_in(&tmp.0, &["init", video.to_str().unwrap()]).status.success());
    let layout = Layout::for_project(tmp.0.join("projects"), "test-sample");

    // ① 素材**没有音轨** → 合法跳过,算完成
    let out = run_in(&tmp.0, &["preprocess", "test-sample"]);
    assert!(out.status.success(), "无音轨应当仍算完成:\n{}", stderr(&out));
    assert!(stdout(&out).contains("没有音轨"), "{}", stdout(&out));
    let mut project = Project::load(&layout.project_json()).unwrap();
    assert!(project.progress.preprocess.done, "没音轨可转,这一步就算做完了");
    assert!(!layout.transcript().exists());

    // ② 换成**有音轨**的视频,但让 whisper 找不到 → 必须判失败
    let with_audio = tmp.0.join("with_audio.mp4");
    let made = Command::new("ffmpeg")
        .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=3"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=3"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest", "-y"])
        .arg(&with_audio)
        .output();
    match made {
        Ok(o) if o.status.success() => {}
        _ => {
            eprintln!("跳过 ②:本机 ffmpeg 造不出带音轨的测试片");
            return;
        }
    }

    let project_path = layout.rel(project.source.path.as_str());
    std::fs::remove_file(&project_path).unwrap();
    std::fs::copy(&with_audio, &project_path).unwrap();
    let meta = vidbrief_core::probe(&project_path, project.source.path.clone()).unwrap();
    meta.save(&layout.video_meta()).unwrap();

    project.progress.preprocess = Default::default();
    project.save(&layout.project_json()).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .current_dir(&tmp.0)
        .env("VIDBRIEF_WHISPER_BIN", "definitely-not-whisper")
        .env("VIDBRIEF_WHISPER_MODEL", "definitely-not-a-model.bin")
        .args(["preprocess", "test-sample"])
        .output()
        .expect("无法启动 vidbrief");

    assert!(
        !out.status.success(),
        "视频确实有音轨、whisper 又用不了,必须判失败:\n{}",
        stdout(&out)
    );
    let project = Project::load(&layout.project_json()).unwrap();
    assert!(
        !project.progress.preprocess.done,
        "转录没完成就不该标记步 1 已完成"
    );
    // 但视觉产出要保留 —— 重跑时不必重算差帧图
    assert!(layout.change_curve().is_file(), "已算好的差帧图不该被丢掉");
    assert!(layout.shots_json().is_file(), "已抽好的截图不该被丢掉");
}

#[test]
fn preprocess_help_and_bad_arguments() {
    let cwd = std::env::temp_dir();

    let out = run_in(&cwd, &["preprocess", "-h"]);
    assert!(out.status.success());
    let text = stdout(&out);
    for expected in ["--no-transcribe", "--no-shots", "-f, --fps"] {
        assert!(text.contains(expected), "用法里缺 `{expected}`:\n{text}");
    }

    let out = run_in(&cwd, &["preprocess"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("用法"), "{}", stderr(&out));

    let out = run_in(&cwd, &["preprocess", "x", "--fps", "abc"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("不是数字"), "{}", stderr(&out));

    let out = run_in(&cwd, &["preprocess", "x", "--fps", "0"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("必须为正数"), "{}", stderr(&out));

    let out = run_in(&cwd, &["preprocess", "nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("找不到项目"), "{}", stderr(&out));
}
