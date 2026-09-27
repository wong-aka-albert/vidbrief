//! 步 4:报告(`build_report`)。
//!
//! 设计见 `docs/DESIGN.md` §8 与 devlog [260926-07]:**数据为源** ——
//! agent 交一整份报告数据,`report.html`(页面)与 `report.md`(导出)都从它渲染。
//!
//! # 这一组为什么只有一个工具
//!
//! 步 4 是**原子产出**:一次调用把数据落了、把页面出了。分成"先写数据再渲染"
//! 两个工具,中间那一半状态对谁都没用 —— 用户要的是那个能点开看的页面。
//! 重做就是再调一次(整份替换);只想重渲染的情况由 `ui --view report` 那条命令行兜。
//!
//! # 它走写锁,与渲染工具不同
//!
//! 它落的是**材料**(`report.json`)并且动进度,所以按写操作对待(取锁 + 排队)。
//! 页面本身是派生视图 —— 但既然数据也要写,就一起待在锁里,免得出现
//! "数据落了一半、页面渲染的是别的版本"。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::json::{blocking, failed, json};
use crate::server::VidbriefServer;

/// **这份报告按什么口味做** —— 用户的提示词里带着他要什么,这一栏就是那个落点。
///
/// **必填**:一份"没说按什么做的报告"交到用户手上,他只能说"这不是我要的",
/// 却说不清该改哪儿。工具在这一栏上不做判断(哪个粒度对他更好,不关工具的事),
/// 只保证填了、不是空的。
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(deny_unknown_fields)]
pub struct StyleParam {
    /// 风格轮廓(**先看 `get_guide(step="styles")`**):`quick` 快扫版 / `teaching` 教学版 /
    /// `story` 故事版 / `review` 元组版 / `exec` 汇报版 / `custom` 自定义。
    /// **它决定"怎么填"** —— 条数、每条骨架、详略、证据密度都跟着它走;
    /// 换风格就是照另一种轮廓重写一遍,不是把语气词换一换。
    pub preset: PresetParam,
    /// 知识粒度:**它同时决定条数与篇幅的档位**。
    /// `brief` = 概括(只留骨架,每条一两句)/ `standard` = 标准(逐个讲的逐个成条)/
    /// `detailed` = 详细(在标准之上把背景、数字、边界条件都写进去)。
    pub granularity: GranularityParam,
    /// 语言风格(**自由文本**):大白话 / 诙谐 / 讲故事 / 行业腔……用户怎么说就怎么写。
    /// 没听出偏好就按平实的讲人话写,并在交付时说清这一版是按什么做的。
    pub tone: String,
    /// 用户还有别的交代写这儿:给谁看、要不要举例子、要不要结论先行。没交代就别给。
    #[serde(default)]
    pub notes: Option<String>,
}

/// 风格轮廓(线上写法与 `report.json` 里的 `ReportPreset` 一致)。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(rename_all = "snake_case")]
pub enum PresetParam {
    /// 快扫版:看过视频的人扫一眼就想起来(结论先行、少图)。
    Quick,
    /// 教学版:没基础的人要真学会(每条讲透、术语首次出现即解释)。
    Teaching,
    /// 故事版:顺着视频时间线一趟讲下来(连着写、少列表)。
    Story,
    /// 元组版:拿它回看自己的操作(判断 → 证据 → 结果 → 怎么用)。
    Review,
    /// 汇报版:给没看过视频的人交代(结论 → 影响 → 动作)。
    Exec,
    /// 自定义:用户的要求拼不出上面任何一种(要求写进 `notes`)。
    Custom,
}

impl From<PresetParam> for vidbrief_project::ReportPreset {
    fn from(v: PresetParam) -> Self {
        match v {
            PresetParam::Quick => Self::Quick,
            PresetParam::Teaching => Self::Teaching,
            PresetParam::Story => Self::Story,
            PresetParam::Review => Self::Review,
            PresetParam::Exec => Self::Exec,
            PresetParam::Custom => Self::Custom,
        }
    }
}

/// 知识粒度(线上写法与 `report.json` 里的 `Granularity` 一致)。
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(rename_all = "snake_case")]
pub enum GranularityParam {
    /// 概括:只留骨架。
    Brief,
    /// 标准:视频里逐个讲的逐个成条。
    Standard,
    /// 详细:背景、数字、边界条件都写进去。
    Detailed,
}

impl From<GranularityParam> for vidbrief_project::Granularity {
    fn from(v: GranularityParam) -> Self {
        match v {
            GranularityParam::Brief => Self::Brief,
            GranularityParam::Standard => Self::Standard,
            GranularityParam::Detailed => Self::Detailed,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BuildReportParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// **这份报告按什么口味做**(粒度 + 风格):从**用户的话**里读出来填这儿。
    pub style: StyleParam,
    /// 出哪几份:`both`(默认,页面 + Markdown 导出)/ `html` / `md`。
    #[serde(default)]
    pub format: Option<String>,
    /// 要不要顺手打开默认浏览器。默认 true(用户就在这台机器上看)。
    /// 无头机器上传 false,只拿 URL。
    #[serde(default)]
    pub open_browser: Option<bool>,
    /// **单文件分享**:图内嵌成 `data:` URL,产物能单独发出去(对方不需要项目目录)。
    ///
    /// 默认 false(相对路径,体积小)。**视频不内嵌** —— 单文件版的图能带走,
    /// 点锚点回跳那一半仍要本机素材 + 本地服务。页面里也这么写。
    #[serde(default)]
    pub self_contained: Option<bool>,
}

/// 给 agent 看的落盘结果。
#[derive(Debug, Serialize)]
struct ReportBrief {
    report_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_md: Option<String>,
    /// 落了几条知识点。
    topics: usize,
    /// 这份报告是按什么口味做的(「详细 · 大白话」)—— **交付时照它跟用户交代**。
    #[serde(skip_serializing_if = "Option::is_none")]
    style: Option<String>,
    /// 全局总结是哪来的:`input`(调用方给的)/ `summary.md`(步 3 那一份)。
    summary_from: &'static str,
    /// 本地查看服务的地址。**给用户点开的就是这个**,不是 report_html。
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    /// **这页要留意的地方**(自由正文里的乱码字符),空数组不写。
    ///
    /// 它**不是错误**:报告照常落盘了,页面也照渲染 —— 见 `vidbrief_ops::report`
    /// 里 `damaged_spots` 的解释(乱码多半来自上游,重写常常换地方再犯)。
    /// 交给用户看的时候要**如实说**这几处,别当没看见。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    hint: String,
}

#[tool_router(router = report_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "出**报告**(步 4 的产出):交一整份报告数据 —— 全局总结 + 局部知识点数组,\
                       每条知识点挂在**一个时刻**上(**视频里逐个讲的案例/步骤,每个各占一条**,\
                       别合并成一条) —— 工具落 `04_report/report.json`,\
                       再渲染出 `report.html`(时间轴上的知识点锚点 + 浮动解析 + 右侧图文总结面板\
                       + 可回跳的视频)与 `report.md` 导出。**整份替换**:重做就是再调一次。\
                       \n\n\
                       **内容从步 3 的段来** —— 这个工具**不用你交任何内容**:\
                       每一条(标题、正文、锚点)就是 `write_summary` 交过的那些段,\
                       它们那一刻的转录与画面由工具就近挂上。你只交代**口味**。\
                       \n\n\
                       **先交代口味**(`style`,必填):用户要什么就填什么 ——\
                       `granularity`(条数已由步 3 的段定下,这里只是这一版的粒度说明)、\
                       `tone`(语言风格,自由文本:大白话 / 诙谐 / 讲故事 / 行业腔……)、\
                       `notes`(还有别的交代:给谁看、要不要例子)。\
                       用户没说出口味就按标准 + 平实的讲人话,并在交付时**说清这一版是按什么做的**。\
                       \n\n\
                       要改内容就**回步 3 改段**(再交一次同名的那段),然后重出;`topics` 这类参数不存在。\n\n\
                       **这道门前置着步 3**:没写过总结(`write_summary`)、或者总结被绕过工具改过,\
                       会被挡住并告诉你怎么办。\n\n\
                       动手前取一次操作说明:`get_guide(step=\"build_report\")`。",
        annotations(title = "出报告", read_only_hint = false, idempotent_hint = true)
    )]
    async fn build_report(
        &self,
        Parameters(p): Parameters<BuildReportParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let format = match p.format.as_deref() {
            None => vidbrief_ops::ReportFormat::Both,
            Some(raw) => {
                vidbrief_ops::ReportFormat::parse(raw).map_err(|e| failed(format!("{e:#}")))?
            }
        };
        let open_browser = p.open_browser.unwrap_or(true);
        let images = if p.self_contained.unwrap_or(false) {
            vidbrief_report::ImageMode::Embedded
        } else {
            vidbrief_report::ImageMode::Relative
        };

        let input = vidbrief_ops::ReportInput {
            style: vidbrief_project::ReportStyle {
                preset: Some(p.style.preset.into()),
                granularity: p.style.granularity.into(),
                tone: p.style.tone,
                notes: p.style.notes,
            },
        };

        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let serving = vidbrief_ops::Serving::On { open_browser };
        let outcome = blocking(move || {
            vidbrief_ops::build_report(&mut opened, &input, format, images, serving)
        })
        .await?;

        let hint = match (&outcome.url, &outcome.report_md) {
            (Some(url), _) => format!(
                "把 {url} 给用户看:左边是视频与时间轴(轴上的圆点是知识点,播放头走到哪就显示哪一条),\
                 右边是图文总结面板,点右上角按钮滑出。点圆点转到视频对应位置。\
                 \n\
                 顺带说清这一版是按什么做的(见 style)—— 用户要换个说法就带着新口味再调一次。"
            ),
            (None, Some(md)) => format!("已导出 Markdown:{}", md.display()),
            (None, None) => "报告数据已落盘。".to_string(),
        };

        // 有坏点就把话说在前面:报告出了,但哪几处不该当真得让用户知道
        // (它们不是错误、不拦落盘 —— 见 `ReportBrief::warnings`)。
        let hint = if outcome.warnings.is_empty() {
            hint
        } else {
            format!(
                "报告出了,但有 {} 处**乱码字符**(页面上是方块)—— 多半是上游把字节吃了一口,\
                 不是内容问题。交付时**如实说**这几处、别当没看见;用户要干净版就照原样重发一次。\n{hint}",
                outcome.warnings.len()
            )
        };

        let brief = ReportBrief {
            report_json: outcome.report_json.display().to_string(),
            report_html: outcome.report_html.map(|p| p.display().to_string()),
            report_md: outcome.report_md.map(|p| p.display().to_string()),
            topics: outcome.topics,
            style: outcome.style,
            summary_from: outcome.summary_from,
            url: outcome.url,
            warnings: outcome.warnings,
            hint,
        };
        Ok(crate::tools::attach_warning(json(&brief)?, slot.warning()))
    }
}
