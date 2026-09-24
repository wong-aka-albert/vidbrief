//! `vidbrief init` 的端到端测试:真的起进程、真的建目录、真的探测视频。
//!
//! 用 `fixtures/test_sample.mp4`(60 秒合成视频)当输入,因此这些测试也顺带验证了
//! ffprobe 探测这条路是通的 —— 探测失败会让 `init` 直接报错。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vidbrief_project::{Layout, Project, Shot, VideoMeta};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

fn fixture() -> PathBuf {
    repo_root().join("fixtures").join("test_sample.mp4")
}

/// 用完自动删的临时目录。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vidbrief-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vidbrief"))
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

#[test]
fn init_builds_the_whole_skeleton() {
    let tmp = TempDir::new("skeleton");
    let projects = tmp.join("projects");

    let out = run(&[
        "init",
        fixture().to_str().unwrap(),
        "--projects-dir",
        projects.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "init 失败:\n{}", stderr(&out));

    // 项目 id 由文件名净化而来:test_sample → test-sample
    let layout = Layout::for_project(&projects, "test-sample");

    // 目录骨架齐全
    for (name, path) in layout.rel_dirs() {
        assert!(path.is_dir(), "目录未建出:{name}");
    }

    // 入口文件与元数据存在且能被本项目的类型读回
    let project = Project::load(&layout.project_json()).expect("project.json 读不回来");
    assert_eq!(project.id.as_ref(), "test-sample");
    assert_eq!(project.name, "test_sample");
    assert_eq!(project.rev, 1);
    assert!(project.source.path.as_str().starts_with("source/video."));
    assert!(!project.progress.preprocess.done, "刚建的项目还没跑过步 1");
    assert!(project.glossary.is_none(), "术语表默认关闭");

    let meta = VideoMeta::load(&layout.video_meta()).expect("video.meta.json 读不回来");
    assert_eq!(meta.path, project.source.path, "两份文件记的视频路径必须一致");
    assert!(
        (meta.duration - 60.0).abs() < 1.0,
        "fixtures/test_sample.mp4 应为 60 秒,实测 {}",
        meta.duration
    );
    assert_eq!(meta.sha256.len(), 64);
    assert!(meta.prober.starts_with("ffprobe"), "prober 应为 ffprobe,实际 {}", meta.prober);

    // 源视频真的在项目里,且能被读出来(硬链接/软链接/复制都算)
    let video = layout.rel(project.source.path.as_str());
    assert!(video.is_file(), "源视频没进项目:{}", video.display());
    assert_eq!(
        std::fs::metadata(&video).unwrap().len(),
        std::fs::metadata(fixture()).unwrap().len(),
        "项目里的源视频大小与原始文件不符"
    );

    // 输出里应给出下一步提示
    let text = stdout(&out);
    assert!(text.contains("项目已建立:test-sample"), "{text}");
    assert!(text.contains("vidbrief preprocess"), "{text}");
}

#[test]
fn init_dedupes_instead_of_overwriting() {
    let tmp = TempDir::new("dedupe");
    let projects = tmp.join("projects");
    let video = fixture();
    let args = [
        "init",
        video.to_str().unwrap(),
        "--projects-dir",
        projects.to_str().unwrap(),
    ];

    let first = run(&args);
    assert!(first.status.success(), "{}", stderr(&first));
    let second = run(&args);
    assert!(second.status.success(), "第二次 init 应另建项目而不是失败:\n{}", stderr(&second));

    let text = stdout(&second);
    assert!(text.contains("项目已建立:test-sample-2"), "{text}");
    assert!(Layout::for_project(&projects, "test-sample").exists());
    assert!(Layout::for_project(&projects, "test-sample-2").exists());
}

#[test]
fn init_asks_for_name_when_stem_is_not_ascii() {
    let tmp = TempDir::new("cn");
    let video = tmp.join("第一讲 开场.mp4");
    std::fs::copy(fixture(), &video).unwrap();
    let projects = tmp.join("projects");

    let out = run(&[
        "init",
        video.to_str().unwrap(),
        "--projects-dir",
        projects.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "中文文件名应要求显式 --name");
    let err = stderr(&out);
    assert!(err.contains("--name"), "错误信息应告诉用户怎么办:\n{err}");

    // 给了 --name 就能建
    let out = run(&[
        "init",
        video.to_str().unwrap(),
        "--name",
        "demo",
        "--title",
        "第一讲 开场",
        "--projects-dir",
        projects.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));

    let layout = Layout::for_project(&projects, "demo");
    let project = Project::load(&layout.project_json()).unwrap();
    assert_eq!(project.id.as_ref(), "demo");
    assert_eq!(project.name, "第一讲 开场", "中文标题应原样保留在 name 里");
}

#[test]
fn init_reports_errors_instead_of_panicking() {
    let tmp = TempDir::new("errors");
    let projects = tmp.join("projects");

    // 视频不存在
    let out = run(&[
        "init",
        tmp.join("nope.mp4").to_str().unwrap(),
        "--projects-dir",
        projects.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("找不到视频文件"), "{}", stderr(&out));

    // 缺参数
    let out = run(&["init"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("用法"), "{}", stderr(&out));

    // 不认识的参数
    let out = run(&[
        "init",
        fixture().to_str().unwrap(),
        "--nope",
        "x",
        "--projects-dir",
        projects.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--nope"), "{}", stderr(&out));

    // 死了三次都不该留下半个项目
    assert!(!projects.exists() || std::fs::read_dir(&projects).unwrap().next().is_none());
}

#[test]
fn init_accepts_equals_form_of_options() {
    let tmp = TempDir::new("equals");
    let projects = tmp.join("projects");
    let arg = format!("--projects-dir={}", projects.display());

    let out = run(&["init", fixture().to_str().unwrap(), &arg, "--name=demo"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(Layout::for_project(&projects, "demo").exists());
}

#[test]
fn unimplemented_commands_exit_two_and_say_so() {
    for cmd in ["preprocess", "ui", "mcp", "status"] {
        let out = run(&[cmd]);
        assert_eq!(out.status.code(), Some(2), "{cmd} 应以退出码 2 结束");
        let err = stderr(&out);
        assert!(err.contains("尚未实现"), "{cmd}:{err}");
        assert!(err.contains("DESIGN.md"), "{cmd} 应指向设计文档:{err}");
    }
}

#[test]
fn help_and_unknown_command() {
    let out = run(&["help"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("init <video>"), "{}", stdout(&out));

    // 不带参数 → 打印用法,退出码 0
    let out = run(&[]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("Usage:"));

    // 未知道的子命令 → 用法 + 退出码 2
    let out = run(&["frobnicate"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("未知子命令"), "{}", stderr(&out));
}

#[test]
fn shot_naming_helper_is_available_from_the_cli_side() {
    // 冒烟:CLI 依赖的公开 API 没有被意外收紧
    use vidbrief_project::{ShotId, ShotOrigin};
    assert_eq!(Shot::file_name(ShotId::new(1).unwrap(), 86.0).unwrap(), "s001_0086.0.jpg");
    assert_eq!(
        Shot::default_rel_path(ShotOrigin::AgentRequest, ShotId::new(4).unwrap(), 18.8)
            .unwrap()
            .as_str(),
        "02_calibrate/shots_extra/s004_0018.8.jpg"
    );
}
