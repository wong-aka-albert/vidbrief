//! MCP 服务的**协议层**端到端测试:真的把 `vidbrief mcp` 拉起来,真的走 stdio 上的 JSON-RPC。
//!
//! 这里管的是"线格式对不对、握手说不说得通、通道通不通"。
//! **"agent 照着自己拿到的那份说明能不能把活干成"在 `agent.rs`** ——
//! 两者失败的修法完全不同:这里坏了是协议实现有问题,那边坏了是**说明书说谎**。

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{McpClient, TempDir, first_text, head, tool_failed};
use serde_json::{Value, json};

// ── 协议层 ───────────────────────────────────────────────────────────

#[test]
fn handshake_declares_all_three_capabilities() {
    let dir = TempDir::new("handshake");
    let mut client = McpClient::start(dir.path());
    let result = client.initialize();

    assert_eq!(
        result["serverInfo"]["name"], "vidbrief-mcp",
        "服务名是接入方看到的身份:{}",
        result
    );
    for capability in ["tools", "prompts", "resources"] {
        assert!(
            !result["capabilities"][capability].is_null(),
            "没声明 {capability} 能力:{result}"
        );
    }
    assert!(
        result["instructions"].as_str().is_some_and(|s| s.contains("四步")),
        "instructions 是给 agent 的用法说明,不该是空的:{result}"
    );
}

#[test]
fn tools_list_covers_every_group_of_the_design() {
    let dir = TempDir::new("tools");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let result = client.call("tools/list", json!({}));
    let names: Vec<String> = result["tools"]
        .as_array()
        .expect("tools 应当是数组")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();

    // 每组至少一个代表 —— 少了任何一组,MCP 就是残的
    for expected in [
        "create_project",           // A
        "preprocess",               // B
        "get_shot",                 // C
        "record_shot_check",        // D
        "extract_shot",             // E
        "render_timeline",          // F
    ] {
        assert!(names.iter().any(|n| n == expected), "缺工具 {expected}:{names:?}");
    }

    // 每个工具都要有 description 与 inputSchema —— 没有描述的工具有等于没有
    for tool in result["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap_or("?");
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 10),
            "{name} 没有像样的描述"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{name} 的 inputSchema 不对");
    }
}

#[test]
fn prompts_and_resources_are_advertised() {
    let dir = TempDir::new("lists");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let prompts = client.call("prompts/list", json!({}));
    let names: Vec<String> = prompts["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in [
        "calibrate_shots",
        "fix_transcript",
        "find_missing_shots",
        "summarize_video",
        "build_report",
    ] {
        assert!(names.iter().any(|n| n == expected), "缺提示模板 {expected}:{names:?}");
    }

    let templates = client.call("resources/templates/list", json!({}));
    assert!(
        templates["resourceTemplates"]
            .as_array()
            .is_some_and(|t| !t.is_empty()),
        "资源模板不该是空的:{templates}"
    );

    // 空工作区下列资源也不该报错 —— 一个项目都没有是正常状态
    let resources = client.call("resources/list", json!({}));
    assert!(resources["resources"].is_array(), "{resources}");
}

/// **两个出口,一份文本**:`prompts/get` 与 `get_guide` 给的东西必须逐字相同。
///
/// M5 真 agent 联调发现:手上的宿主没实现 MCP 的 prompts(那是**可选能力**),
/// 四个模板一个都到不了 agent —— 它只能照着 instructions 里那几行自己摸索。
/// 于是同一份说明又开了一个**工具**出口(工具是必选能力)。
/// 两处一旦漂移,「有 prompts 的宿主」和「没有的宿主」会拿到两套说法,
/// 而这类分叉最难查。这条测试替我们盯着。
#[test]
fn the_guide_tool_and_the_prompts_serve_the_same_text() {
    let dir = TempDir::new("guide");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    for step in [
        "calibrate_shots",
        "fix_transcript",
        "find_missing_shots",
        "summarize_video",
        "build_report",
    ] {
        let prompt = client.call(
            "prompts/get",
            json!({"name": step, "arguments": {"project_id": "示例项目"}}),
        );
        let from_prompt = prompt["messages"][0]["content"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let from_guide =
            client.tool_text("get_guide", json!({"project_id": "示例项目", "step": step}));
        assert_eq!(from_prompt, from_guide, "`{step}` 两个出口的文本不一样了");
    }

    // 不带 step = 目录;瞎填 = 报错,而且给出有哪些步
    let index = client.tool_text("get_guide", json!({"project_id": "示例项目"}));
    assert!(index.contains("summarize_video"), "{index}");
    let bad = client.call_tool("get_guide", json!({"project_id": "示例项目", "step": "nope"}));
    assert!(tool_failed(&bad), "{bad}");
    let text = first_text(&bad);
    assert!(text.contains("summarize_video"), "报错该列出有哪些步:{text}");
}

#[test]
fn a_tool_that_needs_a_missing_project_fails_with_a_usable_message() {
    let dir = TempDir::new("missing");
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let result = client.call_tool("list_shots", json!({"project_id": "没有这个项目"}));
    assert!(tool_failed(&result), "该失败却没失败:{result}");
    let text = first_text(&result);
    assert!(text.contains("找不到项目"), "失败的说明要能看懂:{text}");
    assert!(text.contains("projects/"), "还要告诉人项目该怎么指:{text}");
}

// ── 端到端:走一遍真实的四步开头 ─────────────────────────────────────

#[test]
fn create_preprocess_look_and_write_back() {
    let dir = TempDir::new("loop");
    let video = dir.stage_video();
    let mut client = McpClient::start(dir.path());
    client.initialize();

    // ① 建项目
    let created = client.call_tool(
        "create_project",
        json!({"video_path": video.display().to_string(), "name": "e2e", "title": "端到端样例"}),
    );
    assert!(!tool_failed(&created), "建项目失败:{}", first_text(&created));
    let created: Value = serde_json::from_str(&first_text(&created)).expect("回执应当是 JSON");
    assert_eq!(created["project_id"], "e2e");
    // `dir` 默认是相对的(`projects/e2e`)—— 它相对的是**服务进程**的工作目录,
    // 而不是测试进程的,所以要显式拼一次
    let project_root = dir
        .path()
        .join(created["dir"].as_str().expect("回执里该有目录"));

    // ② 步 1。60 秒的合成视频,**不转录**(它没有音轨,转录会自动跳过)
    let report = client.call_tool("preprocess", json!({"project_id": "e2e"}));
    let report_text = first_text(&report);
    assert!(!tool_failed(&report), "跑步 1 失败:{report_text}");
    let report: Value = serde_json::from_str(&report_text).expect("报告应当是 JSON");
    assert_eq!(
        report["transcript"]["outcome"], "no_audio",
        "素材本来就没有音轨:{report_text}"
    );
    assert!(report["done"].as_bool().unwrap(), "没有音轨不该挡住步 1 完成:{report_text}");
    let shots = report["shots"].as_u64().expect("该有截图数");
    assert!(shots > 0, "60 秒的样例视频该抽出图来:{report_text}");
    assert_eq!(
        report["timeline_error"], Value::Null,
        "视图没渲染出来:{report_text}"
    );
    assert!(
        project_root.join("01_preprocess/timeline.html").is_file(),
        "渲染说成功了,文件却不在:{}",
        project_root.display()
    );

    // ③ 看清单。**这段合成素材的三个切换点恰好都是纯色帧**(画面 → 单色 → 单色),
    //    所以步 1 把它们全预标成了弃用:三张都在清单里(事件与证据都留着),
    //    但"待校准"是 0 —— 没人需要为一张纯色图花一次看图。
    let listed = client.call_tool("list_shots", json!({"project_id": "e2e"}));
    let listed: Value = serde_json::from_str(&first_text(&listed)).expect("清单应当是 JSON");
    assert_eq!(listed["counts"]["total"].as_u64().unwrap(), shots);
    let total = listed["counts"]["total"].as_u64().unwrap();
    let keep = listed["counts"]["keep"].as_u64().unwrap();
    assert_eq!(
        total - keep,
        shots,
        "纯色帧该被**预标弃用**(keep 只数留用的),而不是从清单里消失:{listed}"
    );
    assert_eq!(
        listed["counts"]["unchecked"].as_u64().unwrap(),
        0,
        "弃用的图不该还挂在'待校准'里:{listed}"
    );
    // 预标是**机械粗筛**,不是没头没脑的处置:每张都要说得出为什么、怎么翻回来
    for shot in listed["shots"].as_array().unwrap() {
        let why = shot["reason"].as_str().unwrap_or_default();
        assert!(why.contains("纯色帧"), "预标弃用要带理由:{shot}");
    }

    // ④ agent 觉得那张图有用 → 改回 keep(这条路必须通,不然"预标"就成了替它做决定)
    let first_shot_id = listed["shots"][0]["id"].as_str().unwrap().to_string();
    let kept = client.call_tool(
        "update_shot",
        json!({"project_id": "e2e", "shot_id": first_shot_id, "action": "keep"}),
    );
    assert!(!tool_failed(&kept), "改回 keep 失败:{}", first_text(&kept));

    // ⑤ **看图** —— 这是"能看图"这条前提在真实协议上的第一次受检
    let shot = client.call_tool("get_shot", json!({"project_id": "e2e", "shot_id": first_shot_id}));
    assert!(!tool_failed(&shot), "取图失败:{}", first_text(&shot));
    let blocks = shot["content"].as_array().unwrap();
    let image = blocks
        .iter()
        .find(|b| b["type"] == "image")
        .expect("必须返回图像本身,而不是路径");
    assert_eq!(image["mimeType"], "image/jpeg");
    let data = image["data"].as_str().unwrap();
    assert!(data.len() > 1000, "图像数据太短,不像是真图:{} 字节", data.len());
    // JPEG 的 base64 头是 `/9j/`,解出来应是 FFD8FF
    assert!(
        data.starts_with("/9j/"),
        "不是 JPEG 的 base64:{}",
        head(data, 16)
    );

    // ⑥ 把结论写回
    let checked = client.call_tool(
        "record_shot_check",
        json!({
            "project_id": "e2e",
            "shot_id": first_shot_id,
            "match": true,
            "audio_visual_sync": true,
            "type_read": "合成样例的画面",
            "issues": [],
            "confidence": "medium",
        }),
    );
    assert!(!tool_failed(&checked), "写回失败:{}", first_text(&checked));

    // 写完再读清单:刚校准那张带着结论、状态是 keep(另外两张是弃用的,本来就不算未校准)
    let listed = client.call_tool("list_shots", json!({"project_id": "e2e"}));
    let listed: Value = serde_json::from_str(&first_text(&listed)).unwrap();
    let one = listed["shots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == first_shot_id.as_str())
        .expect("那张图该还在清单里");
    assert!(one["check"].is_object(), "结论没落到清单上:{one}");
    assert_eq!(one["status"], "keep", "改回 keep 之后该是 keep:{one}");
    assert_eq!(listed["counts"]["unchecked"].as_u64().unwrap(), 0);

    // ⑦ 状态里应当看得到进度与材料
    let status = client.call_tool("get_project", json!({"project_id": "e2e"}));
    let status: Value = serde_json::from_str(&first_text(&status)).unwrap();
    assert!(status["progress"]["preprocess"]["done"].as_bool().unwrap());
    assert_eq!(status["materials"]["shots"]["total"].as_u64().unwrap(), shots);

    // ⑧ 刷新视图(不弹浏览器),拿到给用户点的地址
    let timeline = client.call_tool(
        "render_timeline",
        json!({"project_id": "e2e", "open_browser": false}),
    );
    assert!(!tool_failed(&timeline), "渲染失败:{}", first_text(&timeline));
    let timeline: Value = serde_json::from_str(&first_text(&timeline)).unwrap();
    let url = timeline["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "服务该只绑回环地址:{url}");

    // **查看服务跑在另一个进程里**(2026-09-26):从前它是本进程的一条线程,
    // 于是"关掉页面" = 关掉工具自己 —— 而它可能就是 agent 的会话进程(实测踩过)。
    // 现在问服务自己"你是谁"(`GET /@status`),它报的 pid 不该是会话进程的。
    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .split('/')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("URL 里该有端口");
    let status = common::http_get(port, "/@status").expect("查服务状态");
    let status: Value = serde_json::from_str(&status).expect("@status 该回 JSON");
    assert_ne!(
        status["pid"].as_u64().unwrap(),
        client.pid() as u64,
        "查看服务还跑在工具进程里(那就又变成「关页面 = 关会话」了):{status}"
    );
    assert_eq!(
        status["standalone"], true,
        "独立成进程的服务才关得掉、才不连坐:{status}"
    );

    // ⑨ 资源通道也能读到材料
    let read = client.call(
        "resources/read",
        json!({"uri": "vidbrief://project/e2e/01_preprocess/shots.json"}),
    );
    let text = read["contents"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("\"shots\""),
        "资源没读出 shots.json:{}",
        head(text, 80)
    );
}

#[test]
fn the_same_project_pointed_at_two_ways_is_still_one_project() {
    let dir = TempDir::new("identity");
    let video = dir.stage_video();
    let mut client = McpClient::start(dir.path());
    client.initialize();

    let created = client.call_tool(
        "create_project",
        json!({"video_path": video.display().to_string(), "name": "ident"}),
    );
    assert!(!tool_failed(&created), "{}", first_text(&created));

    // 第一次写:用**项目名**指项目
    let first = client.call_tool(
        "write_note",
        json!({"project_id": "ident", "path": "02_calibrate/cross_validate.md", "md": "第一次"}),
    );
    assert!(!tool_failed(&first), "{}", first_text(&first));

    // 从 list_projects 拿到**另一种写法**(它给的是绝对路径),再写一次。
    // 同一个项目不该因为写法不同就被自己挡在门外。
    let listed = client.call_tool("list_projects", json!({}));
    let listed: Value = serde_json::from_str(&first_text(&listed)).expect("应当是 JSON");
    let other_form = listed["projects"][0]["dir"]
        .as_str()
        .expect("列表里该有目录")
        .to_string();
    assert!(
        Path::new(&other_form).is_absolute(),
        "这条测试的前提是 list_projects 给的是绝对路径,实际:{other_form}"
    );

    let second = client.call_tool(
        "write_note",
        json!({
            "project_id": other_form,
            "path": "02_calibrate/cross_validate.md",
            "md": "第二次",
            "mode": "append",
        }),
    );
    assert!(
        !tool_failed(&second),
        "同一个项目换个写法就被自己挡住了:{}\n(先用的 `ident`,后用 `{other_form}`)",
        first_text(&second)
    );

    // 两次都写进去了
    let text = std::fs::read_to_string(
        dir.path().join("projects/ident/02_calibrate/cross_validate.md"),
    )
    .expect("文件该在");
    assert!(text.contains("第一次") && text.contains("第二次"), "{text}");
}

#[test]
fn a_second_session_cannot_write_while_the_first_is_writing() {
    let dir = TempDir::new("lock");
    let video = dir.stage_video();

    let mut first = McpClient::start(dir.path());
    first.initialize();
    let created = first.call_tool(
        "create_project",
        json!({"video_path": video.display().to_string(), "name": "locked"}),
    );
    assert!(!tool_failed(&created), "{}", first_text(&created));
    let report = first.call_tool("preprocess", json!({"project_id": "locked"}));
    assert!(!tool_failed(&report), "{}", first_text(&report));

    let mut second = McpClient::start(dir.path());
    second.initialize();

    // 只读:**任何**时候都不受影响
    let read_only = second.call_tool("list_shots", json!({"project_id": "locked"}));
    assert!(
        !tool_failed(&read_only),
        "另一个会话在写,只读的活不该被挡住:{}",
        first_text(&read_only)
    );

    // **正在写**的时候别人写不进去:测试进程自己把项目锁拿在手上,
    // 扮演"第一个会话正在写"(`project.lock` 是 OS 文件锁,跨进程生效)。
    let project_dir = dir.path().join("projects/locked");
    let layout = vidbrief_project::Layout::new(&project_dir);
    let held = vidbrief_project::acquire(&layout).expect("测试进程先拿住锁");

    let blocked = second.call_tool("write_summary", json!({"project_id": "locked", "md": "# x"}));
    assert!(tool_failed(&blocked), "锁在别人手上,这次写该被挡下:{blocked}");
    let text = first_text(&blocked);
    assert!(text.contains("另一个会话"), "要说清是被谁挡下的:{text}");
    assert!(text.contains("只读"), "还要说清哪些还能用:{text}");

    // 报错里**不会**出现"是哪个会话" —— `project.lock` 是空哨兵,没有任何内容可读。
    // 这不是退化,是取舍:把持有者信息写进锁文件,就会在 Windows 上被强制锁挡住
    // (恰恰在最需要它的时候读不到),绕开它得引平台 FFI 或旁路文件,
    // 换来的只是一句不改变任何人动作的话。详见 `vidbrief_project::lock` 的模块文档。
    assert!(
        !text.contains("mcp#") && !text.contains("pid"),
        "锁文件是空哨兵,报错里不该冒出持有者信息:{text}"
    );

    // **收工之后就该能写**(2026-09-26 改的口径:锁按次取放)。
    //
    // 从前是"首次取到进程结束",于是**空转的会话能把项目占死** —— 实测里那个 agent
    // 自己也无路可走。现在锁只活到一次调用结束;"两次调用之间被别人改过"
    // 由 `rev` 乐观并发兜(`OpenProject::save`,见 ops 里那条 stale 写的测试)。
    drop(held);
    let after = second.call_tool("write_summary", json!({"project_id": "locked", "md": "# x"}));
    assert!(
        !tool_failed(&after),
        "上一个会话已经收工,这个会话该能写:{}
(报「另一个会话」说明锁没按次放)",
        first_text(&after)
    );
    assert!(
        project_dir.join("03_summary/summary.md").is_file(),
        "写成功了,文件就该在"
    );
}

// ── 断线 ─────────────────────────────────────────────────────────────

/// **客户端断开时,服务要留下"谁断了"的一句明白话,而且要干脆地退出。**
///
/// 宿主那边给的通常只有「管道正在被关闭」—— 那是宿主的传输层在说话。
/// 读日志的人(和 agent)从它看不出:是服务崩了、还是客户端走了、还是刚才那次调用
/// 写到一半。所以服务这边退出前必须自己说一句(走 stderr,不污染协议通道)。
#[test]
fn a_client_disconnect_is_announced_and_the_server_leaves() {
    let dir = TempDir::new("disconnect");
    let mut client = McpClient::start(dir.path());
    client.initialize();
    // 先握过手再断:空连接断开与"干过活再断开"是两种场景,这里要的是后者
    client.call("tools/list", json!({}));

    let (code, stderr) = client.disconnect_and_wait();
    assert_eq!(code, Some(0), "客户端断开是正常收尾,不该当异常退出:{stderr}");
    assert!(
        stderr.contains("客户端断开"),
        "断开这件事要写在 stderr 里(宿主那句话看不出是谁断了):{stderr}"
    );
    assert!(
        stderr.contains("落盘"),
        "还要说清已经落盘的东西在不在:{stderr}"
    );
}

// ── 步 1 的作业:起 / 看 / 取(清账五) ────────────────────────────────

/// 起个项目,返回 `(目录, 项目目录)`。
fn project_for(tag: &str) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new(tag);
    let video = dir.stage_video();
    let mut client = McpClient::start(dir.path());
    client.initialize();
    let created = client.call_tool(
        "create_project",
        json!({"video_path": video.display().to_string(), "name": "job"}),
    );
    assert!(!tool_failed(&created), "建项目失败:{}", first_text(&created));
    let created: Value = serde_json::from_str(&first_text(&created)).unwrap();
    let root = dir.path().join(created["dir"].as_str().unwrap());
    (dir, root)
}

/// 返回文本里那一段 JSON(工具说明 + JSON 是混在一段里的)。
fn json_part(text: &str) -> Value {
    let start = text.find('{').unwrap_or_else(|| panic!("返回里该带一段 JSON:{text}"));
    serde_json::from_str(&text[start..]).unwrap_or_else(|e| panic!("那段该是 JSON:{text}\n{e}"))
}

/// **一件作业,三个入口**:起(不等它)、看进度、取结果。
///
/// 这条钉的是「宿主给每次调用 5 分钟上限」那个坑:作业跑在独立进程里,
/// 一次调用等不够(或者被宿主掐掉)都不影响它 —— 而**看着它**的入口必须在。
#[test]
fn step_one_can_be_started_watched_and_collected() {
    let (_dir, root) = project_for("job-flow");
    let mut client = McpClient::start(_dir.path());
    client.initialize();

    // ① 起了就回:返回里带着这一趟的名字 —— 后面两次调用全靠它认人
    let started = client.tool_text(
        "preprocess",
        json!({"project_id": "job", "wait_seconds": 0}),
    );
    let state = json_part(&started);
    let run_id = state["run_id"].as_str().expect("要有 run_id").to_string();
    assert!(
        matches!(state["state"].as_str(), Some("running") | Some("done")),
        "等 0 秒要么说还在跑、要么已经跑完:{state}"
    );
    assert!(
        root.join("01_preprocess/run.json").is_file(),
        "运行记录要落在项目里(它是这一趟的唯一凭据):{}",
        root.display()
    );

    // ② 盯着它走到终点(60 秒的合成素材,几秒就该完)
    let mut state = state;
    for _ in 0..240 {
        if state["state"] == "done" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        state = json_part(&client.tool_text(
            "preprocess_progress",
            json!({"project_id": "job"}),
        ));
        assert_eq!(state["run_id"], run_id, "看进度看到的必须还是同一趟:{state}");
    }
    assert_eq!(state["state"], "done", "等了两分钟还没跑完:{state}");

    // ③ 取结果:**与从前直接跑一次拿到的是同一份**
    let collected = client.tool_text("preprocess_result", json!({"project_id": "job"}));
    let report: Value = serde_json::from_str(&collected).expect("结果该是纯 JSON");
    assert!(report["done"].as_bool().unwrap(), "{collected}");
    assert!(report["shots"].as_u64().unwrap() > 0, "{collected}");
    assert_eq!(report["transcript"]["outcome"], "no_audio", "{collected}");

    // ④ 再跑一趟是**新的一趟**(跑完了就不算"搭车",那是不重跑的意思)
    let again = client.tool_text("preprocess", json!({"project_id": "job", "wait_seconds": 0}));
    let again = json_part(&again);
    assert_ne!(again["run_id"], run_id, "跑完之后再调该是重跑:{again}");
}

/// **进度通知**:只有报了 `progressToken` 的客户端才收得到,数字只增不减。
///
/// 这是协议里唯一一条「服务主动说话」的路。不给 token 的客户端(手上那个宿主就是)
/// 一条都不该收到 —— 那条路按协议是**可选**的,发了等于往没人的地方喊。
#[test]
fn progress_notifications_reach_the_client_that_asked_for_them() {
    let (_dir, _root) = project_for("job-progress");
    let mut client = McpClient::start(_dir.path());
    client.initialize();

    let (result, notes) = client.call_tool_watching_progress(
        "preprocess",
        json!({"project_id": "job", "wait_seconds": 60}),
        Some("tok-q5"),
    );
    assert!(!tool_failed(&result), "跑步 1 失败:{}", first_text(&result));
    assert!(!notes.is_empty(), "报了 token 就该收到进度");

    let mut last = 0.0;
    for note in &notes {
        assert_eq!(note["progressToken"], "tok-q5", "token 要原样回来:{note}");
        assert_eq!(note["total"].as_f64(), Some(5.0), "五步制的总数:{note}");
        let progress = note["progress"].as_f64().expect("progress 得是数字");
        assert!(progress >= last, "进度退了:{last} → {progress}({note})");
        last = progress;
        assert!(
            note["message"].as_str().unwrap_or_default().contains("步 1"),
            "每条通知都要说人话(它可能被原样显示给用户):{note}"
        );
    }
    assert_eq!(last, 5.0, "跑完了最后一条该是 5/5:{notes:?}");

    // 反面:没报 token 的调用一条都收不到
    let (result, none) = client.call_tool_watching_progress(
        "preprocess",
        json!({"project_id": "job", "wait_seconds": 60}),
        None,
    );
    assert!(!tool_failed(&result), "{}", first_text(&result));
    assert!(none.is_empty(), "没报 token 却收到了通知:{none:?}");
}

/// **会话走了,作业照跑完** —— 这是清账五在真机上踩到的那条路。
///
/// 从前是这样死的:agent 把自己的会话关了 → 作业进程往一个**没人读的管道**里写进度
/// → 写失败当场 panic → 一项跑了几分钟的活死在半路,记录上写着「没来得及收尾」。
/// 现在作业的输出去处是**运行记录**,不是管道 —— 会话的死活与它无关。
#[test]
fn the_job_outlives_the_session_that_started_it() {
    let (_dir, root) = project_for("job-survives");
    let mut client = McpClient::start(_dir.path());
    client.initialize();
    client.tool_text(
        "preprocess",
        json!({"project_id": "job", "wait_seconds": 0}),
    );

    // 会话当场消失(关掉 stdin 就是宿主眼里「客户端走了」的样子)
    let (code, _) = client.disconnect_and_wait();
    assert_eq!(code, Some(0), "客户端断开是正常收尾");

    // 已经没人看着它了,它得自己跑完
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let record: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("01_preprocess/run.json")).expect("记录该在"),
        )
        .expect("记录该是 JSON");
        if record["stage"] == "done" {
            assert!(
                record["report"]["shots"].as_u64().unwrap_or(0) > 0,
                "跑完了却没报告:{record}"
            );
            return;
        }
        assert!(
            record["error"].is_null(),
            "会话一走作业就没了:{record}"
        );
        assert!(Instant::now() < deadline, "等了两分钟还没跑完:{record}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 还没跑过步 1 时,两个「看」的入口都要如实说 —— 而**取结果**那个算失败
/// (要的东西不存在,再等也不会自己出来)。
#[test]
fn without_a_run_record_the_watchers_say_so_plainly() {
    let (_dir, _root) = project_for("job-none");
    let mut client = McpClient::start(_dir.path());
    client.initialize();

    let progress = client.tool_text("preprocess_progress", json!({"project_id": "job"}));
    assert!(
        progress.contains("还没有步 1 的运行记录"),
        "如实回答,别装作在跑:{progress}"
    );

    let result = client.call_tool("preprocess_result", json!({"project_id": "job"}));
    assert!(tool_failed(&result), "没有结果可取就该说失败:{result}");
    let text = first_text(&result);
    assert!(text.contains("还没有步 1 的运行记录"), "{text}");
    assert!(text.contains("preprocess"), "要给一条去路:{text}");
}

/// **已经有一趟在跑时不重开**:盘上有一份活着的心跳,调用就该搭那趟车。
///
/// 用一个**故意不还回去**的记录来扮那一趟(`mem::forget`:它的心跳线程会一直跳,
/// 就像真在跑一样)—— 这样"搭车"这件事是确定性的,不必赌真作业跑多久。
#[test]
fn a_live_run_is_joined_instead_of_restarted() {
    let (_dir, root) = project_for("job-live");
    let layout = vidbrief_project::Layout::new(&root);
    let planted = vidbrief_ops::RunRecorder::start_named(
        &layout,
        &vidbrief_ops::PreprocessOptions::default(),
        "另一趟在跑",
    )
    .expect("种一份活记录");
    std::mem::forget(planted);

    let mut client = McpClient::start(_dir.path());
    client.initialize();
    let state = json_part(&client.tool_text(
        "preprocess",
        json!({"project_id": "job", "wait_seconds": 0}),
    ));
    assert_eq!(state["run_id"], "另一趟在跑", "该搭那一趟,而不是重开:{state}");
    assert_eq!(state["state"], "running", "{state}");

    // 而且真没起新进程:记录没被换掉
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("01_preprocess/run.json")).unwrap())
            .unwrap();
    assert_eq!(on_disk["run_id"], "另一趟在跑", "记录被人换掉了:{on_disk}");
}
