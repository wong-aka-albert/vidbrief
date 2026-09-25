//! 测试之间共用的东西:临时目录 + 一个手写的 MCP 客户端。
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

#![allow(dead_code)] // 每个测试文件只用得上这里的一部分

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

/// 用完自动删的临时目录。放在 `target/` 下与素材同卷(见 init.rs 的同类说明)。
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let dir = repo_root()
            .join("target")
            .join("vidbrief-test")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// 素材的**副本**。硬链接会穿透,往项目里那个视频写东西等于改仓库跟踪的文件。
    pub fn stage_video(&self) -> PathBuf {
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

pub struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
}

impl McpClient {
    pub fn start(cwd: &Path) -> Self {
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

    pub fn stderr(&self) -> String {
        self.stderr.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn write_message(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin 已经关了");
        let mut line = serde_json::to_string(message).expect("序列化失败");
        line.push('\n');
        stdin.write_all(line.as_bytes()).expect("写 stdin 失败");
        stdin.flush().expect("flush 失败");
    }

    /// 等一行输出。**必须带超时** —— 没有超时的测试在服务卡死时会一直挂着。
    pub fn next_line(&mut self) -> String {
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
    pub fn request(&mut self, method: &str, params: Value) -> Value {
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
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        if let Some(error) = response.get("error") {
            panic!("{method} 报错:{error}\nstderr:\n{}", self.stderr());
        }
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// 调一个工具,返回整个 `result`(含 `isError`)。
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        )
    }

    /// 调一个工具,**要求它成功**并返回第一段文本。失败时把服务的说明原样报出来。
    pub fn tool_text(&mut self, name: &str, arguments: Value) -> String {
        let result = self.call_tool(name, arguments);
        let text = first_text(&result);
        assert!(!tool_failed(&result), "{name} 失败了:{text}");
        text
    }

    /// 走完握手。
    pub fn initialize(&mut self) -> Value {
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
pub fn first_text(result: &Value) -> String {
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

pub fn tool_failed(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

/// 取开头若干**字符**(不是字节)用于报错消息。
///
/// 不用 `&s[..n]`:中文一个字 3 字节,按字节切会 panic(`AGENTS.md` §1.1)。
pub fn head(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}
