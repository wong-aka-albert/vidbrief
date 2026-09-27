//! D 组:结论回写(`record_shot_check` / `apply_corrections` / `write_note` /
//! `write_summary`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具不判断对错,只搬运与保存。**
//! 这几个是"agent 的判断落成文件"的唯一入口;所有写入都要先拿到项目写锁。
//!
//! 步 4(报告)不在这里:它落在 `tools/report.rs` —— 数据、页面、导出是一次动作
//! (见 devlog [260926-07] 的"数据为源")。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vidbrief_project::{Confidence, CorrectionType, RelPath};

use crate::json::{blocking, failed, json, text};
use crate::server::VidbriefServer;
use crate::tools::materials::parse_shot_id;
use crate::tools::attach_warning;

// ── 两个枚举的镜像 ───────────────────────────────────────────────────
//
// 工具参数要生成 JSON Schema 给客户端,而契约层的 `Confidence` / `CorrectionType`
// 没有 `JsonSchema` 实现(它们属于"文件长什么样",不属于"工具收什么")。
// 所以这里各镜像一份,由下面两条测试钉住**线格式不许漂移**。

/// 你对自己结论的把握程度。**不确定就用 `low`,不要猜。**
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(rename_all = "snake_case")]
pub enum ConfidenceArg {
    High,
    Medium,
    Low,
}

impl From<ConfidenceArg> for Confidence {
    fn from(value: ConfidenceArg) -> Self {
        match value {
            ConfidenceArg::High => Confidence::High,
            ConfidenceArg::Medium => Confidence::Medium,
            ConfidenceArg::Low => Confidence::Low,
        }
    }
}

/// 错的性质 —— **只在 `outcome = corrected` 时有**。
///
/// **没有"存疑"这一档**:存疑是一种结论(定不了),不是一种错误的性质。
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(rename_all = "snake_case")]
pub enum CorrectionTypeArg {
    /// 同音错字(实测:「达摩院」→「打墨院」)。
    Homophone,
    /// 术语 / 专有名词(实测:「Python」→「派森」)。中文 ASR 最高频的错误。
    Terminology,
    /// 数字 / 单位 / 时间。
    Number,
    Other,
}

impl From<CorrectionTypeArg> for CorrectionType {
    fn from(value: CorrectionTypeArg) -> Self {
        match value {
            CorrectionTypeArg::Homophone => CorrectionType::Homophone,
            CorrectionTypeArg::Terminology => CorrectionType::Terminology,
            CorrectionTypeArg::Number => CorrectionType::Number,
            CorrectionTypeArg::Other => CorrectionType::Other,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordShotCheckParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 截图 id,形如 `s007`(也接受纯数字 `7`)。
    pub shot_id: String,
    /// 画面讲的与同期转录讲的**是不是同一件事**(主题 / 动作 / 对象)。
    ///
    /// **用字层面的差异不算不符** —— 转录把一个字听错了,那是转录纠错的活
    /// (`apply_corrections`),不是"图文不符"。判据说白了:**这段话配这张图,
    /// 读的人会不会觉得对不上**。(写死这一条的理由:两种读法都说得通的字段,
    /// 迟早被不同 agent 填成两个样 —— 实测两个模型给过相反答案。)
    pub r#match: bool,
    /// 画面与语音在时间上是否对得上(画面切了但话还在讲上一张,就是 false)。
    pub audio_visual_sync: bool,
    /// 从画面**读到的**内容(对象 / 规格 / 状态 / 数值)。
    /// **画面无法确认时如实写「画面无法确认」,不要顺着转录编。**
    pub type_read: String,
    /// 发现的问题,每条一句。没问题就给空数组。
    #[serde(default)]
    pub issues: Vec<String>,
    /// 你对自己这个结论的把握。**不确定就用 low**,不要猜。
    pub confidence: ConfidenceArg,
    /// 该时刻前后 ±6s 的转录原文。不给则由工具自动填。
    #[serde(default)]
    pub neighbor_text: Option<String>,
    /// **照抄**的那句转录原文 —— 你这条结论凭的就是它。
    ///
    /// 从 `get_transcript` 里原样搬一句过来。工具拿它去转录里搜:搜得到就把它连段号
    /// 一起记下,搜不到就拒收。
    ///
    /// **一句话可以横跨几个碎段**(whisper 按停顿切,一句常常切成两三段),
    /// 但不许跨**组** —— 跨组就是两句不同的话了。搬的时候照抄**整句**最稳。
    ///
    /// 为什么必须给:没有它,这条结论就只是一句话 —— 事后除了再问你一遍,没有别的办法
    /// 核实。**照抄的画面文字同理**,只是画面上写了什么工具核不了(它没有眼睛)。
    #[serde(default)]
    pub evidence_quote: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(deny_unknown_fields)]
pub struct CorrectionItem {
    /// 转录段 id(`transcript.json` 里的序号,从 1 起)。
    pub segment_id: u32,
    /// 这一段是什么结论 —— **三种各自要带什么,见下面的字段说明**:
    ///
    /// | `outcome` | 必给 | 说明 |
    /// |-----------|------|------|
    /// | `corrected` | `text`(改后的**整句**) | 改了正文 |
    /// | `kept` | `note`(为什么决定不改) | 看过,决定保留原句 |
    /// | `uncertain` | `note`(疑似是什么写法) | 定不了,留给用户裁决 |
    ///
    /// 三者都要给 `reason`。**同一段只能有一条结论**:一段里有几处改动,
    /// 合并进同一个 `text`(报两条的话,后一条会盖掉前一条)。
    pub outcome: CorrectionOutcomeArg,
    /// **`outcome = corrected` 时必给**:改后的**整句** ——
    /// 不是只写改动的那个词(那是 `note` / `reason` 里的东西)。
    /// **它必须和原文不一样** —— 想保留原句就用 `kept`。
    #[serde(default)]
    pub text: Option<String>,
    /// `outcome = corrected` 时给:错的性质(`homophone` / `terminology` / `number` / `other`)。
    #[serde(default, rename = "type")]
    pub kind: Option<CorrectionTypeArg>,
    /// 改动内容,如 `派森 → Python`。
    /// **`kept` / `uncertain` 时必给**(前者说为什么不改,后者说疑似是什么写法)——
    /// 否则这条记录没有信息量。`corrected` 不给就按前后缀差异自动掐出来。
    #[serde(default)]
    pub note: Option<String>,
    /// 判断依据(画面文字 / 上下文 / 术语表)。**必须写**,这是事后复核的唯一线索。
    pub reason: String,
}

/// 一条结论是哪一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(rename_all = "snake_case")]
pub enum CorrectionOutcomeArg {
    /// 改了正文。
    Corrected,
    /// 看过,**决定保留原句**。
    Kept,
    /// 看过,但定不了 —— 留给用户裁决。
    Uncertain,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyCorrectionsParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 本次要写的纠错条目。可以分批多次调用 —— 每次都在已有纠错稿上继续改。
    pub corrections: Vec<CorrectionItem>,
    /// **审过、没问题**的段 id —— 比如刚读完 1–50 段,里面只有 2 段要改,
    /// 那 `corrections` 给那 2 段,这里给剩下 48 段。
    ///
    /// 为什么要有它:校准里绝大多数段是"审了、没问题",而 `corrections` 只装
    /// **确实有话说**的段。没有这个列表,"审过没问题"和"压根没审"就分不开 ——
    /// 而审核门要拦的正是"跳过了某一段"。改过的段自动算审过,不用在这里重复列。
    ///
    /// **一整片都没问题就别逐段点名** —— 用 `reviewed_ranges` 报区间。
    #[serde(default)]
    pub reviewed: Vec<u32>,
    /// **审过、没问题**的**区间**(闭区间,两端都算,按段 id)。
    ///
    /// whisper 按停顿切段,一段一两秒是常事 —— 70 分钟的视频能切出两千多段。
    /// 逐段报 id 光吐数字就要两千多个,而它对判断没有任何贡献。
    /// 读完一片、整片没问题,报 `[{"from": 5, "to": 300}]` 就够,和 `reviewed` 取并集。
    #[serde(default)]
    pub reviewed_ranges: Vec<ReviewedRangeArg>,
}

/// 「审过、没问题」的一个区间(闭区间:两端都算)。
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(inline)] // 内联进 schema,不落 `$defs`(见 `tool_schemas_are_self_contained`)
#[serde(deny_unknown_fields)]
pub struct ReviewedRangeArg {
    /// 起点(含),段 id。
    pub from: u32,
    /// 终点(含),段 id。
    pub to: u32,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteNoteParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 项目根相对路径,只允许 `02_calibrate/` 或 `03_summary/` 下的 `.md`,
    /// 如 `02_calibrate/cross_validate.md`、`03_summary/notes/案例一.md`。
    pub path: String,
    /// Markdown 正文。
    pub md: String,
    /// `overwrite` 整篇重写(默认)/ `append` 往后面接一段。
    /// **日志类文件用 append**,否则第二轮会把第一轮抹掉。
    #[serde(default)]
    pub mode: Option<String>,
    /// 这一笔记作一轮校准(轮数 +1 并记时间)。默认 false —— 写个笔记不等于校准了一轮。
    #[serde(default)]
    pub count_round: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteDocParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// Markdown 正文。
    pub md: String,
}

/// `write_summary` 的入参:**一段**总结(不是整篇)。
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SummarySectionParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 这一节讲的是视频里**哪一刻**(秒)。
    ///
    /// **给了就上报告页的时间轴**(那一刻一个圆点,点开是这条的解析);
    /// **不给 = 总述**(对整支视频的总起/收束),只进右侧面板,不上时间轴。
    #[serde(default)]
    pub t: Option<f64>,
    /// 这一段叫什么。**它就是报告页上那条的标题**,也是改稿时认段的凭据
    /// (同名 = 改写那一段)。
    pub title: String,
    /// 正文(markdown)。
    pub md: String,
    /// **待核验**:读的人该回头核一下的点(画面看不清但结论靠它的数字……)。
    /// 一条一句,写清怎么核;没有就空着。
    #[serde(default)]
    pub verify: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApproveParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// **带着未处理项通过**时的说明 —— 最好是用户的原话。
    /// 正常通过(该处理的都处理完了)就别给。
    #[serde(default)]
    pub note: Option<String>,
}

#[tool_router(router = conclusion_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "把**你对一张截图的判断**写回 shots.json(全量替换,复核后重写一次即以最后一次为准)。\
                       record_shot_check 只保存,不评价 —— 判断对不对由用户在审查节点看。",
        annotations(title = "存截图结论", read_only_hint = false, idempotent_hint = true)
    )]
    async fn record_shot_check(
        &self,
        Parameters(p): Parameters<RecordShotCheckParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let id = parse_shot_id(&p.shot_id)?;
        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let input = vidbrief_ops::ShotCheckInput {
            r#match: p.r#match,
            audio_visual_sync: p.audio_visual_sync,
            type_read: p.type_read,
            issues: p.issues,
            confidence: p.confidence.into(),
            neighbor_text: p.neighbor_text,
            evidence_quote: p.evidence_quote,
        };
        let check = blocking(move || vidbrief_ops::record_shot_check(&mut opened, id, &input)).await?;
        Ok(attach_warning(json(&check)?, slot.warning()))
    }

    #[tool(
        description = "把转录纠错写回 `02_calibrate/transcript.fixed.json`(原始转录永不就地改写),\
                       并刷新人可核对的 `transcript.diff.md`。可以分批多次调用。\
                       纠错依据的优先级:**关键帧画面上的文字 > 上下文语义 > 术语表**。\
                       只改明确的识别错误,不润色、不改语序;数字/单位/时间除非明显错了否则保留;\
                       没把握的别改,标 `uncertain` 并在 note 里写疑似写法。\
                       \n\n\
                       **三种 `outcome` 各自要带什么(一次说清,别一条一条试)**:\
                       `corrected` 必给 `text`(**改后的整句**,不是只写改动的那个词)、`type` 选填;\
                       `kept`(看过决定不改)必给 `note`(为什么);\
                       `uncertain`(定不了)必给 `note`(疑似是什么写法);\
                       三者都要给 `reason`(依据)。**同一段只能有一条结论** ——\
                       一段里几处改动合并进同一个 `text`;\
                       `text` 跟原文一字不差会被拒(那不叫改,要保留原句就换 `kept`)。\
                       一批里所有问题**一次全报**给你,改完再交一次即可。\
                       \n\n\
                       **审过、没问题的段用 `reviewed` 报上来**(如 `[1,2,3,…]`);\
                       一整片没问题就用 `reviewed_ranges` 报闭区间(如一条 from 5 to 300),两者取并集;\
                       否则那些段在系统里跟'压根没审'分不开 —— 而审核门拦的就是跳段。\
                       改过的段自动算审过,不用重复列。",
        annotations(title = "写回转录纠错", read_only_hint = false, idempotent_hint = true)
    )]
    async fn apply_corrections(
        &self,
        Parameters(p): Parameters<ApplyCorrectionsParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let mut items = Vec::with_capacity(p.corrections.len());
        for c in p.corrections {
            // 三种结论在这里拼成落盘用的那一个枚举 —— 缺件当场退回去,
            // 而不是让它带着"说要改、文本空着"往下走
            let conclusion = match c.outcome {
                CorrectionOutcomeArg::Corrected => {
                    let Some(text) = c.text.filter(|t| !t.trim().is_empty()) else {
                        return Err(failed(format!(
                            "第 {} 段 `outcome = corrected` 却没给 `text` —— 要保留原句就用 `kept`",
                            c.segment_id
                        )));
                    };
                    vidbrief_project::Conclusion::Corrected {
                        text,
                        kind: c.kind.unwrap_or(CorrectionTypeArg::Other).into(),
                        note: c.note.unwrap_or_default(),
                        reason: c.reason,
                    }
                }
                CorrectionOutcomeArg::Kept => vidbrief_project::Conclusion::Kept {
                    note: c.note.unwrap_or_default(),
                    reason: c.reason,
                },
                CorrectionOutcomeArg::Uncertain => vidbrief_project::Conclusion::Uncertain {
                    note: c.note.unwrap_or_default(),
                    reason: c.reason,
                },
            };
            items.push(vidbrief_ops::CorrectionInput {
                segment_id: c.segment_id,
                conclusion,
            });
        }
        let reviewed = p.reviewed;
        let ranges: Vec<vidbrief_ops::ReviewedRange> = p
            .reviewed_ranges
            .iter()
            .map(|r| vidbrief_ops::ReviewedRange {
                from: r.from,
                to: r.to,
            })
            .collect();
        let outcome = blocking(move || {
            vidbrief_ops::apply_corrections(&mut opened, &items, &reviewed, &ranges)
        })
        .await?;
        Ok(attach_warning(json(&outcome)?, slot.warning()))
    }

    #[tool(
        description = "往项目里写一份 Markdown:交叉印证结论(`02_calibrate/cross_validate.md`)、\
                       校准日志(`02_calibrate/calibration_log.md`)、总结笔记(`03_summary/notes/…`)、\
                       agent 探索轨迹(`03_summary/agent_log.md`)。\
                       只允许写 `02_calibrate/` 与 `03_summary/` 下的 `.md`,而且**有专属工具的文件\
                       这里写不了**:`summary.md` 归 `write_summary`、`transcript.diff.md` 归 \
                       `apply_corrections` —— 它们不只是写文件,还带着审核门、进度和证据刷新,\
                       绕过去那些就一起失效了。",
        annotations(title = "写笔记", read_only_hint = false, idempotent_hint = true)
    )]
    async fn write_note(
        &self,
        Parameters(p): Parameters<WriteNoteParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let rel = RelPath::new(&p.path)
            .map_err(|e| failed(format!("路径不合法:{e:#}\n只允许项目根相对路径,如 `03_summary/notes/案例一.md`")))?;
        let mode = match p.mode.as_deref() {
            None | Some("overwrite") => vidbrief_ops::WriteMode::Overwrite,
            Some("append") => vidbrief_ops::WriteMode::Append,
            Some(other) => {
                return Err(failed(format!(
                    "mode 只能是 `overwrite` 或 `append`,收到 `{other}`"
                )));
            }
        };
        let count_round = p.count_round.unwrap_or(false);

        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let md = p.md;
        let path = blocking(move || {
            vidbrief_ops::write_note(&mut opened, &rel, &md, mode, count_round)
        })
        .await?;
        Ok(attach_warning(
            text(format!("已写入 {}", path.display())),
            slot.warning(),
        ))
    }

    #[tool(
        description = "交**一段**总结(一小节)—— 写完一段落一段,别攒着一次交整篇。\
                       带 `t` 的段会上报告页的**时间轴**(一条一个圆点);不带 `t` 的作**总述**。\
                       \n\n\
                       **按段交是为了报告那一步**:报告页直接由这些段组装,\
                       渲染时**不用再给任何内容数据** —— 你写下的就是读者看到的。\
                       改哪段就再交一次同名的那段(同名 = 改写,顺序不动)。\
                       \n\n\
                       **写完要停下来把内容呈现给用户**,等他说改哪儿。\
                       \n\n\
                       **这道门前置着步 2 的确认**:校准没被用户确认过、或者确认之后\
                       材料又变过,这里会被挡住 —— 那时别绕,把状态给用户看,他点头后\
                       调 `approve`。\
                       \n\n\
                       落下的每一段都会被记进**内容底账**:出报告时工具拿它对一遍,\
                       确认「用户看到的那版总结,就是出报告用的那版」。",
        annotations(title = "写一段总结", read_only_hint = false, idempotent_hint = true)
    )]
    async fn write_summary(
        &self,
        Parameters(p): Parameters<SummarySectionParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let input = vidbrief_ops::SectionInput {
            t: p.t,
            title: p.title,
            md: p.md,
            verify: p.verify,
        };
        let outcome = blocking(move || vidbrief_ops::write_summary(&mut opened, &input)).await?;
        let action = if outcome.added { "新增一段" } else { "改写了一段" };
        Ok(attach_warning(
            text(format!(
                "{action}:`{}`\n现在共 {} 段(其中 {} 段上时间轴,{} 段是总述)。\n\
                 文章落在 {}。\n\n\
                 接着写下一段;全部写完、用户也满意了,再用 `build_report` 出页面 ——\
                 报告直接由这些段组装,不用再交内容(动手前取一次 `get_guide(step=\"build_report\")`)。",
                outcome.sections_path.display(),
                outcome.sections,
                outcome.anchored,
                outcome.sections - outcome.anchored,
                outcome.article_path.display(),
            )),
            slot.warning(),
        ))
    }

    #[tool(
        description = "记下「**用户拍板了悬着的那一批**」—— 只对'等人拍板'那一类有效。\
                       `note` 里放**他的原话**(转述也行,别美化)。\
                       \n\n\
                       什么算拍板:他说'这些我知道,就这样过'算;\
                       **他说'先留着 / 我也不确定 / 别管了'也算** —— 那同样是他在这一步做的决定,\
                       记下来门就开,不必再问一遍。\
                       \n\n\
                       门本身不看签字:它是按**核验状态**判的(还有没核验的就拦着让你接着审;\
                       有等用户拍板的就拦着让你去问)。所以这个工具是给**后一类**用的。\
                       \n\n\
                       **不要替用户点头。** 他没说过什么,就该去问他;\
                       他给了具体决定(比如某张图确实有问题要改),那是把决定落到条目上\
                       (`record_shot_check` / `apply_corrections`),不是调这个。",
        annotations(title = "记用户拍板", read_only_hint = false, idempotent_hint = true)
    )]
    async fn approve(
        &self,
        Parameters(p): Parameters<ApproveParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, slot) = self.open_write(&p.project_id).await?;
        let note = p.note;
        let (approval, still) = blocking(move || {
            let approval = vidbrief_ops::approve(&mut opened, note)?;
            // 拍完板顺手看看门现在什么状态 —— 说"门开了"而不查,就会说错
            let still = vidbrief_ops::gate_calibrate(&opened).err().map(|e| e.to_string());
            Ok::<_, anyhow::Error>((approval, still))
        })
        .await?;

        let c = approval.counts;
        let tail = match still {
            None => "门开了(核验状态上没有拦着的了),可以写总结。".to_string(),
            Some(why) => format!(
                "**门还没开** —— 拍板只顶掉'等人拍板'那一类,核验状态还是拦着的:\n{why}"
            ),
        };
        Ok(attach_warning(
            text(format!(
                "已记下用户拍板({})。\n\
                 当时状态:未校准 {} 张 / 待裁决 {} 张 / 未处理补图候选 {} 条 / 存疑转录 {} 段{}\n\
                 \n\
                 {tail}",
                approval.approved_at.format("%Y-%m-%d %H:%M:%S"),
                c.unchecked,
                c.needing_review,
                c.supplement_pending,
                c.uncertain,
                approval
                    .note
                    .as_deref()
                    .map(|n| format!("\n理由:{n}"))
                    .unwrap_or_default()
            )),
            slot.warning(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 镜像枚举的**线格式**必须与契约层逐字相同。
    ///
    /// 这两处一旦漂移,agent 写 `"low"` 而文件里存的是别的东西 —— 而且不会报错,
    /// 只会静默地把结论存成错的值。所以拿序列化结果直接对拍。
    #[test]
    fn mirrored_enums_speak_the_same_wire_format() {
        for (mine, theirs) in [
            (ConfidenceArg::High, Confidence::High),
            (ConfidenceArg::Medium, Confidence::Medium),
            (ConfidenceArg::Low, Confidence::Low),
        ] {
            assert_eq!(
                serde_json::to_string(&mine).unwrap(),
                serde_json::to_string(&theirs).unwrap()
            );
            assert_eq!(
                serde_json::to_string(&Confidence::from(mine)).unwrap(),
                serde_json::to_string(&theirs).unwrap()
            );
        }

        for (mine, theirs) in [
            (CorrectionTypeArg::Homophone, CorrectionType::Homophone),
            (CorrectionTypeArg::Terminology, CorrectionType::Terminology),
            (CorrectionTypeArg::Number, CorrectionType::Number),
            (CorrectionTypeArg::Other, CorrectionType::Other),
        ] {
            assert_eq!(
                serde_json::to_string(&mine).unwrap(),
                serde_json::to_string(&theirs).unwrap()
            );
        }
    }

    /// 参数结构体能生成出 schema,且枚举值是**闭集** —— agent 才不会瞎编一个类型名。
    #[test]
    fn param_schemas_are_generated_and_close_the_enums() {
        let schema = schemars::schema_for!(CorrectionItem);
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("terminology"), "{text}");

        let schema = schemars::schema_for!(RecordShotCheckParams);
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("type_read"), "{text}");
        assert!(text.contains("confidence"), "{text}");
    }
}
