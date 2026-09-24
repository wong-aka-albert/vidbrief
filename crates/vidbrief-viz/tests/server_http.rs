//! 本地查看服务的端到端测试:**真的起服务、真的发 HTTP 请求**。
//!
//! 这一层最不能只靠单元测试:`Range` 解析对不对、路径穿越挡不挡得住、
//! `<video>` 能不能 seek —— 全都得看**真的走一遍 TCP** 的结果。
//!
//! 用的是标准库的 `TcpStream` 手写 HTTP 请求,不引 HTTP 客户端依赖。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

/// 造一个挂载目录,里面放几个大小可辨的文件。
fn stage(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join("target")
        .join("vidbrief-test")
        .join(format!("viz-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("01_preprocess/shots")).unwrap();
    std::fs::write(dir.join("project.json"), br#"{"ok":true}"#).unwrap();
    // 1000 字节,内容 = 字节值 % 251,便于逐字节校验 Range
    let payload: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("source.mp4"), &payload).unwrap();
    std::fs::write(dir.join("01_preprocess/timeline.html"), "<html>hi</html>").unwrap();
    std::fs::write(dir.join("01_preprocess/shots/a.jpg"), b"jpeg").unwrap();
    // 挂载根之外的一个文件,用来验证第二道关卡。
    // 内容用一个**不会出现在任何错误信息里**的标记 —— 否则"回显了路径"会被误判成"泄露了内容"。
    std::fs::write(
        dir.parent().unwrap().join("outside-secret.txt"),
        b"LEAKED-BYTE-MARKER-9f3c2a",
    )
    .unwrap();
    dir
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// 手写一个 HTTP/1.0 请求。用 1.0 是为了让响应天然带上 `Content-Length` 并直接关连接,
/// 读起来简单。
fn http(port: u16, method: &str, path: &str, range: Option<&str>) -> Response {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连不上本地查看服务");
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n");
    if let Some(r) = range {
        req.push_str(&format!("Range: {r}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("状态行看不懂:{status_line:?}"));

    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    Response { status, headers, body }
}

#[test]
fn serves_files_and_honours_range_requests() {
    let dir = stage("range");
    let id = format!("range-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();

    // ① 完整 GET
    let full = http(port, "GET", &format!("/{id}/source.mp4"), None);
    assert_eq!(full.status, 200, "完整 GET 应当成功");
    assert_eq!(full.body.len(), 1000);
    assert_eq!(full.header("Content-Type"), Some("video/mp4"));
    assert_eq!(
        full.header("Accept-Ranges"),
        Some("bytes"),
        "必须声明支持 Range,否则浏览器不会来 seek"
    );

    // ② 开区间 bytes=100-
    let tail = http(port, "GET", &format!("/{id}/source.mp4"), Some("bytes=100-"));
    assert_eq!(tail.status, 206, "Range 请求必须回 206");
    assert_eq!(tail.body.len(), 900);
    assert_eq!(tail.header("Content-Range"), Some("bytes 100-999/1000"));
    assert_eq!(tail.body[0], 100u8 % 251, "起点字节要对得上");

    // ③ 闭区间,而且越界要被夹住
    let mid = http(port, "GET", &format!("/{id}/source.mp4"), Some("bytes=10-19"));
    assert_eq!(mid.status, 206);
    assert_eq!(mid.body, (10..20u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());

    let clamped = http(port, "GET", &format!("/{id}/source.mp4"), Some("bytes=990-99999"));
    assert_eq!(clamped.status, 206);
    assert_eq!(clamped.header("Content-Range"), Some("bytes 990-999/1000"));

    // ④ 后缀形式(播放器算最后几秒时会用)
    let suffix = http(port, "GET", &format!("/{id}/source.mp4"), Some("bytes=-50"));
    assert_eq!(suffix.status, 206);
    assert_eq!(suffix.header("Content-Range"), Some("bytes 950-999/1000"));

    // ⑤ 不可满足 → 416(而不是把整个文件发出去)
    let bad = http(port, "GET", &format!("/{id}/source.mp4"), Some("bytes=5000-"));
    assert_eq!(bad.status, 416);
    assert_eq!(bad.header("Content-Range"), Some("bytes */1000"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn content_types_are_right_for_everything_the_view_needs() {
    let dir = stage("mime");
    let id = format!("mime-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();

    for (path, want) in [
        ("/project.json", "application/json; charset=utf-8"),
        ("/01_preprocess/timeline.html", "text/html; charset=utf-8"),
        ("/01_preprocess/shots/a.jpg", "image/jpeg"),
        ("/source.mp4", "video/mp4"),
    ] {
        let r = http(port, "GET", &format!("/{id}{path}"), None);
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.header("Content-Type"), Some(want), "{path}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directory_index_defaults_to_project_json() {
    let dir = stage("index");
    let id = format!("index-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();

    let r = http(port, "GET", &format!("/{id}/"), None);
    assert_eq!(r.status, 200);
    assert_eq!(r.body, br#"{"ok":true}"#);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_traversal_is_blocked_at_both_gates() {
    let dir = stage("traversal");
    let id = format!("trav-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();

    // 第一道关:URL 路径本身非法 → 400/404,总之不能把外面的文件发出来
    for evil in [
        "/../outside-secret.txt",
        "/%2e%2e/outside-secret.txt",
        "/a/../../outside-secret.txt",
        "/C:/windows/win.ini",
        "/..\\outside-secret.txt",
    ] {
        let r = http(port, "GET", &format!("/{id}{evil}"), None);
        assert!(
            r.status >= 400,
            "`{evil}` 应当被拒,实际 {} 且回了 {} 字节",
            r.status,
            r.body.len()
        );
        // 找**文件内容**的标记,而不是找路径片段 —— 400 的错误信息里会回显路径,
        // 用 "secret" 这种词判断会把"回显"误判成"泄露"(第一版就是这么写错的)。
        assert!(
            !r.body
                .windows(b"LEAKED-BYTE-MARKER-9f3c2a".len())
                .any(|w| w == b"LEAKED-BYTE-MARKER-9f3c2a"),
            "`{evil}` 把挂载根之外的内容发出来了!"
        );
    }

    // 没挂载的项目 id
    let unknown = http(port, "GET", "/nope-xyz/project.json", None);
    assert_eq!(unknown.status, 404);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(dir.parent().unwrap().join("outside-secret.txt"));
}

#[test]
fn unknown_paths_are_404_and_directories_are_not_listed() {
    let dir = stage("404");
    let id = format!("nf-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();

    assert_eq!(http(port, "GET", &format!("/{id}/nope.txt"), None).status, 404);
    // 目录不列出内容(没有索引页)
    assert_eq!(http(port, "GET", &format!("/{id}/01_preprocess"), None).status, 404);
    // 只允许 GET
    assert_eq!(http(port, "POST", &format!("/{id}/project.json"), None).status, 405);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_server_is_a_process_wide_singleton() {
    // 多会话 = 多进程 = 各自端口;同进程内重复调用必须复用同一个服务
    let a = vidbrief_viz::ensure_running().unwrap();
    let b = vidbrief_viz::ensure_running().unwrap();
    assert_eq!(a, b, "进程内应当是单例");
    assert!(a > 0);
}

#[test]
fn url_for_builds_a_loopback_url() {
    let dir = stage("url");
    let id = format!("url-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();

    let url = vidbrief_viz::url_for(&id, "01_preprocess/timeline.html").unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    assert!(url.ends_with(&format!("/{id}/01_preprocess/timeline.html")), "{url}");

    // 非法相对路径要被拒
    assert!(vidbrief_viz::url_for(&id, "../x").is_err());

    let _ = std::fs::remove_dir_all(&dir);
}
