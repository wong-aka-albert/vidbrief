//! MCP 服务的端到端测试:真的把 `vidbrief mcp` 拉起来,真的走 stdio 上的 JSON-RPC。
//!
//! # 为什么手写客户端,不用 SDK
//!
//! 三条铁律里有一条是 **协议通用** —— 不绑定具体 agent。如果测试用的是同一个 SDK 的客户端,
//! 它只能证明"自家人跟自家说得通";而实际接入的是别人的 agent。
//! 手写的这几行 JSON 才是**别的 agent 会发的东西**,顺便也钉住了线格式:
//! 方法名怎么拼、参数放哪、结果长什么样。
//!
//! # 顺带守住的另一条规矩
//!
//! stdio 上的 **stdout 是协议通道**。这些测试逐行解析 stdout,任何一处 `println!`
//! 混进去都会让它们当场失败 —— 这比人工 review 可靠。

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

// ── 夹具 ─────────────────────────────────────────────────────────────

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

/// 用完自动删的临时目录。放在 `target/` 下与素材同卷(见 init.rs 的同类说明)。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = repo_root()
            .join("target")
            .join("vidbrief-test")
            .join(format!("mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// 素材的**副本**。硬链接会穿透,往项目里那个视频写东西等于改仓库跟踪的文件。
    fn stage_video(&self) -> PathBuf {
        let src = repo_root().join("fixtures").join("test_sample.mp4");
        let name = src.file_name().expect("素材没有文件名").to_owned();
        let dst = self.0.join(name);
        std::fs::copy(&src, &dst).expect("无法复制素材");
        dst
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── 一个够用的 MCP 客户端 ────────────────────────────────────────────

struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
}

impl McpClient {
    fn start(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_vidbrief"))
            .current_dir(cwd)
            .args(["mcp", "--stdio"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("无法启动 vidbrief mcp");

        let stdin = child.stdin.take().expect("没有 stdin");
        let stdout = child.stdout.take().expect("没有 stdout");
        let stderr_pipe = child.stderr.take().expect("没有 stderr");

        // stdout 逐行读出来送进 channel —— 主线程要能带超时地等,不能死等在 read_line 上
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // stderr 只收集不打屏:断言失败时把它贴进消息里,平时别污染测试输出
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = stderr.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
                let mut text = sink.lock().unwrap_or_else(|e| e.into_inner());
                text.push_str(&line);
                text.push('\n');
            }
        });

        Self {
            child,
            stdin: Some(stdin),
            lines,
            stderr,
            next_id: 0,
        }
    }

    fn stderr(&self) -> String {
        self.stderr.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn write_message(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin 已经关了");
        let mut line = serde_json::to_string(message).expect("序列化失败");
        line.push('\n');
        stdin.write_all(line.as_bytes()).expect("写 stdin 失败");
        stdin.flush().expect("flush 失败");
    }

    /// 等一行输出。**必须带超时** —— 没有超时的测试在服务卡死时会一直挂着。
    fn next_line(&mut self) -> String {
        match self.lines.recv_timeout(Duration::from_secs(120)) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!(
                "等服务输出超时了。stderr:\n{}\n进程状态:{:?}",
                self.stderr(),
                self.child.try_wait()
            ),
            Err(RecvTimeoutError::Disconnected) => panic!(
                "服务把 stdout 关了(可能已经退出)。stderr:\n{}\n退出码:{:?}",
                self.stderr(),
                self.child.try_wait()
            ),
        }
    }

    /// 发一个请求并等**对应 id** 的响应。中间的通知会被跳过。
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.write_message(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));

        loop {
            let line = self.next_line();
            let value: Value = serde_json::from_str(&line).unwrap_or_else(|e| {
                panic!(
                    "stdout 上出现了不是 JSON 的东西(`{line}`)—— stdio 的 stdout 是协议通道,\
                     任何 println! 都会毁掉会话:{e}"
                )
            });
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
            // 别人的响应/通知:丢掉继续等(真实客户端也会这么干)
        }
    }

    /// 发请求并**要求成功**,直接返回 `result`。
    fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        if let Some(error) = response.get("error") {
            panic!("{method} 报错:{error}\nstderr:\n{}", self.stderr());
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// 调一个工具,返回整个 `result`(含 `isError`)。
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call("tools/call", json!({"name": name, "arguments": arguments}))
    }

    /// 走完握手。
    fn initialize(&mut self) -> Value {
        let result = self.call(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "vidbrief-e2e", "version": "0"},
            }),
        );
        // 握手之后按规矩补一条 initialized 通知
        self.write_message(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        result
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        // 关掉 stdin,服务应当自己退出 —— 顺便验证了"客户端断开时服务不赖着不走"
        self.stdin = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 工具结果里的第一段文本。
fn first_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .and_then(|blocks| {
            blocks.iter().find_map(|b| {
                (b["type"] == "text")
                    .then(|| b["text"].as_str().map(str::to_string))
                    .flatten()
            })
        })
        .unwrap_or_default()
}

fn tool_failed(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

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
    let first_shot_id = listed["shots"]["shots"][0]["id"].as_str().unwrap().to_string();

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

/// 取开头若干**字符**(不是字节)用于报错消息。
///
/// 不用 `&s[..n]`:中文一个字 3 字节,按字节切会 panic(`AGENTS.md` §1.1)。
fn head(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
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
}
