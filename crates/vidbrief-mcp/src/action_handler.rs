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

/// 这条路的**持锁凭证**活到哪:`approve` 的局部变量里 —— 它 drop 得比函数返回早一点?
///
/// 不:它的作用域就是 `approve` 的函数体,而所有用它的代码都在体内。
/// 单独写一条提醒,是因为"按次取放"这套全靠 Drop 的时机对([`State::enter_write`])。

/// 「这一步我看过了,通过」。
///
/// 报错一律翻译成**给人看的一句话**:读到它的是浏览器里的用户,不是开发者。
fn approve(state: &Arc<State>, rt: &tokio::runtime::Handle, project_id: &str) -> Result<String, String> {
    // 入口序列和 MCP 工具**共用同一套**(`State::enter_write` + 排队 + 那句警告),
    // 动作本身走 ops 的那一个入口(`with_write`)。三段拼起来就是完整的写事务。
    let opened = OpenProject::open(project_id).map_err(|e| explain(&e))?;
    // 凭证:活到函数结束(这个函数就是"一次写调用"的全部)
    let _hold = state.enter_write(&opened).map_err(|e| explain(&e))?;
    let dir = opened.dir().to_path_buf();
    // 锁已经在本进程手上 —— 把它当凭证交给 ops,让那边**不要再取一次**
    // (同进程再取会被自己挡住,报错还长得像"被别的会话占着")。
    let guard = state.guard_of(&dir);

    // **必须和工具调用排同一队。** 两条路都会读改写项目的状态(`state.bin`)
    // (`approve` 写 `review`,`record_shot_check` 写 `progress`),
    // 不排队就是后写的赢、前一个的改动凭空消失 —— `serialize` 存在的唯一理由。
    //
    // 这里 `block_on` 是安全的:本线程是查看服务的普通线程,不是 tokio worker,
    // 不会踩"运行时里再 block_on"那个 panic。阻塞它也不影响别人 ——
    // 它本来就只服务浏览器。
    let _slot = rt.block_on(state.serialize(&dir));

    // 写锁是降级放行(文件系统不支持锁)时,这句话同样要说给用户 —— 和工具那条路一致
    let warning = state.take_lock_warning(&dir);

    // `with_write` 会自己把项目**再开一次**(多一次 JSON 读,一次点击无所谓),
    // 换来的是"写项目的路径只有一条"这件事不用每个入口各记一遍。
    let approval = vidbrief_ops::with_write(project_id, guard.as_deref(), |opened, _| {
        vidbrief_ops::approve(opened, None)
    })
    .map_err(|e| explain(&e))?;
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
            "你确认的是「就这样往下走」—— 这些还欠着的东西已经记进项目状态。\
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    use vidbrief_ops::testkit::Fake;

    /// **页面按钮和工具调用排同一队。**
    ///
    /// 从前这里只有一句话:"它接了 `serialize`,和工具调用排同一队"——**没有任何测试
    /// 能证明它真在排队**(并发跑一次是微秒级窗口,证明不了任何事;`[260925-14]` 教过
    /// "抓不到 bug 的竞态测试比没有更糟")。所以这条测试**占住名额再点按钮**:
    /// 名额没放,按钮就必须**阻塞**;放开的瞬间它才动手。
    ///
    /// 直接调 [`approve`](HTTP 那一跳由 `vidbrief-viz` 自己的
    /// `page_actions_are_handed_to_the_host` 盯着)——因为 `OpenProject::open(项目 id)`
    /// 是**按进程的工作目录**解析 `projects/<id>` 的,测试进程的 cwd 不是那个工作区。
    /// 换句话说:这里验的是"手递到手之后会不会排队",那是这条测试唯一想钉的东西。
    #[test]
    fn the_page_button_waits_for_the_queue() {
        let fake = Fake::new("action-queue");
        let rt = tokio::runtime::Runtime::new().expect("建运行时");
        let state = Arc::new(State::new());
        let project = fake.layout().root().display().to_string();

        // **占住名额**:这一下模拟"agent 正拿着这个项目的写操作在干活"
        let opened = vidbrief_ops::OpenProject::open(&project).expect("打开假项目");
        let held = rt.block_on(state.serialize(opened.dir()));

        let (tx, rx) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker_rt = rt.handle().clone();
        std::thread::spawn(move || {
            let answer = approve(&worker_state, &worker_rt, &project);
            let _ = tx.send(answer);
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "名额还被占着,按钮就**不该**跑完 —— 它是排在同一队里的"
        );

        // 放行:按钮这才被处理。成功或业务报错都行 —— 这条测的是**排队**,不是 approve 的规矩。
        drop(held);
        let answer = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("名额放开了,按钮该有回话");
        assert!(!answer.is_ok() || !answer.unwrap().is_empty(), "按钮要回一句话给页面看");
    }
}
