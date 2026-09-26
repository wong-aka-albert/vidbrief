//! `vidbrief status` 的端到端测试。
//!
//! 重点是**健康检查**三种引入方式的不同行为 —— 这块文案上一轮已经骗过一次人
//! (硬链接被念成复制),所以这里不只测"能跑",而是钉住每种情况**该报什么**。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vidbrief_project::{Layout, Project, SourceLink};

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

/// 与 `tests/init.rs` 一样:刻意放在仓库 `target/` 下,与素材同卷(否则硬链接必失败)。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = repo_root()
            .join("target")
            .join("vidbrief-test")
            .join(format!("status-{tag}-{}", std::process::id()));
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

/// 建一个装好源视频的项目(素材用 fixtures 的**副本**,这样测试可以放心删它)。
fn project(tag: &str, want: SourceLink) -> (TempDir, Layout) {
    let tmp = TempDir::new(tag);
    let video = tmp.join("source-video.mp4");
    std::fs::copy(fixture(), &video).unwrap();

    let out = run_in(
        &tmp.0,
        &["init", video.to_str().unwrap(), "-p", tmp.join("proj").to_str().unwrap()],
    );
    assert!(out.status.success(), "init 失败:\n{}", stderr(&out));

    let layout = Layout::new(tmp.join("proj"));
    let mut project = Project::load(&layout.project_json()).unwrap();
    let dst = layout.rel(project.source.path.as_str());

    match want {
        SourceLink::Hardlink => assert_eq!(
            project.source.link,
            SourceLink::Hardlink,
            "测试目录与素材同在 target/ 下、同卷,应当能建硬链接"
        ),
        SourceLink::Copy => {
            // 本机同卷必然走硬链接,所以要测"复制"就得自己造一个真副本出来
            std::fs::remove_file(&dst).unwrap();
            std::fs::copy(&video, &dst).unwrap();
            project.source.link = SourceLink::Copy;
            project.save(&layout.project_json()).unwrap();
        }
        SourceLink::Symlink => unreachable!("本机建不了符号链接,没有对应测试"),
    }
    (tmp, layout)
}

#[test]
fn status_shows_identity_health_and_progress() {
    let (tmp, _layout) = project("basic", SourceLink::Hardlink);
    let out = run_in(&tmp.0, &["status", "proj"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);

    for expected in ["项目     proj", "名称", "目录", "源视频   source/video.mp4", "健康", "进度"] {
        assert!(text.contains(expected), "输出缺 `{expected}`:\n{text}");
    }
    assert!(text.contains("格式   schema v2 / rev"), "{text}");
    // 刚建的项目四步都没做
    assert!(text.contains("① 前处理   — 未做"), "{text}");
    assert!(text.contains("② 校准     0 轮"), "{text}");
    assert!(!text.contains("✗"), "新项目不该有问题:\n{text}");
}

#[test]
fn status_accepts_a_directory_as_well_as_a_name() {
    let (tmp, layout) = project("resolve", SourceLink::Hardlink);

    // 只给项目名 → 到 projects/ 下找(这里 cwd 是 tmp,项目在 tmp/proj,所以用相对目录)
    let by_dir = run_in(&tmp.0, &["status", "proj"]);
    assert!(by_dir.status.success(), "{}", stderr(&by_dir));

    // 给完整目录
    let by_path = run_in(&tmp.0, &["status", layout.root().to_str().unwrap()]);
    assert!(by_path.status.success(), "{}", stderr(&by_path));

    // 不存在
    let missing = run_in(&tmp.0, &["status", "nope"]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("找不到项目"), "{}", stderr(&missing));
}

#[test]
fn status_reports_copy_as_nothing_to_check() {
    let (tmp, _layout) = project("copy", SourceLink::Copy);
    let out = run_in(&tmp.0, &["status", "proj"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("独立副本"), "{text}");
    assert!(text.contains("无需检查"), "{text}");
}

#[test]
fn deleting_the_original_is_not_a_problem_for_a_hardlink_project() {
    let (tmp, layout) = project("hl-gone", SourceLink::Hardlink);
    let project = Project::load(&layout.project_json()).unwrap();
    let dst = layout.rel(project.source.path.as_str());

    // 删掉的是测试目录里的副本,不是仓库里的 fixtures
    std::fs::remove_file(&project.source.original).unwrap();

    let out = run_in(&tmp.0, &["status", "proj"]);
    assert!(out.status.success(), "源文件没了不该让 status 失败:\n{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("已不在原路径") || text.contains("另一份"), "{text}");
    assert!(text.contains("数据完整") || text.contains("不受影响"), "{text}");
    // 关键:项目里那份仍然读得出来
    assert!(!std::fs::read(&dst).unwrap().is_empty());
}

#[test]
fn status_verify_catches_an_in_place_edit() {
    let (tmp, layout) = project("verify", SourceLink::Copy);
    let project = Project::load(&layout.project_json()).unwrap();
    let video = layout.rel(project.source.path.as_str());

    // 不改大小,只改一个字节 —— 只有哈希能发现
    let mut bytes = std::fs::read(&video).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&video, &bytes).unwrap();

    // 不带 --verify:看不出来
    let plain = run_in(&tmp.0, &["status", "proj"]);
    assert!(plain.status.success(), "{}", stderr(&plain));
    assert!(!stdout(&plain).contains("被改过"), "{}", stdout(&plain));

    // 带 --verify:必须报出来,并且退出码非 0
    let verified = run_in(&tmp.0, &["status", "proj", "--verify"]);
    assert_eq!(verified.status.code(), Some(1), "内容漂移应让 status 失败");
    let text = stdout(&verified);
    assert!(text.contains("被改过"), "{text}");
    assert!(text.contains("记录:"), "{text}");
    assert!(text.contains("实际:"), "{text}");
}

#[test]
fn status_verify_passes_on_an_untouched_project() {
    let (tmp, _layout) = project("verify-ok", SourceLink::Hardlink);
    let out = run_in(&tmp.0, &["status", "proj", "--verify"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("内容与记录一致"), "{}", stdout(&out));
}

#[test]
fn status_help_lists_the_options() {
    let out = run_in(&std::env::temp_dir(), &["status", "-h"]);
    assert!(out.status.success());
    let text = stdout(&out);
    for expected in ["status <project>", "-V, --verify", "hardlink", "symlink"] {
        assert!(text.contains(expected), "用法里缺 `{expected}`:\n{text}");
    }
}

#[test]
fn status_rejects_extra_arguments() {
    let out = run_in(&std::env::temp_dir(), &["status", "a", "b"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("只接受一个项目"), "{}", stderr(&out));

    let out = run_in(&std::env::temp_dir(), &["status", "--nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--nope"), "{}", stderr(&out));

    let out = run_in(&std::env::temp_dir(), &["status"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("用法"), "{}", stderr(&out));
}
