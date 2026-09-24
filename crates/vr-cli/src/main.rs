//! `vr` —— 独立命令行入口(脱离 agent 也能测试与使用)。
//!
//! 子命令见 `docs/DESIGN.md` §10.5。当前为骨架,子命令待各里程碑实现。

const USAGE: &str = "\
vr —— 视频图文总结 MCP 工具集

Usage: vr <command> [args]

Commands:
  init <video> [--name x]   建项目
  preprocess <project>      步 1 前处理(纯算法)
  ui <project>              本地视图 + 视频服务
  mcp [--stdio]             启动 MCP 服务
  status <project>          查看项目状态
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some(cmd @ ("init" | "preprocess" | "ui" | "mcp" | "status")) => {
            eprintln!("`vr {cmd}` 尚未实现(见 docs/DESIGN.md §13 里程碑)");
            std::process::exit(2);
        }
        _ => {
            print!("{USAGE}");
        }
    }
}
