//! `vidbrief` —— 独立命令行入口(脱离 agent 也能测试与使用)。
//!
//! 子命令见 `docs/DESIGN.md` §10.5。**这里只做三件事**:解析参数、调 `vidbrief-ops`、
//! 把结果说给人听。任何"该做什么"的判断都属于动作层 —— 否则 MCP 那边就得再写一遍。
//!
//! 唯一的例外是 `mcp`:它把进程交给 `vidbrief-mcp` 的 stdio 服务,之后 stdout 归协议所有。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use vidbrief_ops::{OpenProject, PreprocessOptions, Stage, TranscriptOutcome};
use vidbrief_project::{SourceLink, human_size};

const USAGE: &str = "\
vidbrief —— 视频图文总结 MCP 工具集

Usage: vidbrief <command> [args]

Commands:
  init <video> [options]    建项目(目录骨架 + 探测元数据 + 搬入源视频)
  preprocess <project>      步 1:差帧图 / 密集区 / 截图 / 转录
  status <project>          查看项目状态与健康检查
  ui <project>              渲染时间线视图 + 起本地服务 + 打开浏览器
  mcp [--stdio]             启动 MCP 服务(stdio 传输,供 agent 接入)
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

说明:
  - 会先取项目写锁;若另一个会话正开着这个项目,这里会直接拒绝而不是硬写
    (`docs/DESIGN.md` §4.7)。查看视图不受影响。
";

const UI_USAGE: &str = "\
vidbrief ui —— 本地视图 + 视频服务

Usage: vidbrief ui <project> [options]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
      --no-open            只起服务并打印 URL,不打开浏览器
  -h, --help               显示本条用法

说明:
  - 渲染 01_preprocess/timeline.html(四层共轴时间线 + 视频回跳)
  - 起一个绑在 127.0.0.1 的静态服务把项目目录发布出去 —— **必须有它浏览器才能 seek**
    (`file://` 打开时对本地媒体有跨域限制,时间轴点了不跳)
  - 端口由操作系统分配,不会和别的东西撞车;只监听回环地址,局域网访问不到
  - 不取写锁:它写的是**派生视图**不是材料,两个会话同时渲染也不会坏事
";

const MCP_USAGE: &str = "\
vidbrief mcp —— 启动 MCP 服务

Usage: vidbrief mcp [--stdio]

Options:
      --stdio              用 stdio 传输(默认,也是唯一已实现的)
  -h, --help               显示本条用法

说明:
  - **这是给 agent 用的入口**:客户端把本进程拉起来,用 stdin/stdout 走 JSON-RPC
  - stdout 从此归协议所有 —— 本命令不会往 stdout 打印任何东西(诊断都在 stderr)
  - 工具清单见 docs/DESIGN.md §11;提示模板 P1–P5 见 §10.4
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
        "ui" => cmd_ui(rest),
        "mcp" => cmd_mcp(rest),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
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

// ── 通用的参数解析小工具 ─────────────────────────────────────────────

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

/// 把 `--k=v` 拆成 `("--k", Some("v"))`。
fn split_option(arg: &str) -> (&str, Option<String>) {
    match arg.split_once('=') {
        Some((k, v)) if is_option(k) => (k, Some(v.to_string())),
        _ => (arg, None),
    }
}

/// 收集位置参数(最多一个),顺便把不认识的选项骂回去。
fn one_positional(args: &[String], cmd: &str, what: &str) -> Result<String> {
    let mut found: Option<String> = None;
    for arg in args {
        if is_option(arg) {
            bail!("{cmd} 不认识参数 `{arg}`(见 `vidbrief {cmd} -h`)");
        }
        if found.is_some() {
            bail!("{cmd} 只接受一个{what},多余的参数:`{arg}`");
        }
        found = Some(arg.clone());
    }
    found.with_context(|| format!("用法:vidbrief {cmd} <{what}> [options](见 `vidbrief {cmd} -h`)"))
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "-h" || a == "--help")
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
        let (key, inline) = split_option(arg);
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

fn cmd_init(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{INIT_USAGE}");
        return Ok(());
    }
    let parsed = parse_init(args)?;

    // 跨卷复制要好几秒,**在动手之前**先说出来
    let mut on_notice = |notice: vidbrief_ops::Notice| match notice {
        vidbrief_ops::Notice::CrossVolumeCopy { size_bytes, hint } => {
            println!(
                "  预判     素材与项目不在同一个卷(盘),硬链接用不了 —— 即将复制 {}",
                human_size(size_bytes)
            );
            if let Some(hint) = hint {
                println!("           想省下这份空间,把项目建到素材同卷:{hint}");
            }
        }
    };

    let created = vidbrief_ops::create(
        &vidbrief_ops::CreateOptions {
            video: parsed.video,
            name: parsed.name,
            title: parsed.title,
            project_dir: parsed.project_dir,
        },
        &mut on_notice,
    )?;

    print_summary(&created);
    Ok(())
}

fn print_summary(created: &vidbrief_ops::Created) {
    let meta = &created.meta;
    let fps = match meta.fps {
        Some(fps) => format!(" @ {fps:.3} fps"),
        None => String::new(),
    };
    let size = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "分辨率未知".to_string(),
    };
    let codec = meta.codec.as_deref().unwrap_or("编码未知");

    println!("项目已建立:{}", created.project.id);
    println!("  目录     {}", display_dir(created.layout.root()));
    println!("  入口     {}", created.layout.project_json().display());
    println!(
        "  源视频   {} → {}",
        created.original.display(),
        created.project.source.path
    );
    println!(
        "  引入     {}",
        link_note(created.project.source.link, meta.size_bytes)
    );
    for reason in &created.fallbacks {
        println!("           └ {reason}");
    }
    println!("  时长     {:.1}s  {size}{fps}  {codec}", meta.duration);
    println!("  指纹     sha256:{}", meta.sha256);
    if !created.project.source.link.is_self_contained() {
        println!("  警告     项目**不**自包含:源文件一旦移动或删除即失效,打包时还可能变成坏链接");
    }
    println!();
    println!("下一步:");
    println!(
        "  vidbrief preprocess {}    # 步 1:差帧图 / 转录 / 预筛选截图",
        created.layout.root().display()
    );
}

/// 一句话说清"源视频是怎么进来的",以及这件事的**实际后果**。
///
/// 措辞在 `vidbrief_project::SourceLink::describe`,那里能不起进程就单测。
fn link_note(link: SourceLink, size_bytes: u64) -> String {
    link.describe(size_bytes)
}

// ── preprocess ───────────────────────────────────────────────────────

fn parse_preprocess(args: &[String]) -> Result<(String, PreprocessOptions)> {
    let mut project: Option<String> = None;
    let mut opts = PreprocessOptions::default();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = split_option(arg);
        match key {
            "-f" | "--fps" => {
                let raw = take_value(args, &mut i, inline, key)?;
                opts.fps = raw
                    .parse()
                    .with_context(|| format!("--fps 不是数字:`{raw}`"))?;
            }
            "-w" | "--width" => {
                let raw = take_value(args, &mut i, inline, key)?;
                opts.width = raw
                    .parse()
                    .with_context(|| format!("--width 不是整数:`{raw}`"))?;
            }
            "--no-transcribe" => opts.transcribe = false,
            "--no-shots" => opts.shots = false,
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

    if opts.fps <= 0.0 || !opts.fps.is_finite() {
        bail!("--fps 必须为正数,实际 {}", opts.fps);
    }
    if opts.width == 0 {
        bail!("--width 必须为正数");
    }

    let project =
        project.context("用法:vidbrief preprocess <project> [options]")?;
    Ok((project, opts))
}

fn cmd_preprocess(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{PREPROCESS_USAGE}");
        return Ok(());
    }
    let (name, opts) = parse_preprocess(args)?;
    let mut opened = OpenProject::open(&name)?;

    // 取写锁:另一个会话正开着这个项目时就**别硬写**(DESIGN §4.7)。
    // 命令结束(或出错)时 guard 随作用域析构,锁自动释放。
    let _lock = vidbrief_project::acquire(opened.layout())?;

    println!("项目     {}", opened.id());
    println!(
        "源视频   {}  ({:.1}s)",
        opened.project().source.path,
        opened.meta().duration
    );
    println!();

    let mut last_print = Instant::now();
    let mut on_progress = |stage: Stage| match stage {
        Stage::Analyzed(a) => {
            println!(
                "差帧图   {} 个采样点({}fps),分数最高 {:.3}",
                a.curve_points, a.fps, a.max_score
            );
            println!(
                "密集区   {} 个  阈值 {:.3}(中位数 {:.3} × {} 与下限 {} 取大)",
                a.regions, a.mass_threshold, a.mass_median, a.mass_k, a.mass_floor
            );
        }
        Stage::Shot { done, total } => {
            // 抽帧是有 IO 的,长视频上几十张要好几秒 —— 给点进度别让人以为卡住了
            if done == total || last_print.elapsed().as_millis() > 300 {
                eprint!("\r  抽帧     {done}/{total}");
                last_print = Instant::now();
            }
        }
    };

    let report = vidbrief_ops::preprocess(&mut opened, &opts, &mut on_progress)?;
    if opts.shots {
        eprintln!();
    }

    match &report.shots {
        Some(count) => {
            println!("截图     {count} 张 → {}", opened.layout().shots_dir().display());
            if !report.shots_skipped.is_empty() {
                println!("         其中 {} 张失败(已跳过):", report.shots_skipped.len());
                for item in report.shots_skipped.iter().take(3) {
                    println!("           t={:.1}s  {}", item.t, item.reason);
                }
            }
        }
        None => println!("截图     已跳过(--no-shots)"),
    }

    match &report.transcript {
        TranscriptOutcome::Done {
            segments,
            model,
            language,
            ..
        } => println!("转录     {segments} 段,模型 {model} ({language})"),
        // 没有音轨是**合法**的空结果,不算失败
        TranscriptOutcome::NoAudio => println!("转录     源视频没有音轨,跳过"),
        TranscriptOutcome::Skipped => println!("转录     已跳过(--no-transcribe)"),
        TranscriptOutcome::Failed { error } => {
            eprintln!("转录     失败:{error}");
            // 差帧图与截图保留,但进度不标完成 —— 重跑一次就能补上
            eprintln!();
            eprintln!("差帧图与截图已保留;转录未完成,**未**标记步 1 为完成。");
            eprintln!("修复后重跑:`vidbrief preprocess {name}`");
            bail!("步 1 未完成:转录失败");
        }
    }

    match (&report.timeline, &report.timeline_error) {
        (Some(path), _) => println!("视图     {}", path.display()),
        (None, Some(error)) => eprintln!("视图     渲染失败(材料不受影响):{error}"),
        (None, None) => {}
    }

    println!();
    println!("完成。下一步:`vidbrief ui {name}` 打开时间线,或让 agent 看图校准");
    Ok(())
}

// ── status ───────────────────────────────────────────────────────────

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
    let project =
        project.context("用法:vidbrief status <project> [--verify](见 `vidbrief status -h`)")?;
    Ok(StatusArgs { project, verify })
}

fn cmd_status(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{STATUS_USAGE}");
        return Ok(());
    }
    let parsed = parse_status(args)?;
    let report = vidbrief_ops::inspect(
        &parsed.project,
        &vidbrief_ops::InspectOptions {
            verify: parsed.verify,
        },
    )?;

    let project = &report.project;
    let meta = &report.meta;
    println!("项目     {}", project.id);
    println!("  名称   {}", project.name);
    println!("  目录   {}", display_dir(&report.dir));
    println!("  建立   {}", project.created_at);
    println!("  格式   schema v{} / rev {}", project.schema_version, project.rev);
    if let Some(glossary) = &project.glossary {
        println!("  术语表 {} 条", glossary.len());
    }

    println!();
    println!("源视频   {}", project.source.path);
    println!("  原始   {}", project.source.original);
    println!(
        "  引入   {}",
        project.source.link.describe(meta.size_bytes)
    );
    println!("  大小   {}", human_size(meta.size_bytes));
    let res = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "未知".to_string(),
    };
    let fps = meta
        .fps
        .map(|f| format!("{f:.3}"))
        .unwrap_or_else(|| "?".into());
    println!(
        "  规格   {:.1}s  {res} @ {fps} fps  {}",
        meta.duration,
        meta.codec.as_deref().unwrap_or("编码未知")
    );
    println!("  指纹   sha256:{}", meta.sha256);

    println!();
    println!("健康");
    print_health(&report.link_health);
    if let Some(health) = &report.content_health {
        print_health(health);
    }

    println!();
    println!("进度");
    let p = &project.progress;
    println!(
        "  ① 前处理   {}",
        step_text(p.preprocess.done, p.preprocess.at.map(|t| t.to_string()))
    );
    println!("  ② 校准     {} 轮", p.calibrate.rounds);
    println!(
        "  ③ 总结     {}",
        step_text(p.summarize.done, p.summarize.at.map(|t| t.to_string()))
    );
    println!(
        "  ④ 报告     {}",
        step_text(p.report.done, p.report.at.map(|t| t.to_string()))
    );

    println!();
    println!("材料");
    print_materials(&report.materials);

    // 有硬伤时用非零退出码,方便脚本和 agent 判断
    if report.unhealthy() {
        bail!("项目存在问题(见上面标 ✗ 的行)");
    }
    Ok(())
}

fn print_health(health: &vidbrief_ops::HealthView) {
    let mut lines = health.message.lines();
    if let Some(first) = lines.next() {
        println!("  {} {first}", health.marker());
    }
    // Problem 的消息可能带换行(如哈希对照),续行缩进对齐
    for extra in lines {
        println!("    {extra}");
    }
}

/// 材料盘点。**给人看的是一眼能看出缺什么**,所以"没做"也要列出来。
fn print_materials(m: &vidbrief_ops::Materials) {
    let count = |v: Option<usize>| match v {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    };
    println!("  差帧图   {} 个采样点", count(m.curve_points));
    println!("  密集区   {} 个", count(m.regions));
    match m.shots {
        Some(s) => println!(
            "  截图     {} 张(留用 {},未校准 {},待裁决 {})",
            s.total, s.keep, s.unchecked, s.needing_review
        ),
        None => println!("  截图     —"),
    }
    println!("  转录     {} 段", count(m.transcript_segments));
    if let Some(fixed) = m.corrected_segments {
        println!("  纠错     {fixed} 段有标记");
    }
    if let Some(total) = m.supplement_total {
        println!(
            "  补图     {total} 条候选({} 条待判断)",
            m.supplement_pending.unwrap_or(0)
        );
    }
    println!(
        "  视图     {}",
        if m.timeline_html { "已渲染" } else { "—" }
    );
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

// ── ui ───────────────────────────────────────────────────────────────

fn cmd_ui(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{UI_USAGE}");
        return Ok(());
    }
    let mut open_browser = true;
    let positional: Vec<String> = args
        .iter()
        .filter(|arg| {
            if arg.as_str() == "--no-open" {
                open_browser = false;
                false
            } else {
                true
            }
        })
        .cloned()
        .collect();
    let name = one_positional(&positional, "ui", "项目")?;

    let opened = OpenProject::open(&name)?;
    let (path, url) = vidbrief_ops::serve_and_open(&opened, open_browser)?;

    println!("视图     {}", path.display());
    println!("服务     {url}");
    if !open_browser {
        println!("         (--no-open:请手动打开上面的地址)");
    }
    println!();
    println!("时间轴上点/拖任意位置即可跳转视频;空格播放暂停,←→ ±1 秒(Shift ±10 秒)。");

    // 服务是后台线程,主线程不能立刻退出 —— 否则服务跟着进程一起没了
    println!();
    println!("按 Ctrl+C 结束(这个进程要一直活着,服务才能响应浏览器)。");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

// ── mcp ──────────────────────────────────────────────────────────────

fn cmd_mcp(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{MCP_USAGE}");
        return Ok(());
    }
    for arg in args {
        match arg.as_str() {
            // 现在只有 stdio 一种;写出来是为了将来加 HTTP 时不用改调用方的脚本
            "--stdio" => {}
            other => bail!("mcp 不认识参数 `{other}`(见 `vidbrief mcp -h`)"),
        }
    }
    // 从这里开始 stdout 归 JSON-RPC 协议所有
    vidbrief_mcp::serve_stdio()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::SourceLink;

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

    #[test]
    fn step_text_distinguishes_never_done_from_done_without_a_time() {
        assert_eq!(step_text(false, None), "— 未做");
        assert_eq!(step_text(true, None), "✓");
        assert_eq!(step_text(true, Some("2026-09-24".into())), "✓ 2026-09-24");
    }

    #[test]
    fn option_splitting_only_treats_leading_dashes_as_options() {
        assert_eq!(split_option("--fps=2"), ("--fps", Some("2".into())));
        assert_eq!(split_option("-f=2"), ("-f", Some("2".into())));
        assert_eq!(split_option("--fps"), ("--fps", None));
        // 单独的 `-` 是普通参数(有人真会传它当"标准输入")
        assert_eq!(split_option("-"), ("-", None));
        // 等号左边不是选项时,整串当位置参数
        assert_eq!(split_option("a=b"), ("a=b", None));
    }
}
