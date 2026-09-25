//! MCP 服务的**协议层**端到端测试:真的把 `vidbrief mcp` 拉起来,真的走 stdio 上的 JSON-RPC。
//!
//! 这里管的是"线格式对不对、握手说不说得通、通道通不通"。
//! **"agent 照着自己拿到的那份说明能不能把活干成"在 `agent.rs`** ——
//! 两者失败的修法完全不同:这里坏了是协议实现有问题,那边坏了是**说明书说谎**。

mod common;

use std::path::Path;

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
    for expected in ["calibrate_shots", "find_missing_shots", "summarize_video"] {
        assert!(names.iter().any(|n| n == expected), "缺提示模板 {expected}:{names:?}");
    }
    // 还没做出来的那两个不许出现(它们引用的工具尚不存在)
    for absent in ["fix_transcript", "build_report"] {
        assert!(!names.iter().any(|n| n == absent), "{absent} 不该放出来:{names:?}");
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

    // ③ 看清单
    let listed = client.call_tool("list_shots", json!({"project_id": "e2e"}));
    let listed: Value = serde_json::from_str(&first_text(&listed)).expect("清单应当是 JSON");
    assert_eq!(listed["counts"]["total"].as_u64().unwrap(), shots);
    assert_eq!(listed["counts"]["unchecked"].as_u64().unwrap(), shots);
    let first_shot_id = listed["shots"][0]["id"].as_str().unwrap().to_string();

    // ④ **看图** —— 这是"能看图"这条前提在真实协议上的第一次受检
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

    // ⑤ 把结论写回
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

    // 写完再读清单,应该少一张未校准
    let listed = client.call_tool("list_shots", json!({"project_id": "e2e"}));
    let listed: Value = serde_json::from_str(&first_text(&listed)).unwrap();
    assert_eq!(listed["counts"]["unchecked"].as_u64().unwrap(), shots - 1);

    // ⑥ 状态里应当看得到进度与材料
    let status = client.call_tool("get_project", json!({"project_id": "e2e"}));
    let status: Value = serde_json::from_str(&first_text(&status)).unwrap();
    assert!(status["project"]["progress"]["preprocess"]["done"].as_bool().unwrap());
    assert_eq!(status["materials"]["shots"]["total"].as_u64().unwrap(), shots);

    // ⑦ 刷新视图(不弹浏览器),拿到给用户点的地址
    let timeline = client.call_tool(
        "render_timeline",
        json!({"project_id": "e2e", "open_browser": false}),
    );
    assert!(!tool_failed(&timeline), "渲染失败:{}", first_text(&timeline));
    let timeline: Value = serde_json::from_str(&first_text(&timeline)).unwrap();
    let url = timeline["url"].as_str().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "服务该只绑回环地址:{url}");

    // ⑧ 资源通道也能读到材料
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
fn a_second_session_cannot_write_while_the_first_holds_the_lock() {
    let dir = TempDir::new("lock");
    let video = dir.stage_video();

    let mut first = McpClient::start(dir.path());
    first.initialize();
    let created = first.call_tool(
        "create_project",
        json!({"video_path": video.display().to_string(), "name": "locked"}),
    );
    assert!(!tool_failed(&created), "{}", first_text(&created));

    // 第一个会话先动一次写操作 → 它从此持有这个项目的写锁
    let report = first.call_tool("preprocess", json!({"project_id": "locked"}));
    assert!(!tool_failed(&report), "{}", first_text(&report));

    let mut second = McpClient::start(dir.path());
    second.initialize();

    // 只读:**不受影响**
    let read_only = second.call_tool("list_shots", json!({"project_id": "locked"}));
    assert!(
        !tool_failed(&read_only),
        "另一个会话在写,只读的活不该被挡住:{}",
        first_text(&read_only)
    );

    // 写:必须被挡下,而且要说清为什么
    let write = second.call_tool("write_summary", json!({"project_id": "locked", "md": "# x"}));
    assert!(tool_failed(&write), "第二个会话竟然写成功了:{write}");
    let text = first_text(&write);
    assert!(text.contains("另一个会话"), "要说清是被谁挡下的:{text}");
    assert!(text.contains("只读"), "还要说清哪些还能用:{text}");

    // 这是**真跨进程**的互斥:两个 McpClient 是两个进程。
    //
    // 报错里**不会**出现"是哪个会话" —— `project.lock` 是空哨兵,没有任何内容可读。
    // 这不是退化,是取舍:把持有者信息写进锁文件,就会在 Windows 上被强制锁挡住
    // (恰恰在最需要它的时候读不到),绕开它得引平台 FFI 或旁路文件,
    // 换来的只是一句不改变任何人动作的话。详见 `vidbrief_project::lock` 的模块文档。
    assert!(
        !text.contains("mcp#") && !text.contains("pid"),
        "锁文件是空哨兵,报错里不该冒出持有者信息:{text}"
    );
}
