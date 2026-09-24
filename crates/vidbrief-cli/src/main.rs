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
  init <video> [--name x] [--title t] [--projects-dir d]
                            建项目(建目录骨架 + 探测元数据 + 搬入源视频)
  preprocess <project>      步 1 前处理(纯算法)
  ui <project>              本地视图 + 视频服务
  mcp [--stdio]             启动 MCP 服务
  status <project>          查看项目状态
  help                      显示本条用法
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
    projects_dir: PathBuf,
}

fn parse_init(args: &[String]) -> Result<InitArgs> {
    let mut video: Option<PathBuf> = None;
    let mut name = None;
    let mut title = None;
    let mut projects_dir = PathBuf::from(vidbrief_project::layout::PROJECTS_DIR);

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        // 支持 `--k v` 与 `--k=v` 两种写法
        let (key, inline) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };

        match key {
            "--name" => name = Some(take_value(args, &mut i, inline, "--name")?),
            "--title" => title = Some(take_value(args, &mut i, inline, "--title")?),
            "--projects-dir" => {
                projects_dir = PathBuf::from(take_value(args, &mut i, inline, "--projects-dir")?)
            }
            other if other.starts_with("--") => bail!("init 不认识参数 `{other}`(见 vidbrief help)"),
            _ => {
                if video.is_some() {
                    bail!("init 只接受一个视频路径,多余的参数:`{arg}`");
                }
                video = Some(PathBuf::from(arg));
            }
        }
        i += 1;
    }

    let video =
        video.context("用法:vidbrief init <video> [--name x] [--title t] [--projects-dir d]")?;
    Ok(InitArgs {
        video,
        name,
        title,
        projects_dir,
    })
}

/// 取选项的值:`--k=v` 用等号右边,`--k v` 吃下一个参数。
fn take_value(
    args: &[String],
    i: &mut usize,
    inline: Option<String>,
    what: &str,
) -> Result<String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .with_context(|| format!("{what} 后面缺少取值"))
}

fn cmd_init(args: &[String]) -> Result<()> {
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

    // 项目 id:显式指定优先,否则由文件名净化
    let base = match &parsed.name {
        Some(raw) => ProjectId::from_video_stem(raw)
            .with_context(|| format!("--name `{raw}` 净化后为空,请用一个 ASCII 名字"))?,
        None => ProjectId::from_video_stem(&stem).with_context(|| {
            format!(
                "无法从文件名 `{stem}` 推导项目 id(可能是中文名),请用 --name 指定一个 ASCII 名字"
            )
        })?,
    };

    let projects_dir = &parsed.projects_dir;
    let id = ProjectId::dedupe(&base, |candidate| projects_dir.join(candidate).exists());
    let id = ProjectId::new(id.clone())?;
    let layout = Layout::for_project(projects_dir, id.as_ref());

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
    println!("  vidbrief preprocess {}    # 步 1:差帧图 / 转录 / 预筛选截图", layout.root().display());
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
