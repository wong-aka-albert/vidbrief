//! 服务本体:能力声明 + 工具/提示/资源三张表。
//!
//! 传输与握手由 `rmcp` 负责;这里只回答三个问题:
//! **我会什么**(capabilities)、**我有什么工具**(tools/prompts)、**我能读什么文件**(resources)。

use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::model::{
    Implementation, InitializeResult, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ServerCapabilities,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{prompt_handler, tool_handler};

use crate::state::State;

/// MCP 服务。**每个 agent 会话一个进程、一个实例**(stdio 是 1:1 的)。
#[derive(Clone)]
pub struct VidbriefServer {
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
    pub(crate) state: Arc<State>,
}

impl VidbriefServer {
    pub fn new() -> Self {
        Self {
            // 六组工具各在自己的模块里声明,这里只是把它们接起来。
            // 分组的意义:一个模块对应 DESIGN §11 里的一组,改哪组看哪个文件。
            tool_router: VidbriefServer::project_tools()
                + VidbriefServer::preprocess_tools()
                + VidbriefServer::materials_tools()
                + VidbriefServer::conclusion_tools()
                + VidbriefServer::shot_tools()
                + VidbriefServer::view_tools(),
            prompt_router: crate::prompts::router(),
            state: Arc::new(State::new()),
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// 给测试用:本服务注册的全部工具名。
    pub fn tool_names_for_test(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect()
    }
}

impl Default for VidbriefServer {
    fn default() -> Self {
        Self::new()
    }
}

/// 一句话告诉 agent 这个服务是干什么的、按什么顺序用。
///
/// `instructions` 会被客户端塞进系统提示,写错了会**主动误导** ——
/// 所以只写"怎么用",不写"这个工具集多厉害"。
const INSTRUCTIONS: &str = "\
vidbrief:把长视频变成图文总结的**材料供给与结论落盘**工具集。

它不做任何识别与判断 —— 判断是你的活。它负责:把视频拆成差帧图 / 转录 / 关键截图,
把材料准确递给你(包括**图像本身**),再把你得出的结论原样存回文件。

四步(每一步做完都要停下来给用户看,不要一路跑到底):
  ① create_project → preprocess          建项目、跑前处理(纯算法,一次做完)
  ② 校准:list_shots → get_shot 逐张看图 → record_shot_check 存回;转录纠错用 apply_corrections
  ③ 总结:search_transcript / get_shot 自主探索 → write_summary
  ④ 报告:write_report_md

三条硬规矩:
  1. **看了图再下结论。** get_shot 会把图像交给你;不要只看转录就推断画面上有什么。
  2. **转录可能听错。** 实测「Python」被听成「派森」、「元组」被听成「元祖」。
     纠错的第一依据是**画面上的文字**,其次是上下文,术语表最后。
  3. **诚实地写\"看不出来\"。** get_shot 返回的图像若是糊的/切走的/与转录不符,
     如实写进 issues 并给 low confidence —— 用户会在审查节点裁决。";

// 两个宏必须叠在**同一个** impl 块上:一个 trait 只能实现一次。
// `get_info` 我们自己写了,所以宏不会覆盖它 —— 能力声明以我们写的为准(工具+提示+资源三种)。
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl rmcp::ServerHandler for VidbriefServer {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("vidbrief-mcp", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // 本项目不做分页:一个会话碰过的项目数量是人手可数的,
        // 而分页会让 agent 多花一轮往返去取它本来一次就能拿完的东西
        let _ = request;
        let state = self.state.clone();
        crate::json::blocking(move || Ok(crate::resources::list_resources(&state)))
            .await
            .map_err(crate::json::to_error_data)
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let _ = request;
        Ok(crate::resources::list_templates())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let state = self.state.clone();
        let uri = request.uri.clone();
        // 资源读不出来是**协议级**的失败(客户端要的 URI 不存在),
        // 与工具错不同:它没有"agent 自己换个做法"的余地,所以走 JSON-RPC error
        let result = crate::json::blocking(move || crate::resources::read(&state, &uri))
            .await
            .map_err(crate::json::to_error_data)?;
        Ok(result.into())
    }
}
