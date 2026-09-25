//! 一个**从没注册过执行者**的进程里,页面按钮会怎样。
//!
//! 单独一个文件是因为:执行者是进程内单例(`set_action_handler` 只认第一次),
//! 而集成测试**各自一个进程** —— 这是唯一能拿到干净进程的办法。
//! 现实中对应的正是 `vidbrief ui`:它只读,背后没有 MCP 会话。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

#[test]
fn without_a_host_the_button_says_so_instead_of_failing_silently() {
    let dir = repo_root()
        .join("target")
        .join("vidbrief-test")
        .join(format!("viz-nohost-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let id = format!("nohost-{}", std::process::id());
    vidbrief_viz::mount(&id, &dir).unwrap();
    let port = vidbrief_viz::ensure_running().unwrap();
    assert!(
        !vidbrief_viz::has_action_handler(),
        "这个测试的前提就是这个进程没注册过执行者"
    );

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .write_all(
            format!("POST /{id}/@action/approve HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").as_bytes(),
        )
        .unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    let mut rest = String::new();
    reader.read_to_string(&mut rest).unwrap();

    assert!(
        status_line.contains("503"),
        "没有宿主时该回 503(暂时没法干),不是 404(没有这东西):{status_line}"
    );
    assert!(rest.contains("\"ok\":false"), "回话该带 ok 字段:{rest}");
    // 关键:**要给出路**。用户看到的是浏览器里这一行字,他不知道还能怎么办。
    assert!(
        rest.contains("vidbrief approve"),
        "该告诉用户还能怎么走(命令行那条路):{rest}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
