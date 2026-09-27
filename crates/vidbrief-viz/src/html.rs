//! 渲染 HTML 视图。
//!
//! 模板在 `ui/` 下(`timeline.html` / `calibrate.html`),里面留了一个占位符
//! `/*__VIDBRIEF_DATA__*/null`,渲染时替换成真实数据。**不在这里用 `format!` 拼 HTML**
//! —— 模板里有大量 CSS/JS 的 `{}`,拼起来是一场地狱。占位符替换是唯一能在模板里
//! 同时容纳 CSS 和 JS 的做法。

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::Layout;

use crate::data::{self, CalibrationData, TimelineData};

/// 时间线模板。`include_str!` 让路径在**编译期**就被检查 —— 模板改名会编译不过,
/// 而不是等到运行时才发现页面空白。
const TIMELINE_TEMPLATE: &str = include_str!("../../../ui/timeline.html");

/// 校准对照表模板(§9.3)。
const CALIBRATION_TEMPLATE: &str = include_str!("../../../ui/calibrate.html");

/// 数据占位符。模板里那个 `null` 只是为了让 HTML 在没被渲染时也是合法 JSON。
const DATA_PLACEHOLDER: &str = "/*__VIDBRIEF_DATA__*/null";

/// 把数据塞进模板 —— 两个视图共用的唯一一道工序。
fn inject(template: &str, data: &impl Serialize) -> Result<String> {
    let json = serde_json::to_string(data).context("序列化视图数据失败")?;
    // JSON 里可能出现 `</script>`,那会提前结束 script 标签。转义掉。
    let json = json.replace("</", "<\\/");
    if !template.contains(DATA_PLACEHOLDER) {
        anyhow::bail!("视图模板里找不到数据占位符 `{DATA_PLACEHOLDER}` —— 模板被改坏了?");
    }
    Ok(template.replace(DATA_PLACEHOLDER, &json))
}

/// 把数据塞进时间线模板。
pub fn render_timeline_html(timeline: &TimelineData) -> Result<String> {
    inject(TIMELINE_TEMPLATE, timeline)
}

/// 把数据塞进校准对照表模板。
pub fn render_calibration_table_html(data: &CalibrationData) -> Result<String> {
    inject(CALIBRATION_TEMPLATE, data)
}

/// 渲染步 1 的检查视图:`01_preprocess/timeline.html`。
///
/// 返回写出的路径。
pub fn render_timeline(layout: &Layout) -> Result<std::path::PathBuf> {
    let rel = vidbrief_project::layout::PREPROCESS_TIMELINE_FILE;
    let html = render_timeline_html(&data::collect(layout, rel)?)?;
    write_view(layout, rel, &html)
}

/// 渲染步 2 的对照视图:`02_calibrate/calibrate.html`。
///
/// 返回写出的路径。
pub fn render_calibration_table(layout: &Layout) -> Result<std::path::PathBuf> {
    let rel = vidbrief_project::layout::CALIBRATE_TABLE_FILE;
    let html = render_calibration_table_html(&data::collect_calibration(layout, rel)?)?;
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

    fn sample() -> TimelineData {
        TimelineData {
            project: ProjectBrief {
                id: "demo".into(),
                name: "样例</script><script>alert(1)</script>".into(),
                duration: 60.0,
            },
            video_url: "../source/video.mp4".into(),
            probe_url: "../project.json".into(),
            curve: CurveBrief {
                step: 1.0,
                max_score: 1.0,
                scores: vec![0.0, 0.5, 1.0],
            },
            regions: vec![],
            shots: vec![],
            segments: vec![],
        }
    }

    #[test]
    fn template_has_the_placeholder() {
        assert!(
            TIMELINE_TEMPLATE.contains(DATA_PLACEHOLDER),
            "模板里必须留着数据占位符"
        );
    }

    #[test]
    fn rendering_substitutes_the_data() {
        let html = render_timeline_html(&sample()).unwrap();
        assert!(!html.contains(DATA_PLACEHOLDER), "占位符应当被替换掉");
        assert!(html.contains("\"id\":\"demo\""));
        assert!(html.contains("../source/video.mp4"));
    }

    #[test]
    fn script_tags_inside_data_are_escaped() {
        // 项目名里出现 `</script>` 会提前关掉数据块,后面的脚本全乱。
        // 这不是臆想 —— 项目名来自视频文件名,用户想叫什么叫什么。
        let html = render_timeline_html(&sample()).unwrap();
        assert!(
            !html.contains("</script><script>alert(1)"),
            "数据里的 </script> 必须被转义"
        );
        assert!(html.contains("<\\/script>"), "应当是转义后的形态");
    }

    #[test]
    fn rendered_html_still_declares_the_data_block() {
        let html = render_timeline_html(&sample()).unwrap();
        assert!(html.contains(r#"<script id="vidbrief-data" type="application/json">"#));
    }

    fn calib_sample() -> CalibrationData {
        CalibrationData {
            project: ProjectBrief {
                id: "demo".into(),
                name: "样例</script><script>alert(1)</script>".into(),
                duration: 60.0,
            },
            generated_at: "2026-09-26 10:00:00 +08:00".into(),
            rounds: Some(2),
            shots: vec![],
            segments: vec![],
            timeline_url: "../01_preprocess/timeline.html".into(),
        }
    }

    #[test]
    fn calibration_template_has_the_placeholder() {
        assert!(
            CALIBRATION_TEMPLATE.contains(DATA_PLACEHOLDER),
            "对照表模板里必须留着数据占位符"
        );
    }

    #[test]
    fn calibration_rendering_substitutes_and_escapes() {
        let html = render_calibration_table_html(&calib_sample()).unwrap();
        assert!(!html.contains(DATA_PLACEHOLDER), "占位符应当被替换掉");
        assert!(html.contains("\"rounds\":2"));
        assert!(
            !html.contains("校准日志"),
            "页面上不该再有校准日志区块(它是 agent 的账,不是页面素材):{html}"
        );
        assert!(
            !html.contains("</script><script>alert(1)"),
            "数据里的 </script> 必须被转义"
        );
    }
}
