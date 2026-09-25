//! 可视化动作:F 组的 `render_timeline`。
//!
//! 设计见 `docs/DESIGN.md` §9.1。两件事分开,因为它们的代价与失败方式完全不同:
//!
//! | 动作 | 做了什么 | 失败了会怎样 |
//! |------|---------|-------------|
//! | [`render_timeline`] | 把材料塞进模板,写出 `timeline.html` | 材料没变,重跑即可(**不算步失败**) |
//! | [`serve_and_open`] | 起本地服务 + 挂载项目 + 打开浏览器 | 页面已经写好了,用户自己去开也能看 |
//!
//! **渲染必须写文件**(浏览器要能直接打开),所以它不算"只读操作" ——
//! 但它写的是**派生视图**,不是材料:两个会话同时渲染出不同内容也无所谓,下次重渲染就一致了。
//! 写锁保护的是材料与结论(`project.json` / `shots.json` / `transcript.fixed.json`),
//! 不保护这个 HTML。

use std::path::PathBuf;

use anyhow::Result;

use crate::project::OpenProject;

/// 渲染时间线视图,返回 HTML 的位置。
pub fn render_timeline(opened: &OpenProject) -> Result<PathBuf> {
    vidbrief_viz::render_timeline(opened.layout())
}

/// 渲染 + 起本地服务 + (可选)打开浏览器。返回 `(html 路径, 浏览器 URL)`。
///
/// **服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,时间轴点了不跳。
/// 端口由操作系统分配,不会和别的东西撞车(§9.1)。
///
/// 服务是进程内单例 —— 同一个 MCP 进程反复调用只会有一个服务,端口也就一个。
pub fn serve_and_open(opened: &OpenProject, open_browser: bool) -> Result<(PathBuf, String)> {
    let layout = opened.layout();
    let path = vidbrief_viz::render_timeline(layout)?;
    vidbrief_viz::mount(opened.id(), layout.root())?;
    let url = vidbrief_viz::url_for(opened.id(), vidbrief_project::layout::PREPROCESS_TIMELINE_FILE)?;
    if open_browser {
        // 打不开浏览器**不算失败** —— 无头机器上开不了是正常的,HTML 已经写好了
        if let Err(e) = vidbrief_viz::browser::open(&url) {
            eprintln!("提示:{e}");
        }
    }
    Ok((path, url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;

    #[test]
    fn rendering_writes_the_html_into_the_project() {
        let fake = Fake::new("view-render").with_curve(5).with_shot(1, 9.0);
        let opened = fake.open();

        let path = render_timeline(&opened).unwrap();
        assert_eq!(path, fake.layout().preprocess_timeline());
        assert!(path.is_file());
    }
}
