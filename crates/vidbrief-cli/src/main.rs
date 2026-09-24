//! `vidbrief` —— 独立命令行入口(脱离 agent 也能测试与使用)。
//!
//! 子命令见 `docs/DESIGN.md` §10.5。当前已实现 `init`,其余在各里程碑补齐。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use vidbrief_asr::{Config as AsrConfig, transcribe};
use vidbrief_core::{
    CurveOptions, ShotQuality, analyze, extract_shots, plan_shots,
};
use vidbrief_project::{
    Health, Layout, Materialized, Project, ProjectId, RegionParams, SourceLink, VideoMeta,
    check_source_link, human_size, materialize, simplify_path, verify_content, video_rel_for,
};

const USAGE: &str = "\
vidbrief —— 视频图文总结 MCP 工具集

Usage: vidbrief <command> [args]

Commands:
  init <video> [options]    建项目(目录骨架 + 探测元数据 + 搬入源视频)
  preprocess <project>      步 1:差帧图 / 密集区 / 截图 / 转录
  status <project>          查看项目状态与健康检查
  ui <project>              本地视图 + 视频服务
  mcp [--stdio]             启动 MCP 服务
  help                      显示本条用法

Run `vidbrief <command> -h` for the options of a command.
";

const INIT_USAGE: &str = "\
vidbrief init —— 建项目

Usage: vidbrief init <video> [options]

Arguments:
  <video>                  源视频路径(任何 ffmpeg 认得的格式)

Options:
  -n, --name <ID>          项目 id:ASCII 小写字母/数字/`-`/`_`。默认由视频文件名净化;
                           净化不出来(如纯中文名)且未指定 -p 时会报错
  -t, --title <TEXT>       人类可读名,可含中文。默认取视频文件名
  -p, --project-dir <DIR>  项目目录本身,直接把项目建在 <DIR> 里;该目录已存在则报错,不覆盖。
                           不给 -p 时用默认位置 projects/<id>,该位置被占用会自动加后缀
                           (-2 / -3 …)
  -h, --help               显示本条用法

Examples:
  vidbrief init D:/videos/demo.mp4
  vidbrief init D:/videos/第一讲.mp4 -n demo -t \"第一讲 开场\"
  vidbrief init demo.mp4 -p D:/proj/demo

说明:
  - 只建目录与元数据,不跑任何算法;源视频按 硬链接 → 软链接 → 复制 的顺序引入
  - 步 1 用 `vidbrief preprocess <project>`
";

const STATUS_USAGE: &str = "\
vidbrief status —— 查看项目状态与健康检查

Usage: vidbrief status <project> [--verify]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
  -V, --verify             重算源视频 sha256 并与记录比对。能发现\"源文件被就地改写\",
                           但要把整个视频读一遍(几百 MB 要几秒)
  -h, --help               显示本条用法

健康检查查什么(依 source.link 而定):
  hardlink   两边是否仍是同一份数据。源文件被移动/删除/替换都**不影响**项目,只提示
  symlink    链接目标是否还在。**失效即项目不可用**,会以 ✗ 报出来
  copy       无需检查
";

const PREPROCESS_USAGE: &str = "\
vidbrief preprocess —— 步 1 前处理(纯算法,一次做完)

Usage: vidbrief preprocess <project> [options]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
  -f, --fps <N>            采样帧率,默认 1.0(每秒一个点)
  -w, --width <N>          参与打分的缩小宽度,默认 480
      --no-transcribe      跳过语音转文字(只出差帧图与截图)
      --no-shots           只出差帧图与密集区,不抽截图
  -h, --help               显示本条用法

产出(全部落在 01_preprocess/):
  change_curve.json   差帧图
  dense_regions.json  变化密集区
  shots.json + shots/ 预筛选截图
  transcript.json     + .srt/.txt/.vtt(源视频没有音轨时跳过)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((cmd, rest)) = args.split_first() else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    let result = match cmd.as_str() {
        "init" => cmd_init(rest),
        "status" => cmd_status(rest),
        "preprocess" => cmd_preprocess(rest),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other @ ("ui" | "mcp") => {
            eprintln!("`vidbrief {other}` 尚未实现(见 docs/DESIGN.md §13 里程碑)");
            return ExitCode::from(2);
        }
        other => {
            eprintln!("未知子命令:`{other}`\n");
            print!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` 打印 anyhow 的整条因果链,"为什么会失败"比"失败了"有用得多
            eprintln!("错误:{err:#}");
            ExitCode::FAILURE
        }
    }
}

// ── init ─────────────────────────────────────────────────────────────

struct InitArgs {
    video: PathBuf,
    name: Option<String>,
    title: Option<String>,
    /// 项目目录本身。`None` = 用默认位置 `projects/<id>`。
    project_dir: Option<PathBuf>,
}

fn parse_init(args: &[String]) -> Result<InitArgs> {
    let mut video: Option<PathBuf> = None;
    let mut name = None;
    let mut title = None;
    let mut project_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // 支持 `--k=v` / `-k=v` 的等号写法(短选项不加等号,如 `-p x`)
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if is_option(k) => (k, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };

        match key {
            "-n" | "--name" => name = Some(take_value(args, &mut i, inline, key)?),
            "-t" | "--title" => title = Some(take_value(args, &mut i, inline, key)?),
            "-p" | "--project-dir" => {
                project_dir = Some(PathBuf::from(take_value(args, &mut i, inline, key)?))
            }
            other if is_option(other) => {
                bail!("init 不认识参数 `{other}`(见 `vidbrief init -h`)")
            }
            _ => {
                if video.is_some() {
                    bail!("init 只接受一个视频路径,多余的参数:`{arg}`");
                }
                video = Some(PathBuf::from(arg));
            }
        }
        i += 1;
    }

    let video = video.context("用法:vidbrief init <video> [options](见 `vidbrief init -h`)")?;
    Ok(InitArgs {
        video,
        name,
        title,
        project_dir,
    })
}

/// 看起来像选项(以 `-` 开头且不止一个字符)。单独的 `-` 视为普通参数。
fn is_option(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1
}

/// 取选项的值:`-k=v` / `--k=v` 用等号右边,`-k v` / `--k v` 吃下一个参数。
fn take_value(args: &[String], i: &mut usize, inline: Option<String>, what: &str) -> Result<String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .with_context(|| format!("{what} 后面缺少取值"))
}

/// 从目录路径的最后一段推导 id(`/tmp/vb/s4` → `s4`)。
///
/// 路径以 `.` / `..` 结尾,或末段净化不出 ASCII 时返回 `None`(交给下一优先级)。
fn id_from_dir(dir: &Path) -> Option<String> {
    ProjectId::from_video_stem(dir.file_name()?.to_str()?)
}

fn cmd_init(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{INIT_USAGE}");
        return Ok(());
    }
    let parsed = parse_init(args)?;

    let video = parsed
        .video
        .canonicalize()
        .with_context(|| format!("找不到视频文件:{}", parsed.video.display()))?;
    if !video.is_file() {
        bail!("不是文件:{}", video.display());
    }

    let stem = video
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();

    // 项目 id 的优先级:--name → -p 的最后一段 → 视频文件名净化
    let base = match &parsed.name {
        Some(raw) => ProjectId::from_video_stem(raw)
            .with_context(|| format!("--name `{raw}` 净化后为空,请用一个 ASCII 名字"))?,
        None => {
            let from_dir = parsed.project_dir.as_deref().and_then(id_from_dir);
            from_dir
                .or_else(|| ProjectId::from_video_stem(&stem))
                .with_context(|| {
                    let source = match &parsed.project_dir {
                        Some(dir) => format!("文件名 `{stem}` 或目录名 `{}`", dir.display()),
                        None => format!("文件名 `{stem}`"),
                    };
                    format!("无法从{source}推导项目 id(可能是中文名),请用 --name 指定一个 ASCII 名字")
                })?
        }
    };

    // 目录:显式 -p 就用它本身;否则 projects/<id>,冲突时加后缀
    let (id, layout) = match &parsed.project_dir {
        Some(dir) => (ProjectId::new(base)?, Layout::new(dir)),
        None => {
            let root = Path::new(vidbrief_project::layout::PROJECTS_DIR);
            let id = ProjectId::dedupe(&base, |candidate| root.join(candidate).exists());
            let id = ProjectId::new(id)?;
            let layout = Layout::for_project(root, id.as_ref());
            (id, layout)
        }
    };

    if layout.root().exists() {
        bail!("项目目录已存在,拒绝覆盖:{}", layout.root().display());
    }
    layout.create_dirs()?;

    // 把源视频放进项目(硬链接 → 软链接 → 复制,按代价从低到高)
    let rel = video_rel_for(&video);
    let dst = layout.rel(rel.as_str());

    // 动手**之前**先预判:要复制几百 MB 的话,别让用户干等到结束才知道在等什么
    if let Ok(placement) = vidbrief_project::plan(&video, &dst) {
        if placement.will_copy_across_volumes() {
            println!(
                "  预判     素材与项目不在同一个卷(盘),硬链接用不了 —— 即将复制 {}",
                vidbrief_project::human_size(placement.size_bytes)
            );
            if let Some(hint) = same_volume_hint(&video, id.as_ref()) {
                println!("           想省下这份空间,把项目建到素材同卷:{hint}");
            }
        }
    }

    let placed: Materialized = materialize(&video, &dst)?;

    // 探测的是项目里那一份(符号链接的情况会跟随到原文件)
    let meta = vidbrief_core::probe(&dst, rel.clone())?;
    meta.save(&layout.video_meta())?;

    let title = parsed.title.clone().unwrap_or_else(|| stem.clone());
    // `canonicalize` 在 Windows 上会带上 `\\?\` 前缀;写进 project.json 之前去掉,
    // 否则这个字段对人就没法看了
    let original = simplify_path(&video);
    let mut project = Project::new(
        id.clone(),
        title,
        rel,
        original.display().to_string(),
        placed.link,
    );
    project.save(&layout.project_json())?;

    print_summary(&layout, &project, &meta, &original, &placed.fallbacks);
    Ok(())
}

fn print_summary(
    layout: &Layout,
    project: &Project,
    meta: &VideoMeta,
    original: &Path,
    fallbacks: &[String],
) {
    let fps = match meta.fps {
        Some(fps) => format!(" @ {fps:.3} fps"),
        None => String::new(),
    };
    let size = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "分辨率未知".to_string(),
    };
    let codec = meta.codec.as_deref().unwrap_or("编码未知");

    println!("项目已建立:{}", project.id);
    println!("  目录     {}", display_dir(layout.root()));
    println!("  入口     {}", layout.project_json().display());
    println!(
        "  源视频   {} → {}",
        original.display(),
        project.source.path
    );
    println!("  引入     {}", link_note(project.source.link, meta.size_bytes));
    for reason in fallbacks {
        println!("           └ {reason}");
    }
    println!("  时长     {:.1}s  {size}{fps}  {codec}", meta.duration);
    println!("  指纹     sha256:{}", meta.sha256);
    if !project.source.link.is_self_contained() {
        println!("  警告     项目**不**自包含:源文件一旦移动或删除即失效,打包时还可能变成坏链接");
    }
    println!();
    println!("下一步:");
    println!(
        "  vidbrief preprocess {}    # 步 1:差帧图 / 转录 / 预筛选截图",
        layout.root().display()
    );
}

/// 一句话说清"源视频是怎么进来的",以及这件事的**实际后果**。
///
/// 措辞在 `vidbrief_project::SourceLink::describe`,那里能不起进程就单测。
fn link_note(link: SourceLink, size_bytes: u64) -> String {
    link.describe(size_bytes)
}

/// 给出"把项目建到素材同卷"的具体命令,而不是只说一句"建议同卷"。
fn same_volume_hint(video: &Path, id: &str) -> Option<String> {
    let dir = simplify_path(video).parent()?.join("vidbrief").join(id);
    Some(format!("vidbrief init <同一个视频> -p {}", dir.display()))
}

// ── preprocess ───────────────────────────────────────────────────────

struct PreprocessArgs {
    project: String,
    fps: f64,
    width: u32,
    transcribe: bool,
    shots: bool,
}

fn parse_preprocess(args: &[String]) -> Result<PreprocessArgs> {
    let mut project: Option<String> = None;
    let mut fps = vidbrief_project::curve::DEFAULT_FPS_SAMPLED;
    let mut width = vidbrief_project::curve::DEFAULT_WIDTH;
    let mut transcribe = true;
    let mut shots = true;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if is_option(k) => (k, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        match key {
            "-f" | "--fps" => {
                let raw = take_value(args, &mut i, inline, key)?;
                fps = raw.parse().with_context(|| format!("--fps 不是数字:`{raw}`"))?;
            }
            "-w" | "--width" => {
                let raw = take_value(args, &mut i, inline, key)?;
                width = raw.parse().with_context(|| format!("--width 不是整数:`{raw}`"))?;
            }
            "--no-transcribe" => transcribe = false,
            "--no-shots" => shots = false,
            other if is_option(other) => {
                bail!("preprocess 不认识参数 `{other}`(见 `vidbrief preprocess -h`)")
            }
            _ => {
                if project.is_some() {
                    bail!("preprocess 只接受一个项目,多余的参数:`{arg}`");
                }
                project = Some(arg.clone());
            }
        }
        i += 1;
    }

    if fps <= 0.0 || !fps.is_finite() {
        bail!("--fps 必须为正数,实际 {fps}");
    }
    if width == 0 {
        bail!("--width 必须为正数");
    }

    Ok(PreprocessArgs {
        project: project.context("用法:vidbrief preprocess <project> [options]")?,
        fps,
        width,
        transcribe,
        shots,
    })
}

fn cmd_preprocess(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{PREPROCESS_USAGE}");
        return Ok(());
    }
    let parsed = parse_preprocess(args)?;
    let layout = resolve_project(&parsed.project)?;
    let mut project = Project::load(&layout.project_json())?;
    let meta = VideoMeta::load(&layout.video_meta())?;
    let video = layout.rel(project.source.path.as_str());
    if !video.is_file() {
        bail!(
            "项目里的源视频不见了:{}\n先跑 `vidbrief status {}` 看看",
            video.display(),
            parsed.project
        );
    }

    println!("项目     {}", project.id);
    println!("源视频   {}  ({:.1}s)", project.source.path, meta.duration);
    println!();

    // ── 视觉部分:差帧图 → 密集区 → 截图 ────────────────────────────
    let opts = CurveOptions {
        fps: parsed.fps,
        width: parsed.width,
        ..CurveOptions::for_video(&meta)
    };
    let params = RegionParams::default();

    let mut last_print = std::time::Instant::now();
    let (curve, regions) = analyze(&video, &meta, opts, params)?;
    curve.save(&layout.change_curve())?;
    regions.save(&layout.dense_regions())?;
    println!(
        "差帧图   {} 个采样点({}fps),分数最高 {:.3}",
        curve.changes.len(),
        opts.fps,
        curve.max_score()
    );
    println!(
        "密集区   {} 个  阈值 {:.3}(中位数 {:.3} × {} 与下限 {} 取大)",
        regions.regions.len(),
        regions.stats.mass_threshold,
        regions.stats.mass_median,
        regions.params.mass_k,
        regions.params.mass_floor
    );

    let planned = plan_shots(&regions, &curve);
    if parsed.shots {
        let (shots, skipped) = extract_shots(
            &video,
            &layout,
            meta.duration,
            &planned,
            ShotQuality::default(),
            &mut |done, total| {
                // 抽帧是有 IO 的,长视频上几十张要好几秒 —— 给点进度别让人以为卡住了
                if done == total || last_print.elapsed().as_millis() > 300 {
                    eprint!("\r  抽帧     {done}/{total}");
                    last_print = std::time::Instant::now();
                }
            },
        )?;
        eprintln!();
        shots.save(&layout.shots_json())?;
        println!("截图     {} 张 → {}", shots.shots.len(), layout.shots_dir().display());
        if !skipped.is_empty() {
            println!("         其中 {} 张失败(已跳过):", skipped.len());
            for (t, why) in skipped.iter().take(3) {
                println!("           t={t:.1}s  {why}");
            }
        }
    } else {
        println!("截图     已跳过(--no-shots)");
    }

    // ── 转录 ─────────────────────────────────────────────────────────
    if parsed.transcribe {
        match AsrConfig::resolve() {
            Ok(cfg) => match transcribe(&video, &layout, &meta, &cfg) {
                Ok(Some(t)) => println!(
                    "转录     {} 段,模型 {} ({})",
                    t.segments.len(),
                    t.model,
                    t.language
                ),
                Ok(None) => println!("转录     源视频没有音轨,跳过"),
                Err(e) => {
                    // 转录失败不该让整轮白跑 —— 差帧图和截图已经落盘了
                    eprintln!("转录     失败(其余产出保留):{e:#}");
                }
            },
            Err(e) => eprintln!("转录     跳过(未配置 whisper):{e:#}"),
        }
    } else {
        println!("转录     已跳过(--no-transcribe)");
    }

    project.progress.preprocess.mark_done();
    project.save(&layout.project_json())?;

    println!();
    println!("完成。下一步:`vidbrief status {}` 或让 agent 看图校准", parsed.project);
    Ok(())
}

struct StatusArgs {
    project: String,
    verify: bool,
}

fn parse_status(args: &[String]) -> Result<StatusArgs> {
    let mut project: Option<String> = None;
    let mut verify = false;
    for arg in args {
        match arg.as_str() {
            "-V" | "--verify" => verify = true,
            other if is_option(other) => bail!("status 不认识参数 `{other}`(见 `vidbrief status -h`)"),
            _ => {
                if project.is_some() {
                    bail!("status 只接受一个项目,多余的参数:`{arg}`");
                }
                project = Some(arg.clone());
            }
        }
    }
    let project = project.context("用法:vidbrief status <project> [--verify](见 `vidbrief status -h`)")?;
    Ok(StatusArgs { project, verify })
}

/// 把命令行给的 `<project>` 解析成项目目录。
///
/// 两种写法都认:直接给目录(`projects/demo`、`E:\temp\vb_test`),或只给项目名(`demo` —— 到
/// `projects/` 下找)。
fn resolve_project(arg: &str) -> Result<Layout> {
    let direct = Path::new(arg);
    if direct.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
        return Ok(Layout::new(direct));
    }
    let under = Path::new(vidbrief_project::layout::PROJECTS_DIR).join(arg);
    if under.join(vidbrief_project::layout::PROJECT_FILE).is_file() {
        return Ok(Layout::new(under));
    }
    bail!(
        "找不到项目:`{arg}` —— 它既不是含 {} 的目录,也不是 projects/ 下的项目名",
        vidbrief_project::layout::PROJECT_FILE
    )
}

fn cmd_status(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{STATUS_USAGE}");
        return Ok(());
    }
    let parsed = parse_status(args)?;
    let layout = resolve_project(&parsed.project)?;
    let project = Project::load(&layout.project_json())?;
    let meta = VideoMeta::load(&layout.video_meta())?;

    println!("项目     {}", project.id);
    println!("  名称   {}", project.name);
    println!("  目录   {}", display_dir(layout.root()));
    println!("  建立   {}", project.created_at);
    println!("  格式   schema v{} / rev {}", project.schema_version, project.rev);
    if let Some(glossary) = &project.glossary {
        println!("  术语表 {} 条", glossary.len());
    }

    println!();
    println!("源视频   {}", project.source.path);
    println!("  原始   {}", project.source.original);
    println!("  引入   {}", project.source.link.describe(meta.size_bytes));
    println!("  大小   {}", human_size(meta.size_bytes));
    let res = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "未知".to_string(),
    };
    let fps = meta.fps.map(|f| format!("{f:.3}")).unwrap_or_else(|| "?".into());
    println!(
        "  规格   {:.1}s  {res} @ {fps} fps  {}",
        meta.duration,
        meta.codec.as_deref().unwrap_or("编码未知")
    );
    println!("  指纹   sha256:{}", meta.sha256);

    // 健康检查只跑一次,打印与退出码共用同一个结论
    // (verify 要读整个视频,重复跑是实打实的浪费)
    let link_health = check_source_link(&project, &layout);
    let content_health = if parsed.verify {
        Some(verify_content(&project, &meta, &layout)?)
    } else {
        None
    };

    println!();
    println!("健康");
    print_health(&link_health);
    if let Some(h) = &content_health {
        print_health(h);
    }

    println!();
    println!("进度");
    let p = &project.progress;
    println!("  ① 前处理   {}", step_text(p.preprocess.done, p.preprocess.at.map(|t| t.to_string())));
    println!("  ② 校准     {} 轮", p.calibrate.rounds);
    println!("  ③ 总结     {}", step_text(p.summarize.done, p.summarize.at.map(|t| t.to_string())));
    println!("  ④ 报告     {}", step_text(p.report.done, p.report.at.map(|t| t.to_string())));

    // 有硬伤时用非零退出码,方便脚本和 agent 判断
    let unhealthy = link_health.is_problem()
        || content_health.as_ref().is_some_and(Health::is_problem);
    if unhealthy {
        bail!("项目存在问题(见上面标 ✗ 的行)");
    }
    Ok(())
}

fn print_health(health: &Health) {
    let mut lines = health.message().lines();
    if let Some(first) = lines.next() {
        println!("  {} {first}", health.marker());
    }
    // Problem 的消息可能带换行(如哈希对照),续行缩进对齐
    for extra in lines {
        println!("    {extra}");
    }
}

fn step_text(done: bool, at: Option<String>) -> String {
    match (done, at) {
        (true, Some(at)) => format!("✓ {at}"),
        (true, None) => "✓".to_string(),
        (false, _) => "— 未做".to_string(),
    }
}

fn display_dir(path: &Path) -> String {
    let mut text = path.display().to_string();
    if !text.ends_with(['/', '\\']) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_size_picks_a_readable_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(1_953_106), "1.9 MB");
        assert_eq!(human_size(3_221_225_472), "3.00 GB");
    }

    #[test]
    fn link_note_does_not_call_a_hardlink_a_copy() {
        // 这条文案曾经让人以为"硬链接"等于"复制了一份文件",措辞必须区分清楚
        let hard = link_note(SourceLink::Hardlink, 1000);
        assert!(hard.contains("硬链接"), "{hard}");
        assert!(hard.contains("不占额外磁盘"), "{hard}");
        assert!(!hard.contains("占用"), "硬链接不该说'占用空间':{hard}");

        let copy = link_note(SourceLink::Copy, 1_953_106);
        assert!(copy.contains("复制") && copy.contains("1.9 MB"), "{copy}");

        let sym = link_note(SourceLink::Symlink, 0);
        assert!(sym.contains("失效"), "软链接必须说清风险:{sym}");
    }
}
