//! `vidbrief ui` 与工作台视图的端到端测试。
//!
//! 最要紧的一条是 [`the_workbench_actually_plays_the_video`] ——
//! 它把整个链路串起来验:**渲染出的 HTML 引用的视频 URL,通过本地服务真的取得到**,
//! 而且是支持 Range 的。这两件事缺一个,"拖共轴→视频回跳"就是空谈。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use vidbrief_project::Layout;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = repo_root()
            .join("target")
            .join("vidbrief-test")
            .join(format!("ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("无法创建临时目录");
        Self(dir)
    }

    /// 素材副本。别硬链仓库里的 fixtures —— 硬链接会穿透。
    fn stage_video(&self) -> PathBuf {
        let name = repo_root()
            .join("fixtures")
            .join("test_sample.mp4")
            .file_name()
            .expect("素材没有文件名")
            .to_owned();
        let dst = self.0.join(name);
        std::fs::copy(repo_root().join("fixtures").join("test_sample.mp4"), &dst).unwrap();
        dst
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("无法启动 vidbrief")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// 建项目 + 跑步 1(不带转录)。
fn prepared(tag: &str) -> (TempDir, Layout) {
    let tmp = TempDir::new(tag);
    let video = tmp.stage_video();
    assert!(
        run_in(&tmp.0, &["init", video.to_str().unwrap()]).status.success(),
        "init 失败"
    );
    let out = run_in(&tmp.0, &["preprocess", "test-sample", "--no-transcribe"]);
    assert!(out.status.success(), "preprocess 失败:\n{}", stderr(&out));
    let layout = Layout::for_project(tmp.0.join("projects"), "test-sample");
    (tmp, layout)
}

#[test]
fn preprocess_renders_the_workbench_by_itself() {
    // DESIGN §5.3:步 1 完成即产出可视化
    let (_tmp, layout) = prepared("auto-render");
    let html = layout.workbench();
    assert!(html.is_file(), "preprocess 应当自动渲染出工作台:{}", html.display());
    let text = std::fs::read_to_string(&html).unwrap();
    assert!(text.contains("<video"), "渲染出来得是个能播视频的页面");
    assert!(!text.contains("/*__VIDBRIEF_DATA__*/null"), "数据占位符没被替换");
    // 壳出厂就带 `hidden` 的话,页面是**全白**的 —— DOM / 数据 / CSS 一个不少,就是什么都看不见。
    // (2026-09-27 实测:删掉换壳那行 JS 之后忘了摘属性,截图一片白,而 DOM 断言全绿。)
    assert!(
        !text.contains("id=\"app\" hidden"),
        "工作台壳出厂就是 hidden —— 页面会全白"
    );
    // 共用片段(报告正文的渲染)也得注进去 —— 少了它页面一开就白
    assert!(!text.contains("/*__SHARED_JS__*/"), "共用片段占位符没被替换");
    assert!(text.contains("function topicNodes("), "共用片段没进工作台");
}

/// 工作台里那个「单独打开图文报告页」得**指对地方**。
///
/// 这种链接最容易错在"往上走几级":`../04_report/report.html` 与
/// `../../04_report/report.html` 只差一个字符,而后者是个 404 ——
/// 页面上看不出来,非点一次不可。
#[test]
fn the_workbench_links_to_the_report_page_next_door() {
    let (_tmp, layout) = prepared("link");
    let html = std::fs::read_to_string(layout.workbench()).unwrap();

    let href = html
        .split_once("href=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(href, _)| href.to_string())
        .expect("工作台里应当有一个指向报告页的链接");
    assert_eq!(
        resolve("01_preprocess", &href).unwrap(),
        vidbrief_project::layout::REPORT_HTML_FILE,
        "链接没指到报告页:{href}"
    );
}

/// 把相对 URL 按所在目录解成项目根相对路径;`..` 冒出去就是写错了。
fn resolve(base_dir: &str, href: &str) -> Result<String, String> {
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in href.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(format!("从 {base_dir} 出发走不到 {href}"));
                }
            }
            s => parts.push(s),
        }
    }
    Ok(parts.join("/"))
}

#[test]
fn the_rendered_page_carries_the_real_material() {
    let (_tmp, layout) = prepared("data");
    let html = std::fs::read_to_string(layout.workbench()).unwrap();

    let json = html
        .split_once(r#"<script id="vidbrief-data" type="application/json">"#)
        .expect("数据块不见了")
        .1
        .split_once("</script>")
        .expect("数据块没闭合")
        .0;
    let data: serde_json::Value = serde_json::from_str(json).expect("数据块不是合法 JSON");

    assert_eq!(data["project"]["id"], "test-sample");
    assert!(data["project"]["duration"].as_f64().unwrap() > 59.0);
    // 三个切换点都在数据里
    let times: Vec<f64> = data["shots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["t"].as_f64().unwrap())
        .collect();
    for want in [20.0, 30.0, 50.0] {
        assert!(
            times.iter().any(|t| (t - want).abs() < 1.0),
            "截图数据里缺 t={want}:{times:?}"
        );
    }
    // 曲线点数与时长对得上
    assert_eq!(
        data["curve"]["scores"].as_array().unwrap().len(),
        60,
        "60 秒 1fps 应当是 60 个点"
    );
    // 视频 URL 必须是相对的 —— 这样 HTML 打包带走也还能看
    let url = data["video_url"].as_str().unwrap();
    assert!(url.starts_with("../"), "应当是相对路径:{url}");
    assert!(url.ends_with("video.mp4"), "{url}");
    // 截图 URL 同理
    let shot_url = data["shots"][0]["url"].as_str().unwrap();
    assert!(shot_url.starts_with("../01_preprocess/shots/"), "{shot_url}");
    // 步 1 刚跑完,报告那栏还空着 —— 工作台从步 2 起就能用,别让它等着报告
    assert!(data["report"].is_null(), "还没跑步 4,报告那栏该是空的:{}", data["report"]);
}

#[test]
fn the_workbench_actually_plays_the_video() {
    // 这一条是整个 M2 的验收:**渲染好的页面 + 本地服务 = 视频能加载能 seek**。
    let (tmp, layout) = prepared("serve");
    let _ = (&tmp, &layout);

    let mut child: Child = Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .current_dir(&tmp.0)
        .args(["ui", "test-sample", "--no-open"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("起不了 ui");

    // 从它打印的 URL 里拿到端口(不猜端口号)
    let out = child.stdout.take().unwrap();
    let mut reader = BufReader::new(out);
    let mut url = None;
    for _ in 0..20 {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            break;
        }
        if let Some(rest) = line.trim().strip_prefix("服务") {
            url = Some(rest.trim().to_string());
            break;
        }
    }
    let url = url.unwrap_or_else(|| {
        let _ = child.kill();
        panic!("ui 没有打印服务 URL");
    });

    let port: u16 = url
        .trim_start_matches("http://127.0.0.1:")
        .split('/')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("URL 解析不出端口:{url}"));
    let path = url.trim_start_matches(&format!("http://127.0.0.1:{port}"));

    // ① 页面取得到
    let page = get(port, path, None);
    assert_eq!(page.0, 200, "工作台页面取不到");
    assert!(String::from_utf8_lossy(&page.1).contains("<video"));

    // ② 视频取得到,而且声明支持 Range
    let video = get(port, "/test-sample/source/video.mp4", None);
    assert_eq!(video.0, 200, "视频取不到 —— 视频回跳就是空谈");
    assert_eq!(video.2.as_deref(), Some("bytes"), "必须声明 Accept-Ranges");
    assert_eq!(video.1.len(), 1_953_106, "发出来的字节数应当等于文件大小");

    // ③ Range 请求回 206 且字节对得上
    let part = get(port, "/test-sample/source/video.mp4", Some("bytes=100-199"));
    assert_eq!(part.0, 206, "Range 请求应当回 206");
    assert_eq!(part.1.len(), 100);
    assert_eq!(part.1[..], video.1[100..200], "Range 的内容必须与完整文件一致");

    // ④ 截图取得到
    let shot = get(port, "/test-sample/01_preprocess/shots/s001_0020.0.jpg", None);
    assert_eq!(shot.0, 200);
    assert!(shot.1.starts_with(&[0xFF, 0xD8]), "应当是一张 JPEG");

    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn ui_help_and_bad_arguments() {
    let cwd = std::env::temp_dir();
    let out = run_in(&cwd, &["ui", "-h"]);
    assert!(out.status.success());
    let text = stdout(&out);
    for expected in ["--no-open", "127.0.0.1"] {
        assert!(text.contains(expected), "用法里缺 `{expected}`:\n{text}");
    }

    let out = run_in(&cwd, &["ui"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("用法"), "{}", stderr(&out));

    let out = run_in(&cwd, &["ui", "a", "b"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("只接受一个项目"), "{}", stderr(&out));

    let out = run_in(&cwd, &["ui", "nope"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("找不到项目"), "{}", stderr(&out));
}

/// 手写一个 HTTP/1.0 GET,返回 `(状态码, 正文, Accept-Ranges)`。
fn get(port: u16, path: &str, range: Option<&str>) -> (u16, Vec<u8>, Option<String>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("连不上查看服务");
    let mut req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n");
    if let Some(r) = range {
        req.push_str(&format!("Range: {r}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap();

    let mut accept_ranges = None;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("accept-ranges")
        {
            accept_ranges = Some(v.trim().to_string());
        }
    }
    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    (status, body, accept_ranges)
}
