//! 报告导出为 Markdown —— `04_report/report.md`。
//!
//! **它是投影,不是源**([260926-07]):同一份报告数据的另一个视图(分享、归档)。
//! 所以这里**不解析**任何 HTML,只读 [`Report`] —— 渲染链两条路都从数据出发,
//! 谁也不从谁那儿反推。
//!
//! 图片走**相对路径**(与 `report.html` 同一份 `04_report/` 基准),不塞 base64:
//! 单文件分享是 `self_contained` 模式的活,不是默认路径(旧样例报告曾因此膨胀到 889KB)。

use anyhow::{Context, Result};
use vidbrief_project::{Layout, Report, Shots, TopicSource};

use crate::ImageMode;

/// 报告数据 → Markdown 文本。
///
/// 需要 `layout` 只为一件小事:把知识点引用的截图 id 翻成**文件相对路径**
/// (图在 `shots.json` 里,不在报告数据里 —— 报告只记 id,那是它的锚点)。
pub fn export_markdown(layout: &Layout, report: &Report, images: ImageMode) -> Result<String> {
    let shots = Shots::load(&layout.shots_json()).unwrap_or_else(|_| Shots::new());

    let mut out = String::new();
    // 口味写在最前面:这份导出到了别人手里,第一眼要能看出"它是按什么做的"
    // (页面上是标题栏里那个小标签,同一句话 —— 见 `ReportStyle::describe`)。
    if let Some(style) = &report.style {
        out.push_str(&format!("> 这份报告按「{}」做。\n", style.describe()));
        if let Some(notes) = &style.notes {
            out.push_str(&format!("> 补充要求:{notes}\n"));
        }
        out.push('\n');
    }
    let summary = report.summary.trim_end();
    if !summary.is_empty() {
        out.push_str(summary);
        out.push_str("\n\n");
    }

    if report.topics.is_empty() {
        out.push_str("> 还没有知识点。\n");
        return Ok(out);
    }

    out.push_str("---\n\n## 知识点\n");
    for topic in &report.topics {
        out.push_str(&format!(
            "\n### {} · {} ({})\n\n",
            topic.id,
            topic.title.trim(),
            format_t(topic.t)
        ));
        out.push_str(topic.body.trim_end());
        out.push('\n');

        for sid in &topic.shots {
            match shots.get(*sid) {
                Some(shot) => {
                    let rel = vidbrief_viz::relative_url(
                        vidbrief_project::layout::REPORT_MD_FILE,
                        shot.file.as_str(),
                    )
                    .unwrap_or_else(|_| shot.file.as_str().to_string());
                    let url = images.encode(layout, shot.file.as_str(), &rel);
                    out.push_str(&format!("\n![{sid}]({url})\n"));
                }
                // 图不在清单里:如实写出来,别静静少一张(报告是给人核的)
                None => out.push_str(&format!("\n> 截图 {sid} 不在截图清单里。\n")),
            }
        }

        if !topic.sources.is_empty() {
            out.push_str("\n**来源**\n\n");
            for src in &topic.sources {
                match src {
                    TopicSource::Speaker { segment, quote } => {
                        out.push_str(&format!("- 讲者:[段 {segment}]「{quote}」\n"));
                    }
                    TopicSource::External { title, url } => {
                        out.push_str(&format!("- 外部:[{title}]({url})\n"));
                    }
                }
            }
        }

        if !topic.verify.is_empty() {
            out.push_str("
**待核验**(对着视频核一下):
");
            for item in &topic.verify {
                out.push_str(&format!("- {item}
"));
            }
        }

        if !topic.segments.is_empty() {
            let segs: Vec<String> = topic.segments.iter().map(|s| s.to_string()).collect();
            out.push_str(&format!("\n> 关联转录段:{}\n", segs.join("、")));
        }
    }

    Ok(out)
}

/// 渲染并落盘 `04_report/report.md`,返回写出的路径。
pub fn write_markdown(layout: &Layout, report: &Report, images: ImageMode) -> Result<std::path::PathBuf> {
    let md = export_markdown(layout, report, images)?;
    let path = layout.report_md();
    vidbrief_project::write_atomic_str(&path, &md)
        .with_context(|| format!("写入失败:{}", path.display()))?;
    Ok(path)
}

/// `86.0` → `1:26`(分钟:秒,秒补零)。
///
/// 这是**给人对着视频看的标签**,不是材料的口径 —— 材料里一律用秒
/// (`vidbrief_ops::materials::seconds`)。既然只是标签,就不再自成一派:
/// 先把秒数舍到整秒、**再**拆分钟(原来是先 `as u64` 截断再拆,
/// 于是 59.9 秒会印成 `0:59`;顺序反过来才对得上页面上的写法)。
fn format_t(t: f64) -> String {
    let total = t.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::{RelPath, ReportTopic, Shot, ShotCheck, ShotId, ShotOrigin, ShotStatus};

    /// 页面/文章上的时刻标签:**先舍后拆**,不许出现 `0:60` 这种不存在的时刻。
    #[test]
    fn the_time_label_never_rounds_into_sixty_seconds() {
        assert_eq!(format_t(0.0), "0:00");
        assert_eq!(format_t(86.0), "1:26");
        assert_eq!(format_t(59.6), "1:00", "59.6 秒是 1 分 00 秒,不是 0:60");
        assert_eq!(format_t(899.6), "15:00");
        assert_eq!(format_t(985.43), "16:25");
        assert_eq!(format_t(-1.0), "0:00");
    }

    fn sample() -> Report {
        let mut r = Report::new();
        r.style = Some(vidbrief_project::ReportStyle {
            preset: Some(vidbrief_project::ReportPreset::Teaching),
            granularity: vidbrief_project::Granularity::Detailed,
            tone: "大白话".into(),
            notes: Some("给刚入门的人看".into()),
        });
        r.summary = "# 总述\n\n两句话讲完。".into();
        r.topics.push(ReportTopic {
            id: 1,
            t: 86.0,
            title: "切主图".into(),
            body: "这是**正文**。".into(),
            shots: vec![ShotId::new(1).unwrap()],
            segments: vec![4, 5],
            sources: vec![
                TopicSource::Speaker {
                    segment: 5,
                    quote: "现在看到的是派森列表的结构图".into(),
                },
                TopicSource::External {
                    title: "语法文档".into(),
                    url: "https://example.com/ss".into(),
                },
            ],
            verify: vec!["s001 上的数字看不清,核一下".into()],
        });
        r
    }

    #[test]
    fn markdown_carries_summary_topics_sources_and_relative_image() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-md-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        std::fs::create_dir_all(layout.shots_dir()).unwrap();
        let mut shots = Shots::new();
        let sid = ShotId::new(1).unwrap();
        shots.shots.push(Shot {
            id: sid,
            t: 9.0,
            file: Shot::default_rel_path(ShotOrigin::DenseRegion, sid, 9.0).unwrap(),
            origin: ShotOrigin::DenseRegion,
            region_id: Some(1),
            status: ShotStatus::Keep,
            reason: None,
            created_at: vidbrief_project::now(),
            check: None::<ShotCheck>,
        });
        shots.save(&layout.shots_json()).unwrap();

        let md = export_markdown(&layout, &sample(), ImageMode::Relative).unwrap();
        assert!(md.contains("> 这份报告按「教学版 · 详细 · 大白话」做。"), "{md}");
        assert!(md.contains("> 补充要求:给刚入门的人看"), "{md}");
        assert!(md.contains("# 总述"), "{md}");
        assert!(md.contains("### 1 · 切主图 (1:26)"), "{md}");
        // 图走相对路径:report.md 在 04_report/ 下,要回两层再进 01_preprocess/
        assert!(
            md.contains("(../01_preprocess/shots/s001_0009.0.jpg)"),
            "图片该是相对路径:{md}"
        );
        assert!(md.contains("讲者:[段 5]"), "{md}");
        assert!(md.contains("[语法文档](https://example.com/ss)"), "{md}");
        assert!(md.contains("关联转录段:4、5"), "{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_report_degrades_gracefully() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-empty-{}", std::process::id()));
        let layout = Layout::new(&dir);
        let md = export_markdown(&layout, &Report::new(), ImageMode::Relative).unwrap();
        assert!(md.contains("还没有知识点"), "{md}");
    }

    #[test]
    fn a_missing_shot_is_reported_not_silently_dropped() {
        let dir = std::env::temp_dir().join(format!("vidbrief-report-gone-{}", std::process::id()));
        let layout = Layout::new(&dir);
        let md = export_markdown(&layout, &sample(), ImageMode::Relative).unwrap();
        assert!(md.contains("截图 s001 不在截图清单里"), "{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relative_url_helper_is_shared_with_the_views() {
        // 这条只是钉住"报告与视图用同一套相对路径算法",省得有人另写一份
        let u = vidbrief_viz::relative_url(
            vidbrief_project::layout::REPORT_MD_FILE,
            "01_preprocess/shots/s001_0009.0.jpg",
        )
        .unwrap();
        assert_eq!(u, "../01_preprocess/shots/s001_0009.0.jpg");
        let _ = RelPath::new("04_report/report.md").unwrap();
    }
    /// 单文件分享:Markdown 里的图也内嵌(否则导出物离了项目目录就是一堆裂图)。
    #[test]
    fn embedded_markdown_carries_the_image_inside() {
        let dir = std::env::temp_dir().join(format!("vidbrief-md-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let layout = Layout::new(&dir);
        let file = stage_one_shot(&layout);

        let md = export_markdown(&layout, &sample(), ImageMode::Embedded).unwrap();
        assert!(md.contains("![s001](data:image/jpeg;base64,"), "{md}");
        assert!(!md.contains(&file), "内嵌之后不该再引路径:{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 摆一张图 + 一份截图清单(单文件那条测试要用),返回图的相对路径。
    fn stage_one_shot(layout: &Layout) -> String {
        let mut shots = Shots::new();
        let sid = ShotId::new(1).unwrap();
        let file = Shot::default_rel_path(ShotOrigin::DenseRegion, sid, 9.0).unwrap();
        shots.shots.push(Shot {
            id: sid,
            t: 9.0,
            file: file.clone(),
            origin: ShotOrigin::DenseRegion,
            region_id: Some(1),
            status: ShotStatus::Keep,
            reason: None,
            created_at: vidbrief_project::now(),
            check: None::<ShotCheck>,
        });
        std::fs::create_dir_all(layout.shots_dir()).unwrap();
        shots.save(&layout.shots_json()).unwrap();
        std::fs::write(layout.rel(file.as_str()), b"JPEG-JUNK").unwrap();
        file.as_str().to_string()
    }

}
