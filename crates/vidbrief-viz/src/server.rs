//! 本地查看服务:把项目目录当静态站点发布,让 `<video>` 能加载能 seek。
//!
//! 设计见 `docs/DESIGN.md` §9.1。**为什么非要有它**:浏览器用 `file://` 打开页面时,
//! 对本地媒体有跨域/权限限制,"拖时间轴 → 视频回跳"会失效;走 `http://127.0.0.1` 之后,
//! 视图 / 图片 / 视频同源,一切都正常了。
//!
//! # 四条设计约束
//!
//! 1. **绑 `127.0.0.1:0`** —— 端口由操作系统分配,不可能撞车。**只监听回环地址**,
//!    局域网里的别人访问不到。
//! 2. **进程内单例** —— 第一次用时启动,之后复用。多会话 = 多 MCP 进程 = 各自的端口,
//!    天然隔离(§4.7)。
//! 3. **挂载表而不是单一根目录** —— 一个进程里同时看多个项目时,按 `/{项目 id}/…` 路由。
//! 4. **支持 Range 请求** —— 这一条是"能 seek"的关键。没有 `206 Partial Content`,
//!    浏览器要么把整个视频下完才让你拖,要么干脆拖不动。
//!
//! # 安全
//!
//! 这是一个**会读磁盘并把内容发出去**的服务,所以路径必须严管:
//!
//! - URL 解码后交给 [`RelPath`] 校验 —— 它构造即拒绝 `..`、`.`、反斜杠、盘符、
//!   以点开头的段、空段(见 `docs/schema/README.md` §1.3);
//! - 再把拼出来的绝对路径 `canonicalize`,确认它**仍在挂载根之内**
//!   (防符号链接指向外面 —— `RelPath` 管不了这一层)。
//!
//! 两道关卡独立生效:第一道挡 URL 里的花样,第二道挡文件系统里的花样。

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use vidbrief_project::RelPath;

/// 常驻服务的句柄。
struct Running {
    port: u16,
    /// 挂载点(项目 id)→ 项目根目录。
    mounts: Mutex<BTreeMap<String, PathBuf>>,
}

static SERVER: OnceLock<Running> = OnceLock::new();

/// 页面上能按的按钮。
///
/// **加一个动作 = 加一个变体**,而不是让前端传任意字符串进来 ——
/// 这是个会写用户项目的入口,不许有"随便调"的余地。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 「这一步我看过了,通过」—— 记下用户对校准的确认(见 `vidbrief-ops::review`)。
    Approve,
}

impl Action {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "approve" => Some(Self::Approve),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Approve => "approve",
        }
    }
}

/// 宿主注册进来的执行者:`(项目 id, 动作) -> 给页面看的话`。
///
/// # 为什么 viz 不自己写
///
/// **写项目的只能有一处。** 这个服务有时候跑在 MCP 会话进程里(那时锁在该进程手上、
/// 状态也在),有时候跑在 `vidbrief ui` 进程里(那时压根没有会话)。让它自己写文件,
/// 就有了第二条写入路径 —— 锁、状态、报错文案全得再来一套。
///
/// 所以这里只负责**收**和**回话**:把动作交回宿主,宿主爱怎么执行怎么执行。
pub type ActionHandler =
    dyn Fn(&str, Action) -> std::result::Result<String, String> + Send + Sync;

static HANDLER: OnceLock<std::sync::Arc<ActionHandler>> = OnceLock::new();

/// 注册执行者。**进程内只认第一次** —— 重复注册会被忽略(同一个进程没必要换执行者)。
pub fn set_action_handler(handler: std::sync::Arc<ActionHandler>) {
    let _ = HANDLER.set(handler);
}

/// 有没有执行者。`vidbrief ui` 起的进程没有,所以页面上的按钮要据此说明自己为什么不能用。
pub fn has_action_handler() -> bool {
    HANDLER.get().is_some()
}

/// 确保本地查看服务在跑,返回端口号。**进程内单例。**
///
/// `root` 是首个挂载的目录(用于日志);真正的路径映射靠 [`mount`]。
pub fn ensure_running() -> Result<u16> {
    if let Some(running) = SERVER.get() {
        return Ok(running.port);
    }

    // `127.0.0.1:0` —— 只监听回环,端口交给操作系统挑
    let server = Server::http("127.0.0.1:0")
        .map_err(|e| anyhow::anyhow!("无法启动本地查看服务:{e}"))?;
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
    };

    let running = Running {
        port,
        mounts: Mutex::new(BTreeMap::new()),
    };
    if SERVER.set(running).is_err() {
        // 另一个线程先跑赢了 —— 用它那份就行
        return Ok(SERVER.get().map(|r| r.port).unwrap_or(port));
    }

    std::thread::Builder::new()
        .name("vidbrief-view-server".into())
        .spawn(move || serve_forever(server))
        .context("无法启动查看服务线程")?;

    Ok(port)
}

/// 把一个项目挂到 `/{id}/…` 下。
pub fn mount(id: &str, root: &Path) -> Result<()> {
    ensure_running()?;
    let canonical = root
        .canonicalize()
        .with_context(|| format!("挂载的目录不存在:{}", root.display()))?;
    let running = SERVER.get().context("查看服务没起来")?;
    running
        .mounts
        .lock()
        .map_err(|_| anyhow::anyhow!("挂载表被毒化了"))?
        .insert(id.to_string(), canonical);
    Ok(())
}

/// 项目内某个文件对应的 URL。
pub fn url_for(id: &str, rel: &str) -> Result<String> {
    let port = ensure_running()?;
    // rel 已由 RelPath 保证安全,这里再挡一次调用方直接塞进来的字符串
    let safe = RelPath::new(rel)?.as_str().to_string();
    Ok(format!("http://127.0.0.1:{port}/{id}/{safe}"))
}

fn serve_forever(server: Server) {
    for request in server.incoming_requests() {
        // 单个请求出错不能让服务死掉
        if let Err(e) = handle(&server, request) {
            eprintln!("查看服务:请求处理失败:{e:#}");
        }
    }
}

fn handle(_server: &Server, request: Request) -> Result<()> {
    let is_post = request.method() == &Method::Post;
    if !is_post && request.method() != &Method::Get && request.method() != &Method::Head {
        return respond_text(request, 405, "只支持 GET / HEAD / POST");
    }

    let raw_url = request.url().to_string();
    let path = raw_url.split(['?', '#']).next().unwrap_or("").to_string();
    let decoded = percent_decode(&path);

    // `/{id}/{项目内相对路径}`
    let trimmed = decoded.trim_start_matches('/');
    let Some((id, rest)) = trimmed.split_once('/') else {
        return respond_text(request, 404, "路径应当形如 /<项目 id>/<文件>");
    };

    // POST 只有一条路:页面上的按钮
    if is_post {
        return handle_action(request, id, rest);
    }

    let rest = if rest.is_empty() { "project.json" } else { rest };

    let root = {
        let running = SERVER.get().context("服务不在跑")?;
        let mounts = running.mounts.lock().map_err(|_| anyhow::anyhow!("挂载表中毒"))?;
        match mounts.get(id) {
            Some(p) => p.clone(),
            None => return respond_text(request, 404, &format!("没有挂载项目 `{id}`")),
        }
    };

    // 第一道关:URL 路径本身必须是一个合法的项目内相对路径
    let rel = match RelPath::new(rest) {
        Ok(rel) => rel,
        Err(e) => {
            return respond_text(request, 400, &format!("非法路径:{e}"));
        }
    };
    let candidate = rel.to_path(&root);

    // 第二道关:解析出来之后必须仍在挂载根之内(挡符号链接指向外面)
    let resolved = match candidate.canonicalize() {
        Ok(p) => p,
        Err(_) => return respond_text(request, 404, "文件不存在"),
    };
    if !resolved.starts_with(&root) {
        return respond_text(request, 403, "拒绝访问挂载根之外的路径");
    }
    if resolved.is_dir() {
        return respond_text(request, 404, "这是一个目录");
    }

    let meta = std::fs::metadata(&resolved)?;
    let total = meta.len();
    let mime = mime_of(&resolved);

    // Range:没有它浏览器就没法 seek
    if let Some(range_header) = header_value(&request, "range") {
        match parse_range(&range_header, total) {
            Some((start, end)) => {
                let len = end - start + 1;
                let mut file = std::fs::File::open(&resolved)?;
                file.seek(SeekFrom::Start(start))?;
                let response = Response::new(
                    StatusCode(206),
                    vec![
                        header("Content-Type", mime),
                        header("Accept-Ranges", "bytes"),
                        header("Content-Range", &format!("bytes {start}-{end}/{total}")),
                        header("Content-Length", &len.to_string()),
                    ],
                    file.take(len),
                    Some(len as usize),
                    None,
                );
                return request.respond(response).map_err(Into::into);
            }
            None => {
                // 语法认识但不可满足(如 start 越界)→ 按 RFC 回 416
                return request
                    .respond(
                        Response::empty(StatusCode(416)).with_header(
                            header("Content-Range", &format!("bytes */{total}")),
                        ),
                    )
                    .map_err(Into::into);
            }
        }
    }

    let file = std::fs::File::open(&resolved)?;
    let response = Response::new(
        StatusCode(200),
        vec![
            header("Content-Type", mime),
            header("Accept-Ranges", "bytes"),
            header("Content-Length", &total.to_string()),
            // 本地开发视图,别让浏览器缓存住旧版本
            header("Cache-Control", "no-store"),
        ],
        file,
        Some(total as usize),
        None,
    );
    request.respond(response).map_err(Into::into)
}

fn respond_text(request: Request, code: u16, text: &str) -> Result<()> {
    request
        .respond(
            Response::from_string(text)
                .with_status_code(StatusCode(code))
                .with_header(header("Content-Type", "text/plain; charset=utf-8")),
        )
        .map_err(Into::into)
}

/// `POST /<项目 id>/@action/<名字>` —— 页面上的按钮走这条。
///
/// **它自己不碰项目文件**:把动作交给宿主注册的 [`ActionHandler`]。
fn handle_action(mut request: Request, id: &str, rest: &str) -> Result<()> {
    let Some(name) = rest.strip_prefix("@action/") else {
        return respond_text(
            request,
            405,
            "POST 只用于页面动作(/<项目 id>/@action/<名字>);读文件请用 GET",
        );
    };
    let Some(action) = Action::parse(name) else {
        let known: Vec<&str> = [Action::Approve].iter().map(|a| a.name()).collect();
        return respond_json(
            request,
            404,
            &format!("不认识的页面动作 `{name}`;支持的是:{}", known.join("、")),
            false,
        );
    };

    // 请求体读掉(这个动作不需要参数)。不读干净会让连接状态错乱。
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let Some(handler) = HANDLER.get() else {
        return respond_json(
            request,
            503,
            "这个页面背后没有能执行动作的宿主(多半是 `vidbrief ui` 起的,它只读)。\
             请从 agent 会话里打开的那份页面点,或者用命令行:`vidbrief approve <项目>`",
            false,
        );
    };

    match handler(id, action) {
        Ok(message) => respond_json(request, 200, &message, true),
        Err(e) => respond_json(request, 409, &e, false),
    }
}

/// 给页面的回话。**成功失败都带 `ok`**,前端只看这一个字段。
fn respond_json(request: Request, code: u16, message: &str, ok: bool) -> Result<()> {
    let payload = serde_json::json!({ "ok": ok, "message": message }).to_string();
    request
        .respond(
            Response::from_string(payload)
                .with_status_code(StatusCode(code))
                .with_header(header("Content-Type", "application/json; charset=utf-8")),
        )
        .map_err(Into::into)
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("头部名与值都是常量或已校验的 ASCII")
}

fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

/// 解析 `bytes=start-end` / `bytes=start-` / `bytes=-suffix`。
///
/// - `Some((start, end))`:闭区间,已夹进 `[0, total-1]`
/// - `None`:语法认识但不可满足(调用方回 416)
/// - 调用方只在**有 Range 头**时调它;头本身不认识的话也该回 416,
///   所以"解析不了"与"不可满足"在这里合并成 `None`。
fn parse_range(raw: &str, total: u64) -> Option<(u64, u64)> {
    let spec = raw.trim().strip_prefix("bytes=")?;
    // 多段 Range(用逗号分隔)我们不支持,按 RFC 可以只满足第一段 —— 但简单起见当作不可满足
    if spec.contains(',') {
        return None;
    }
    let (left, right) = spec.split_once('-')?;
    if total == 0 {
        return None;
    }

    let (start, end) = if left.trim().is_empty() {
        // `-N`:最后 N 个字节
        let suffix: u64 = right.trim().parse().ok()?;
        if suffix == 0 {
            return None;
        }
        (total.saturating_sub(suffix), total - 1)
    } else {
        let start: u64 = left.trim().parse().ok()?;
        let end = if right.trim().is_empty() {
            total - 1
        } else {
            right.trim().parse::<u64>().ok()?.min(total - 1)
        };
        (start, end)
    };

    if start > end || start >= total {
        return None;
    }
    Some((start, end))
}

/// 够用的百分号解码。解不出来就原样返回(后面还会被 `RelPath` 挡)。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime_of(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("md") | Some("txt") | Some("srt") | Some("vtt") => "text/plain; charset=utf-8",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mov") => "video/quicktime",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_full_and_partial() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=0-0", 100), Some((0, 0)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        // 末尾超出要夹住,不是报错
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
    }

    #[test]
    fn range_suffix_form() {
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        // 后缀比文件还长 → 整个文件
        assert_eq!(parse_range("bytes=-500", 100), Some((0, 99)));
    }

    #[test]
    fn range_unsatisfiable_returns_none() {
        assert_eq!(parse_range("bytes=100-", 100), None, "起点越界");
        assert_eq!(parse_range("bytes=50-10", 100), None, "终点在起点之前");
        assert_eq!(parse_range("bytes=0-", 0), None, "空文件没有可满足的区间");
        assert_eq!(parse_range("bytes=-0", 100), None);
        assert_eq!(parse_range("items=0-", 100), None, "单位不对");
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None, "多段不支持");
        assert_eq!(parse_range("bytes=abc", 100), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/a%20b/c.jpg"), "/a b/c.jpg");
        assert_eq!(percent_decode("/%E4%B8%AD%E6%96%87"), "/中文");
        // 畸形的百分号序列原样留下,交给 RelPath 去拒
        assert_eq!(percent_decode("/a%zz"), "/a%zz");
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }

    #[test]
    fn traversal_attempts_are_rejected_by_relpath() {
        // 这些会被第一道关挡下,根本走不到文件系统。
        //
        // 注意 URL 的形状是 `/{项目 id}/{项目内相对路径}` —— 开头的 `/` 会被路由吃掉,
        // 所以 `/etc/passwd` 在这里意味着"项目 etc 下的 passwd",是合法路由(项目没挂载 → 404)。
        // 真正要挡的是**路由之后**那一段里出现花样。
        for evil in [
            "../secret",
            "a/../../b",
            "//etc/passwd",       // 路由后 rest 以 `/` 开头
            "C:/windows/win.ini",
            "a\\b",
            ".git/config",
            "a//b",
            "a/",
        ] {
            let decoded = percent_decode(evil);
            assert!(
                RelPath::new(decoded.as_str()).is_err(),
                "`{evil}`(解码后 `{decoded}`)不该被接受"
            );
        }

        // 而这条是合法路由,靠"项目没挂载"来挡
        let decoded = percent_decode("/etc/passwd");
        let rest = decoded.trim_start_matches('/').split_once('/').map(|(_, r)| r).unwrap();
        assert_eq!(rest, "passwd");
        assert!(RelPath::new(rest).is_ok(), "到这里已经只是普通文件名了");
    }

    #[test]
    fn normal_paths_are_accepted() {
        for good in [
            "project.json",
            "01_preprocess/timeline.html",
            "01_preprocess/shots/s001_0020.0.jpg",
            "source/video.mp4",
        ] {
            assert!(RelPath::new(good).is_ok(), "{good} 应当被接受");
        }
    }

    #[test]
    fn mime_types_cover_what_we_serve() {
        assert!(mime_of(Path::new("a.html")).starts_with("text/html"));
        assert_eq!(mime_of(Path::new("a.MP4")), "video/mp4", "扩展名大小写不敏感");
        assert_eq!(mime_of(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(mime_of(Path::new("a.unknown")), "application/octet-stream");
    }

    #[test]
    fn url_for_rejects_bad_relative_paths() {
        // 不真的起服务也能测到校验那一层 —— 它排在 ensure_running 之后,
        // 所以这里只测 RelPath 本身(服务的路径校验在 traversal 那条测试里)
        assert!(RelPath::new("../x").is_err());
    }

    /// 页面动作的名字是**线格式**:前端 URL 里写的、这里解析的,必须是同一串字。
    ///
    /// 漂了的话按钮会永远 404,而 404 的理由看着像"路径写错了" —— 很难查。
    #[test]
    fn action_names_round_trip_on_the_wire() {
        for action in [Action::Approve] {
            assert_eq!(Action::parse(action.name()), Some(action));
        }
        assert_eq!(Action::parse("delete_everything"), None, "不认识的必须被拒");
    }
}
