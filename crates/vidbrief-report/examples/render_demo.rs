//! 实拍:把某个项目的 `04_report/report.json` 渲染成 `report.html` + `report.md`。
//!
//! ```text
//! cargo run -p vidbrief-report --example render_demo -- <项目目录>
//! ```
//!
//! 存在的理由:渲染器要**用真材料看一眼**才算跑过 —— 单测能证明"字符串里有啥",
//! 证明不了"页面打开是什么样"。

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .expect("用法:cargo run -p vidbrief-report --example render_demo -- <项目目录>");
    let layout = vidbrief_project::Layout::new(std::path::Path::new(&dir));

    // 锚点过期:这条实拍只看渲染,不复核(判据在 `vidbrief-ops::report::stale_anchors`)
    let html = vidbrief_report::render_report(&layout, &vidbrief_report::Notices::default(), vidbrief_report::ImageMode::Relative)?;
    println!("html: {}", html.display());

    let report = vidbrief_project::Report::load(&layout.report_json())?;
    let md = vidbrief_report::write_markdown(&layout, &report, vidbrief_report::ImageMode::Relative)?;
    println!("md:   {}", md.display());
    Ok(())
}
