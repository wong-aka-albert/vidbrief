//! D 组:结论回写(`record_shot_check` / `apply_corrections` / `write_note` /
//! `write_summary` / `write_report_md`)。
//!
//! 设计见 `docs/DESIGN.md` §6.1 —— **工具不判断对错,只搬运与保存。**
//! 这几个是"agent 的判断落成文件"的唯一入口;所有写入都要先拿到项目写锁。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vidbrief_project::{Confidence, CorrectionType, RelPath};

use crate::json::{blocking, failed, json, text};
use crate::server::VidbriefServer;
use crate::tools::materials::parse_shot_id;

// ── 两个枚举的镜像 ───────────────────────────────────────────────────
//
// 工具参数要生成 JSON Schema 给客户端,而契约层的 `Confidence` / `CorrectionType`
// 没有 `JsonSchema` 实现(它们属于"文件长什么样",不属于"工具收什么")。
// 所以这里各镜像一份,由下面两条测试钉住**线格式不许漂移**。

/// 你对自己结论的把握程度。**不确定就用 `low`,不要猜。**
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
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

/// 纠错类型。
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionTypeArg {
    /// 同音错字(实测:「达摩院」→「打墨院」)。
    Homophone,
    /// 术语 / 专有名词(实测:「Python」→「派森」)。中文 ASR 最高频的错误。
    Terminology,
    /// 数字 / 单位 / 时间。
    Number,
    /// **存疑但没改**:怀疑这里有问题却不敢断言。此时 `corrected_text` 必须不给,
    /// 疑似写法写在 `note` 里,留给用户在审查节点裁决。
    Uncertain,
    Other,
}

impl From<CorrectionTypeArg> for CorrectionType {
    fn from(value: CorrectionTypeArg) -> Self {
        match value {
            CorrectionTypeArg::Homophone => CorrectionType::Homophone,
            CorrectionTypeArg::Terminology => CorrectionType::Terminology,
            CorrectionTypeArg::Number => CorrectionType::Number,
            CorrectionTypeArg::Uncertain => CorrectionType::Uncertain,
            CorrectionTypeArg::Other => CorrectionType::Other,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecordShotCheckParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 截图 id,形如 `s007`(也接受纯数字 `7`)。
    pub shot_id: String,
    /// 画面内容与同期转录是否相符。
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
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CorrectionItem {
    /// 转录段 id(`transcript.json` 里的序号,从 1 起)。
    pub segment_id: u32,
    /// 改后的整句。**不改就别给**(标存疑时才这样)。
    #[serde(default)]
    pub corrected_text: Option<String>,
    /// `homophone` 同音字 / `terminology` 术语 / `number` 数字 / `uncertain` 存疑未改 / `other`。
    #[serde(rename = "type")]
    pub kind: CorrectionTypeArg,
    /// 改动内容,如 `派森 → Python`。不给就按前后缀差异自动掐出来;
    /// `corrected_text` 不给时**必须**给这个,否则这条记录没有信息量。
    #[serde(default)]
    pub note: Option<String>,
    /// 判断依据(画面文字 / 上下文 / 术语表)。**必须写**,这是事后复核的唯一线索。
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyCorrectionsParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 本次要写的纠错条目。可以分批多次调用 —— 每次都在已有纠错稿上继续改。
    pub corrections: Vec<CorrectionItem>,
}

#[derive(Debug, Deserialize, JsonSchema)]
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
pub struct WriteDocParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// Markdown 正文。
    pub md: String,
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
        let (opened, _slot) = self.open_write(&p.project_id).await?;
        let input = vidbrief_ops::ShotCheckInput {
            r#match: p.r#match,
            audio_visual_sync: p.audio_visual_sync,
            type_read: p.type_read,
            issues: p.issues,
            confidence: p.confidence.into(),
            neighbor_text: p.neighbor_text,
        };
        let check = blocking(move || vidbrief_ops::record_shot_check(&opened, id, &input)).await?;
        json(&check)
    }

    #[tool(
        description = "把转录纠错写回 `02_calibrate/transcript.fixed.json`(原始转录永不就地改写),\
                       并刷新人可核对的 `transcript.diff.md`。可以分批多次调用。\
                       纠错依据的优先级:**关键帧画面上的文字 > 上下文语义 > 术语表**。\
                       只改明确的识别错误,不润色、不改语序;数字/单位/时间除非明显错了否则保留;\
                       没把握的别改,标 `uncertain` 并在 note 里写疑似写法。",
        annotations(title = "写回转录纠错", read_only_hint = false, idempotent_hint = true)
    )]
    async fn apply_corrections(
        &self,
        Parameters(p): Parameters<ApplyCorrectionsParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (opened, _slot) = self.open_write(&p.project_id).await?;
        let items: Vec<vidbrief_ops::CorrectionInput> = p
            .corrections
            .into_iter()
            .map(|c| vidbrief_ops::CorrectionInput {
                segment_id: c.segment_id,
                corrected_text: c.corrected_text,
                kind: c.kind.into(),
                note: c.note,
                reason: c.reason,
            })
            .collect();
        let outcome = blocking(move || vidbrief_ops::apply_corrections(&opened, &items)).await?;
        json(&outcome)
    }

    #[tool(
        description = "往项目里写一份 Markdown:交叉印证结论(`02_calibrate/cross_validate.md`)、\
                       校准日志(`02_calibrate/calibration_log.md`)、总结笔记(`03_summary/notes/…`)。\
                       只允许写这两个目录下的 `.md` —— 材料文件有自己的工具,别绕过它们。",
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

        let (mut opened, _slot) = self.open_write(&p.project_id).await?;
        let md = p.md;
        let path = blocking(move || {
            vidbrief_ops::write_note(&mut opened, &rel, &md, mode, count_round)
        })
        .await?;
        Ok(text(format!("已写入 {}", path.display())))
    }

    #[tool(
        description = "写 `03_summary/summary.md`,并标记步 3 完成。\
                       **写完要停下来把内容呈现给用户**,等他说改哪儿。",
        annotations(title = "写总结", read_only_hint = false, idempotent_hint = true)
    )]
    async fn write_summary(
        &self,
        Parameters(p): Parameters<WriteDocParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, _slot) = self.open_write(&p.project_id).await?;
        let md = p.md;
        let path = blocking(move || vidbrief_ops::write_summary(&mut opened, &md)).await?;
        Ok(text(format!(
            "已写入 {}\n下一步是报告:写 report.md 用 write_report_md。",
            path.display()
        )))
    }

    #[tool(
        description = "写 `04_report/report.md`,并标记步 4 完成。\
                       图片引用请用相对路径 `04_report/assets/…`(报告 HTML 会去那儿找);\
                       **图表化 HTML 还没实现**(见 DESIGN §13 的 M6),这一步只落 Markdown。",
        annotations(title = "写报告", read_only_hint = false, idempotent_hint = true)
    )]
    async fn write_report_md(
        &self,
        Parameters(p): Parameters<WriteDocParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, _slot) = self.open_write(&p.project_id).await?;
        let md = p.md;
        let path = blocking(move || vidbrief_ops::write_report_md(&mut opened, &md)).await?;
        Ok(text(format!("已写入 {}", path.display())))
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
            (CorrectionTypeArg::Uncertain, CorrectionType::Uncertain),
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
