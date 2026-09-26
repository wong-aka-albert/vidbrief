//! F 组:可视化(`render_timeline` / `render_calibration_table`)。
//!
//! 设计见 `docs/DESIGN.md` §9.1。渲染 + 起本地服务 + (可选)打开浏览器 ——
//! **服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,时间轴点了不跳。
//!
//! # 为什么这组工具不取写锁
//!
//! 它们写的是 `timeline.html` / `calibrate.html`,**派生视图** —— 内容完全由材料推出来。
//! 两个会话同时渲染出不同内容也无所谓,下次重渲染就一致了。
//! 写锁保护的是材料与结论(`state.bin` / `shots.json` / `transcript.fixed.json`),
//! 不是视图。要是它们也要锁,"另一个会话正开着项目"就变成连看都看不了,
//! 而查看恰恰是最不该被挡住的事。

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::json::{blocking, json};
use crate::server::VidbriefServer;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderTimelineParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 要不要顺手打开默认浏览器。默认 true(用户就在这台机器上看)。
    /// 无头机器上传 false,只拿 URL。
    #[serde(default)]
    pub open_browser: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderCalibrationTableParams {
    #[schemars(description = "项目:项目目录的路径,或 `projects/` 下的项目名")]
    pub project_id: String,
    /// 要不要顺手打开默认浏览器。默认 true(用户就在这台机器上看)。
    /// 无头机器上传 false,只拿 URL。
    #[serde(default)]
    pub open_browser: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
struct ViewBrief {
    html_path: String,
    /// 本地查看服务的地址。**给用户点开的就是这个**,不是 html_path。
    url: String,
    /// 服务监听在哪 —— 只绑回环地址,局域网访问不到。
    listen: String,
    hint: String,
}

#[tool_router(router = view_tools, vis = "pub")]
impl VidbriefServer {
    #[tool(
        description = "把最新材料渲染成四层共轴时间线(`01_preprocess/timeline.html`),\
                       起本地查看服务,并打开用户浏览器。**每次改完材料都调一次** —— \
                       用户就是靠这个页面检查你干的活的。返回的 url 给用户点。\
                       渲染不取写锁:它写的是派生视图,不受「另一个会话正开着项目」影响。",
        annotations(title = "刷新时间线视图", read_only_hint = true, idempotent_hint = true)
    )]
    async fn render_timeline(
        &self,
        Parameters(p): Parameters<RenderTimelineParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let open = p.open_browser.unwrap_or(true);
        let opened = self.open_read(&p.project_id).await?;
        let (path, url) = blocking(move || vidbrief_ops::serve_and_open(&opened, open)).await?;

        json(&ViewBrief {
            html_path: path.display().to_string(),
            url: url.clone(),
            listen: "127.0.0.1(端口由操作系统分配,只监听本机)".into(),
            hint: format!(
                "把 {url} 给用户看。时间轴上点/拖任意位置即可跳转视频,空格播放暂停,←→ ±1 秒。"
            ),
        })
    }

    #[tool(
        description = "把校准结果渲染成对照表(`02_calibrate/calibrate.html`),起本地查看服务,\
                       并打开用户浏览器。那一页一屏看完:**每张截图的结论**(图文不符/没把握高亮,\
                       弃用的带理由)、**每段转录的改动**(原文/改后/依据并排,未核验的淡显),\
                       末尾内嵌校准日志原文 —— 每条结论上都标了写下的时刻,对照日志的轮次\
                       时间段,就能看出它是哪一轮写的。**每次改完转录或看图结论都调一次**,\
                       返回的 url 给用户点。渲染不取写锁:它写的是派生视图。",
        annotations(title = "刷新校准对照表", read_only_hint = true, idempotent_hint = true)
    )]
    async fn render_calibration_table(
        &self,
        Parameters(p): Parameters<RenderCalibrationTableParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let open = p.open_browser.unwrap_or(true);
        let opened = self.open_read(&p.project_id).await?;
        let (path, url) =
            blocking(move || vidbrief_ops::serve_and_open_table(&opened, open)).await?;

        json(&ViewBrief {
            html_path: path.display().to_string(),
            url: url.clone(),
            listen: "127.0.0.1(端口由操作系统分配,只监听本机)".into(),
            hint: format!(
                "把 {url} 给用户看。顶部可按「还没核验 / 没把握 / 已弃用 / 有结论」筛选;点缩略图看大图。"
            ),
        })
    }
}
