//! 页面上的按钮 → 落到 MCP 进程里执行。
//!
//! # 为什么要有这一层
//!
//! 本地查看服务(`vidbrief-viz`)是**渲染层**:它把项目目录当静态站点发出去,
//! 顶多再收一个"用户按了按钮"的信号。**它不该会写文件** —— 一旦会写,
//! 写项目就有了第二条路径:锁、状态、报错文案全得再来一套,而且两边迟早不一致。
//!
//! 所以按钮的请求交回**宿主**执行。MCP 会话进程就是那个宿主:它手上有写锁、
//! 有 `State`、有和 agent 工具调用完全相同的 `vidbrief-ops` 动作。
//! 于是"用户在页面上点通过"和"agent 调 `approve` 工具"是**同一条路**,
//! 只是入口不同。
//!
//! # 页面本身不知道这件事
//!
//! 页面只 POST 一下,拿到一句话显示出来。它不知道自己背后是 MCP 还是别的什么 ——
//! 背后没人的时候(`vidbrief ui` 起的页面),它会拿到一句"这里不能执行,请用命令行"。
//!
//! # 线程
//!
//! 这个回调在查看服务自己的线程上跑(不是 tokio worker),而它做的全是阻塞 IO
//! (开文件、上锁、写 JSON)。**正好**:不用包 `spawn_blocking`,
//! 也不会把 tokio 的线程堵住。

use std::sync::Arc;

use vidbrief_ops::{Action, ActionHandler, OpenProject};

use crate::state::State;

/// 注册宿主执行者。进程内只认第一次(`serve_stdio` 一个进程只调一次)。
///
/// `rt` 是 MCP 运行时的句柄 —— 页面动作要**和工具调用排同一队**
/// (见 `approve` 里那句 `serialize`)。
pub fn install_action_handler(state: Arc<State>, rt: tokio::runtime::Handle) {
    let handler: Arc<ActionHandler> = Arc::new(move |project_id: &str, action: Action| {
        match action {
            Action::Approve => approve(&state, &rt, project_id),
        }
    });
    vidbrief_ops::set_action_handler(handler);
}

/// 「这一步我看过了,通过」。
///
/// 报错一律翻译成**给人看的一句话**:读到它的是浏览器里的用户,不是开发者。
fn approve(state: &State, rt: &tokio::runtime::Handle, project_id: &str) -> Result<String, String> {
    let mut opened = OpenProject::open(project_id).map_err(|e| explain(&e))?;

    // 和工具调用走同一把锁:本进程已经持有就是幂等的(见 `State::ensure_write_lock`);
    // 真拿不到说明这个项目正被**别的会话**编辑,那时页面上该说清楚。
    state.ensure_write_lock(&opened).map_err(|e| explain(&e))?;
    state.note(opened.dir(), opened.id());

    // **必须和工具调用排同一队。** 两条路都会整份读改写 `project.json`
    // (`approve` 写 `review`,`record_shot_check` 写 `progress`),
    // 不排队就是后写的赢、前一个的改动凭空消失 —— `serialize` 存在的唯一理由。
    //
    // 这里 `block_on` 是安全的:本线程是查看服务的普通线程,不是 tokio worker,
    // 不会踩"运行时里再 block_on"那个 panic。阻塞它也不影响别人 ——
    // 它本来就只服务浏览器。
    let _slot = rt.block_on(state.serialize(opened.dir()));

    // 写锁是降级放行(文件系统不支持锁)时,这句话同样要说给用户 —— 和工具那条路一致
    let warning = state.take_lock_warning(opened.dir());

    let approval = vidbrief_ops::approve(&mut opened, None).map_err(|e| explain(&e))?;
    let c = approval.counts;
    let pending = c.unchecked + c.needing_review + c.supplement_pending + c.uncertain;

    Ok(format!(
        "{}\
         已记下用户确认({})。\n\
         未校准 {} 张 · 待裁决 {} 张 · 未处理补图候选 {} 条 · 存疑转录 {} 段\n\
         {}\n\
         \n\
         门开了,agent 可以写总结了。之后再改任何材料,这次确认作废。",
        warning
            .map(|w| format!("⚠️ {w}\n\n"))
            .unwrap_or_default(),
        approval.approved_at.format("%m-%d %H:%M"),
        c.unchecked,
        c.needing_review,
        c.supplement_pending,
        c.uncertain,
        if pending > 0 {
            "你确认的是「就这样往下走」—— 这些还欠着的东西已经记进 project.json。\
             想留一句理由,告诉 agent 或用命令行 `vidbrief approve -n \"…\"`。"
        } else {
            "东西都处理干净了。"
        }
    ))
}

/// 把 anyhow 的错误链压成一句话(页面上只显示一行,太长的没人读)。
fn explain(e: &anyhow::Error) -> String {
    format!("{e:#}")
}
