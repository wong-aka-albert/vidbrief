//! 可视化动作:F 组的 `render_timeline` / `render_calibration_table`。
//!
//! 设计见 `docs/DESIGN.md` §9.1。两件事分开,因为它们的代价与失败方式完全不同:
//!
//! | 动作 | 做了什么 | 失败了会怎样 |
//! |------|---------|-------------|
//! | [`render_timeline`] / [`render_calibration_table`] | 把材料塞进模板,写出 HTML | 材料没变,重跑即可(**不算步失败**) |
//! | [`serve_and_open`] / [`serve_and_open_table`] | 起本地服务 + 挂载项目 + 打开浏览器 | 页面已经写好了,用户自己去开也能看 |
//!
//! **渲染必须写文件**(浏览器要能直接打开),所以它不算"只读操作" ——
//! 但它写的是**派生视图**,不是材料:两个会话同时渲染出不同内容也无所谓,下次重渲染就一致了。
//! 写锁保护的是材料与结论(`state.bin` / `shots.json` / `transcript.fixed.json`),
//! 不保护这个 HTML。

use std::path::PathBuf;

use anyhow::Result;

use crate::project::OpenProject;

/// 渲染时间线视图,返回 HTML 的位置。
pub fn render_timeline(opened: &OpenProject) -> Result<PathBuf> {
    vidbrief_viz::render_timeline(opened.layout())
}

/// 渲染校准对照表视图(`02_calibrate/calibrate.html`),返回 HTML 的位置。
pub fn render_calibration_table(opened: &OpenProject) -> Result<PathBuf> {
    vidbrief_viz::render_calibration_table(opened.layout())
}

/// 渲染 + 起本地服务 + (可选)打开浏览器。返回 `(html 路径, 浏览器 URL)`。
///
/// **服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,时间轴点了不跳。
/// 端口由操作系统分配,不会和别的东西撞车(§9.1)。
///
/// 服务是进程内单例 —— 同一个 MCP 进程反复调用只会有一个服务,端口也就一个。
pub fn serve_and_open(opened: &OpenProject, open_browser: bool) -> Result<(PathBuf, String)> {
    let path = render_timeline(opened)?;
    let url = serve(opened, vidbrief_project::layout::PREPROCESS_TIMELINE_FILE, open_browser)?;
    Ok((path, url))
}

/// 对照表版本:渲染 + 起服务 + (可选)打开浏览器。
///
/// 对照表本身不播视频,`file://` 也能看;走服务是为了跟时间线**同一个入口**
/// (两页互相链接,时间线那页离了服务拖不动进度条)。
pub fn serve_and_open_table(opened: &OpenProject, open_browser: bool) -> Result<(PathBuf, String)> {
    let path = render_calibration_table(opened)?;
    let url = serve(opened, vidbrief_project::layout::CALIBRATE_TABLE_FILE, open_browser)?;
    Ok((path, url))
}

/// 报告页版本:渲染 `04_report/report.html` + 起服务 + (可选)打开浏览器。
///
/// 与上面两条**同一道工序**:报告页也要播视频(点锚点回跳),`file://` 下拖不动。
/// 只渲染、不落数据 —— 数据是 `crate::report::build_report` 的活。
///
/// 第三个返回值是**锚点过期**清单(见 [`crate::report::stale_anchors`]):
/// 这条路正是"材料改过之后再看一眼旧报告",所以它是唯一需要报这件事的入口。
pub fn serve_and_open_report(
    opened: &OpenProject,
    open_browser: bool,
) -> Result<(PathBuf, String, Vec<String>)> {
    let stale = match vidbrief_project::Report::load(&opened.layout().report_json()) {
        Ok(report) => crate::report::stale_anchors(opened, &report),
        // 报告数据还没有/读不动 —— 下面渲染那步会给出更准的报错
        Err(_) => Vec::new(),
    };
    // 这条路是"起服务 + 开着视频看页面",图用相对路径就够(素材就在项目里)
    let path = vidbrief_report::html::render_report(
        opened.layout(),
        &stale,
        vidbrief_report::ImageMode::Relative,
    )?;
    let url = serve(
        opened,
        vidbrief_project::layout::REPORT_HTML_FILE,
        open_browser,
    )?;
    Ok((path, url, stale))
}

/// 起本地服务 + 挂载项目 + (可选)打开浏览器,返回浏览器 URL。
///
/// **给同一 crate 里的渲染动作共用**(报告页在 `crate::report` 里渲染)——
/// "起服务"这件事只有一处实现,不然迟早有一个人忘了 `mount`。
pub(crate) fn serve_view(
    opened: &OpenProject,
    view_rel: &str,
    open_browser: bool,
) -> Result<String> {
    serve(opened, view_rel, open_browser)
}

/// 起本地服务 + 挂载项目 + (可选)打开浏览器,返回浏览器 URL。
fn serve(opened: &OpenProject, view_rel: &str, open_browser: bool) -> Result<String> {
    vidbrief_viz::mount(opened.id(), opened.layout().root())?;
    let url = vidbrief_viz::url_for(opened.id(), view_rel)?;
    if open_browser {
        // 打不开浏览器**不算失败** —— 无头机器上开不了是正常的,HTML 已经写好了
        if let Err(e) = vidbrief_viz::browser::open(&url) {
            eprintln!("提示:{e}");
        }
    }
    Ok(url)
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

    /// 对照表的四个必答问题(PLAN「近期待办」)都落在这一页上:
    /// 空图/重复图靠并排的缩略图与弃用标记,动过的段靠结论,轮次靠 checked_at + 内嵌日志。
    /// 这条测试钉住的是后两样 —— 结论、轮数、日志原文,一个都不许丢。
    #[test]
    fn calibration_table_carries_conclusions_rounds_and_the_log() {
        let fake = Fake::new("view-calib")
            .with_curve(5)
            .with_shot(1, 9.0)
            .with_transcript(&[(0.0, 2.0, "派森列表结构"), (2.0, 4.0, "第二段没问题")]);
        fake.set_segment_conclusion(
            1,
            Some(vidbrief_project::Conclusion::Corrected {
                text: "Python 列表结构".into(),
                kind: vidbrief_project::CorrectionType::Terminology,
                note: "派森 → Python".into(),
                reason: "画面写着「Python 列表」".into(),
            }),
        );
        fake.mark_segments_checked(&[2]);
        fake.set_shot_check(
            1,
            Some(vidbrief_project::ShotCheck {
                r#match: true,
                audio_visual_sync: true,
                type_read: "Python 列表".into(),
                neighbor_text: None,
                issues: vec![],
                confidence: vidbrief_project::Confidence::High,
                evidence: Some(vidbrief_project::Evidence {
                    segment: 1,
                    quote: "派森列表结构".into(),
                }),
                checked_at: vidbrief_project::now(),
            }),
        );
        let mut state = fake.state();
        state.progress.calibrate.bump();
        fake.set_state(&state);
        std::fs::write(
            fake.layout().calibration_log(),
            "# 校准日志\n\n## 第 1 轮(2026-09-26 10:00–10:05)\n- 段 1 改字。\n",
        )
        .unwrap();

        let opened = fake.open();
        let path = render_calibration_table(&opened).unwrap();
        assert_eq!(path, fake.layout().calibrate_table());
        let html = std::fs::read_to_string(&path).unwrap();
        for needle in [
            "派森",           // 原文
            "Python 列表结构",   // 改后
            "派森 → Python",  // 改动说明
            "\"rounds\":1",     // 轮数
            "## 第 1 轮",       // 内嵌的校准日志
            "\"state\":\"checked\"", // 段 2 审过没话说,也得标出来
        ] {
            assert!(html.contains(needle), "对照表丢了 {needle}");
        }
    }

    /// 状态读不动时视图**照常渲染**(它是给人看的,不是判据),但要把
    /// 「结论栏来自明文投影」这件事亮出来 —— `rounds` 为 `null` 就是那句话的开关。
    ///
    /// 注意这一枪打在 **viz 层**(layout 级):`OpenProject` 在状态读不动时拒绝开工
    /// (那是门的规矩,每件工具都如此),所以这里不经过它。
    #[test]
    fn calibration_table_still_renders_when_state_is_unreadable() {
        let fake = Fake::new("view-calib-broken").with_curve(5).with_shot(1, 9.0);
        std::fs::write(fake.layout().state_file(), b"garbage").unwrap();

        let path = vidbrief_viz::render_calibration_table(fake.layout()).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(
            html.contains("\"rounds\":null"),
            "状态读不动时 rounds 必须是 null:{html}"
        );
    }
}
