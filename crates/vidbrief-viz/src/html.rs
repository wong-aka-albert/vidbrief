//! 渲染 HTML 视图。
//!
//! 模板在 `ui/` 下(`workbench.html`),里面留了一个占位符 `/*__VIDBRIEF_DATA__*/null`,
//! 渲染时替换成真实数据。**不在这里用 `format!` 拼 HTML** —— 模板里有大量 CSS/JS 的
//! `{}`,拼起来是一场地狱。占位符替换是唯一能在模板里同时容纳 CSS 和 JS 的做法。
//!
//! 报告页(`04_report/report.html`)走的是**同一道工序**,只是模板与数据在
//! `vidbrief-report` 里 —— 那一页要能单独分享,所以是一个独立文件(见 DESIGN §9.3)。
//!
//! # 共用片段
//!
//! 工作台与报告页显示的是同一批东西(知识点条:标题 / 正文 / 来源 / 待核验 / 关联段),
//! 正文渲染抄两份必然漂 —— 抽成 `ui/_shared.js`,两边各注入同一份(见 [`inject`])。

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::Layout;

use crate::data::{self, WorkbenchData};

/// 工作台模板 —— **一个页面看全部**的那一个(`docs/DESIGN.md` §9.3)。
///
/// `include_str!` 让路径在**编译期**就被检查 —— 模板改名会编译不过,
/// 而不是等到运行时才发现页面空白。
const WORKBENCH_TEMPLATE: &str = include_str!("../../../ui/workbench.html");

/// 数据占位符。模板里那个 `null` 只是为了让 HTML 在没被渲染时也是合法 JSON。
const DATA_PLACEHOLDER: &str = "/*__VIDBRIEF_DATA__*/null";

/// **共用片段**:工具 + 报告正文的渲染。工作台与纯图文报告页注入的是同一份 ——
/// 两边显示的是同一批东西,抄两份必然漂(头注见 `ui/_shared.js`)。
const SHARED_JS: &str = include_str!("../../../ui/_shared.js");

/// 片段占位符。
const SHARED_PLACEHOLDER: &str = "/*__SHARED_JS__*/";

/// 把数据塞进模板 —— 视图渲染唯一的一道工序。
fn inject(template: &str, data: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(data).context("序列化视图数据失败")?;
    // JSON 里可能出现 `</script>`,那会提前结束 script 标签。转义掉。
    let json = json.replace("</", "<\\/");
    if !template.contains(DATA_PLACEHOLDER) {
        anyhow::bail!("视图模板里找不到数据占位符 `{DATA_PLACEHOLDER}` —— 模板被改坏了?");
    }
    // 共用片段只有带占位符的模板才注入(报告页那一份在 `vidbrief-report` 里注)
    let template = if template.contains(SHARED_PLACEHOLDER) {
        template.replace(SHARED_PLACEHOLDER, SHARED_JS)
    } else {
        template.to_string()
    };
    Ok(template.replace(DATA_PLACEHOLDER, &json))
}

/// 把数据塞进工作台模板。
pub fn render_workbench_html(data: &WorkbenchData) -> Result<String> {
    inject(WORKBENCH_TEMPLATE, data)
}

/// 渲染工作台:`01_preprocess/workbench.html`。
///
/// 它是**一个页面看全部**的那一个:视频 + 五层共轴时间轴 + 轴上浮层 + 抽屉三块。
/// 步 2 起就能用(那时还没有报告,抽屉里的知识点栏空着)。
///
/// 返回写出的路径。
pub fn render_workbench(layout: &Layout) -> Result<std::path::PathBuf> {
    let rel = vidbrief_project::layout::WORKBENCH_FILE;
    let html = render_workbench_html(&data::collect_workbench(layout, rel)?)?;
    write_view(layout, rel, &html)
}

/// **现算**工作台那一页(给服务用;顺手把盘上那份快照也刷新)。
///
/// # 为什么这一页要现算,而报告页不要
///
/// 工作台是"**看材料**"的那一页,而材料一直在变(校准在写结论、总结在写、报告在落)。
/// 烤进 HTML 的那份快照**只在渲染那一瞬间是对的** —— 实测(2026-09-27,样本 D
/// 重跑):agent 在步 2 中途渲过一次(那时刚清完场),步 4 跑完用户打开看到的还是
/// 「全部未核验 322 段 / 还没有报告」,差点当成"活没干"。
///
/// 报告页反过来:它是**交付物**(能单独存、发给别人),快照正是它要的形态 ——
/// 所以那一页照旧烤数据。
///
/// 盘上那份文件仍然写着(双击打开、换个静态服务器打开时看到的也是新的);
/// 写失败不影响这次请求:它是**投影**,丢了再渲一次就有。
pub fn workbench_page(root: &std::path::Path) -> Result<String> {
    let layout = Layout::new(root);
    let rel = vidbrief_project::layout::WORKBENCH_FILE;
    let html = render_workbench_html(&data::collect_workbench(&layout, rel)?)?;
    let _ = write_view(&layout, rel, &html);
    Ok(html)
}

fn write_view(layout: &Layout, rel: &str, html: &str) -> Result<std::path::PathBuf> {
    let out = layout.rel(rel);
    vidbrief_project::write_atomic_str(&out, html)
        .with_context(|| format!("写入失败:{}", out.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{CurveBrief, ProjectBrief};

    fn sample() -> WorkbenchData {
        WorkbenchData {
            project: ProjectBrief {
                id: "demo".into(),
                name: "样例</script><script>alert(1)</script>".into(),
                duration: 60.0,
            },
            video_url: "../source/video.mp4".into(),
            rounds: Some(2),
            curve: CurveBrief {
                step: 1.0,
                max_score: 1.0,
                scores: vec![0.0, 0.5, 1.0],
            },
            regions: vec![],
            shots: vec![],
            segments: vec![],
            groups: vec![],
            report: None,
        }
    }

    #[test]
    fn template_has_the_placeholder() {
        assert!(
            WORKBENCH_TEMPLATE.contains(DATA_PLACEHOLDER),
            "模板里必须留着数据占位符"
        );
    }

    #[test]
    fn rendering_substitutes_the_data() {
        let html = render_workbench_html(&sample()).unwrap();
        assert!(!html.contains(DATA_PLACEHOLDER), "占位符应当被替换掉");
        assert!(html.contains("\"id\":\"demo\""));
        assert!(html.contains("../source/video.mp4"));
    }

    /// 共用片段**真的注进去了** —— 少了它页面一开就报 `$ is not defined`,
    /// 而这在模板上完全看不出来(占位符和一个小注释长得一样)。
    #[test]
    fn the_shared_snippet_lands_in_the_template() {
        let html = render_workbench_html(&sample()).unwrap();
        assert!(!html.contains(SHARED_PLACEHOLDER), "占位符该被替换掉");
        for needle in ["const $ = (s) => document.querySelector(s)", "function topicNodes("] {
            assert!(html.contains(needle), "共用片段没进模板:{needle}");
        }
        // 片段只该进来一份 —— 进重了会撞 `const` 重定义,而页面会直接白掉
        assert_eq!(
            html.matches("function topicNodes(items, opts) {").count(),
            1,
            "共用片段进重了"
        );
    }

    #[test]
    fn script_tags_inside_data_are_escaped() {
        // 项目名里出现 `</script>` 会提前关掉数据块,后面的脚本全乱。
        // 这不是臆想 —— 项目名来自视频文件名,用户想叫什么叫什么。
        let html = render_workbench_html(&sample()).unwrap();
        assert!(
            !html.contains("</script><script>alert(1)"),
            "数据里的 </script> 必须被转义"
        );
        assert!(html.contains("<\\/script>"), "应当是转义后的形态");
    }

    #[test]
    fn rendered_html_still_declares_the_data_block() {
        let html = render_workbench_html(&sample()).unwrap();
        assert!(html.contains(r#"<script id="vidbrief-data" type="application/json">"#));
    }

    /// **服务给的那一页是现算的** —— 文件里那份是快照,材料变了再打开就是新的。
    ///
    /// 这条钉的是 2026-09-27 那次踩坑:agent 在步 2 中途渲过一次工作台
    /// (那时刚清完场),步 4 跑完用户打开看到的还是「全部未核验 / 还没有报告」——
    /// 因为盘上那份 HTML 只在渲染那一瞬间是对的。
    #[test]
    fn the_served_page_is_collected_fresh_every_time() {
        const MARK: &str = "LIVEPAGE-MARK-7f3a";
        let root = std::env::temp_dir().join(format!("vidbrief-live-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        vidbrief_project::Layout::new(&root).create_dirs().unwrap();

        std::fs::write(
            root.join("01_preprocess/../project.json"),
            r#"{"schema_version":2,"id":"demo","name":"现算样例",
                "created_at":"2026-09-24T21:00:00+08:00",
                "source":{"path":"source/video.mp4","original":"D:/videos/demo.mp4","link":"hardlink"},
                "glossary":null}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("01_preprocess/change_curve.json"),
            r#"{"source":"source/video.mp4","duration":10.0,"fps_sampled":1.0,
                "width":480,"height":270,"metric":"hist_l1_0.75+luma_0.25",
                "generated_at":"2026-09-24T21:03:11+08:00",
                "changes":[{"t":0.0,"score":0.0},{"t":1.0,"score":0.5}]}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("01_preprocess/dense_regions.json"),
            r#"{"generated_at":"2026-09-24T21:03:11+08:00",
                "params":{"window":8.0,"peak_threshold":0.25,"mass_k":3.0,"mass_floor":0.6,
                          "merge_gap":4.0,"static_break":120.0},
                "stats":{"sampled_points":2,"mass_median":0.25,"mass_threshold":0.6},
                "regions":[]}"#,
        )
        .unwrap();

        // ① 材料里还没有转录 —— 现算那一页里不该有这个标记
        let before = workbench_page(&root).unwrap();
        assert!(!before.contains(MARK), "这会儿还没有这条结论:{before}");

        // ② 材料变了(转录里多了一条结论):**同一个请求**再算一次就该带上它
        std::fs::write(
            root.join("01_preprocess/transcript.json"),
            format!(
                r#"{{"source":"source/video.mp4","model":"m","language":"zh","duration":10.0,
                    "generated_at":"2026-09-24T21:03:11+08:00",
                    "segments":[{{"id":1,"start":0.0,"end":2.0,"text":"各位好",
                      "conclusion":{{"outcome":"kept","note":"{MARK}","reason":"原句通顺"}},
                      "checked_at":null}}]}}"#
            ),
        )
        .unwrap();

        let after = workbench_page(&root).unwrap();
        assert!(after.contains(MARK), "材料变了,现算的那一页就该跟着变:{after}");

        // 顺手也把盘上那份快照刷新了(双击打开 / 换个静态服务器打开时看到的也是新的)
        let on_disk = std::fs::read_to_string(root.join("01_preprocess/workbench.html")).unwrap();
        assert!(on_disk.contains(MARK), "盘上那份快照也该跟着刷新");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
