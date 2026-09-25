//! B 组:步 1 前处理(`preprocess`)。
//!
//! 设计见 `docs/DESIGN.md` §5。**一次调用做完步 1 的全部产出**,无中间态、不调模型。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::json::{blocking, json};
use crate::tools::attach_warning;
use crate::server::VidbriefServer;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PreprocessParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 采样帧率(每秒几个点)。默认 1.0。调高更细但更慢。
    #[serde(default)]
    pub fps: Option<f64>,
    /// 参与打分的缩小宽度。默认 480。
    #[serde(default)]
    pub width: Option<u32>,
    /// 要不要跑语音转文字。默认 true。
    ///
    /// **只在明确不需要转录时才传 false** —— "转录失败"与"我选择跳过"是两回事,
    /// 拿它掩盖失败会让项目状态说谎(那样步 1 会被标成完成)。
    #[serde(default)]
    pub transcribe: Option<bool>,
    /// 要不要抽关键截图。默认 true。
    #[serde(default)]
    pub shots: Option<bool>,
}

#[tool_router(router = preprocess_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "步 1 前处理:抽帧算差帧图 → 找变化密集区 → 抽关键截图 → 语音转文字 → \
                       渲染时间线。**纯算法,一次做完,不调任何模型。**\
                       长视频要几十秒到几分钟,期间不会有进度回报,请耐心等。\
                       返回的 `done` 为 false 表示转录失败(材料保留、未标记完成),\
                       此时应把 `transcript.error` 转告用户,而不是继续往下走。",
        annotations(title = "跑前处理", read_only_hint = false, idempotent_hint = true)
    )]
    async fn preprocess(
        &self,
        Parameters(p): Parameters<PreprocessParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let (mut opened, slot) = self.open_write(&p.project_id).await?;

        let mut options = vidbrief_ops::PreprocessOptions::default();
        if let Some(fps) = p.fps {
            options.fps = fps;
        }
        if let Some(width) = p.width {
            options.width = width;
        }
        if let Some(transcribe) = p.transcribe {
            options.transcribe = transcribe;
        }
        if let Some(shots) = p.shots {
            options.shots = shots;
        }

        let report = blocking(move || {
            // 库层的进度回调在 MCP 场景下没有接收方(stdio 上不能画画),
            // 传一个空实现,免得白白维护一套没人看的进度
            vidbrief_ops::preprocess(&mut opened, &options, &mut |_stage| {})
        })
        .await?;

        // 转录失败时**内容照给**(agent 需要看视觉部分做到了什么),
        // 但 `isError` 置真,免得 agent 以为步 1 完整跑完了
        let mut result = json(&report)?;
        if !report.done {
            result.is_error = Some(true);
        }
        Ok(attach_warning(result, slot.warning()))
    }
}
