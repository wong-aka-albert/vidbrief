//! `vidbrief init` 的端到端测试:真的起进程、真的建目录、真的探测视频。
//!
//! 用 `fixtures/test_sample.mp4`(60 秒合成视频)当输入,因此这些测试也顺带验证了
//! ffprobe 探测这条路是通的 —— 探测失败会让 `init` 直接报错。
//!
//! 大多数测试把**工作目录**设成临时目录,于是默认位置 `projects/<id>` 落在临时目录里,
//! 既不污染仓库,又真实验证了默认路径与"撞车即报错"的逻辑。

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
///
/// **刻意放在仓库的 `target/` 下,而不是系统临时目录** —— 系统临时目录常在 `C:`,
/// 而素材在 `E:`,跨卷会让硬链接退化成复制,于是"同卷应当硬链接"这条根本没被测到。
/// `target/` 已被 gitignore,且与素材同卷。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let root = repo_root().join("target").join("vidbrief-test");
        let dir = root.join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// 素材的**副本**。
    ///
    /// 不要直接用 `fixtures/` 里的原件:`init` 在同卷时会建**硬链接**,
    /// 于是任何往"项目里那个视频"写东西的代码都会穿透改坏仓库里被跟踪的文件。
    /// 这不是假想 —— 演示 `status --verify` 时翻转一个字节,就把
    /// `fixtures/test_sample.mp4` 改了(靠 `git checkout` 才恢复)。
    fn stage_video(&self) -> PathBuf {
        // 保持**原文件名** —— 项目 id 由文件名净化而来,改名会让所有断言失效
        let name = fixture().file_name().expect("素材没有文件名").to_owned();
        let dst = self.0.join(name);
        std::fs::copy(fixture(), &dst).expect("无法复制素材");
        dst
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 在 `cwd` 下跑 `vidbrief`。
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

// ── 默认位置 projects/<id> ───────────────────────────────────────────

#[test]
fn init_builds_the_whole_skeleton() {
    let tmp = TempDir::new("skeleton");
    let out = run_in(&tmp.0, &["init", tmp.stage_video().to_str().unwrap()]);
    assert!(out.status.success(), "init 失败:\n{}", stderr(&out));

    // 项目 id 由文件名净化而来:test_sample → test-sample;默认落在 ./projects/<id>
    let layout = Layout::for_project(tmp.join("projects"), "test-sample");

    for (name, path) in layout.rel_dirs() {
        assert!(path.is_dir(), "目录未建出:{name}");
    }

    let project = Project::load(&layout.project_json()).expect("project.json 读不回来");
    assert_eq!(project.id.as_ref(), "test-sample");
    assert_eq!(project.name, "test_sample", "未给 --title 时用视频文件名");
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

    let video = layout.rel(project.source.path.as_str());
    assert!(video.is_file(), "源视频没进项目:{}", video.display());
    assert_eq!(
        std::fs::metadata(&video).unwrap().len(),
        std::fs::metadata(&tmp.stage_video()).unwrap().len(),
        "项目里的源视频大小与原始文件不符"
    );

    let text = stdout(&out);
    assert!(text.contains("项目已建立:test-sample"), "{text}");
    assert!(text.contains("vidbrief preprocess"), "应给出下一步提示:{text}");
    // 同卷时必须是硬链接,且要说清"不占额外磁盘" —— 这条输出曾经让人误以为复制了文件
    assert!(text.contains("  引入     硬链接"), "{text}");
    assert!(!text.contains("警告"), "同卷硬链接不该有警告:{text}");
}

/// **同一个视频建第二次:报错,而且要把选择权交回去。**
///
/// 这里曾经断言"自动建出 test-sample-2"。改掉了 —— 那是命名,命名是人的决定:
/// `test-sample-2` 是用户会看到、会拿它调工具、会写进报告的标识,工具不该悄悄替他定。
/// 撞名往往也不是"换一个",而是"别建"(已经建过了 / 上次建到一半 / 点错了)。
#[test]
fn init_refuses_a_taken_name_and_says_what_to_do() {
    let tmp = TempDir::new("taken-name");
    let video = tmp.stage_video();
    let args = ["init", video.to_str().unwrap()];

    let first = run_in(&tmp.0, &args);
    assert!(first.status.success(), "{}", stderr(&first));

    let second = run_in(&tmp.0, &args);
    assert!(!second.status.success(), "第二次不该悄悄另建一个:\n{}", stdout(&second));

    let err = stderr(&second);
    // 不是"已存在"就完了 —— 得说清**去路**
    assert!(err.contains("已经有一个项目了"), "要说清那是什么:{err}");
    assert!(err.contains("不需要重新建"), "要说清「直接用就行」:{err}");

    // 没有第二个项目被造出来
    assert!(
        !tmp.join("projects/test-sample-2").exists(),
        "不该自动加后缀"
    );
}

/// 位置被一个**还没建完的目录**占着时,报错要同时给出两种可能与各自的做法。
///
/// 为什么不能只说一句"已存在":从外面看,"**另一个会话正在建**"和"**上次建到一半失败了**"
/// 是**同一个样子** —— 目录在,`project.json` 还没有。工具分不出来,就别装作分得出来;
/// 该做的是把两种可能和各自的做法都摆出来让人选。
///
/// 这也不是假想的坑:上一次建到一半失败留下的目录,会把之后**所有**重试都挡死,
/// 而用户看到"已存在"却找不到项目,只能猜。
#[test]
fn init_explains_an_unfinished_directory_instead_of_just_refusing() {
    let tmp = TempDir::new("unfinished");
    let video = tmp.stage_video();

    // 造一个"像项目但没建成"的目录:目录在,project.json 不在
    std::fs::create_dir_all(tmp.join("projects/test-sample")).unwrap();
    std::fs::write(tmp.join("projects/test-sample/leftover.txt"), b"x").unwrap();

    let out = run_in(&tmp.0, &["init", video.to_str().unwrap()]);
    assert!(!out.status.success());

    let err = stderr(&out);
    assert!(err.contains("还没建完的目录"), "要说清那是什么:{err}");
    assert!(err.contains("正在"), "要提到「别人可能正在建」:{err}");
    assert!(err.contains("另起一个"), "要给出「换个名字再建」:{err}");
    // **报错里不能劝人删**:读这句话的常常是模型,你写"删",它就会删
    assert!(!err.contains("删"), "别在报错里提删除:{err}");
    assert!(err.contains("不要动"), "要明确挡住「顺手清理」这个念头:{err}");
    assert!(
        tmp.join("projects/test-sample/leftover.txt").is_file(),
        "报错时一个字都不该动它"
    );
}

// ── -p / --project-dir:项目目录本身 ─────────────────────────────────

#[test]
fn project_dir_flag_puts_the_project_exactly_there() {
    let tmp = TempDir::new("pdir");
    let exact = tmp.join("my-place");

    let out = run_in(
        &tmp.0,
        &["init", tmp.stage_video().to_str().unwrap(), "-p", exact.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    // 就是那个目录本身,没有再套一层
    let project = Project::load(&exact.join("project.json")).expect("project.json 不在 -p 指定的位置");
    assert_eq!(project.id.as_ref(), "my-place", "未给 -n 时,id 取 -p 目录名的净化结果");
    assert_eq!(project.name, "test_sample");
    assert!(!tmp.join("projects").exists(), "给了 -p 就不该再建默认的 projects/");

    assert!(exact.join("01_preprocess/shots").is_dir(), "骨架仍要建全");
    assert!(exact.join("source/video.mp4").is_file());
}

#[test]
fn project_dir_flag_refuses_to_overwrite() {
    let tmp = TempDir::new("pdir-exists");
    let exact = tmp.join("taken");
    std::fs::create_dir_all(&exact).unwrap();
    std::fs::write(exact.join("keep.txt"), b"do not touch").unwrap();

    let out = run_in(
        &tmp.0,
        &["init", tmp.stage_video().to_str().unwrap(), "-p", exact.to_str().unwrap()],
    );
    assert!(!out.status.success(), "目录已存在就该拒绝");
    // 那条目录是空的(没有 project.json),所以走的是"还没建完"那一路;
    // 两种去路的文案不同,这里只钉"它确实说了原因",别把措辞钉死
    assert!(stderr(&out).contains("占着"), "{}", stderr(&out));
    assert_eq!(std::fs::read(exact.join("keep.txt")).unwrap(), b"do not touch");
    assert!(!exact.join("project.json").exists(), "不该写进去任何东西");
}

#[test]
fn project_dir_id_falls_back_to_video_name_when_dir_is_not_ascii() {
    let tmp = TempDir::new("pdir-cn");
    let exact = tmp.join("第一讲");

    let out = run_in(
        &tmp.0,
        &["init", tmp.stage_video().to_str().unwrap(), "-p", exact.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let project = Project::load(&exact.join("project.json")).unwrap();
    assert_eq!(
        project.id.as_ref(),
        "test-sample",
        "目录名净化不出来时应退回视频文件名,而不是报错"
    );
}

// ── id 推导与显式指定 ────────────────────────────────────────────────

#[test]
fn init_asks_for_name_when_stem_is_not_ascii() {
    let tmp = TempDir::new("cn");
    let video = tmp.join("第一讲 开场.mp4");
    std::fs::copy(fixture(), &video).unwrap();

    let out = run_in(&tmp.0, &["init", video.to_str().unwrap()]);
    assert!(!out.status.success(), "中文文件名 + 无 -p 应要求显式 -n");
    assert!(stderr(&out).contains("--name") || stderr(&out).contains("-n"), "{}", stderr(&out));

    let out = run_in(
        &tmp.0,
        &["init", video.to_str().unwrap(), "-n", "demo", "-t", "第一讲 开场"],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let project = Project::load(&tmp.join("projects/demo/project.json")).unwrap();
    assert_eq!(project.id.as_ref(), "demo");
    assert_eq!(project.name, "第一讲 开场", "中文标题应原样保留在 name 里");
}

// ── 参数解析 ─────────────────────────────────────────────────────────

#[test]
fn short_flags_and_equals_forms() {
    let tmp = TempDir::new("forms");
    let video = tmp.stage_video();
    let video = video.to_str().unwrap();

    // 短选项 `-p x`
    let out = run_in(
        &tmp.0,
        &["init", video, "-n", "a", "-t", "T", "-p", tmp.join("a").to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(Project::load(&tmp.join("a/project.json")).unwrap().name, "T");

    // 长选项 `--key=value`
    let out = run_in(
        &tmp.0,
        &[
            "init",
            video,
            &format!("--project-dir={}", tmp.join("b").display()),
            "--name=b",
        ],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(tmp.join("b/project.json").is_file());

    // 短选项带等号 `-p=value`
    let out = run_in(
        &tmp.0,
        &["init", video, &format!("-p={}", tmp.join("c").display())],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(tmp.join("c/project.json").is_file());
}

#[test]
fn init_help_lists_the_options() {
    let tmp = TempDir::new("help");
    let out = run_in(&tmp.0, &["init", "-h"]);
    assert!(out.status.success());
    let text = stdout(&out);
    for expected in ["-n, --name", "-t, --title", "-p, --project-dir", "-h, --help"] {
        assert!(text.contains(expected), "用法里缺 `{expected}`:\n{text}");
    }
    assert!(!tmp.join("projects").exists(), "-h 不该建任何东西");
}

#[test]
fn init_reports_errors_instead_of_panicking() {
    let tmp = TempDir::new("errors");

    let out = run_in(&tmp.0, &["init", tmp.join("nope.mp4").to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("找不到视频文件"), "{}", stderr(&out));

    let out = run_in(&tmp.0, &["init"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("用法"), "{}", stderr(&out));

    let out = run_in(&tmp.0, &["init", tmp.stage_video().to_str().unwrap(), "--nmae", "x"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--nmae"), "{}", stderr(&out));

    let out = run_in(&tmp.0, &["init", "a.mp4", "b.mp4"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("只接受一个视频路径"), "{}", stderr(&out));

    let out = run_in(&tmp.0, &["init", tmp.stage_video().to_str().unwrap(), "-p"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("缺少取值"), "{}", stderr(&out));

    // 六次失败都不该留下半个项目
    assert!(
        !tmp.join("projects").exists(),
        "失败的 init 不该留下 projects/ 目录"
    );
}

// ── 顶层命令 ─────────────────────────────────────────────────────────

/// **回退必须自证**:输出里说"复制"时,必须跟着说明为什么没能用更省的方式。
///
/// 这条不变式是有来历的:早先 `materialize` 用三个 `is_ok()` 把失败原因全吞了,
/// 用户看到"(复制)"只能猜是跨卷、权限还是文件系统——而工具本来就知道答案。
#[test]
fn falling_back_to_copy_always_explains_itself() {
    // 同卷(仓库在 E:)→ 期望硬链接
    let same = TempDir::new("vol-same");
    let video = same.stage_video();
    let mut seen = vec![run_in(&same.0, &["init", video.to_str().unwrap()])];

    // 另一个卷(系统临时目录,本机是 C:)→ 期望复制 + 原因
    let cross = TempDir::new("vol-cross");
    let elsewhere = std::env::temp_dir().join(format!("vidbrief-cross-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&elsewhere);
    seen.push(run_in(
        &cross.0,
        &["init", video.to_str().unwrap(), "-p", elsewhere.to_str().unwrap()],
    ));

    let mut kinds = Vec::new();
    for out in &seen {
        assert!(out.status.success(), "{}", stderr(out));
        let text = stdout(out);
        let has_reason = text.lines().any(|l| l.trim_start().starts_with("└"));

        if text.contains("  引入     硬链接") {
            assert!(!has_reason, "硬链接是第一选择,不该有回退原因:\n{text}");
            kinds.push("hardlink");
        } else if text.contains("  引入     复制") {
            assert!(has_reason, "退到复制必须给出原因:\n{text}");
            assert!(text.contains("硬链接:"), "原因里应记下硬链接为何失败:\n{text}");
            kinds.push("copy");
        } else if text.contains("  引入     软链接") {
            assert!(has_reason, "软链接是回退结果,必须给出原因:\n{text}");
            assert!(text.contains("警告"), "软链接必须给出不自包含的警告:\n{text}");
            kinds.push("symlink");
        } else {
            panic!("输出里没有『引入』行:\n{text}");
        }
    }

    let _ = std::fs::remove_dir_all(&elsewhere);
    assert!(
        kinds.contains(&"hardlink"),
        "同卷(仓库与素材都在 E:)应当用硬链接,实际:{kinds:?}"
    );
}

/// **同一时刻、同一个 `-p` 路径建项目:必须有且只有一个成功。**
///
/// 修之前 `create` 是"先看目录存不存在,再 `create_dir_all`" —— 典型的 check-then-act。
/// 两个会话同时建项目时,两边都可能看到"不存在",然后**都**往下走:都去搬源视频、
/// 都写 `project.json`,留下一个谁也不认识的半成品(搬到一半的视频被另一次复制覆盖)。
/// 现在靠 `create_dir` 的**原子性**占位(已存在即失败)。
///
/// # 为什么是"同进程 + 栅栏",而不是起 4 个 `vidbrief init` 进程
///
/// 起进程试过,**抓不到**:四次 `Command::spawn` 的启动抖动是毫秒级,而
/// `exists()` 到 `create_dir_all()` 之间的窗口只有微秒级 —— 四个进程根本撞不上,
/// 临时把修复退回去重跑三次,它照样绿。一条抓不到 bug 的回归测试比没有更糟:
/// 它谎报覆盖。
///
/// 所以这里用 [`Barrier`] 让 4 个线程在**同一纳秒**进入 `create`(同进程,无启动抖动),
/// 那个窗口才真的会被撞上。
#[test]
fn only_one_of_many_concurrent_inits_wins() {
    use std::sync::Barrier;

    let tmp = TempDir::new("concurrent-init");
    let video = tmp.stage_video();
    let target = tmp.join("same-place");

    const RACERS: usize = 4;
    let barrier = Barrier::new(RACERS);

    let results: Vec<Result<(), String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..RACERS)
            .map(|_| {
                let barrier = &barrier;
                let video = video.clone();
                let target = target.clone();
                scope.spawn(move || {
                    // 所有线程在这儿对齐,然后一起冲进去
                    barrier.wait();
                    vidbrief_ops::create(
                        &vidbrief_ops::CreateOptions {
                            video,
                            name: None,
                            title: None,
                            project_dir: Some(target),
                        },
                        &mut |_notice| {},
                    )
                    .map(|_| ())
                    .map_err(|e| format!("{e:#}"))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        winners,
        1,
        "应当恰好有一个成功,实际 {winners} 个:{}",
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 输家要说得清为什么被拒(而不是留下一个坏项目)。
    // 此刻赢家多半还没写完 `project.json`,所以走的是"还没建完"那一路 ——
    // 这条断言只要求"说了原因",不钉死是哪一路(两条都是合法的)。
    for loser in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            loser.contains("占着") || loser.contains("已经有一个项目了"),
            "输家该说清为什么被拒:{loser}"
        );
    }

    // 赢家留下的项目必须是**完整的**:目录齐、源视频在、project.json 读得回来
    let layout = Layout::new(&target);
    for (name, path) in layout.rel_dirs() {
        assert!(path.is_dir(), "赢家的项目缺目录:{name}");
    }
    let project = Project::load(&layout.project_json()).expect("赢家的 project.json 读不回来");
    assert!(layout.rel(project.source.path.as_str()).is_file(), "源视频不在");
    assert!(layout.video_meta().is_file(), "video.meta.json 不在");
}

#[test]
fn every_documented_subcommand_is_recognised() {
    // `mcp` 曾经在这里被断言"未实现、退出码 2"。它现在是**真能用的**子命令,
    // 所以那条断言换成了端到端握手(见 tests/mcp.rs)。这里只守"没有未知子命令"。
    let cwd = std::env::temp_dir();
    for cmd in ["init", "preprocess", "status", "ui", "mcp"] {
        let out = run_in(&cwd, &[cmd, "-h"]);
        assert!(
            out.status.success(),
            "{cmd} -h 应当成功(说明这个子命令存在):{}",
            stderr(&out)
        );
        assert!(
            !stderr(&out).contains("未知子命令"),
            "{cmd} 被当成未知子命令了:{}",
            stderr(&out)
        );
    }
}

#[test]
fn help_and_unknown_command() {
    let cwd = std::env::temp_dir();

    let out = run_in(&cwd, &["help"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("init <video>"), "{}", stdout(&out));

    let out = run_in(&cwd, &[]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("Usage:"));

    let out = run_in(&cwd, &["frobnicate"]);
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
