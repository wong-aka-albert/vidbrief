//! 可视化动作:把材料渲染成页面 + 起本地服务 + 打开浏览器(`docs/DESIGN.md` §9.1)。
//!
//! 三件事分开,因为它们的代价与失败方式完全不同:
//!
//! | 动作 | 做了什么 | 失败了会怎样 |
//! |------|---------|-------------|
//! | [`render_workbench`] | 把材料塞进模板,写出 HTML | 材料没变,重跑即可(**不算步失败**) |
//! | [`serve_and_open_workbench`] / [`serve_and_open_report`] | 起本地服务 + 挂载项目 + 打开浏览器 | 页面已经写好了,用户自己去开也能看 |
//!
//! **渲染必须写文件**(浏览器要能直接打开),所以它不算"只读操作" ——
//! 但它写的是**派生视图**,不是材料:两个会话同时渲染出不同内容也无所谓,下次重渲染就一致了。
//! 写锁保护的是材料与结论(`state.bin` / `shots.json` / `transcript.fixed.json`),
//! 不保护这个 HTML。
//!
//! # 页面只有两个
//!
//! **工作台**一个页面看全部(视频 + 共轴 + 抽屉),**报告页**是它的纯图文投影
//! (`04_report/report.html`,能单独分享)。从前那两页(时间线 / 校准对照表)已经并进工作台:
//! 同一批材料的三份页面,只会让人猜"该看哪一页"。

use std::path::PathBuf;

use anyhow::Result;

use crate::project::OpenProject;

/// 渲染**工作台**(`01_preprocess/workbench.html`),返回 HTML 的位置。
///
/// 它是视频 + 五层共轴时间轴 + 轴上浮层 + 抽屉三块合起来的那一个页面。
/// 步 2 起就能用(那时还没有报告,抽屉里的知识点栏空着)。
pub fn render_workbench(opened: &OpenProject) -> Result<PathBuf> {
    vidbrief_viz::render_workbench(opened.layout())
}

/// 工作台版本:渲染 + 起服务 + (可选)打开浏览器。
///
/// **服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,轴上点了不跳。
/// 端口由操作系统分配,不会和别的东西撞车(§9.1)。
///
/// 服务是进程内单例 —— 同一个 MCP 进程反复调用只会有一个服务,端口也就一个。
pub fn serve_and_open_workbench(
    opened: &OpenProject,
    open_browser: bool,
) -> Result<(PathBuf, String)> {
    let path = render_workbench(opened)?;
    // 渲染**已经成了**(上面那一步),起服务是"顺手给你个能点开的地址"。
    // 起不来时说清这件事,并且提示重试就会再试一次 —— 别让调用方以为整件事没干成。
    // (服务被杀之后**换端口重来**这件事在 `vidbrief_viz::ensure_running` 里兜着;
    //  2026-09-27 实测:那一层缺了自愈,会话会一直拿着一个死端口报错。)
    let url = serve(opened, vidbrief_project::layout::WORKBENCH_FILE, open_browser).map_err(|e| {
        anyhow::anyhow!(
            "工作台**已经渲染好了**({}),只是本地查看服务没起来:{e:#}\n\
             再调一次就会重试;或者直接打开那个文件 —— 页面本身能看,只有视频回跳需要服务。",
            path.display()
        )
    })?;
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
    // 材料可能在报告之后被改过 —— 重渲染这条路**只报不拦**(页面是发现它的窗口);
    // 乱码同一条道理(数据是上一轮落下的,但这一页现在要给人看)
    let (stale, damaged) = match vidbrief_project::Report::load(&opened.layout().report_json()) {
        Ok(report) => (
            crate::report::stale_anchors(opened, &report),
            crate::report::damaged_in(&report),
        ),
        // 报告数据还没有/读不动 —— 下面渲染那步会给出更准的报错
        Err(_) => (Vec::new(), Vec::new()),
    };
    // 这条路是"起服务 + 开着视频看页面",图用相对路径就够(素材就在项目里)
    let path = vidbrief_report::html::render_report(
        opened.layout(),
        &vidbrief_report::Notices {
            stale: stale.clone(),
            damaged,
        },
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

        let path = render_workbench(&opened).unwrap();
        assert_eq!(path, fake.layout().workbench());
        assert!(path.is_file());
    }

    /// 一条知识点要看的东西**都落在这一页上**:图/重复图靠并排的缩略图与弃用标记,
    /// 动过的段靠结论,轮次靠 `checked_at`。这条测试钉住的是后两样 —— 结论、轮数。
    /// **日志是反面**:文件照旧写,但一个字节都不该进页面(见 devlog [260927-07])。
    #[test]
    fn workbench_carries_conclusions_and_rounds() {
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
        let path = render_workbench(&opened).unwrap();
        assert_eq!(path, fake.layout().workbench());
        let html = std::fs::read_to_string(&path).unwrap();
        for needle in [
            "派森",           // 原文
            "Python 列表结构",   // 改后
            "派森 → Python",  // 改动说明
            "\"rounds\":1",     // 轮数
            "\"state\":\"checked\"", // 段 2 审过没话说,也得标出来
        ] {
            assert!(html.contains(needle), "工作台丢了 {needle}");
        }
        // 日志文件在上面写过(就在这个测试里),它**一个字节都不该进页面** ——
        // 拿它当素材,等于让读者拿一段手写的轮次时段去对每一行的时刻。
        assert!(
            !html.contains("第 1 轮") && !html.contains("校准日志"),
            "校准日志不该进工作台:{html}"
        );
    }

    /// 状态读不动时视图**照常渲染**(它是给人看的,不是判据),但要把
    /// 「结论栏来自明文投影」这件事亮出来 —— `rounds` 为 `null` 就是那句话的开关。
    ///
    /// 注意这一枪打在 **viz 层**(layout 级):`OpenProject` 在状态读不动时拒绝开工
    /// (那是门的规矩,每件工具都如此),所以这里不经过它。
    #[test]
    fn workbench_still_renders_when_state_is_unreadable() {
        let fake = Fake::new("view-calib-broken").with_curve(5).with_shot(1, 9.0);
        std::fs::write(fake.layout().state_file(), b"garbage").unwrap();

        let path = vidbrief_viz::render_workbench(fake.layout()).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(
            html.contains("\"rounds\":null"),
            "状态读不动时 rounds 必须是 null:{html}"
        );
    }
}
