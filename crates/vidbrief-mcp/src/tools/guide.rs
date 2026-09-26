//! 「说明书」组:把每一步该怎么做递给 agent。
//!
//! # 为什么要有这一个工具(M5 真 agent 联调)
//!
//! MCP 的提示模板(`prompts/get`)是**可选能力** —— 宿主可以不实现。
//! 实测手上的宿主就没实现 `prompts/list`:四个提示模板(P1–P4)**一个都到不了 agent**,
//! 它只能照着 `instructions` 里那几行自己摸索(摸出来的路径居然不错,但
//! 「先通读再补看」「旧结论别照抄」「补来的图也要核」这些要点它没拿到)。
//!
//! **工具是 MCP 的必选能力**,所以同一份说明再开一个工具出口。
//! 文本只有一处(`crate::prompts` 里的那几个 `*_text` 函数):能取到 prompts 的宿主
//! 走那边,取不到的走 `get_guide` —— 两个出口,一份维护。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::json::{failed, text};
use crate::prompts;
use crate::server::VidbriefServer;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GuideParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 哪一步。不给 = 返回目录(有哪几步、各自能拿到什么说明)。
    #[serde(default)]
    pub step: Option<String>,
}

#[tool_router(router = guide_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "每一步该怎么做 —— 与服务的提示模板(prompts)同一份文本。\
                       开工前先取一次:不看说明就动手,容易漏掉「审过的段也要报」\
                       「补来的图也要核」这类规矩。\
                       `step` 不给时返回目录(列有哪几步);\
                       取哪一步就填哪一步:`preprocess` / `calibrate_shots` / \
                       `fix_transcript` / `find_missing_shots` / `summarize_video` / \
                       `build_report` / `styles`。\
                       **`styles` 是报告的风格轮廓**(五种预置风格怎么填)—— 报告动笔前先取它。",
        annotations(title = "取操作说明", read_only_hint = true, idempotent_hint = true)
    )]
    async fn get_guide(
        &self,
        Parameters(p): Parameters<GuideParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let steps = "preprocess / calibrate_shots / fix_transcript / find_missing_shots / summarize_video / build_report / styles";
        let body = match p.step.as_deref() {
            None => prompts::guide_index(&p.project_id),
            Some("calibrate_shots") => prompts::calibrate_shots_text(&p.project_id),
            Some("fix_transcript") => prompts::fix_transcript_text(&p.project_id),
            Some("find_missing_shots") => prompts::find_missing_shots_text(&p.project_id),
            Some("summarize_video") => prompts::summarize_video_text(&p.project_id),
            Some("build_report") => prompts::build_report_text(&p.project_id),
            // 风格那一份**不是提示**:它是"怎么写"的手艺,住在 `skill/styles.md`,
            // 由这里原样发出去(同一份文件既给装了技能目录的宿主读,也给只认协议的宿主取——
            // 两个出口一份维护,与 `get_guide` 本身同一个道理)。
            Some("styles") => {
                format!("项目:`{}`。\n\n{}", p.project_id, prompts::styles_text())
            }
            Some("preprocess") => format!(
                "项目:`{}`。步 1 是**一件作业,三个入口**:`preprocess` 起作业并等它\
                 (默认最多等 180 秒,抽帧 → 密集区 → 截图 → 转录 → 时间线,纯算法),\
                 等够了它还在跑就如实返回当前进度(**不重开**;再调一次是接着等);\
                 `preprocess_progress` 只看一眼跑到哪儿了,`preprocess_result` 取结果\
                 (跑完了是完整交代;还在跑会说「还没好」,不是错误)。\n\
                 作业跑在**独立进程**里,进度落在 `01_preprocess/run.json` —— \
                 所以这次调用断了、会话关了,它照样跑完。\n\
                 跑完看返回里的 `done`:false 表示转录失败(材料保留、未标记完成)。\n\
                 细节见服务的 instructions —— 这一步没有单独的长说明,因为它不需要判断。",
                p.project_id
            ),
            Some(other) => {
                return Err(failed(format!(
                    "没有 `{other}` 这一步。有的是:{steps}。\n\
                     不带 `step` 再调一次能拿到目录。"
                )));
            }
        };
        // 回的是**纯文本**:它是给人读的说明,套一层 JSON 字符串只是多一层引号要剥
        Ok(text(body))
    }
}
