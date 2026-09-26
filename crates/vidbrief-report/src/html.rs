//! 渲染 `04_report/report.html`。
//!
//! 与 viz 的两个视图**同一道工序**:模板(`ui/report.html`)里留一个占位符
//! `/*__VIDBRIEF_DATA__*/null`,渲染时替换成真实数据 —— **不用 `format!` 拼 HTML**
//! (模板里有大量 CSS/JS 的花括号,拼起来是一场地狱;这条经验来自 `vidbrief-viz`)。
//!
//! 数据**原样**发出去:前端拿到的就是 `report.json` 的那份对象 + 一点补充
//! (视频 URL、截图文件、关联段文本)—— 补充项都是"渲染要用、但不在报告数据里"的
//! 事实(图在哪、段说了什么),不是又一份结论。

use anyhow::{Context, Result};
use serde::Serialize;
use vidbrief_project::{Layout, Report, Segment, Shots, Transcript};

use crate::ImageMode;

const REPORT_TEMPLATE: &str = include_str!("../../../ui/report.html");
const DATA_PLACEHOLDER: &str = "/*__VIDBRIEF_DATA__*/null";

/// 前端要的全部数据。
#[derive(Debug, Serialize)]
pub struct ReportPayload {
    /// 报告数据,**原样** —— 两级内容与来源都在里面,前端直接渲染。
    pub report: Report,
    /// 这份报告按什么口味做的(「详细 · 大白话」)—— 中文档位名在 Rust 侧算,
    /// 免得模板与导出各写一份映射。旧数据没这一栏时为 `None`,页面就不显示。
    pub style_text: Option<String>,
    /// 视频的相对 URL(相对 `report.html` 所在目录),用于回跳。
    pub video_url: String,
    /// 知识点可能引用的截图:id → 文件(相对 URL)。
    pub shots: Vec<ShotBrief>,
    /// 知识点可能关联的转录段:id → 文本(点回原文用)。
    pub segments: Vec<SegmentBrief>,
    /// **锚点过期**:报告引的图 / 段 / 话在当前材料里找不到了(人读的一句条)。
    ///
    /// 谁算的、为什么只报不拦,见 `vidbrief_ops::report::stale_anchors` ——
    /// 判据在动作层(它才拿得到"原文 + 状态"那份现行转录),这里只负责让它**在页面上可见**:
    /// 读者照着页面上的一句"讲者原话"回去核、转录里却没有,他至少要知道这件事。
    #[serde(default)]
    pub stale: Vec<String>,
    /// 图是不是内嵌的(**单文件分享版**)—— 页面上标一句:图能带走,视频那一半带不走。
    pub self_contained: bool,
}

#[derive(Debug, Serialize)]
pub struct ShotBrief {
    pub id: String,
    pub t: f64,
    /// 相对 `report.html` 的 URL。
    pub url: String,
}

#[derive(Debug, Serialize)]
pub struct SegmentBrief {
    pub id: u32,
    pub start: f64,
    pub text: String,
}

/// 组一份自包含的前端数据(报告 + 它的锚点落在哪)。
///
/// `stale` 由调用方算好了传进来(判据在动作层:`build_report` / `render_existing`
/// 走的是同一份 `stale_anchors`)—— 这里不自己判,免得两处对材料的读法不一样。
pub fn build_payload(
    layout: &Layout,
    report: &Report,
    stale: &[String],
    images: ImageMode,
) -> Result<ReportPayload> {
    let rel_page = vidbrief_project::layout::REPORT_HTML_FILE;

    // 视频:项目里那一份。meta 不在就当没有(报告照样能渲染,只是没有回跳)
    let video_url = match vidbrief_project::VideoMeta::load(&layout.video_meta()) {
        Ok(meta) => vidbrief_viz::relative_url(rel_page, meta.path.as_str())
            .context("算视频相对 URL 失败")?,
        Err(_) => String::new(),
    };

    let shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());
    let shot_briefs = shots
        .shots
        .iter()
        .map(|s| {
            let relative =
                vidbrief_viz::relative_url(rel_page, s.file.as_str()).unwrap_or_else(|_| s.file.as_str().to_string());
            ShotBrief {
                id: s.id.to_string(),
                t: s.t,
                // 单文件分享时这里是 `data:` URL —— 页面从此不依赖项目目录里的图
                url: images.encode(layout, s.file.as_str(), &relative),
            }
        })
        .collect();

    // 关联段:先从纠错稿读(有没有都行),没有退回原文
    let transcript = Transcript::load(&layout.transcript_fixed())
        .or_else(|_| Transcript::load(&layout.transcript()))
        .unwrap_or_else(|_| Transcript {
            source: vidbrief_project::RelPath::new("source/video.mp4").unwrap(),
            model: String::new(),
            language: String::new(),
            duration: 0.0,
            generated_at: vidbrief_project::now(),
            segments: Vec::new(),
        });
    let segments = transcript
        .segments
        .iter()
        .map(|s: &Segment| SegmentBrief {
            id: s.id,
            start: s.start,
            text: s.text.clone(),
        })
        .collect();

    Ok(ReportPayload {
        report: report.clone(),
        style_text: report.style.as_ref().map(|s| s.describe()),
        video_url,
        shots: shot_briefs,
        segments,
        stale: stale.to_vec(),
        self_contained: images == ImageMode::Embedded,
    })
}

/// 把数据塞进报告模板。
pub fn render_report_html(payload: &ReportPayload) -> Result<String> {
    let json = serde_json::to_string(payload).context("序列化报告数据失败")?;
    // JSON 里可能出现 `</script>`,那会提前结束 script 标签
    let json = json.replace("</", "<\\/");
    if !REPORT_TEMPLATE.contains(DATA_PLACEHOLDER) {
        anyhow::bail!("报告模板里找不到数据占位符 `{DATA_PLACEHOLDER}` —— 模板被改坏了?");
    }
    Ok(REPORT_TEMPLATE.replace(DATA_PLACEHOLDER, &json))
}

/// 从项目里的 `report.json` 渲染 `report.html`,返回写出的路径。
///
/// `stale` 是**锚点过期**的清单(人读的一句条)—— 由调用方(`vidbrief-ops`)算好传进来,
/// 页面把它顶在标题栏。判据只有一处,见 `vidbrief_ops::report::stale_anchors`。
///
/// `images` 决定图怎么进页面:相对路径(默认)还是内嵌 base64(单文件分享,见 [`ImageMode`])。
pub fn render_report(
    layout: &Layout,
    stale: &[String],
    images: ImageMode,
) -> Result<std::path::PathBuf> {
    let report = Report::load(&layout.report_json())
        .with_context(|| format!("读取 {} 失败 —— 步 4 的输入是它", layout.report_json().display()))?;
    let payload = build_payload(layout, &report, stale, images)?;
    let html = render_report_html(&payload)?;
    let out = layout.report_html();
    vidbrief_project::write_atomic_str(&out, &html)
        .with_context(|| format!("写入失败:{}", out.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::{ReportTopic, TopicSource};

    fn sample_report() -> Report {
        let mut r = Report::new();
        r.summary = "# 总述".into();
        r.topics.push(ReportTopic {
            id: 1,
            t: 86.0,
            title: "切主图".into(),
            body: "正文".into(),
            shots: vec![],
            segments: vec![],
            sources: vec![TopicSource::External {
                title: "语法文档".into(),
                url: "https://example.com".into(),
            }],
            verify: vec!["s001 上的数字看不清,核一下".into()],
        });
        r
    }

    #[test]
    fn template_gets_data_injected_and_keeps_its_css() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-html-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let layout = Layout::new(&dir);

        let payload = build_payload(&layout, &sample_report(), &[], ImageMode::Relative).unwrap();
        let html = render_report_html(&payload).unwrap();
        assert!(html.contains("\"切主图\""), "数据该被注入");
        assert!(!html.contains(DATA_PLACEHOLDER), "占位符该被替换掉");
        assert!(html.contains("<style>"), "模板的 CSS 该原样保留");
        // 数据里的 `</` 会被转义,免得提前结束 script 标签
        let mut tricky = sample_report();
        tricky.summary = "结束标签 </script> 在正文里".into();
        let payload = build_payload(&layout, &tricky, &[], ImageMode::Relative).unwrap();
        let html = render_report_html(&payload).unwrap();
        assert!(!html.contains("</script> 在正文里"), "该转义 </:");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_writes_the_page_next_to_the_data() {
        let dir =
            std::env::temp_dir().join(format!("vidbrief-report-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        std::fs::create_dir_all(layout.report_dir()).unwrap();
        sample_report().save(&layout.report_json()).unwrap();

        let path = render_report(&layout, &[], ImageMode::Relative).unwrap();
        assert!(path.ends_with("report.html"), "{}", path.display());
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("\"切主图\""), "{html:.200}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 摆一张图 + 一份截图清单(单文件那条测试要用)。
    fn stage_one_shot(layout: &Layout) -> (Vec<vidbrief_project::Shot>, String) {
        use vidbrief_project::{Shot, ShotId, ShotOrigin, ShotStatus};
        let mut shots = Shots::new();
        let sid = ShotId::new(1).unwrap();
        let file = Shot::default_rel_path(ShotOrigin::DenseRegion, sid, 2.0).unwrap();
        shots.shots.push(Shot {
            id: sid,
            t: 2.0,
            file: file.clone(),
            origin: ShotOrigin::DenseRegion,
            region_id: Some(1),
            status: ShotStatus::Keep,
            reason: None,
            created_at: vidbrief_project::now(),
            check: None,
        });
        std::fs::create_dir_all(layout.shots_dir()).unwrap();
        shots.save(&layout.shots_json()).unwrap();
        std::fs::write(layout.rel(file.as_str()), b"JPEG-JUNK").unwrap();
        (shots.shots, file.as_str().to_string())
    }

    /// 单文件分享:图变成 `data:` URL —— 页面从此不依赖项目目录里的图。
    /// (顺带钉住"页面知道自己是单文件版":标题栏那个标签靠 `self_contained`。)
    #[test]
    fn embedded_images_travel_inside_the_page() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        std::fs::create_dir_all(layout.report_dir()).unwrap();
        let (_, file) = stage_one_shot(&layout);
        sample_report().save(&layout.report_json()).unwrap();

        let relative = render_report(&layout, &[], ImageMode::Relative).unwrap();
        let html = std::fs::read_to_string(&relative).unwrap();
        assert!(html.contains(&file), "默认走相对路径:{html:.300}");

        let embedded = render_report(&layout, &[], ImageMode::Embedded).unwrap();
        let html = std::fs::read_to_string(&embedded).unwrap();
        assert!(
            html.contains("data:image/jpeg;base64,"),
            "单文件版该把图内嵌进页面:{html:.300}"
        );
        assert!(!html.contains(&file), "内嵌之后不该再引素材路径:{html:.300}");
        assert!(
            html.contains("\"self_contained\":true"),
            "页面要知道自己是单文件版:{html:.300}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 锚点过期那条:判据在动作层,这里只保证它**进得了页面**(标题栏那个红标签靠它)。
    #[test]
    fn stale_anchors_reach_the_page() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        std::fs::create_dir_all(layout.report_dir()).unwrap();
        sample_report().save(&layout.report_json()).unwrap();

        let stale = vec!["第 1 条(`切主图`)引的截图 `s001` 不在清单里了".to_string()];
        let path = render_report(&layout, &stale, ImageMode::Relative).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("引的截图"), "过期的那句话该原样进页面:{html:.200}");
        assert!(
            html.contains("id=\"stale\""),
            "标题栏得有那个位置(显不显示由前端按数据定)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
