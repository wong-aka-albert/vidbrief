//! 可视化:工具生成的 HTML 视图 + 本地查看服务。
//!
//! 设计见 `docs/DESIGN.md` §9。
//!
//! | 模块 | 干什么 |
//! |------|--------|
//! | [`data`] | 把项目文件组装成前端要用的那一坨 JSON |
//! | [`html`] | 把数据塞进 `ui/timeline.html` 模板 |
//! | [`server`] | 本地查看服务:把项目目录当静态站点发布(支持 Range,所以视频能 seek) |
//! | [`browser`] | 渲染完打开默认浏览器 —— "弹出 UI" |
//!
//! # 为什么必须有本地服务
//!
//! 浏览器用 `file://` 打开页面时,对本地媒体有跨域/权限限制 ——
//! `<video>` 能显示但**拖时间轴不一定能跳**。走 `http://127.0.0.1` 之后,
//! 视图 / 图片 / 视频同源,联动才可靠。这是 §9.1 的结论,也是这一层存在的**唯一**理由。

pub mod browser;
pub mod data;
pub mod html;
pub mod server;

pub use data::{TimelineData, relative_url};
pub use html::{render_timeline, render_timeline_html};
pub use server::{
    Action, ActionHandler, ensure_running, has_action_handler, mount, set_action_handler, url_for,
};

/// 一次到位的"渲染 + 挂载 + 打开浏览器"。
///
/// 返回 `(html 路径, 浏览器 URL)`。**打开浏览器失败不算失败** ——
/// 无头机器上开不了是正常的,HTML 已经写好了,用户可以自己去开。
pub fn show_timeline(layout: &vidbrief_project::Layout, id: &str) -> anyhow::Result<(std::path::PathBuf, String)> {
    let path = render_timeline(layout)?;
    mount(id, layout.root())?;
    let url = url_for(id, vidbrief_project::layout::PREPROCESS_TIMELINE_FILE)?;
    if let Err(e) = browser::open(&url) {
        eprintln!("提示:{e}");
    }
    Ok((path, url))
}
