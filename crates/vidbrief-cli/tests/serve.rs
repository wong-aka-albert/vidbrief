//! **查看服务是独立进程**:起得来、挂得上、关得掉(而且是走正门关的)。
//!
//! 这条测试盯的是"挪出工具进程"这件事本身:从前服务跑在工具进程的一条线程里,
//! 于是"关掉页面" = 杀掉工具自己 —— 实测里那一刀等于把 agent 会话一起关了。
//!
//! 这里用手写的 HTTP(不引客户端依赖,与 `vidbrief-viz` 的测试同一个路子):
//! 服务本身就是个本地 HTTP 服务,客户端越笨越能说明问题。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_vidbrief")
}

/// 一个"像项目"的目录:登记表与 URL 都只需要文件在那儿。
fn stage(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vidbrief-serve-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("01_preprocess")).unwrap();
    std::fs::write(dir.join("project.json"), br#"{"name":"serve-test"}"#).unwrap();
    std::fs::write(dir.join("01_preprocess/timeline.html"), "<html>时间线</html>").unwrap();
    dir
}

/// 每个用例**自己一份登记目录**:并行跑的用例不该互相看见对方的服务
/// (`--stop` 是"按目录找"的,串了会误伤)。
fn own_registry(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vidbrief-view-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 起一份服务,把 `(子进程, 端口)` 交回来。**stdout 上那一行握手就是协议**。
fn start_serve(registry: &Path) -> (Child, u16) {
    let mut child = Command::new(exe())
        .args(["serve", "--port", "0"])
        .env("VIDBRIEF_VIEW_REGISTRY", registry)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // stderr 不给管道:没人读的管道写满就**把服务堵住**(那正是"连不上"的样子)
        .stderr(Stdio::null())
        .spawn()
        .expect("起 vidbrief serve");
    let stdout = child.stdout.take().expect("stdout");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("读握手行");
    let value: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("握手行不是 JSON(`{line}`):{e}"));
    let port = value["port"].as_u64().expect("有 port") as u16;
    assert_eq!(
        value["pid"].as_u64().expect("有 pid") as u32,
        child.id(),
        "握手里的 pid 该就是那个子进程:{value}"
    );
    (child, port)
}

struct Reply {
    status: u16,
    body: String,
}

/// 手写一个 HTTP/1.0 请求(与 viz 的测试同一个路子)。
fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连不上服务");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let payload = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(request.as_bytes()).expect("写请求");
    let mut text = String::new();
    stream.read_to_string(&mut text).expect("读响应");
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Reply {
        status,
        body: body.to_string(),
    }
}

/// 等子进程退出,最多等这么久。
fn wait_exit(child: &mut Child, limit: Duration) -> Option<i32> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    None
}

/// 服务自己一个进程:挂载走控制面、文件发得出来、**正门能把它关掉**。
#[test]
fn a_serve_process_can_be_mounted_and_closed_through_the_front_door() {
    let dir = stage("front-door");
    let registry = own_registry("front-door");
    let (mut child, port) = start_serve(&registry);

    // 起手没挂任何项目
    let status: serde_json::Value = serde_json::from_str(&http(port, "GET", "/@status", None).body)
        .expect("@status 该回 JSON");
    assert!(status["mounts"].as_array().unwrap().is_empty(), "{status}");
    assert_eq!(status["pid"].as_u64().unwrap() as u32, child.id());

    // 动态挂一个项目(这就是 MCP 会话走的那个控制面)
    let reply = http(
        port,
        "POST",
        "/@mount",
        Some(&format!(
            r#"{{"id":"demo","root":"{}"}}"#,
            dir.display().to_string().replace('\\', "\\\\")
        )),
    );
    assert_eq!(reply.status, 200, "{}", reply.body);

    // 挂上就能取文件
    let page = http(port, "GET", "/demo/01_preprocess/timeline.html", None);
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("时间线"), "{}", page.body);

    // **正门**:一次 `@shutdown`,进程退出、登记表也收干净
    let entry = registry.join(format!("{}.json", child.id()));
    assert!(entry.is_file(), "在跑的服务该登记自己:{}", entry.display());
    let bye = http(port, "POST", "/@shutdown", Some("{}"));
    assert_eq!(bye.status, 200, "{}", bye.body);
    assert_eq!(
        wait_exit(&mut child, Duration::from_secs(10)),
        Some(0),
        "关掉是正常收尾,不该是异常退出"
    );
    assert!(
        !entry.is_file(),
        "退出时该把自己的登记表收掉:{}",
        entry.display()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `serve --stop <项目>` 能找到在跑的服务并把它关掉 —— 那是"关掉"的**命令行正门**。
#[test]
fn serve_stop_closes_the_server_that_holds_that_project() {
    let dir = stage("stop");
    let other = stage("stop-other");
    let registry = own_registry("stop");
    let (mut mine, mine_port) = start_serve(&registry);
    let (mut theirs, theirs_port) = start_serve(&registry);

    // 两边各挂各的项目
    let mount = |port: u16, id: &str, root: &Path| {
        let reply = http(
            port,
            "POST",
            "/@mount",
            Some(&format!(
                r#"{{"id":"{id}","root":"{}"}}"#,
                root.display().to_string().replace('\\', "\\\\")
            )),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    mount(mine_port, "mine", &dir);
    mount(theirs_port, "theirs", &other);

    // 只关挂着 `dir` 的那一份
    let out = Command::new(exe())
        .args(["serve", "--stop", &dir.display().to_string()])
        .env("VIDBRIEF_VIEW_REGISTRY", &registry)
        .output()
        .expect("跑 serve --stop");
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{text}");
    assert!(text.contains("已关掉"), "--stop 该说清关了谁:{text}");

    assert_eq!(
        wait_exit(&mut mine, Duration::from_secs(10)),
        Some(0),
        "被点名的服务该收工:{text}"
    );
    assert!(
        wait_exit(&mut theirs, Duration::from_millis(300)).is_none(),
        "没点名的服务不该被牵连:{text}"
    );

    // 收尾:把另一份也关掉,别留孤儿
    let _ = http(theirs_port, "POST", "/@shutdown", Some("{}"));
    let _ = wait_exit(&mut theirs, Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&other);
}
