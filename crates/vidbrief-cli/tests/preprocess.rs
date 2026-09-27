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

/// 读项目状态(它现在住在 `state.bin` 里,不在 `project.json`)。
fn state_of(layout: &Layout) -> vidbrief_project::ProjectState {
    vidbrief_project::ProjectState::read_file(&layout.state_file())
        .expect("读状态")
        .expect("状态文件该在")
}

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

/// **跑完要留下一份运行记录** —— 那是「别的会话想知道跑到哪儿了」的唯一入口
/// (MCP 的 `preprocess_progress` / `preprocess_result` 读的就是它)。
///
/// 记录里最要紧的是两件事:**这一趟叫什么**(认人)与**跑完的完整交代**
/// (与命令直接返回的同一份,agent 不必学两种结果形状)。
#[test]
fn a_finished_run_leaves_a_record_of_itself() {
    let (_tmp, layout) = prepared("run-record");
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(layout.preprocess_dir().join("run.json")).unwrap(),
    )
    .expect("运行记录该是 JSON");

    assert_eq!(record["stage"], "done", "{record}");
    assert_eq!(record["error"], serde_json::Value::Null, "{record}");
    assert_eq!(
        record["options"]["transcribe"], false,
        "这一趟用的是哪套参数要记下来(不然两趟的记录长得一样):{record}"
    );
    assert!(
        record["run_id"].as_str().is_some_and(|id| id.len() >= 8),
        "要有名字才认得出是哪一趟:{record}"
    );
    assert!(record["pid"].as_u64().unwrap_or(0) > 0, "{record}");
    assert!(
        record["report"]["shots"].as_u64().unwrap_or(0) > 0,
        "跑完的交代要跟着记录一起留下:{record}"
    );
    assert_eq!(
        record["report"]["transcript"]["outcome"], "skipped",
        "报告就是命令/工具直接返回的那一份(这一趟带了 --no-transcribe):{record}"
    );
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
fn preprocess_marks_the_step_done_in_the_state() {
    let (_tmp, layout) = prepared("progress");
    let state = state_of(&layout);
    assert!(state.progress.preprocess.done, "跑完了就该标记");
    assert!(state.progress.preprocess.at.is_some());
    assert!(state.rev > 1, "写一次状态就该推进 rev");
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
    let project = Project::load(&layout.project_json()).unwrap();
    assert!(state_of(&layout).progress.preprocess.done, "没音轨可转,这一步就算做完了");
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

    // 把步 1 的进度清回去,验"失败就不标记"
    let mut state = state_of(&layout);
    state.progress.preprocess = Default::default();
    state.write(&layout.state_file()).unwrap();

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
    assert!(
        !state_of(&layout).progress.preprocess.done,
        "转录没完成就不该标记步 1 已完成"
    );
    // 但视觉产出要保留 —— 重跑时不必重算差帧图
    assert!(layout.change_curve().is_file(), "已算好的差帧图不该被丢掉");
    assert!(layout.shots_json().is_file(), "已抽好的截图不该被丢掉");

    // ③ **再跑一趟(whisper 还是坏的)**:做成的那部分**不该重做**。
    //
    // 这是「失败重试:接着跑,不是重来」那条的核心判据 —— 而它现在由**增量重跑**兜着:
    // 上一趟留下的画面材料是完整的一份,输入又没变,于是沿用、只重跑转录。
    let before = shot_stamps(&layout);
    let out = Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .current_dir(&tmp.0)
        .env("VIDBRIEF_WHISPER_BIN", "definitely-not-whisper")
        .env("VIDBRIEF_WHISPER_MODEL", "definitely-not-a-model.bin")
        .args(["preprocess", "test-sample"])
        .output()
        .expect("无法启动 vidbrief");
    assert!(!out.status.success(), "whisper 还是坏的,这一趟该照旧判失败");
    assert!(
        stdout(&out).contains("沿用上次那批"),
        "重试该说清画面是沿用的:\n{}",
        stdout(&out)
    );
    assert_eq!(shot_stamps(&layout), before, "重试把截图重抽了一遍");
    let report = run_report(&layout);
    assert_eq!(
        report["reuse"]["visual"],
        serde_json::Value::Bool(true),
        "画面那半是上一趟做成的,重试不该重做:{report}"
    );
}

#[test]
fn preprocess_help_and_bad_arguments() {
    let cwd = std::env::temp_dir();

    let out = run_in(&cwd, &["preprocess", "-h"]);
    assert!(out.status.success());
    let text = stdout(&out);
    for expected in ["--no-transcribe", "--no-shots", "--force", "-f, --fps"] {
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

// ── 增量重跑:输入没变就不重算,更不作废成果 ─────────────────────────

/// 每张截图的 (相对路径, 修改时间) —— 用来证明「一张都没重写」。
///
/// 为什么不拿"报告里说沿用了"当唯一判据:那是**自述**。文件是不是真没被碰,
/// 只有文件系统说了算;两条一起看,才算证明了"没重算"。
fn shot_stamps(layout: &Layout) -> Vec<(String, std::time::SystemTime)> {
    let shots = Shots::load(&layout.shots_json()).expect("读截图清单");
    let mut out: Vec<(String, std::time::SystemTime)> = shots
        .shots
        .iter()
        .map(|s| {
            let path = layout.rel(s.file.as_str());
            let at = std::fs::metadata(&path)
                .unwrap_or_else(|e| panic!("截图 {} 不在盘上:{e}", s.id))
                .modified()
                .expect("取不到修改时间");
            (s.file.as_str().to_string(), at)
        })
        .collect();
    out.sort();
    out
}

/// 读这一趟的运行记录里那份报告。
fn run_report(layout: &Layout) -> serde_json::Value {
    let text = std::fs::read_to_string(layout.preprocess_dir().join("run.json"))
        .expect("运行记录该在");
    let record: serde_json::Value = serde_json::from_str(&text).expect("记录该是 JSON");
    record["report"].clone()
}

/// **同样的输入再跑一遍:不重算、也不作废成果。**
///
/// 靶子是这条路上最贵的东西 —— 校准(逐张看图 + 逐段核验)。从前重跑步 1
/// 就把它全清掉,`shot_checks_cleared` / `segment_checks_cleared` 报的就是那个数。
#[test]
fn a_second_run_with_the_same_inputs_reuses_the_pictures() {
    let (tmp, layout) = prepared("reuse-same");
    let before = shot_stamps(&layout);
    assert!(!before.is_empty(), "第一次跑该留下截图");
    assert!(
        layout.preprocess_inputs().is_file(),
        "第一趟跑完该留下输入指纹({})",
        layout.preprocess_inputs().display()
    );

    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-transcribe"]);
    assert!(out.status.success(), "二跑失败:\n{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("沿用上次那批"), "没说清沿用了:\n{text}");
    assert_eq!(shot_stamps(&layout), before, "截图被重写了");

    let report = run_report(&layout);
    assert_eq!(report["reuse"]["visual"], serde_json::Value::Bool(true), "{report}");
    assert_eq!(
        report["reuse"]["visual_why_not"],
        serde_json::Value::Null,
        "沿用了就不该有「为什么没沿用」:{report}"
    );
    assert_eq!(report["shot_checks_cleared"], serde_json::json!(0), "{report}");
}

/// **改了 `fps`:画面重算,而且说得出是哪一项变的。**
///
/// 转录那半不在这一条里(它会跑 whisper,几十秒)—— 那半的判据在
/// `vidbrief_ops::preprocess` 的单测里,一条一条钉着。
#[test]
fn changing_fps_recomputes_the_pictures_and_says_why() {
    let (tmp, layout) = prepared("reuse-fps");
    let before = shot_stamps(&layout);

    let out = run_in(
        &tmp.0,
        &["preprocess", "test-sample", "--no-transcribe", "--fps", "2"],
    );
    assert!(out.status.success(), "重跑失败:\n{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("画面重算"), "没说清重算了:\n{text}");
    assert!(
        text.contains("fps"),
        "要指名道姓说是哪一项变的:\n{text}"
    );
    assert_ne!(shot_stamps(&layout), before, "改了 fps 就该重抽截图");

    let report = run_report(&layout);
    assert_eq!(report["reuse"]["visual"], serde_json::Value::Bool(false), "{report}");
    let why = report["reuse"]["visual_why_not"].as_str().unwrap_or_default();
    assert!(why.contains("fps"), "{why}");
}

/// **源视频被就地改过:不得复用,而且要把这件事说出来。**
///
/// 硬链接场景下源文件与项目里那份是同一份数据 —— 源被改了,项目里的材料就全不可信。
/// 判据是**当场重算**的 sha256,不是 `project.json` 里 init 时记的那个。
#[test]
fn a_changed_source_video_forces_a_recompute_and_says_so() {
    let (tmp, layout) = prepared("reuse-source");

    // 往源视频尾部加一个字节:内容变了(sha256 不同),ffmpeg 照样读得动
    let project = Project::load(&layout.project_json()).expect("读 project.json");
    let video = layout.rel(project.source.path.as_str());
    let mut bytes = std::fs::read(&video).expect("读源视频");
    bytes.push(0);
    std::fs::write(&video, &bytes).expect("写回源视频");

    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-transcribe"]);
    assert!(out.status.success(), "重跑失败:\n{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains("源视频内容跟 project.json 里记的对不上"),
        "源被改过这件事必须说出来:\n{text}"
    );
    assert!(text.contains("源视频内容变了"), "重算的理由要指名:\n{text}");

    let report = run_report(&layout);
    assert_eq!(report["reuse"]["source_changed"], serde_json::Value::Bool(true), "{report}");
    assert_eq!(report["reuse"]["visual"], serde_json::Value::Bool(false), "{report}");
}

/// **`--force`:输入一个字没变,也照重算。**
///
/// 「能省则省」不该变成「没法强制重来」—— 怀疑上次那批材料、或者换了算法想干净地
/// 重跑一遍时,得有这么一条正门。这条也是复用那套判据的**反面对照**:同样的场景,
/// 不加 `--force` 是沿用(见 [`a_second_run_with_the_same_inputs_reuses_the_pictures`])。
#[test]
fn force_recomputes_even_when_nothing_changed() {
    let (tmp, layout) = prepared("reuse-force");
    let before = shot_stamps(&layout);

    let out = run_in(
        &tmp.0,
        &["preprocess", "test-sample", "--no-transcribe", "--force"],
    );
    assert!(out.status.success(), "重跑失败:\n{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("force"), "要说是 `--force` 让重算的:\n{text}");
    assert_ne!(shot_stamps(&layout), before, "--force 就该重抽截图");

    let report = run_report(&layout);
    assert_eq!(report["reuse"]["visual"], serde_json::Value::Bool(false), "{report}");
}

/// **复用不吃模型的选择。**
///
/// 复用是"这一步要不要重算"的判断,**不是**"哪些成果不要了"的判断:模型在步 2 里
/// 把某张图弃用(`update_shot` 的 `drop`)、给另一张写下看图结论 —— 那些决定住在
/// `state.bin` 里,沿用那条路上**一个都不许碰**。只有真的重算(输入变了 / `--force`)
/// 才会作废它们,理由是硬的:重抽之后那些结论说的是旧帧。
///
/// 这条钉住的是「工具不替模型做主」:复用绝不等于把模型之前的判断一并锁死或清掉。
#[test]
fn reuse_never_touches_what_the_model_decided() {
    use vidbrief_project::{Confidence, ShotCheck, ShotId, ShotStatus};

    let (tmp, layout) = prepared("reuse-keeps");
    let mut state = state_of(&layout);

    let dropped = ShotId::new(1).expect("合法 shot id");
    let checked = ShotId::new(2).expect("合法 shot id");
    state.shot_mut(dropped).status = ShotStatus::Drop;
    state.shot_mut(checked).check = Some(ShotCheck {
        r#match: true,
        audio_visual_sync: true,
        type_read: "一张示意图".into(),
        neighbor_text: None,
        issues: Vec::new(),
        confidence: Confidence::High,
        evidence: None,
        checked_at: vidbrief_project::now(),
    });
    state.write(&layout.state_file()).expect("写状态");

    // ① 沿用(输入没变):模型的决定一条不许动
    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-transcribe"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let kept = state_of(&layout);
    assert_eq!(
        kept.shot(dropped).map(|s| s.status),
        Some(ShotStatus::Drop),
        "复用把模型的弃用抹了"
    );
    assert!(
        kept.shot(checked).and_then(|s| s.check.as_ref()).is_some(),
        "复用把模型的看图结论抹了"
    );

    // ② 对照:重算才作废 —— 而且理由是真的(那些结论说的是旧帧)
    let out = run_in(
        &tmp.0,
        &["preprocess", "test-sample", "--no-transcribe", "--force"],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let after = state_of(&layout);
    assert!(
        after.shot(checked).and_then(|s| s.check.as_ref()).is_none(),
        "重算就该作废旧结论"
    );
}
