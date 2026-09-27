//! 看一眼 `state.bin` 里**到底是什么**(排查用,不是产品的一部分)。
//!
//! ```text
//! cargo run -p vidbrief-project --example dump_state -- <项目目录|state.bin>
//! ```
//!
//! 为什么要有它:`status` / `get_project` 只把状态**摘要**成人话,而排查那种
//! "某个字段怎么没了"的问题时要看的是原始那几个数(rev、四步进度、三张条目表、底账)。
//! `ProjectState::to_payload` 那句注释里写的"看一眼里面是什么"指的就是这条路。

use vidbrief_project::ProjectState;

fn main() -> anyhow::Result<()> {
    let arg = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "用法:dump_state <项目目录|state.bin>".to_string());
    let path = std::path::PathBuf::from(arg);
    let path = if path.is_dir() {
        path.join("state.bin")
    } else {
        path
    };

    let Some(state) = ProjectState::read_file(&path)? else {
        println!("{}:不在(按空状态算)", path.display());
        return Ok(());
    };
    println!("文件  {}", path.display());
    println!("rev   {}", state.rev);
    println!(
        "进度  前处理={} ({:?})  校准={} 轮  总结={} ({:?})  报告={} ({:?})",
        state.progress.preprocess.done,
        state.progress.preprocess.at,
        state.progress.calibrate.rounds,
        state.progress.summarize.done,
        state.progress.summarize.at,
        state.progress.report.done,
        state.progress.report.at,
    );
    println!(
        "条目  截图 {} / 转录 {} / 候选 {}",
        state.shots.len(),
        state.segments.len(),
        state.supplements.len()
    );
    println!("底账  {} 份", state.content.len());
    for (rel, print) in &state.content {
        println!("      {rel}  {print:?}");
    }
    Ok(())
}
