//! E 组里跟截图有关的算法动作(`find_supplement_candidates` / `extract_shot` /
//! `request_shot` / `update_shot`)。
//!
//! 设计见 `docs/DESIGN.md` §6.2 ④。**算法只列候选、只搬像素;要不要补是你的判断。**

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use vidbrief_project::ShotOrigin;

use crate::json::{blocking, failed, json};
use crate::server::VidbriefServer;
use crate::tools::materials::parse_shot_id;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SupplementParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// "附近有图"的判定半径(秒)。默认 3.0。
    #[serde(default)]
    pub window: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExtractShotParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 截取时刻(秒)。会被夹进视频有效范围。
    pub t: f64,
    /// **为什么要补这张图**。写给用户看的一句话,会存进 shots.json 的 `reason`。
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateShotParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 截图 id,形如 `s007`(也接受纯数字 `7`)。
    pub shot_id: String,
    /// `keep` 留用 / `drop` 弃用(不回收 id)/ `retime` 换个时刻重截。
    pub action: String,
    /// `action = retime` 时必给:新的截取时刻(秒)。
    #[serde(default)]
    pub t: Option<f64>,
}

#[tool_router(router = shot_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "扫转录,列出**可能缺图**的位置(语音提到画面上的东西但附近没有截图),\
                       写进 `02_calibrate/supplement_requests.json` 并返回。\
                       **这只是候选,不是结论** —— 用户说「看这里」不代表真有个图要看,\
                       逐条自己判断;确实要补的再调 extract_shot。\
                       再次调用会保留你上次对同一段的判断。",
        annotations(title = "找补图候选", read_only_hint = false, idempotent_hint = true)
    )]
    async fn find_supplement_candidates(
        &self,
        Parameters(p): Parameters<SupplementParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let window = p.window.unwrap_or_else(vidbrief_ops::supplement_window);
        let (opened, _slot) = self.open_write(&p.project_id).await?;
        let requests =
            blocking(move || vidbrief_ops::find_supplement_candidates(&opened, window)).await?;
        json(&requests)
    }

    #[tool(
        description = "在指定时刻补截一张图(登记进 shots.json,`origin = agent_request`)。\
                       用在:算法漏了的地方、转录提到但没图的地方、或用户说「重新截这里」。\
                       补完用 get_shot 看新图。",
        annotations(title = "补一张截图", read_only_hint = false)
    )]
    async fn extract_shot(
        &self,
        Parameters(p): Parameters<ExtractShotParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        self.extract_shot_impl(p).await
    }

    #[tool(
        description = "同 extract_shot:在指定时刻补截一张图。两个名字都注册了,\
                       是因为提示模板里两种说法都会出现,不必让 agent 记哪一个才对。",
        annotations(title = "补一张截图(别名)", read_only_hint = false)
    )]
    async fn request_shot(
        &self,
        Parameters(p): Parameters<ExtractShotParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        self.extract_shot_impl(p).await
    }

    #[tool(
        description = "改一张现有截图:`keep` 留用 / `drop` 弃用 / `retime` 换个时刻重截\
                       (重抽帧、换文件名、删旧图)。\
                       `drop` **不回收 id**,只是标记 —— 历史留在文件里可查。",
        annotations(title = "改截图", read_only_hint = false, idempotent_hint = true)
    )]
    async fn update_shot(
        &self,
        Parameters(p): Parameters<UpdateShotParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let id = parse_shot_id(&p.shot_id)?;
        let action = match p.action.as_str() {
            "keep" => vidbrief_ops::ShotAction::Keep,
            "drop" => vidbrief_ops::ShotAction::Drop,
            "retime" => {
                let Some(t) = p.t else {
                    return Err(failed(
                        "action = retime 时必须给 t(新的截取时刻,秒)",
                    ));
                };
                vidbrief_ops::ShotAction::Retime { t }
            }
            other => {
                return Err(failed(format!(
                    "action 只能是 `keep` / `drop` / `retime`,收到 `{other}`"
                )));
            }
        };
        let (opened, _slot) = self.open_write(&p.project_id).await?;
        let shot = blocking(move || vidbrief_ops::update_shot(&opened, id, action)).await?;
        json(&shot)
    }
}

impl VidbriefServer {
    async fn extract_shot_impl(
        &self,
        p: ExtractShotParams,
    ) -> Result<CallToolResult, CallToolResult> {
        let (opened, _slot) = self.open_write(&p.project_id).await?;
        let shot = blocking(move || {
            vidbrief_ops::extract_shot(&opened, p.t, &p.reason, ShotOrigin::AgentRequest)
        })
        .await?;
        json(&shot)
    }
}
