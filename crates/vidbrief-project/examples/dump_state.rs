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
    // 结论按种类数一遍 —— 排查"某个变体怎么解成了别的"这类事时,这一行就是判据
    //(2026-09-27 那次落盘枚举挪号:改字/不改/存疑 三项的数目当场对不上投影)。
    let mut kinds = std::collections::BTreeMap::new();
    for seg in state.segments.values() {
        let kind = match seg.conclusion() {
            Some(vidbrief_project::Conclusion::Corrected { .. }) => "改了",
            Some(vidbrief_project::Conclusion::Punctuated { .. }) => "断句",
            Some(vidbrief_project::Conclusion::Kept { .. }) => "决定不改",
            Some(vidbrief_project::Conclusion::Uncertain { .. }) => "存疑",
            None => "看过没问题",
        };
        *kinds.entry(kind).or_insert(0usize) += 1;
    }
    let shown: Vec<String> = kinds.iter().map(|(k, v)| format!("{k} {v}")).collect();
    println!("结论  {}", shown.join(" / "));
    println!("底账  {} 份", state.content.len());
    for (rel, print) in &state.content {
        println!("      {rel}  {print:?}");
    }
    Ok(())
}
