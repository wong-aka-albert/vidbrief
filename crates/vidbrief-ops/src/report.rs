//! 步 4:报告 —— **数据落盘 + 两个投影的渲染**。
//!
//! 设计见 `docs/DESIGN.md` §8 与 devlog [260926-07] 的"数据为源":
//!
//! ```text
//! agent 的判断 ──► 04_report/report.json ──┬──► report.html   页面(给人看)
//!                  (判据)                  └──► report.md     导出(分享、归档)
//! ```
//!
//! 两个投影都是**投影**:谁也不从谁那儿反推,换样式不动数据。这也是为什么
//! `report.md` 不再由调用方手写 —— 手写的 Markdown 让"数据为源"当场变成两处判据,
//! 而它俩迟早各说各话。
//!
//! # 门在数据前面
//!
//! 落数据之前先过[步 3 的门](crate::review::gate_summarize):**没有总结就没有报告**,
//! 而且出报告用的那份总结,得和用户在对话里看到的那份是同一份。
//!
//! # 这一层替 agent 挡的三样(全是**机械**的)
//!
//! | 挡什么 | 为什么工具挡得住 |
//! |--------|------------------|
//! | 图 / 转录段**不存在** | 清单就在项目里,一查便知 |
//! | 讲者来源的 `quote` **不在材料里** | 与 `ShotCheck.evidence` 同一条规矩:照抄的一句话,工具拿它去搜 |
//! | 一条知识点**没有来源** | 没有来源的结论不可核 —— 页面上也标不出它是听来的还是找来的 |
//!
//! 内容对不对仍然是 agent 的活;这里只保证"**它引的每一样都真在材料里**"。

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use vidbrief_report::ImageMode;
use vidbrief_project::{Report, ReportStyle, ReportTopic, ShotId, TopicSource};

use crate::project::OpenProject;

/// 报告里一条来源的**输入**形状。
///
/// 与落盘的 [`TopicSource`] 只差一处:**讲者原话只给 `quote`,段号由工具去找**。
/// 让模型自己报段号,它就会报错段号 —— 而那又是一处没法核对的自述
/// (与 `evidence_quote` 同一条规矩,见 `docs/DESIGN.md` §4.6)。
#[derive(Debug, Clone)]
pub enum SourceInput {
    /// 讲者原话:**照抄**的一句转录原文(工具拿它去材料里搜,搜不到就拒收)。
    Speaker { quote: String },
    /// 外部补充:标题 + 出处链接。页面上必须能点回去。
    External { title: String, url: String },
}

/// 一条知识点的输入。
#[derive(Debug, Clone)]
pub struct TopicInput {
    /// 项目内唯一,从 1 起。**要么每条都给,要么一条都不给**(不给就按时刻编 1…n)。
    pub id: Option<u32>,
    /// 锚点:视频内时刻(秒)。
    pub t: f64,
    /// 一句话标题。
    pub title: String,
    /// 解析正文(markdown)。
    pub body: String,
    /// 关联截图 id(接受 `s001` 或 `1`)。
    pub shots: Vec<String>,
    /// 关联转录段 id。
    pub segments: Vec<u32>,
    /// 来源。**至少要有一条**。
    pub sources: Vec<SourceInput>,
    /// **待核验**:读的人该回头核一下的点(见 [`vidbrief_project::ReportTopic::verify`])。
    /// 两头空白会被去掉;没有就空着。
    pub verify: Vec<String>,
}

/// `build_report` 的入参。
///
/// **没有 `Default`**:口味那一栏(`style`)是必填的 —— 一份"没说按什么做的报告"
/// 交到用户手上,他只能说"这不是我要的",却说不清该改哪儿。
#[derive(Debug, Clone)]
pub struct ReportInput {
    /// **这份报告按什么口味做**(粒度 + 风格 + 补充要求)—— 见 [`ReportStyle`]。
    /// 用户的提示词里带着他要什么,这一栏就是那个落点。
    pub style: ReportStyle,
    /// 全局总结(markdown)。**不给就用 `03_summary/summary.md` 那一份** ——
    /// 那正是步 3 的产物,报告只是把它摆进页面。
    pub summary: Option<String>,
    /// 局部知识点。**至少一条** —— 这一页的主体是时间轴上那些锚点。
    pub topics: Vec<TopicInput>,
}

/// 要生成哪几份投影。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    /// 只出页面。
    Html,
    /// 只出 Markdown 导出。
    Md,
    /// 两份都出(默认)。
    Both,
}

impl ReportFormat {
    pub fn wants_html(self) -> bool {
        matches!(self, Self::Html | Self::Both)
    }

    pub fn wants_md(self) -> bool {
        matches!(self, Self::Md | Self::Both)
    }

    /// 命令行 / 工具参数里的写法(`both` / `html` / `md`)。
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "both" => Ok(Self::Both),
            "html" => Ok(Self::Html),
            "md" => Ok(Self::Md),
            other => bail!("format 只能是 both / html / md,收到 `{other}`"),
        }
    }
}

/// 报告落好之后,**要不要起本地查看服务**(以及要不要顺手打开浏览器)。
///
/// 为什么这两件事必须分开:服务是个**住在这个进程里**的东西 ——
/// MCP 那个进程活得很久(整个会话期间都在),命令行那个**命令跑完就走**。
/// 命令行印一个"进程一退就成死链"的地址,比不印更坏;而"只渲染、不起服务"
/// 对 MCP 又不合适(它要给用户一个能点开的地址)。所以调用方各选各的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Serving {
    /// 只把文件写好,不起服务。
    Off,
    /// 起本地查看服务(页面要播视频,`file://` 下拖不动),`open_browser` 决定要不要
    /// 顺手打开浏览器 —— 无头机器上开不了是正常的,那**不算失败**。
    On { open_browser: bool },
}

/// 报告的落盘结果(两个入口照着它报话)。
#[derive(Debug, Clone, Serialize)]
pub struct ReportOutcome {
    /// 判据那一份:`04_report/report.json`。
    pub report_json: PathBuf,
    /// 导出视图(`format` 里有 `md` 才有)。
    pub report_md: Option<PathBuf>,
    /// 页面(`format` 里有 `html` 才有)。
    pub report_html: Option<PathBuf>,
    /// 知识点条数。
    pub topics: usize,
    /// 这份报告是按什么口味做的(「详细 · 大白话」)。重渲染旧数据时可能没有。
    pub style: Option<String>,
    /// 全局总结是哪来的:`input`(调用方给的)/ `summary.md`(步 3 那一份)。
    pub summary_from: &'static str,
    /// 本地查看服务的地址(出了页面、并且起了服务时才有)。
    pub url: Option<String>,
    /// 这份产物是不是**单文件分享版**(图内嵌 base64)。体积差一个数量级,所以要报出来。
    pub self_contained: bool,
    /// **锚点过期**:这份报告引的图 / 段 / 话,在当前材料里已经找不到了(见 [`stale_anchors`])。
    ///
    /// 空数组 = 每一处锚点都还对得上。**它不是错误**:重渲染这条路只报不拦 ——
    /// 页面恰恰是发现"材料变过"的窗口,拦下来连看都看不到。
    pub stale_anchors: Vec<String>,
}

/// **锚点复核**:这份报告引的图 / 段 / 话,在**当前**材料里还在不在。
///
/// `build_report` 那条路上这些是**校验**(对不上就退回,不许落盘);这条函数是给
/// **重渲染**用的 —— 材料在报告之后被改过(校准改了转录、截图被重抽、视频换了),
/// 页面就会引着"材料里已经没有的东西"。**只报不拦**:
///
/// - 页面是发现这件事的窗口(它本来就会把找不到的段显灰),拦下来连看都看不到;
/// - 但**不能不说** —— 读者照着页面上的一句"讲者原话"回去核,转录里却没有这句,
///   他就不知道该信谁了。
///
/// 判据与 `build_topic` 逐条对齐:图在清单里、段在转录里、引的话逐字在**某一段之内**
/// (跨段不算,与落盘时同一条道理)。返回的每一条都是**人读的一句话**。
pub fn stale_anchors(opened: &OpenProject, report: &Report) -> Vec<String> {
    let shots = crate::materials::shots_optional(opened)
        .ok()
        .flatten()
        .unwrap_or_else(vidbrief_project::Shots::new);
    let transcript = crate::materials::effective_transcript_optional(opened)
        .ok()
        .flatten()
        .map(|(t, _, _)| t);
    let duration = opened.meta().duration;
    let mut out: Vec<String> = Vec::new();

    for topic in &report.topics {
        let who = format!("第 {} 条(`{}`)", topic.id, topic.title);

        if duration > 0.0 && topic.t > duration + 1.0 {
            out.push(format!(
                "{who}的锚点 {:.1}s 落在视频之外(整支 {duration:.1}s)—— 时间轴上点不到它",
                topic.t
            ));
        }
        for id in &topic.shots {
            if shots.get(*id).is_none() {
                out.push(format!(
                    "{who}引的截图 `{id}` 不在清单里了 —— 页面上是一张打不开的图"
                ));
            }
        }
        let Some(transcript) = transcript.as_ref() else {
            let quotes_any = topic
                .sources
                .iter()
                .any(|s| matches!(s, vidbrief_project::TopicSource::Speaker { .. }));
            if !topic.segments.is_empty() || quotes_any {
                out.push(format!(
                    "{who}引了转录(段 / 讲者原话),而这个项目现在读不到转录"
                ));
            }
            continue;
        };
        for id in &topic.segments {
            if !transcript.segments.iter().any(|s| s.id == *id) {
                out.push(format!(
                    "{who}关联的转录段 seg {id} 不在当前的转录里了(全稿现在 {} 段)",
                    transcript.segments.len()
                ));
            }
        }
        for source in &topic.sources {
            let vidbrief_project::TopicSource::Speaker { segment, quote } = source else {
                continue;
            };
            let hit = transcript
                .segments
                .iter()
                .find(|s| s.effective_text().contains(quote.as_str()));
            match hit {
                None => out.push(format!(
                    "{who}引的讲者原话在转录里找不到了 ——「{}」;\
                     读者会拿它回去核,而那句话已经不在材料里",
                    short(quote, 40)
                )),
                Some(now) if now.id != *segment => out.push(format!(
                    "{who}引的讲者原话现在落在 seg {} 里(报告上记的是 seg {segment})—— 段号飘了",
                    now.id
                )),
                Some(_) => {}
            }
        }
    }
    out
}

/// 报错与提示里引用一句话时截断 —— 一整段转录塞进一行会把其它条目挤没。
fn short(text: &str, max_chars: usize) -> String {
    let mut it = text.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 落一份报告数据,按 `format` 把投影渲染出来,并标记步 4 完成。
///
/// 顺序要紧:**门 → 校验 → 落数据 → 渲染**。门和校验都在写盘之前 ——
/// 半成品报告比没有报告更难收拾(用户会拿它当定稿)。
///
/// 重做就是再调一次:**整份替换**,与 `write_summary` 同一个口径。
pub fn build_report(
    opened: &mut OpenProject,
    input: &ReportInput,
    format: ReportFormat,
    images: ImageMode,
    serving: Serving,
) -> Result<ReportOutcome> {
    crate::review::gate_summarize(opened)?;

    let summary = match input
        .summary
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        Some(given) => (given.to_string(), "input"),
        None => {
            let path = opened.layout().summary_md();
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("读取 {} 失败", path.display()))?;
            if text.trim().is_empty() {
                bail!(
                    "{} 是空的 —— 报告的全局总结没有内容可摆",
                    vidbrief_project::layout::SUMMARY_FILE
                );
            }
            (text, "summary.md")
        }
    };
    if summary.0.contains('\u{FFFD}') {
        bail!(
            "全局总结里有**乱码字符**(U+FFFD,页面上显示成一个方块)—— 编码坏掉留下的,不是内容。\n\
             把那段重写一遍再给(它来自{},页面上是读者看到的第一段话)。",
            if summary.1 == "input" {
                "你给的 `summary`"
            } else {
                vidbrief_project::layout::SUMMARY_FILE
            }
        );
    }

    let mut report = Report::new();
    report.style = Some(check_style(&input.style)?);
    report.summary = summary.0;
    report.topics = build_topics(opened, &input.topics)?;
    report.sort_by_time();

    let path = opened.layout().report_json();
    report
        .save(&path)
        .with_context(|| format!("写入 {} 失败", path.display()))?;

    // 两个投影都从**刚落盘的那份数据**渲染 —— 不从前一份,也不从对方
    let report_md = if format.wants_md() {
        Some(vidbrief_report::md::write_markdown(
            opened.layout(),
            &report,
            images,
        )?)
    } else {
        None
    };
    // 刚校完的这份锚点全对得上;照样算一遍(便宜的复核,而且材料可能在两步之间被动过)
    let stale = stale_anchors(opened, &report);
    let report_html = if format.wants_html() {
        Some(vidbrief_report::html::render_report(
            opened.layout(),
            &stale,
            images,
        )?)
    } else {
        None
    };

    // 页面要播视频(点锚点回跳),所以要有个本地服务 —— 与两个视图同一条路(§9.1)。
    // 起不起由调用方定(MCP 常驻会话 vs 命令行跑完就走,见 [`Serving`])。
    let url = match (report_html.as_ref(), serving) {
        (Some(_), Serving::On { open_browser }) => Some(crate::view::serve_view(
            opened,
            vidbrief_project::layout::REPORT_HTML_FILE,
            open_browser,
        )?),
        _ => None,
    };

    opened.state_mut().progress.report.mark_done();
    opened.save()?;

    Ok(ReportOutcome {
        report_json: path,
        report_md,
        report_html,
        topics: report.topics.len(),
        style: report.style.as_ref().map(ReportStyle::describe),
        summary_from: summary.1,
        url,
        self_contained: images == ImageMode::Embedded,
        stale_anchors: stale,
    })
}

/// 拿现有的 `report.json` 重渲染一遍(不碰数据、不动进度)。
///
/// 给"只想再看一眼"用:数据没变、样式改了、或者就是想把服务再起起来。
/// 渲染不取写锁 —— 它写的是派生视图(`timeline` / `calibrate` 同一个道理)。
pub fn render_existing(
    opened: &OpenProject,
    format: ReportFormat,
    images: ImageMode,
    serving: Serving,
) -> Result<ReportOutcome> {
    let report = Report::load(&opened.layout().report_json()).with_context(|| {
        format!(
            "读取 {} 失败 —— 步 4 的输入是它(`build_report` 落的就是这份)",
            opened.layout().report_json().display()
        )
    })?;

    // 材料可能在报告之后被改过 —— 重渲染这条路**只报不拦**(页面是发现它的窗口)
    let stale = stale_anchors(opened, &report);
    let report_md = if format.wants_md() {
        Some(vidbrief_report::md::write_markdown(
            opened.layout(),
            &report,
            images,
        )?)
    } else {
        None
    };
    let report_html = if format.wants_html() {
        Some(vidbrief_report::html::render_report(
            opened.layout(),
            &stale,
            images,
        )?)
    } else {
        None
    };
    let url = match (report_html.as_ref(), serving) {
        (Some(_), Serving::On { open_browser }) => Some(crate::view::serve_view(
            opened,
            vidbrief_project::layout::REPORT_HTML_FILE,
            open_browser,
        )?),
        _ => None,
    };

    Ok(ReportOutcome {
        report_json: opened.layout().report_json(),
        report_md,
        report_html,
        topics: report.topics.len(),
        style: report.style.as_ref().map(ReportStyle::describe),
        summary_from: "report.json(没有重新落数据)",
        url,
        self_contained: images == ImageMode::Embedded,
        stale_anchors: stale,
    })
}

/// 校"口味"那一栏 —— **必填、不许空着**。
///
/// 工具在这一栏上不做判断(哪个粒度对、什么风格好,不关它的事),只保证三件事:
/// **填了、不是空的、没有乱码字符**。空着是最常见的糊弄法:字段在、内容是空白,
/// 交付时照样说不清"这份是按什么做的"。
fn check_style(input: &ReportStyle) -> Result<ReportStyle> {
    let preset = input.preset.ok_or_else(|| {
        anyhow::anyhow!(
            "`style.preset` 没给 —— 风格轮廓是**必填**的:快扫版 / 教学版 / 故事版 /              元组版 / 汇报版 / 自定义。

             每种怎么填(条数怎么按视频长度换算、每条按什么骨架写)看              `get_guide(step=\"styles\")`;挑一种,或按用户的要求用 `custom`。"
        )
    })?;
    let tone = input.tone.trim();
    if tone.is_empty() {
        bail!(
            "`style.tone` 空着 —— 语言风格是**必填**的:用户要白话就写「大白话」,\
             要行业腔就写「行业腔」。\n\
             \n\
             没听出他偏好的话,按平实的讲人话写,并且**在交付时说清这一版是按什么做的**\
             (「要换个说法说一声,我重出」)。"
        );
    }
    let notes = input
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    for (what, text) in [("tone", Some(tone)), ("notes", notes.as_deref())] {
        if let Some(text) = text {
            if text.contains('\u{FFFD}') {
                bail!(
                    "`style.{what}` 里有**乱码字符**(U+FFFD,页面上显示成一个方块)——\
                     重写一遍再给。"
                );
            }
        }
    }
    Ok(ReportStyle {
        preset: Some(preset),
        granularity: input.granularity,
        tone: tone.to_string(),
        notes,
    })
}

/// 把输入的知识点校成落盘的那一份 —— **一处不合格就整批退回**。
///
/// 整批退回而不是"收好的、扔坏的":报告是一份**一起看过才有意义**的东西,
/// 缺一条的知识点数组等于一份没人知道自己少了什么的报告。
fn build_topics(opened: &OpenProject, inputs: &[TopicInput]) -> Result<Vec<ReportTopic>> {
    if inputs.is_empty() {
        bail!(
            "一条知识点都没给 —— 报告的主体是时间轴上那些锚点。\n\
             \n\
             每条要有:一个**时刻**(`t`)、一句话标题、一段解析正文、\
             至少一条**来源**(讲者原话照抄 / 外部补充的标题与链接)。\n\
             操作说明:`get_guide(step=\"build_report\")`。"
        );
    }

    let shots = crate::materials::shots(opened)?;
    let transcript = crate::materials::effective_transcript(opened)
        .ok()
        .map(|(t, _, _)| t);
    let duration = opened.meta().duration;

    // id:要么每条都给(唯一、≥1),要么一条都不给(按时刻编 1…n)
    let given: Vec<Option<u32>> = inputs.iter().map(|t| t.id).collect();
    let any = given.iter().any(Option::is_some);
    if any && !given.iter().all(Option::is_some) {
        bail!(
            "知识点的 `id` 要么每条都给,要么一条都不给(不给就按时刻顺序编)——\
             现在 {} 条给、{} 条没给",
            given.iter().filter(|id| id.is_some()).count(),
            given.iter().filter(|id| id.is_none()).count()
        );
    }
    if any {
        let mut seen: Vec<u32> = Vec::new();
        for id in given.iter().flatten() {
            if *id == 0 {
                bail!("知识点的 `id` 从 1 起,收到 0");
            }
            if seen.contains(id) {
                bail!("知识点的 id `{id}` 出现了两次 —— 每条要一个不重复的号");
            }
            seen.push(*id);
        }
    }

    let mut built: Vec<ReportTopic> = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        built.push(build_topic(
            input,
            index + 1,
            &shots,
            transcript.as_ref(),
            duration,
        )?);
    }

    // 没给 id 才编号 —— 给的号原样保留(它可能是正文里引用过的)
    if !any {
        let mut order: Vec<usize> = (0..built.len()).collect();
        order.sort_by(|a, b| {
            built[*a]
                .t
                .partial_cmp(&built[*b].t)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (n, index) in order.into_iter().enumerate() {
            built[index].id = (n + 1) as u32;
        }
    }
    Ok(built)
}

/// 校一条知识点。`at` 是"第几条"(从 1 起,报错里用它指人)。
fn build_topic(
    input: &TopicInput,
    at: usize,
    shots: &vidbrief_project::Shots,
    transcript: Option<&vidbrief_project::Transcript>,
    duration: f64,
) -> Result<ReportTopic> {
    let title = input.title.trim();
    if title.is_empty() {
        bail!("第 {at} 条知识点没给标题 —— 浮层与列表都靠它认人");
    }
    let body = input.body.trim();
    if body.is_empty() {
        bail!("第 {at} 条知识点(`{title}`)没给正文 —— 浮层里要放的是**完整解析**,不是一个标题");
    }
    // 乱码字符:编码坏掉留下的占位符(U+FFFD)。没人会故意写它,而它在页面上就是个方块。
    // 这一条是 M6 真 agent 联调撞出来的:模型自己产出的正文里带了一个。
    for (what, text) in [("标题", title), ("正文", body)] {
        if text.contains('\u{FFFD}') {
            bail!(
                "第 {at} 条知识点(`{title}`)的{what}里有**乱码字符**(U+FFFD,页面上显示成一个方块)——\
                 那是编码坏掉留下的,不是内容。\n\
                 \n\
                 把那一段重写一遍再给:这条数据是给人读的,一个方块会让人以为整句都不可信。"
            );
        }
    }
    if !input.t.is_finite() || input.t < 0.0 {
        bail!("第 {at} 条知识点的时刻 `t` 不合法:{}", input.t);
    }
    if duration > 0.0 && input.t > duration + 1.0 {
        bail!(
            "第 {at} 条知识点的时刻 {} 落在视频之外(整支 {duration:.1}s)——\
             锚点会飘在时间轴外头",
            input.t
        );
    }

    // 关联截图:必须在清单里(不在的话页面上是一张打不开的图)
    let mut shot_ids: Vec<ShotId> = Vec::new();
    for raw in &input.shots {
        let id = crate::shots::parse_shot_id(raw)
            .with_context(|| format!("第 {at} 条知识点引用截图 `{raw}`"))?;
        if shots.get(id).is_none() {
            bail!(
                "第 {at} 条知识点引的截图 `{raw}` 不在清单里。\n\
                 \n\
                 要用哪张图先看一眼清单(`list_shots`);缺画面就让工具抽一张\
                 (`extract_shot` / `request_shot`)。"
            );
        }
        if !shot_ids.contains(&id) {
            shot_ids.push(id);
        }
    }

    // 来源:讲者原话**照抄**,工具去材料里搜段号
    let mut sources: Vec<TopicSource> = Vec::new();
    let mut segments = input.segments.clone();
    // 关联段也得真在材料里 —— 页面上它是那条"点回原文"的入口
    if let Some(transcript) = transcript {
        for id in &segments {
            if !transcript.segments.iter().any(|s| s.id == *id) {
                bail!(
                    "第 {at} 条知识点(`{title}`)关联的转录段 seg {id} 不存在\
                     (全稿 {} 段,id 从 1 起)",
                    transcript.segments.len()
                );
            }
        }
    }
    for source in &input.sources {
        match source {
            SourceInput::Speaker { quote } => {
                let quote = quote.trim();
                if quote.is_empty() {
                    bail!(
                        "第 {at} 条知识点(`{title}`)有一条空的讲者来源 ——\
                         要么照抄一句原文,要么别给这一条"
                    );
                }
                let transcript = transcript.with_context(|| {
                    format!(
                        "第 {at} 条知识点引了讲者原话,但这个项目还没有可读的转录(`{}`)",
                        vidbrief_project::layout::TRANSCRIPT_FILE
                    )
                })?;
                // **逐字**在某一**段**里找。跨段不算:那样落下来的段号是拼的,
                // 而"这句话在哪一段"正是要留给人事后回去核对的定位。
                let hit = transcript
                    .segments
                    .iter()
                    .find(|s| s.effective_text().contains(quote));
                let Some(segment) = hit else {
                    bail!(
                        "第 {at} 条知识点(`{title}`)引了这句话,材料里找不到:\n  「{quote}」\n\
                         \n\
                         来源必须是**照抄**的一句转录原文(从 `get_transcript` 里原样搬过来,\
                         落在一段之内)。找不到就说明它不是转录里的话 ——\
                         那这条知识点就少了凭据。"
                    );
                };
                // 引到的那一段自动进"关联转录段":同一条知识点里两处别对不上
                if !segments.contains(&segment.id) {
                    segments.push(segment.id);
                }
                sources.push(TopicSource::Speaker {
                    segment: segment.id,
                    quote: quote.to_string(),
                });
            }
            SourceInput::External { title: name, url } => {
                let name = name.trim();
                let link = url.trim();
                if name.is_empty() || link.is_empty() {
                    bail!("第 {at} 条知识点(`{title}`)的外部来源缺标题或链接");
                }
                if !link.starts_with("http://") && !link.starts_with("https://") {
                    bail!(
                        "第 {at} 条知识点(`{title}`)的外部来源链接不是个能点的地址:`{link}`\n\
                         页面上它是个链接,要能从 `http://` 或 `https://` 开头"
                    );
                }
                sources.push(TopicSource::External {
                    title: name.to_string(),
                    url: link.to_string(),
                });
            }
        }
    }
    if sources.is_empty() {
        bail!(
            "第 {at} 条知识点(`{title}`)一条来源都没给 —— 没有来源的结论没法核,\
             页面上也标不出它是听来的还是找来的。\n\
             \n\
             讲者原话给 `speaker` + **照抄**的 `quote`;外部查到的给 `external` + 标题与链接。"
        );
    }

    segments.sort_unstable();
    segments.dedup();
    // 待核验:空白的丢掉;乱码字符与标题正文同罪 —— 它是给人照着核的,方块核不了
    let mut verify: Vec<String> = Vec::new();
    for raw in &input.verify {
        let one = raw.trim();
        if one.is_empty() {
            continue;
        }
        if one.contains('\u{FFFD}') {
            bail!(
                "第 {at} 条知识点(`{title}`)的**待核验**里有乱码字符(U+FFFD)—— 重写一遍再给。"
            );
        }
        verify.push(one.to_string());
    }

    Ok(ReportTopic {
        id: input.id.unwrap_or(0), // 0 = 待编号,由 build_topics 填
        t: input.t,
        title: title.to_string(),
        body: body.to_string(),
        shots: shot_ids,
        segments,
        sources,
        verify,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fake;
    use vidbrief_project::{Granularity, ReportPreset};

    /// 一个"材料齐全 + 总结已经写过"的项目:步 4 的前置条件。
    fn ready(tag: &str) -> Fake {
        let fake = Fake::new(tag)
            .with_transcript(&[
                (0.0, 2.0, "各位好,今天讲 Python 列表"),
                (2.0, 4.0, "看这一段结构图"),
            ])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        finish_calibration(&mut opened);
        crate::conclusions::write_summary(&mut opened, "# 总述\n\n两句话讲完。\n").unwrap();
        fake
    }

    /// 把步 2 的活干完(看图结论 + 转录核验)—— 好让门只剩下"总结"这一件事要说。
    fn finish_calibration(opened: &mut OpenProject) {
        opened
            .state_mut()
            .shot_mut(ShotId::new(1).unwrap())
            .check = Some(vidbrief_project::ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "Python 列表 结构".into(),
            neighbor_text: None,
            issues: vec![],
            confidence: vidbrief_project::Confidence::High,
            evidence: Some(vidbrief_project::Evidence {
                segment: 1,
                quote: "各位好,今天讲 Python 列表".into(),
            }),
            checked_at: vidbrief_project::now(),
        });
        crate::conclusions::apply_corrections(opened, &[], &[1, 2]).unwrap();
    }

    /// 一份"用户要的口味":详细 + 大白话。测试里当缺省用 ——
    /// 它的值不重要,重要的是**每次调用都得交代**(参数是必填的)。
    fn a_style() -> ReportStyle {
        ReportStyle {
            preset: Some(ReportPreset::Teaching),
            granularity: Granularity::Detailed,
            tone: "大白话".into(),
            notes: None,
        }
    }

    fn a_topic() -> TopicInput {
        TopicInput {
            id: None,
            t: 2.0,
            title: "切到解释器主窗口".into(),
            body: "讲者从大纲切到解释器主窗口,**看的是同一个对象**。".into(),
            shots: vec!["s001".into()],
            segments: vec![],
            sources: vec![SourceInput::Speaker {
                quote: "看这一段结构图".into(),
            }],
            verify: vec!["画面左下角的版本号看不清,核一下 s001".into()],
        }
    }

    #[test]
    fn it_writes_the_data_and_both_projections() {
        let fake = ready("report-build");
        let mut opened = fake.open();

        let outcome = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();

        assert_eq!(outcome.topics, 1);
        assert_eq!(outcome.summary_from, "summary.md", "不给就用步 3 那一份");
        assert!(outcome.report_json.is_file());
        assert!(outcome.report_md.unwrap().is_file());
        assert!(outcome.report_html.unwrap().is_file());

        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert!(report.summary.contains("两句话讲完"), "{}", report.summary);
        assert_eq!(report.topics.len(), 1);
        assert_eq!(report.topics[0].id, 1, "没给 id 就按时刻编号");
        // 讲者原话的段号是**工具找出来的**,不是调用方报的
        assert_eq!(
            report.topics[0].sources,
            vec![TopicSource::Speaker {
                segment: 2,
                quote: "看这一段结构图".into()
            }]
        );
        // 找到的那一段自动进了"关联转录段"
        assert_eq!(report.topics[0].segments, vec![2]);
        // 口味落进数据里 —— 页面上的小标签、导出的头一行、交付时那句话都从它来
        assert_eq!(
            report.style.as_ref().map(ReportStyle::describe),
            Some("教学版 · 详细 · 大白话".to_string())
        );

        assert!(fake.state().progress.report.done, "步 4 该标完成");
    }

    /// **口味是必填的,空着不算填** —— 用户拿到报告时说不清"这份按什么做的",
    /// 就没法说"我要的不是这个"。工具在这一栏上只做三件事:填了、不为空、没乱码。
    #[test]
    fn a_report_without_a_declared_tone_is_refused() {
        let fake = ready("report-style");
        let mut opened = fake.open();

        let err = build_report(
            &mut opened,
            &ReportInput {
                style: ReportStyle {
                     preset: Some(ReportPreset::Teaching),
                     granularity: Granularity::Standard,
                    tone: "   ".into(),
                    notes: None,
                },
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("tone"), "{err}");
        assert!(err.contains("必填"), "{err}");
        assert!(
            !fake.layout().report_json().exists(),
            "拒了就该一个字节都不落盘(半成品报告比没有报告更难收拾)"
        );

        // 两头空白去掉再存,`notes` 只剩空白就当没给
        build_report(
            &mut opened,
            &ReportInput {
                style: ReportStyle {
                    preset: Some(ReportPreset::Custom),
                    granularity: Granularity::Brief,
                    tone: "  大白话  ".into(),
                    notes: Some("   给刚入门的人看   ".into()),
                },
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
        let report = Report::load(&fake.layout().report_json()).unwrap();
        let style = report.style.expect("口味该落进数据里");
        assert_eq!(style.tone, "大白话");
        assert_eq!(style.notes.as_deref(), Some("给刚入门的人看"));
        assert_eq!(style.granularity, Granularity::Brief);
        assert_eq!(style.describe(), "自定义 · 概括 · 大白话");
    }

    /// **步 3 没做完,报告出不来。** 这是这道门最主要的一档。
    #[test]
    fn the_gate_needs_a_summary_written_by_the_tool() {
        let fake = Fake::new("report-gate")
            .with_transcript(&[
                (0.0, 2.0, "各位好,今天讲 Python 列表"),
                (2.0, 4.0, "看这一段结构图"),
            ])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        // 步 2 的活先干完:下面要测的是另一道门,别让它先拦下
        finish_calibration(&mut opened);

        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("还没有总结"), "{err}");
        assert!(err.contains("write_summary"), "要说出路:{err}");
        assert!(
            !fake.layout().report_json().is_file(),
            "被挡住就不该落半成品"
        );

        // 总结是**手写**的也不行:它没有底账
        std::fs::write(fake.layout().summary_md(), "# 手写的总结\n").unwrap();
        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("不是工具写出去的那一份"), "{err}");

        // 交回 write_summary 落一遍 —— 同样一个字都不改,门就开
        crate::conclusions::write_summary(&mut opened, "# 手写的总结\n").unwrap();
        build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
    }

    /// 写完之后**又被绕过工具改过** —— 底账对不上,门照样拦。
    #[test]
    fn editing_the_summary_by_hand_shuts_the_gate_again() {
        let fake = ready("report-gate-edit");
        let mut opened = fake.open();
        std::fs::write(
            fake.layout().summary_md(),
            "# 总述\n\n两句话讲完。\n\n(手加的一段)\n",
        )
        .unwrap();

        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("对不上"), "{err}");
        assert!(err.contains("write_summary"), "要说出路:{err}");
    }

    /// 锚点与凭据:引的图不在清单里 / 引的话不在材料里 / 来源是条点不开的链接 /
    /// 一条来源都不给 —— 四种都当场退回,不留半成品。
    #[test]
    fn bad_anchors_and_quotes_are_refused_before_anything_is_written() {
        let fake = ready("report-anchors");
        let mut opened = fake.open();

        let cases: [(TopicInput, &str); 5] = [
            (
                TopicInput {
                    shots: vec!["s009".into()],
                    ..a_topic()
                },
                "不在清单里",
            ),
            (
                TopicInput {
                    segments: vec![99],
                    ..a_topic()
                },
                "关联的转录段 seg 99 不存在",
            ),
            (
                TopicInput {
                    sources: vec![SourceInput::Speaker {
                        quote: "这句话材料里没有".into(),
                    }],
                    ..a_topic()
                },
                "材料里找不到",
            ),
            (
                TopicInput {
                    sources: vec![SourceInput::External {
                        title: "语法文档".into(),
                        url: "example.com/ss".into(),
                    }],
                    ..a_topic()
                },
                "不是个能点的地址",
            ),
            (
                TopicInput {
                    sources: vec![],
                    ..a_topic()
                },
                "一条来源都没给",
            ),
        ];
        for (topic, needle) in cases {
            let err = build_report(
                &mut opened,
                &ReportInput {
                    style: a_style(),
                    summary: None,
                    topics: vec![topic],
                },
                ReportFormat::Both,
                ImageMode::Relative,
                Serving::Off,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(needle), "该被拦下的是「{needle}」:{err}");
            assert!(
                !fake.layout().report_json().is_file(),
                "整批退回时不该落下半个文件"
            );
        }

        // 一条知识点都不给也不行 —— 这一页的主体就是那些锚点
        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("一条知识点都没给"), "{err}");
    }

    /// 渲染不出来的形状问题 —— 都是**机械**的,工具挡得住。
    #[test]
    fn shapes_that_cannot_be_rendered_are_refused() {
        let fake = ready("report-shape");
        let mut opened = fake.open();

        let bad: [(TopicInput, &str); 5] = [
            (
                TopicInput {
                    t: f64::NAN,
                    ..a_topic()
                },
                "不合法",
            ),
            (
                TopicInput {
                    t: 10_000.0,
                    ..a_topic()
                },
                "落在视频之外",
            ),
            (
                TopicInput {
                    title: "  ".into(),
                    ..a_topic()
                },
                "没给标题",
            ),
            (
                TopicInput {
                    body: String::new(),
                    ..a_topic()
                },
                "没给正文",
            ),
            (
                TopicInput {
                    body: "整理之后展开运行,无论后面是升还是\u{FFFD},都可以定义为 这一段。".into(),
                    ..a_topic()
                },
                "乱码字符",
            ),
        ];
        for (topic, needle) in bad {
            let err = build_report(
                &mut opened,
                &ReportInput {
                    style: a_style(),
                    summary: None,
                    topics: vec![topic],
                },
                ReportFormat::Both,
                ImageMode::Relative,
                Serving::Off,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains(needle), "该被拦下的是「{needle}」:{err}");
        }

        // `id` 半给半不给:说不清该按给的号还是按时刻编
        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![
                    TopicInput {
                        id: Some(1),
                        ..a_topic()
                    },
                    TopicInput {
                        t: 3.0,
                        ..a_topic()
                    },
                ],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("要么每条都给"), "{err}");

        // 全局总结里的乱码字符一样要挡下(它来自输入或 summary.md)
        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: Some("# 总述\n\n这里有个 \u{FFFD} 方块\n".into()),
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("乱码字符"), "{err}");

        // 重号要点出来
        let err = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![
                    TopicInput {
                        id: Some(3),
                        ..a_topic()
                    },
                    TopicInput {
                        id: Some(3),
                        t: 3.0,
                        ..a_topic()
                    },
                ],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("出现了两次"), "{err}");
    }

    /// 给了 `id` 就按给的用,而且知识点按**时刻**排好 —— 时间轴要的顺序。
    #[test]
    fn topics_come_out_sorted_by_time_with_the_ids_they_were_given() {
        let fake = ready("report-order");
        let mut opened = fake.open();

        let outcome = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: Some("# 我给的总结\n".into()),
                topics: vec![
                    TopicInput {
                        id: Some(7),
                        t: 5.0,
                        ..a_topic()
                    },
                    TopicInput {
                        id: Some(2),
                        t: 1.0,
                        ..a_topic()
                    },
                ],
            },
            ReportFormat::Md,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
        assert_eq!(outcome.summary_from, "input");
        assert!(outcome.report_html.is_none(), "只要 md 就不该出页面");
        assert!(outcome.report_md.unwrap().is_file());

        let report = Report::load(&fake.layout().report_json()).unwrap();
        let order: Vec<(u32, f64)> = report.topics.iter().map(|t| (t.id, t.t)).collect();
        assert_eq!(order, vec![(2, 1.0), (7, 5.0)]);
    }

    /// 重做一遍:**整份替换**,不是往后追加。重渲染则是"数据一个字节都不动"。
    #[test]
    fn building_again_replaces_the_draft_and_rendering_alone_keeps_it() {
        let fake = ready("report-redo");
        let mut opened = fake.open();

        build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![
                    a_topic(),
                    TopicInput {
                        t: 3.0,
                        ..a_topic()
                    },
                ],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
        build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();

        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert_eq!(report.topics.len(), 1, "第二遍该把第一遍整个换掉");

        // 只重渲染:数据不动,页面照写
        let before = std::fs::read_to_string(fake.layout().report_json()).unwrap();
        let outcome = render_existing(&opened, ReportFormat::Html, ImageMode::Relative, Serving::Off).unwrap();
        assert!(outcome.report_html.unwrap().is_file());
        assert!(outcome.report_md.is_none(), "没要 md 就不该写它");
        assert_eq!(
            std::fs::read_to_string(fake.layout().report_json()).unwrap(),
            before,
            "重渲染不该动数据"
        );
    }

    /// **单文件分享**:开关打开之后图内嵌进页面,返回里也说清"这一份是单文件的"。
    ///
    /// 为什么要有它:DESIGN §8 一直写着"需单文件分享时图内嵌 base64",
    /// 而参数里根本没有这一栏 —— 说明书写着一个不存在的开关,等于没有那个功能。
    #[test]
    fn a_self_contained_report_carries_its_pictures() {
        let fake = ready("report-embedded");
        let mut opened = fake.open();

        let outcome = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Both,
            ImageMode::Embedded,
            Serving::Off,
        )
        .unwrap();

        assert!(outcome.self_contained, "开关打开了,返回里要说清");
        let html = std::fs::read_to_string(outcome.report_html.as_ref().unwrap()).unwrap();
        assert!(
            html.contains("data:image/jpeg;base64,"),
            "图该被内嵌进页面:{html:.300}"
        );
        // 素材路径不该再出现在页面里(内嵌之后它不需要项目目录)
        assert!(
            !html.contains("01_preprocess/shots/s001"),
            "内嵌之后不该再引素材路径:{html:.300}"
        );
        // 导出物同理
        let md = std::fs::read_to_string(outcome.report_md.as_ref().unwrap()).unwrap();
        assert!(md.contains("data:image/jpeg;base64,"), "{md}");

        // 默认那一档(相对路径)照旧 —— 单文件是**选项**,不是新默认
        let plain = build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Html,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
        assert!(!plain.self_contained);
        let html = std::fs::read_to_string(plain.report_html.as_ref().unwrap()).unwrap();
        assert!(html.contains("s001"), "默认还是相对路径:{html:.300}");
    }

    /// 材料在报告之后被改过 —— 重渲染**照渲染,但把过期的锚点报出来**。
    ///
    /// 这条是技术债表里"报告数据的锚点是引用,材料变了不失效"的落点:
    /// 不拦(页面是发现这件事的窗口),但一句也不许漏(读者要拿它回去核)。
    #[test]
    fn stale_anchors_report_what_the_materials_no_longer_hold() {
        let fake = ready("report-stale");
        let mut opened = fake.open();
        build_report(
            &mut opened,
            &ReportInput {
                style: a_style(),
                summary: None,
                topics: vec![a_topic()],
            },
            ReportFormat::Html,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();

        // 刚出完:每一处锚点都对得上
        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert!(
            stale_anchors(&opened, &report).is_empty(),
            "刚校过一遍的报告不该有过期锚点"
        );

        // 材料被改过:讲者那句话没了(同一段被改成别的内容)
        let mut transcript =
            vidbrief_project::Transcript::load(&fake.layout().transcript()).unwrap();
        transcript.segments[1].text = "这里说的是另一码事".into();
        transcript.save(&fake.layout().transcript()).unwrap();
        // 截图被重抽:s001 不在清单里了
        let mut shots = vidbrief_project::Shots::load(&fake.layout().shots_json()).unwrap();
        let gone = shots.shots[0].id.to_string();
        shots.shots.clear();
        shots.save(&fake.layout().shots_json()).unwrap();

        let stale = stale_anchors(&opened, &report);
        assert!(
            stale.iter().any(|s| s.contains(&gone)),
            "引的截图没了要报出来:{stale:?}"
        );
        assert!(
            stale.iter().any(|s| s.contains("讲者原话")),
            "引的话在转录里找不到也要报:{stale:?}"
        );
        // 报出来的每一条都点得出是第几条 —— 不然读者不知道去哪儿修
        assert!(
            stale.iter().all(|s| s.contains("第 1 条")),
            "每条都要点名是哪一条知识点:{stale:?}"
        );

        // 重渲染那条路:照渲染(不拦),但返回里带着它,页面数据里也带着它
        let outcome = render_existing(&opened, ReportFormat::Html, ImageMode::Relative, Serving::Off).unwrap();
        assert_eq!(
            outcome.stale_anchors, stale,
            "重渲染这条路算的该是同一份清单"
        );
        let html = std::fs::read_to_string(fake.layout().report_html()).unwrap();
        assert!(
            html.contains("讲者原话"),
            "页面数据里要带上过期清单(标题栏那个红标签靠它)"
        );
    }
}
