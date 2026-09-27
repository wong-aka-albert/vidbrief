//! F 组:可视化(`render_workbench`)。
//!
//! 设计见 `docs/DESIGN.md` §9.1。渲染 + 起本地服务 + (可选)打开浏览器 ——
//! **服务是必须的**:浏览器用 `file://` 打开时对本地媒体有跨域限制,轴上点了不跳。
//!
//! # 为什么这组工具不取写锁
//!
//! 它写的是 `workbench.html`,**派生视图** —— 内容完全由材料推出来。
//! 两个会话同时渲染出不同内容也无所谓,下次重渲染就一致了。
//! 写锁保护的是材料与结论(`state.bin` / `shots.json` / `transcript.fixed.json`),
//! 不是视图。要是它也要锁,"另一个会话正开着项目"就变成连看都看不了,
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
pub struct RenderWorkbenchParams {
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
        description = "把项目渲染成**工作台**(`01_preprocess/workbench.html`)—— 一个页面看全部:\
                       左边视频 + 五层共轴时间轴(差帧图/密集区/截图/转录/知识点),\
                       轴上点一下就地展开那一条的解析(浮层),右边抽屉里三块(截图/转录/报告)。\
                       起本地查看服务,并打开用户浏览器。**改完材料、或报告出来了都调它** —— \
                       用户要一个地方能同时看画面、数据和结论。步 2 起就能用(那时报告栏空着)。\
                       返回的 url 给用户点。渲染不取写锁:它写的是派生视图。",
        annotations(title = "刷新工作台", read_only_hint = true, idempotent_hint = true)
    )]
    async fn render_workbench(
        &self,
        Parameters(p): Parameters<RenderWorkbenchParams>,
    ) -> Result<CallToolResult, CallToolResult> {
        let open = p.open_browser.unwrap_or(true);
        let opened = self.open_read(&p.project_id).await?;
        let (path, url) =
            blocking(move || vidbrief_ops::serve_and_open_workbench(&opened, open)).await?;

        json(&ViewBrief {
            html_path: path.display().to_string(),
            url: url.clone(),
            listen: "127.0.0.1(端口由操作系统分配,只监听本机)".into(),
            hint: format!(
                "把 {url} 给用户看。左边点轴上任意位置跳到那一刻;轴上的锚点点一下会就地展开那一条;\
                 右边抽屉的标签切三块内容,开合归右上角那个按钮。"
            ),
        })
    }
}
