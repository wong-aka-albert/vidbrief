//! 可视化:工具生成的 HTML 视图 + 本地查看服务。
//!
//! 设计见 `docs/DESIGN.md` §9。
//!
//! | 模块 | 干什么 |
//! |------|--------|
//! | [`data`] | 把项目文件组装成前端要用的那一坨 JSON |
//! | [`html`] | 把数据塞进 `ui/workbench.html` 模板(报告页那一份在 `vidbrief-report`) |
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

pub use data::{WorkbenchData, relative_url};
pub use html::{render_workbench, render_workbench_html};
pub use server::{
    Action, ActionHandler, ensure_running, has_action_handler, mount, post_control, print_handshake,
    registry_dir, remove_registry_entry, run_in_this_process, set_action_handler, shutdown, url_for,
    use_separate_process, watch_parent_on_stdin,
};
