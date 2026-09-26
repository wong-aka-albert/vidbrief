//! MCP 服务(本项目的对外形态)。
//!
//! 设计见 `docs/DESIGN.md` §3、§10.2、§11。
//!
//! 三条铁律(§0.1):
//! 1. **识别与判断归 agent** —— 本 crate 不做任何模型推理
//! 2. **工具零密钥** —— 不存在调用外部模型 API 的代码路径
//! 3. **协议通用** —— 不绑定具体 agent,只假设"能看图 + 能调工具"
//!
//! | 模块 | 干什么 |
//! |------|--------|
//! | [`server`] | 服务本体:能力声明 + 工具/提示/资源三张表 |
//! | [`state`] | 进程内状态:写锁登记、每项目串行、见过的项目 |
//! | [`tools`] | §11 的 A–F 组工具,一个模块一组 |
//! | [`prompts`] | §10.4 的提示模板(只放引用的工具都已就绪的那些) |
//! | [`resources`] | 只读项目文件 |
//!
//! # stdio 上有一条不能破的规矩
//!
//! **stdout 是协议通道。** 任何 `println!`、进度条、调试输出掉进去,
//! 客户端就会收到一段不是 JSON-RPC 的字节流,整个会话当场坏掉。
//! 所以这里:**诊断一律走 stderr**;库层(`vidbrief-ops` 往下)根本不打印。
//!
//! # 传输
//!
//! 用官方 Rust SDK(`rmcp`)的 stdio 传输。DESIGN §10.2 的原话是"优先 Rust 生态 MCP SDK;
//! API 不稳则直接实现 tools 的 JSON-RPC" —— 3.1.2 的 `#[tool_router]` / `#[tool_handler]`
//! 宏已经够用,不必手写 JSON-RPC。

pub mod action_handler;
pub mod json;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod state;
pub mod tools;

pub use action_handler::install_action_handler;
pub use server::VidbriefServer;

use anyhow::Context as _;

/// 起一个 stdio MCP 服务,**直到客户端断开**。
///
/// 这个函数返回之后 stdout 才重新属于调用方 —— 在它运行期间,stdout 归 JSON-RPC 所有。
/// CLI 的 `vidbrief mcp` 就是它的唯一调用者。
pub fn serve_stdio() -> anyhow::Result<()> {
    // 不用 `current_thread`:本项目的每个动作都是阻塞的(ffmpeg / whisper / 读几百 MB 文件),
    // 单线程运行时下 `spawn_blocking` 会退化成同一线程排队,连 ping 都响应不了。
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("vidbrief-mcp")
        .build()
        .context("建 tokio 运行时失败")?;

    runtime.block_on(async {
        use rmcp::ServiceExt as _;

        // **服务跑在别的进程里**(2026-09-26):从前查看服务是本进程里的一条线程,
        // 于是"关掉页面"= 杀掉工具自己 —— 而它可能就是 agent 的会话进程。
        // 现在第一次要视图时 spawn 一份 `vidbrief serve`,页面与它同生共死,不连坐。
        vidbrief_ops::use_separate_process();

        let server = VidbriefServer::new();
        // 页面上的按钮要能落到这个进程里执行 —— **用同一份 State**(同一把写锁、
        // 同一张"见过的项目"表),并用同一个运行时排队,免得和工具调用各写各的
        // 项目状态(`state.bin`)。见 `action_handler` 的模块说明。
        install_action_handler(server.state_arc(), tokio::runtime::Handle::current());
        let running = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow::anyhow!("MCP 握手失败:{e}"))?;
        // 客户端关掉 stdin(或直接退出)时这里返回
        running
            .waiting()
            .await
            .map_err(|e| anyhow::anyhow!("MCP 服务异常结束:{e}"))?;
        // **断线要是一句明白话。** 宿主那边通常只会说「管道正在被关闭」—— 那是它自己的
        // 传输层在说话,读的人(和 agent)看不出"谁断了、已经落盘的东西在不在"。
        // stderr 不进协议通道,但它在日志里是**唯一**写着这件事的地方。
        eprintln!(
            "vidbrief: 客户端断开了连接(stdin 关闭),服务退出。\
             已经返回过的每次调用都已落盘;没跑完的那一次不会补写。\
             步 1 例外 —— 它的作业跑在**独立进程**里,会话走了它照样跑完;\
             看 `<项目>/01_preprocess/run.json` 就知道它跑到哪儿了。"
        );
        Ok(())
    })
}
