//! `vidbrief` —— 独立命令行入口(脱离 agent 也能测试与使用)。
//!
//! 子命令见 `docs/DESIGN.md` §10.5。当前已实现 `init`,其余在各里程碑补齐。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use vidbrief_project::{
    Layout, Project, ProjectId, VideoMeta, materialize, simplify_path, video_rel_for,
};

const USAGE: &str = "\
vidbrief —— 视频图文总结 MCP 工具集

Usage: vidbrief <command> [args]

Commands:
  init <video> [options]    建项目(目录骨架 + 探测元数据 + 搬入源视频)
  preprocess <project>      步 1 前处理(纯算法)
  ui <project>              本地视图 + 视频服务
  mcp [--stdio]             启动 MCP 服务
  status <project>          查看项目状态
  help                      显示本条用法

Run `vidbrief init -h` for the options of init.
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((cmd, rest)) = args.split_first() else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    let result = match cmd.as_str() {
        "init" => cmd_init(rest),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other @ ("preprocess" | "ui" | "mcp" | "status") => {
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
    let link = materialize(&video, &dst)?;

    // 探测的是项目里那一份(符号链接的情况会跟随到原文件)
    let meta = VideoMeta::probe(&dst, rel.clone())?;
    meta.save(&layout.video_meta())?;

    let title = parsed.title.clone().unwrap_or_else(|| stem.clone());
    // `canonicalize` 在 Windows 上会带上 `\\?\` 前缀;写进 project.json 之前去掉,
    // 否则这个字段对人就没法看了
    let original = simplify_path(&video);
    let mut project = Project::new(id.clone(), title, rel, original.display().to_string(), link);
    project.save(&layout.project_json())?;

    print_summary(&layout, &project, &meta, &original);
    Ok(())
}

fn print_summary(layout: &Layout, project: &Project, meta: &VideoMeta, original: &Path) {
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
        "  源视频   {} → {}({})",
        original.display(),
        project.source.path,
        link_label(project.source.link)
    );
    println!("  时长     {:.1}s  {size}{fps}  {codec}", meta.duration);
    println!("  指纹     sha256:{}", meta.sha256);
    if !project.source.link.is_self_contained() {
        println!(
            "  提示     当前用 {} 引入源视频,项目**不**自包含;要打包带走请改用复制",
            link_label(project.source.link)
        );
    }
    println!();
    println!("下一步:");
    println!(
        "  vidbrief preprocess {}    # 步 1:差帧图 / 转录 / 预筛选截图",
        layout.root().display()
    );
}

fn display_dir(path: &Path) -> String {
    let mut text = path.display().to_string();
    if !text.ends_with(['/', '\\']) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

fn link_label(link: vidbrief_project::SourceLink) -> &'static str {
    use vidbrief_project::SourceLink::*;
    match link {
        Hardlink => "硬链接",
        Symlink => "软链接",
        Copy => "复制",
    }
}
