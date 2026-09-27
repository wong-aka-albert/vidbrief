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
}
