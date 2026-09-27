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
use vidbrief_project::{Report, ReportStyle, ReportTopic};

use crate::project::OpenProject;

/// 报告里一条来源的**输入**形状。
///
/// **只有一类:外部查找的**。讲者原话不从这里进 —— 它由 `segments`(关联转录段 id)
/// 带出来,工具拿段号去转录里取原文。
///
/// 为什么改(2026-09-27 真 agent 实测):从前要求模型**逐字照抄**一句原话,
/// 工具再拿它去转录里搜、搜不到就整份退回。本意是"不让模型自报段号"(报错了没法核),
/// 代价却是**让模型当打字机**:一次真跑里连着退回五次,五次全是抄写翻车 ——
/// 掉字(「市场**上面**的」→「市场上的」)、换近义词(经常→常常)、跨段拼句,
/// 甚至**把 ASR 的错字顺手改对了**(转录是「奔发」,它照上下文写成「爆发」)——
/// 那本来是校准该夸的事,却被门当成错。而段号模型**本来就给**(`segments`),
/// 工具一秒钟就能取到原文。机械的活留给工具,别让它转手。
///
/// 数据侧不受影响:落盘的 [`TopicSource::Speaker`] 仍然读得动(旧报告照样渲染),
/// 只是**新报告不再往里写**。
#[derive(Debug, Clone)]
pub enum SourceInput {
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
    /// 关联转录段 id —— **讲者凭据就是它**:工具拿段号去转录里取原文,
    /// 模型不用抄。
    pub segments: Vec<u32>,
    /// 来源:**只放外部查找的那些**(标题 + 链接)。讲者那侧由 `segments` 带出。
    pub sources: Vec<SourceInput>,
    /// **待核验**:读的人该回头核一下的点(见 [`vidbrief_project::ReportTopic::verify`])。
    /// 两头空白会被去掉;没有就空着。
    pub verify: Vec<String>,
}

/// `build_report` 的入参 —— **内容不从这里进**。
///
/// 报告页的内容(每一条的标题、正文、锚点)来自步 3 的分段
/// (`03_summary/sections.json`,见 [`vidbrief_project::Sections`]):
/// **渲染不再需要模型给任何数据**。这里只剩"这一版按什么口味做"。
///
/// 从前不是这样:步 3 只落一篇散文,报告页要往时间轴上挂点,于是步 4 只能回头
/// 再问模型要一遍结构化数据(一次十几 KB 的 `topics[]`)—— 同一份内容写两遍,
/// 还逼着模型抄原文、报段号。2026-09-27 改成分段之后,那条路整个没了。
///
/// **没有 `Default`**:口味那一栏(`style`)是必填的 —— 一份"没说按什么做的报告"
/// 交到用户手上,他只能说"这不是我要的",却说不清该改哪儿。
#[derive(Debug, Clone)]
pub struct ReportInput {
    /// **这份报告按什么口味做**(粒度 + 风格 + 补充要求)—— 见 [`ReportStyle`]。
    /// 用户的提示词里带着他要什么,这一栏就是那个落点。
    pub style: ReportStyle,
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
    /// **这页要留意的地方**(人读的一句话),空数组 = 没有。
    ///
    /// 目前只有一类:自由正文里的乱码字符(U+FFFD)。它**不是错误**、不拦落盘 ——
    /// 理由见 [`damaged_in`]。页面把它标出来,调用方看到也可以选择重发一次。
    pub warnings: Vec<String>,
    /// 有几条知识点的**关联段是按锚点时刻就近取的**(调用方没给 `segments`)。
    ///
    /// 报出来是为了如实:页面上"那一刻他说了什么"是工具就近挂的,不是调用方挑的。
    /// 想更准就显式给段号。
    pub derived_segments: usize,
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
            // 按**组**找(引文可以横跨几个碎段),段号 = 引文起点那一段 ——
            // 与工具侧 `record_shot_check` 一个判据,免得"写的时候过了、复核说过期"。
            match transcript.locate_quote(quote) {
                None => out.push(format!(
                    "{who}引的讲者原话在转录里找不到了 ——「{}」;\
                     读者会拿它回去核,而那句话已经不在材料里",
                    short(quote, 40)
                )),
                Some(hit) if hit.segment != *segment => out.push(format!(
                    "{who}引的讲者原话现在落在 seg {} 里(报告上记的是 seg {segment})—— 段号飘了",
                    hit.segment
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

/// 扫一份**即将落盘的报告数据**里所有自由正文的 U+FFFD,每处报一条 —— **不是错**。
///
/// 为什么只报不拦(2026-09-27 定的):材料的正确性是校准/总结那几步保证的,
/// 这一步的职责是**编排**(把审过的材料挂到时间轴上)。乱码既不是编排问题,
/// 也多半不是调用方写歪的 —— 上游(宿主解码 / 网关 / 模型自己)把字节吃了一口,
/// 重写一遍常常换个地方再犯:实测三次重投,坏点是 3/6/10 → 5/8/9 → 13,
/// 改一条冒一条,拿它当拒收理由只会把 agent 逼进打地鼠,最后**产出为零**。
///
/// 该标不该毙 —— 与 [`stale_anchors`] 同一条道理:页面是发现这件事的窗口,
/// 拦下来连看都看不到;但也不能不说,读者得知道哪句有洞。
///
/// 真该拦的是**编排类**的错(引的图不在清单里、照抄的话转录里找不到、锚点落在
/// 视频外)—— 那些坏一处页面就是坏的,而且调用方改一下就能过。
///
/// 判据只有这一处:扫的是**要写下去的那份数据**(`build_report` 与 `render_existing`
/// 共用),不散在校验的各步里 —— 那样迟早出现"某条路没扫到"。
pub fn damaged_in(report: &Report) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |what: &str, text: &str| {
        let count = text.matches('\u{FFFD}').count();
        if count > 0 {
            out.push(format!(
                "{what}里有 {count} 处乱码字符(U+FFFD,页面上显示成一个方块)—— 编码坏掉留下的,\
                 不是内容。报告照常出了,页面上会标出来;想弄干净就重发一次(改那一处),\
                 不修也能这样交。"
            ));
        }
    };
    push("全局总结", &report.summary);
    if let Some(style) = &report.style {
        push("`style.tone`", &style.tone);
        if let Some(notes) = &style.notes {
            push("`style.notes`", notes);
        }
    }
    for topic in &report.topics {
        let who = format!("第 {} 条知识点(`{}`)", topic.id, topic.title);
        push(&format!("{who}的标题"), &topic.title);
        push(&format!("{who}的正文"), &topic.body);
        for one in &topic.verify {
            push(&format!("{who}的**待核验**"), one);
        }
    }
    out
}

/// 读步 3 的分段 —— 报告的内容全从它来。
///
/// 没有分段时**如实说清怎么办**,而不是出一份空报告:老项目(只有一篇
/// `summary.md`)照样能补 —— 照着旧稿分段,一次交一段。
fn load_sections(opened: &OpenProject) -> Result<vidbrief_project::Sections> {
    let path = opened.layout().summary_sections();
    if !path.is_file() {
        bail!(
            "这个项目还没有**分段总结**(`{}`)—— 报告页由它组装。\n\
             \n\
             先按段写一遍:每一段一次 `write_summary`(`t` = 这一节讲的时刻,不给 `t` 的作总述)。\n\
             项目里那篇 `{}` 还在,照着它分段即可。",
            vidbrief_project::layout::SUMMARY_SECTIONS_FILE,
            vidbrief_project::layout::SUMMARY_FILE
        );
    }
    vidbrief_project::Sections::load(&path)
        .with_context(|| format!("读 {} 失败", path.display()))
}

/// 落一份报告数据,按 `format` 把投影渲染出来,并标记步 4 完成。
///
/// **内容从步 3 的分段来**(`03_summary/sections.json`):有锚点的段 → 时间轴上的
/// 一条;没锚点的段 → 全局总结。**渲染这一步不需要模型给任何数据** ——
/// 它只交代口味(`style`)。
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

    let sections = load_sections(opened)?;
    let mut report = Report::new();
    report.style = Some(check_style(&input.style)?);
    // 没锚点的段是总述(读在最前面);一段都没有也认(报告照样出,只是没有总述)
    report.summary = sections
        .unanchored()
        .iter()
        .map(|s| s.md.trim())
        .filter(|md| !md.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    report.topics = build_topics(opened, &sections)?;
    report.sort_by_time();
    let derived_segments = sections.anchored().len();

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
    let damaged = damaged_in(&report);
    let report_html = if format.wants_html() {
        Some(vidbrief_report::html::render_report(
            opened.layout(),
            &vidbrief_report::Notices {
                stale: stale.clone(),
                damaged: damaged.clone(),
            },
            images,
        )?)
    } else {
        None
    };

    // 页面要播视频(点锚点回跳),所以要有个本地服务 —— 与两个视图同一条路(§9.1)。
    // 起不起由调用方定(MCP 常驻会话 vs 命令行跑完就走,见 [`Serving`])。
    //
    // **起不来不算这次调用的失败**:这次调用的活是"把报告落下来",而数据与两个投影
    // 上面已经写完了。实测(2026-09-27,样本 C 那趟):会话挂的查看服务被误关,
    // 这里一个 `?` 把整次调用炸掉 —— 报告在盘上、`④ 报告` 却没标记,agent 拿到一个
    // error 以为自己没成。**便利动作失败不该连坐真动作**。
    let mut warnings = damaged;
    let url = match (report_html.as_ref(), serving) {
        (Some(_), Serving::On { open_browser }) => match crate::view::serve_view(
            opened,
            vidbrief_project::layout::REPORT_HTML_FILE,
            open_browser,
        ) {
            Ok(url) => Some(url),
            Err(e) => {
                warnings.push(format!(
                    "报告已经落盘了,但**本地预览服务没起来**:{e:#}\
                     —— 想再看那一页,重新渲染一次就会再试:`vidbrief ui <项目> --view report`"
                ));
                None
            }
        },
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
        summary_from: "sections.json(步 3 的分段)",
        url,
        self_contained: images == ImageMode::Embedded,
        stale_anchors: stale,
        warnings,
        derived_segments,
    })
}

/// 拿现有的 `report.json` 重渲染一遍(不碰数据、不动进度)。
///
/// 给"只想再看一眼"用:数据没变、样式改了、或者就是想把服务再起起来。
/// 渲染不取写锁 —— 它写的是派生视图(工作台那一页同一个道理)。
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
    // 乱码同一条道理:数据是上一轮落下的,但这一页现在要给人看 —— 哪儿有洞就说哪儿
    let damaged = damaged_in(&report);
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
            &vidbrief_report::Notices {
                stale: stale.clone(),
                damaged: damaged.clone(),
            },
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
        warnings: damaged,
        // 重渲染这条路没经过输入校验:就近取过几条无从知道,报 0(页面上的段本来就是落下来的)
        derived_segments: 0,
    })
}

/// 校"口味"那一栏 —— **必填、不许空着**。
///
/// 工具在这一栏上不做判断(哪个粒度对、什么风格好,不关它的事),只保证两件事:
/// **填了、不是空的**。空着是最常见的糊弄法:字段在、内容是空白,
/// 交付时照样说不清"这份是按什么做的"。
///
/// 乱码**不在这里判**(见 [`damaged_in`])。
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
    // 乱码**不在这里判**:它由 `damaged_in` 在数据组装好之后统一扫(判据一处)。
    Ok(ReportStyle {
        preset: Some(preset),
        granularity: input.granularity,
        tone: tone.to_string(),
        notes,
    })
}

/// 把步 3 的分段组装成报告的知识点 —— **一条 = 一个带锚点的段**。
///
/// 这就是"渲染不需要模型给数据"的落点:标题、正文、锚点都在段里,
/// 证据(那一刻的转录段、那一刻的画面)由工具按 `t` 就近挂。
fn build_topics(opened: &OpenProject, sections: &vidbrief_project::Sections) -> Result<Vec<ReportTopic>> {
    let anchored = sections.anchored();
    if anchored.is_empty() {
        bail!(
            "没有任何**带锚点**的段 —— 报告页的主体是时间轴上那些点。
             
             写总结时给每一段带上它讲的**时刻**(`write_summary` 的 `t`):
             有 `t` 的段上时间轴(报告页一条),没 `t` 的段只作总述。"
        );
    }

    let shots = crate::materials::shots(opened)?;
    let transcript = crate::materials::effective_transcript(opened)
        .ok()
        .map(|(t, _, _)| t);
    let duration = opened.meta().duration;

    let mut built: Vec<ReportTopic> = Vec::with_capacity(anchored.len());
    for (index, section) in anchored.iter().enumerate() {
        built.push(build_topic(
            section,
            index + 1,
            &shots,
            transcript.as_ref(),
            duration,
        )?);
    }
    Ok(built)
}

/// 一张段落在哪一刻,就拿那一刻的**画面**:窗口内(与转录段同一个窗口)的第一张截图。
///
/// 为什么可以就近挂(而"挑图"仍然值得做):图是**离散采样**,只在画面切换点有 ——
/// 窗口内那张就是"讲这条时屏幕上的样子",时间对得上;真想要别处的图,
/// 段正文里能用 markdown 引它(`![](../01_preprocess/shots/s003_0192.0.jpg)`),
/// 页面照样渲染。
fn derived_shots(shots: &vidbrief_project::Shots, t: f64) -> Vec<vidbrief_project::ShotId> {
    let mut picked: Vec<&vidbrief_project::Shot> = shots
        .shots
        .iter()
        .filter(|s| s.t >= t && s.t < t + DERIVED_SEGMENT_WINDOW)
        .collect();
    picked.sort_by(|a, b| a.t.total_cmp(&b.t));
    picked
        .into_iter()
        .take(DERIVED_SHOT_LIMIT)
        .map(|s| s.id)
        .collect()
}

/// 就近挂图最多挂几张 —— 一张够说明"那一刻画面上是什么";多了是堆图。
const DERIVED_SHOT_LIMIT: usize = 2;

/// 一条知识点没给关联段时,按**锚点时刻就近取**:讲者在那一刻说了什么,材料里就有。
///
/// 取法(简单到能解释清):落在 `[t, t + 窗口)` 的段;一个都没有就取**起点离 t 最近**
/// 的那一段。窗口取 [`DERIVED_SEGMENT_WINDOW`] —— 一句话的长度。
///
/// 为什么要有它(2026-09-27):报告这一步的活是**组装**,证据指针本该由工具补默认值。
/// 实测里让模型自己指,它要么指歪、要么在"抄原文"上翻车(一次真跑退回五次);
/// 而这些指针**工具算得出来** —— 时间与转录都在手上。模型想更准(比如这条讲的是
/// 前面第 3 分钟那句话)仍然可以显式给 `segments`,给了就按给的。
fn derived_segments(transcript: &vidbrief_project::Transcript, t: f64) -> Vec<u32> {
    let picked: Vec<u32> = transcript
        .segments
        .iter()
        .filter(|s| s.start >= t && s.start < t + DERIVED_SEGMENT_WINDOW)
        .map(|s| s.id)
        .collect();
    if !picked.is_empty() {
        return picked;
    }
    // 那一刻正好落在两段之间(或末尾没人说话):取起点最近的那一段
    transcript
        .segments
        .iter()
        .min_by(|a, b| {
            (a.start - t)
                .abs()
                .total_cmp(&(b.start - t).abs())
        })
        .map(|s| vec![s.id])
        .unwrap_or_default()
}

/// 就近取段的窗口(秒)—— 一句话的量级。
const DERIVED_SEGMENT_WINDOW: f64 = 20.0;

/// 把**一段**组装成报告的一条知识点。`at` 是"第几条"(从 1 起,报错里用它指人)。
///
/// 退回的是**材料/编排类**的错(标题正文空着、时刻越界);段号与图由工具就近挂
/// (见 [`derived_segments`] / [`derived_shots`]),乱码由 `damaged_in` 事后统一扫。
fn build_topic(
    section: &vidbrief_project::Section,
    at: usize,
    shots: &vidbrief_project::Shots,
    transcript: Option<&vidbrief_project::Transcript>,
    duration: f64,
) -> Result<ReportTopic> {
    let title = section.title.trim();
    if title.is_empty() {
        bail!("第 {at} 段没给标题 —— 浮层与列表都靠它认人");
    }
    let body = section.md.trim();
    if body.is_empty() {
        bail!("第 {at} 段(`{title}`)没给正文 —— 浮层里要放的是**完整解析**,不是一个标题");
    }
    let Some(t) = section.t else {
        bail!("第 {at} 段(`{title}`)没有锚点 —— 上时间轴的那些段必须有 `t`");
    };
    if !t.is_finite() || t < 0.0 {
        bail!("第 {at} 段(`{title}`)的时刻 `t` 不合法:{t}");
    }
    if duration > 0.0 && t > duration + 1.0 {
        bail!("第 {at} 段(`{title}`)的时刻 {t} 落在视频之外(整支 {duration:.1}s)—— 锚点会飘在时间轴外头");
    }

    let segments = transcript.map(|tr| derived_segments(tr, t)).unwrap_or_default();
    let verify: Vec<String> = section
        .verify
        .iter()
        .map(|one| one.trim().to_string())
        .filter(|one| !one.is_empty())
        .collect();

    Ok(ReportTopic {
        id: at as u32, // 按时刻编(1..n)—— `at` 就是 build_topics 递进来的序号
        t,
        title: title.to_string(),
        body: body.to_string(),
        shots: derived_shots(shots, t),
        segments,
        // 来源只有"外部查找"那一类,而分段形态下它就写在正文里(markdown 链接)——
        // 所以这里空着;数据形态不变,老报告里的 `sources` 照样渲染。
        sources: Vec::new(),
        verify,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conclusions::SectionInput;
    use crate::testkit::Fake;
    use vidbrief_project::{Granularity, ReportPreset, ShotId, TopicSource};

    /// 一段总结的输入。待核验那一栏在 `conclusions` 自己的用例里测 ——
    /// 这里造的是"报告该长成什么样"的场景。
    fn a_section(t: Option<f64>, title: &str, md: &str) -> SectionInput {
        SectionInput {
            t,
            title: title.into(),
            md: md.into(),
            verify: Vec::new(),
        }
    }

    /// 落一段总结 —— 渲染那几步的输入就是它,统一从这儿写。
    fn write_section(opened: &mut OpenProject, input: SectionInput) {
        crate::conclusions::write_summary(opened, &input)
            .unwrap_or_else(|e| panic!("落分段失败:{e:#}"));
    }

    /// 出一道报告(图用相对路径、不起服务)—— 用例里翻来覆去就这一种调法。
    fn build(opened: &mut OpenProject, format: ReportFormat) -> Result<ReportOutcome> {
        build_report(
            opened,
            &ReportInput { style: a_style() },
            format,
            ImageMode::Relative,
            Serving::Off,
        )
    }

    /// 一个"材料齐全 + 总结已经写过"的项目:步 4 的前置条件。
    ///
    /// 总结里两段:**一段总述**(没有 `t`,只进右侧面板)+ **一段带锚点**
    /// (`t = 2.0`,报告页时间轴上的那一条)—— 报告页的两边各取所需。
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
        write_section(&mut opened, a_section(None, "总述", "两句话讲完整支视频。"));
        write_section(
            &mut opened,
            SectionInput {
                t: Some(2.0),
                title: "切到解释器主窗口".into(),
                md: "讲者从大纲切到解释器主窗口,**看的是同一个对象**。".into(),
                verify: vec!["  画面左下角的版本号看不清,核一下 s001  ".into()],
            },
        );
        fake
    }

    /// 把步 2 的活干完(看图结论 + 转录核验)—— 好让门只剩下"总结"这一件事要说。
    fn finish_calibration(opened: &mut OpenProject) {
        finish_all(opened, &[1], &[1, 2]);
    }

    /// 把清单里的图逐张核过、转录逐段报过审。
    ///
    /// 写的是**状态**(`state_mut`),不是 `shots.json` 那几栏投影 —— 门读的就是状态
    /// (理由见 `testkit` 里那几个助手的说明)。证据照抄转录里真有的那句:
    /// 门的第二档查的是"凭据还在不在材料里",这里不该被它拦下。
    fn finish_all(opened: &mut OpenProject, shot_ids: &[u32], segment_ids: &[u32]) {
        for id in shot_ids {
            opened
                .state_mut()
                .shot_mut(ShotId::new(*id).unwrap())
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
        }
        crate::conclusions::apply_corrections(opened, &[], segment_ids, &[]).unwrap();
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

    #[test]
    fn it_writes_the_data_and_both_projections() {
        let fake = ready("report-build");
        let mut opened = fake.open();

        let outcome = build(&mut opened, ReportFormat::Both).unwrap();

        assert_eq!(outcome.topics, 1, "两段里只有带锚点的那一段上时间轴");
        assert_eq!(
            outcome.summary_from, "sections.json(步 3 的分段)",
            "内容全从分段来,没有第二个来源"
        );
        assert_eq!(outcome.derived_segments, 1, "证据是工具按锚点就近挂的");
        assert!(outcome.report_json.is_file());
        assert!(outcome.report_md.unwrap().is_file());
        assert!(outcome.report_html.unwrap().is_file());

        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert!(report.summary.contains("两句话讲完"), "{}", report.summary);
        assert_eq!(report.topics.len(), 1);

        let topic = &report.topics[0];
        // 编号按时刻从 1 起(与 `docs/schema/report.schema.json` 的 `minimum: 1` 对齐)。
        // 这一条只有一段,连续性由 `topic_ids_are_numbered_from_one` 钉。
        assert_eq!(topic.id, 1, "第 1 条就该编 1");
        assert_eq!(topic.t, 2.0, "锚点原样");
        assert_eq!(topic.title, "切到解释器主窗口", "标题原样");
        assert_eq!(
            topic.body, "讲者从大纲切到解释器主窗口,**看的是同一个对象**。",
            "正文原样"
        );
        assert_eq!(
            topic.verify,
            vec!["画面左下角的版本号看不清,核一下 s001"],
            "待核验跟着段走,两头空白去掉"
        );
        assert_eq!(topic.segments, vec![2], "t=2.0 落在 [2,22) 窗口里的那一段");
        assert_eq!(topic.shots, vec![ShotId::new(1).unwrap()], "那一刻的画面");
        assert!(
            topic.sources.is_empty(),
            "分段形态下来源写在正文里(markdown 链接),这一栏恒空:{:?}",
            topic.sources
        );
        // 口味落进数据里 —— 页面上的小标签、导出的头一行、交付时那句话都从它来
        assert_eq!(
            report.style.as_ref().map(ReportStyle::describe),
            Some("教学版 · 详细 · 大白话".to_string())
        );

        assert!(fake.state().progress.report.done, "步 4 该标完成");
    }

    /// 编号一栏:**按时刻从 1 连续编**。这条栏位从前恒为 0(注释写着「由 build_topics
    /// 按时刻编」而实际没编),`stale_anchors` 报诊断时就会打「第 0 条」指不准是谁;
    /// schema 里它是 `minimum: 1`。多段才看得出"连续",所以单独立一条。
    #[test]
    fn topic_ids_are_numbered_from_one() {
        let fake = Fake::new("report-topic-ids")
            .with_transcript(&[
                (0.0, 2.0, "各位好,今天讲 Python 列表"),
                (2.0, 4.0, "看这一段结构图"),
            ])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        finish_calibration(&mut opened);
        write_section(&mut opened, a_section(Some(0.5), "第一节", "第一节的正文。"));
        write_section(&mut opened, a_section(Some(1.5), "第二节", "第二节的正文。"));
        write_section(&mut opened, a_section(Some(2.5), "第三节", "第三节的正文。"));

        let outcome = build(&mut opened, ReportFormat::Html).unwrap();
        assert_eq!(outcome.topics, 3);

        let report = Report::load(&fake.layout().report_json()).unwrap();
        let ids: Vec<u32> = report.topics.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "id 按时刻从 1 连续编,不许留 0");

        // 总述段(没有 `t`)不占号 —— 编号跟着上时间轴的那些走。
        write_section(&mut opened, a_section(None, "收束", "回头看,讲的是同一件事。"));
        let outcome = build(&mut opened, ReportFormat::Html).unwrap();
        assert_eq!(outcome.topics, 3, "没锚点的段不上时间轴");
        let report = Report::load(&fake.layout().report_json()).unwrap();
        let ids: Vec<u32> = report.topics.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 2, 3], "多一段总述不该把编号撑出空号");
    }

    /// 没有 `t` 的段只作**总述**:进 `report.summary`,不当成一条知识点。
    #[test]
    fn unanchored_sections_go_to_the_overview_and_not_the_topic_list() {
        let fake = ready("report-overview");
        let mut opened = fake.open();
        write_section(
            &mut opened,
            a_section(None, "收束", "回头看,整支讲的是同一件事。"),
        );

        let outcome = build(&mut opened, ReportFormat::Html).unwrap();

        assert_eq!(outcome.topics, 1, "总述不是知识点");
        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert!(report.summary.contains("两句话讲完"), "{}", report.summary);
        assert!(
            report.summary.contains("回头看"),
            "两段总述都拼进总述里:{}",
            report.summary
        );
        assert!(
            report.topics.iter().all(|t| t.title != "收束"),
            "总述不该出现在知识点里:{:?}",
            report.topics
        );
    }

    /// **一个带锚点的段 = 时间轴上的一条**:标题 / 正文 / 锚点原样,
    /// 证据(那一刻他说了什么、画面上是什么)由工具按 `t` 就近挂。
    ///
    /// 这条同时钉住顺序与上限:段是按提交顺序存的,报告按**时刻**排;
    /// 窗口内的截图**最多两张**(全挂上就是堆图,时间轴反而看不清)。
    ///
    /// `id` 那一栏本该跟着时刻编 1..n,而生产代码现在没编(落下来都是 0)——
    /// 这条照样按现状断言,好让那个缺口别被悄悄放过。
    #[test]
    fn an_anchored_section_becomes_one_topic_with_derived_evidence() {
        let fake = Fake::new("report-derived-topic")
            .with_transcript(&[
                (0.0, 2.0, "各位好,今天讲 Python 列表"),
                (2.0, 4.0, "看这一段结构图"),
            ])
            .with_curve(5)
            .with_shot(1, 2.0)
            .with_shot(2, 3.0)
            .with_shot(3, 5.0)
            .with_shot(4, 50.0);
        let mut opened = fake.open();
        finish_all(&mut opened, &[1, 2, 3, 4], &[1, 2]);
        // 倒着写:报告的条序与 id 该跟着**时刻**走,不跟着提交顺序
        write_section(
            &mut opened,
            a_section(Some(30.0), "后半程", "半小时之后讲的是另一件事。"),
        );
        write_section(
            &mut opened,
            a_section(Some(2.0), "切到解释器主窗口", "讲者从大纲切到解释器主窗口。"),
        );

        build(&mut opened, ReportFormat::Html).unwrap();

        let report = Report::load(&fake.layout().report_json()).unwrap();
        let times: Vec<f64> = report.topics.iter().map(|t| t.t).collect();
        assert_eq!(times, vec![2.0, 30.0], "顺序按时刻,不按提交顺序");
        assert_eq!(
            report.topics.iter().map(|t| t.id).collect::<Vec<u32>>(),
            vec![1, 2],
            "id 按时刻编:第一条是 2.0 那一节,第二条是 30.0 那一节"
        );

        let first = &report.topics[0];
        assert_eq!(first.title, "切到解释器主窗口");
        assert_eq!(first.body, "讲者从大纲切到解释器主窗口。");
        assert_eq!(first.segments, vec![2], "落在 [2,22) 窗口里的转录段");
        assert_eq!(
            first.shots,
            vec![ShotId::new(1).unwrap(), ShotId::new(2).unwrap()],
            "窗口内最早的截图,最多两张(s003 也在窗口里,但排第三)"
        );

        let second = &report.topics[1];
        assert_eq!(
            second.segments,
            vec![2],
            "t=30 落在稿子外面,取起点最近的那一段(也不是瞎填)"
        );
        assert!(second.shots.is_empty(), "那一刻附近没有截图,就不挂");
    }

    /// **口味是必填的,空着不算填** —— 用户拿到报告时说不清"这份按什么做的",
    /// 就没法说"我要的不是这个"。工具在这一栏上只做两件事:填了、不为空
    /// (乱码只报不拦,见 `damaged_in`)。
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

        // 两头空白去掉再存;`notes` 全是空白就当没给
        build_report(
            &mut opened,
            &ReportInput {
                style: ReportStyle {
                    preset: Some(ReportPreset::Custom),
                    granularity: Granularity::Brief,
                    tone: "  大白话  ".into(),
                    notes: Some("   给刚入门的人看   ".into()),
                },
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

        let err = build(&mut opened, ReportFormat::Both).unwrap_err().to_string();
        assert!(err.contains("还没有总结"), "{err}");
        assert!(err.contains("write_summary"), "要说出路:{err}");
        assert!(
            !fake.layout().report_json().is_file(),
            "被挡住就不该落半成品"
        );

        // 总结是**手写**的也不行:它没有底账
        std::fs::write(fake.layout().summary_md(), "# 手写的总结\n").unwrap();
        let err = build(&mut opened, ReportFormat::Both).unwrap_err().to_string();
        assert!(err.contains("不是工具写出去的那一份"), "{err}");

        // 交回 write_summary 落一遍 —— 内容一字不改也行,门就开
        write_section(
            &mut opened,
            a_section(Some(2.0), "手写的那一段", "# 手写的总结"),
        );
        build(&mut opened, ReportFormat::Both).unwrap();
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

        let err = build(&mut opened, ReportFormat::Both).unwrap_err().to_string();
        assert!(err.contains("对不上"), "{err}");
        assert!(err.contains("write_summary"), "要说出路:{err}");
    }

    /// **没有分段时,报错要能照做** —— 点出该走哪个工具、项目里那篇旧稿还在。
    ///
    /// 挡的是"把缺输入当成没有内容,出一页空白报告"(用户会拿它当定稿)。
    /// 老项目(只有一篇散文)照样补得回来:照着旧稿分段,一次交一段。
    #[test]
    fn a_project_without_sections_points_at_write_summary() {
        let fake = ready("report-no-sections");
        let mut opened = fake.open();
        std::fs::remove_file(fake.layout().summary_sections()).unwrap();

        let err = build(&mut opened, ReportFormat::Both).unwrap_err().to_string();

        assert!(err.contains("write_summary"), "要指出该走哪个工具:{err}");
        assert!(
            err.contains(vidbrief_project::layout::SUMMARY_FILE),
            "还要说清照着哪一份旧稿分段:{err}"
        );
        assert!(
            !fake.layout().report_json().is_file(),
            "拒绝了就不该落半成品"
        );
    }

    /// **只有总述也出不了报告** —— 报告页的主体是时间轴上那些点。
    #[test]
    fn a_report_of_overview_sections_only_is_refused() {
        let fake = Fake::new("report-all-overview")
            .with_transcript(&[(0.0, 2.0, "各位好,今天讲 Python 列表")])
            .with_curve(5)
            .with_shot(1, 2.0);
        let mut opened = fake.open();
        finish_all(&mut opened, &[1], &[1]);
        write_section(&mut opened, a_section(None, "总述", "整支讲一件事。"));

        let err = build(&mut opened, ReportFormat::Both).unwrap_err().to_string();

        assert!(err.contains("带锚点"), "要说清缺的是哪一类段:{err}");
        assert!(err.contains("write_summary"), "还要说出路:{err}");
        assert!(
            !fake.layout().report_json().is_file(),
            "拒绝了就不该落半成品"
        );
    }

    /// **乱码只报不拦**(2026-09-27 定的口径)。
    ///
    /// 为什么:材料的正确性是校准/总结那几步保证的,报告这一步的活是**编排**;
    /// 而乱码多半不是这一步的调用方写歪的 —— 上游(宿主解码 / 网关 / 模型自己)
    /// 把字节吃了一口,重写一遍常常换个地方再犯(实测三次重投:3/6/10 → 5/8/9 → 13),
    /// 拿它当拒收理由只会把 agent 逼进打地鼠,最后产出为零。
    ///
    /// 所以判据是:**照常落盘 + 把坏点报出来**(页面上会标)。
    #[test]
    fn damaged_text_is_marked_not_refused() {
        let fake = ready("report-damaged");
        let mut opened = fake.open();

        // 坏点分别落在:总述的正文、某一条的正文、这一版的 `notes`
        write_section(
            &mut opened,
            a_section(None, "总述", "这里有个 \u{FFFD} 方块。"),
        );
        write_section(
            &mut opened,
            a_section(
                Some(2.0),
                "切到解释器主窗口",
                "整理之后展开运行,无论后面是升还是\u{FFFD},都可以定义为 这一段。",
            ),
        );

        let outcome = build_report(
            &mut opened,
            &ReportInput {
                style: ReportStyle {
                    notes: Some("给刚入门的人看(这里有个 \u{FFFD} 方块)".into()),
                    ..a_style()
                },
            },
            ReportFormat::Both,
            ImageMode::Relative,
            Serving::Off,
        )
        .expect("乱码不该拦下整份报告");

        assert_eq!(outcome.topics, 1, "这一条要照常落盘");
        assert!(outcome.report_json.is_file(), "数据要真写下去");
        assert!(
            fake.state().progress.report.done,
            "步 4 该标完成(改一处乱码不等于没做完)"
        );

        let joined = outcome.warnings.join("\n");
        assert_eq!(outcome.warnings.len(), 3, "三处坏点各报一条:{joined}");
        // 报了坏点就要点得准 —— 包括编号(它从前恒为 0,报出来是「第 0 条」,指不准是谁)。
        for needle in ["全局总结", "style.notes", "第 1 条", "知识点", "正文"] {
            assert!(
                joined.contains(needle),
                "报出来的话要点到「{needle}」:{joined}"
            );
        }
        assert!(
            joined.contains("照常出了"),
            "要让调用方知道报告已经落了:{joined}"
        );
    }

    /// 干净的输入**一条提示都不该有** —— 否则"有提示"就不再是信号。
    #[test]
    fn clean_text_produces_no_warnings() {
        let fake = ready("report-clean");
        let mut opened = fake.open();

        let outcome = build(&mut opened, ReportFormat::Md).expect("正常输入该过");

        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert!(outcome.report_md.unwrap().is_file(), "只要 md 就该写它");
        assert!(outcome.report_html.is_none(), "没要页面就不该出");
    }

    /// 重做一遍:**按当前分段整份重出**,不是往后追加。重渲染则是"数据一个字节都不动"。
    #[test]
    fn building_again_reflects_the_current_sections_and_rendering_alone_keeps_them() {
        let fake = ready("report-redo");
        let mut opened = fake.open();

        build(&mut opened, ReportFormat::Both).unwrap();
        // 同名再落一段 = 改写那一段:报告该整份重出,不是多出一条
        write_section(
            &mut opened,
            a_section(Some(2.0), "切到解释器主窗口", "改过一遍的正文。"),
        );
        build(&mut opened, ReportFormat::Both).unwrap();

        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert_eq!(report.topics.len(), 1, "同名改稿不该多出一条");
        assert_eq!(
            report.topics[0].body, "改过一遍的正文。",
            "该是这一版的分段,不是上一版的"
        );

        // 只重渲染:数据不动,页面照写
        let before = std::fs::read_to_string(fake.layout().report_json()).unwrap();
        let outcome = render_existing(
            &opened,
            ReportFormat::Html,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
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
            &ReportInput { style: a_style() },
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
            &ReportInput { style: a_style() },
            ReportFormat::Html,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
        assert!(!plain.self_contained);
        let html = std::fs::read_to_string(plain.report_html.as_ref().unwrap()).unwrap();
        assert!(html.contains("s001"), "默认还是相对路径:{html:.300}");
    }

    /// **证据一律由工具按锚点时刻就近挂**(输入里已经没有段号那一栏了)——
    /// 取不到窗口里的段,就取起点最近的那一段;视频末尾附近既没说话也没画面时不瞎填。
    #[test]
    fn evidence_is_derived_from_the_anchor_time_and_never_invented() {
        let fake = ready("report-derived");
        let mut opened = fake.open();
        // 转录只覆盖 0–4 秒,整支 60 秒 —— 这一刻稿子里没人说话
        write_section(
            &mut opened,
            a_section(Some(45.0), "收尾之后", "这一节讲的是屏幕外的结论。"),
        );

        let outcome = build(&mut opened, ReportFormat::Html).expect("锚点落在稿子外面不该拦");

        assert_eq!(outcome.derived_segments, 2, "两条都是就近取的,要如实报数");
        let report = Report::load(&fake.layout().report_json()).unwrap();
        assert_eq!(report.topics[0].segments, vec![2], "t=2 落在 [2,22) 窗口里");
        assert_eq!(
            report.topics[1].segments,
            vec![2],
            "t=45 取起点最近的那一段(也不是瞎填)"
        );
        assert!(
            report.topics[1].shots.is_empty(),
            "那一刻没有截图,就不挂"
        );
    }

    /// 材料在报告之后被改过 —— 重渲染**照渲染,但把过期的锚点报出来**。
    ///
    /// 这条是技术债表里"报告数据的锚点是引用,材料变了不失效"的落点:
    /// 不拦(页面是发现这件事的窗口),但一句也不许漏(读者要拿它回去核)。
    #[test]
    fn stale_anchors_report_what_the_materials_no_longer_hold() {
        let fake = ready("report-stale");
        let mut opened = fake.open();
        build(&mut opened, ReportFormat::Html).unwrap();

        // 刚出完:每一处锚点都对得上
        let mut report = Report::load(&fake.layout().report_json()).unwrap();
        assert!(
            stale_anchors(&opened, &report).is_empty(),
            "刚校过一遍的报告不该有过期锚点"
        );

        // **旧数据**里的讲者原话照样要复核:`sources` 里那一类新报告不再写,
        // 但存量报告还带着它(读得动、也报得出)。
        report.topics[0].sources.push(TopicSource::Speaker {
            segment: 2,
            quote: "看这一段结构图".into(),
        });
        report.save(&fake.layout().report_json()).unwrap();

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
        // 报出来的每一条都点得出是**哪一条** —— 不然读者不知道去哪儿修。
        // (点名靠标题:`id` 那一栏现在恒为 0,见交付说明里的生产代码疑点。)
        assert!(
            stale.iter().all(|s| s.contains("切到解释器主窗口")),
            "每条都要点名是哪一条知识点:{stale:?}"
        );

        // 重渲染那条路:照渲染(不拦),但返回里带着它,页面数据里也带着它
        let outcome = render_existing(
            &opened,
            ReportFormat::Html,
            ImageMode::Relative,
            Serving::Off,
        )
        .unwrap();
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
