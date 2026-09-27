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
//! 2. **单例** —— 第一次用时启动,之后复用。两种跑法(2026-09-26 起):
//!    - **自己一个进程**(`vidbrief serve`,MCP 会话用这条):关页面 = 关那个进程,
//!      **不连坐**到 agent 会话 —— 从前服务跑在工具进程里,关它等于把会话一起关了;
//!    - **进程内线程**(`vidbrief ui` 那条前台命令):它本来就是"这个进程活着服务才在"。
//!    多会话 = 多进程 = 各自的端口,天然隔离(§4.7)。
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
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, bail};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use vidbrief_project::RelPath;

/// 服务跑在哪 —— 决定"挂载"与"关掉"是本地动作还是控制面请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// 服务就在**这个进程**里(前台 `vidbrief ui` 的线程,或 `vidbrief serve` 本尊)。
    /// `standalone` = 这个进程只为服务而活 → `@shutdown` 可以真的退出它。
    Local { standalone: bool },
    /// 服务在**另一个进程**里(我们 spawn 的 `vidbrief serve`)。
    /// 挂载与关掉都要走 HTTP 控制面 —— 挂载表在对面。
    Remote,
}

/// 常驻服务的句柄。
struct Running {
    /// 服务端口。**可改**:远端那份服务可能被杀掉(实测:2026-09-27 误关过一次),
    /// 那时要换一个新端口并把它记回来 —— 只读的 `u16` 会让这个会话此后所有视图全废。
    port: std::sync::atomic::AtomicU16,
    /// 挂载点(项目 id)→ 项目根目录。`Remote` 时这张表**不用管**(在对面)。
    mounts: Mutex<BTreeMap<String, PathBuf>>,
    placement: Placement,
}

static SERVER: OnceLock<Running> = OnceLock::new();

/// 那个端口上还有人在听吗。**一条 TCP 连接就够**(服务只绑回环,握手都不必发)。
fn alive(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400)).is_ok()
}

/// 要不要把服务**放到另一个进程**里跑。
///
/// MCP 会话在启动时调 [`use_separate_process`]:那之后第一次要视图时,会
/// spawn 一个 `vidbrief serve` 子进程并记住它的端口 —— 页面上的"关掉"从此只关它自己。
/// 默认(CLI 前台命令)仍然是进程内线程:那个进程本来就只为服务活着。
static SEPARATE_PROCESS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 声明"本进程要服务跑在别处"(MCP 会话在 `serve_stdio` 里调一次)。
pub fn use_separate_process() {
    SEPARATE_PROCESS.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 注册表目录:`<临时目录>/vidbrief-view/` —— 每个在跑的服务一个 `<pid>.json`。
///
/// 它是"关掉"的**发现路径**:服务自己写、退出时自己收;`vidbrief serve --stop`
/// 靠它找到"现在有哪些服务在跑、各挂着哪些项目"。**不放进项目目录** ——
/// 项目里只该有材料,运行时的东西别混进去(它还会随进程生灭)。
pub fn registry_dir() -> PathBuf {
    // 环境变量优先:测试(与将来的 CI)要**各自一份**,不然几个并行跑的用例
    // 会互相看见对方的服务 —— 而 `--stop` 是"按目录/项目找"的,串了会误伤。
    match std::env::var_os("VIDBRIEF_VIEW_REGISTRY") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => std::env::temp_dir().join("vidbrief-view"),
    }
}

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
        let port = running.port.load(std::sync::atomic::Ordering::SeqCst);
        // **远端那份先探活**:它可能已经不在(被关掉、崩了、机器重启过)。
        // 从前这里直接把它交出去,于是"服务死了"这件事会**一直**留在会话内存里 ——
        // 实测(2026-09-27):会话挂的查看服务被误关之后,`render_workbench` 每次都报
        // 「连不上查看服务(端口 11834)」,直到重启那个会话为止。**它会自愈**才对。
        if running.placement == Placement::Remote && !alive(port) {
            let fresh = spawn_server_process(standalone_serve())?;
            running
                .port
                .store(fresh, std::sync::atomic::Ordering::SeqCst);
            return Ok(fresh);
        }
        return Ok(port);
    }

    // 要独立进程:spawn `vidbrief serve` 并把端口拿回来(见 `spawn_server_process`)
    if SEPARATE_PROCESS.load(std::sync::atomic::Ordering::SeqCst) {
        return spawn_server_process(standalone_serve());
    }

    // `127.0.0.1:0` —— 只监听回环,端口交给操作系统挑
    let server = Server::http("127.0.0.1:0")
        .map_err(|e| anyhow::anyhow!("无法启动本地查看服务:{e}"))?;
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
    };

    let running = Running {
        port: std::sync::atomic::AtomicU16::new(port),
        mounts: Mutex::new(BTreeMap::new()),
        placement: Placement::Local { standalone: false },
    };
    if SERVER.set(running).is_err() {
        // 另一个线程先跑赢了 —— 用它那份就行
        return Ok(SERVER
            .get()
            .map(|r| r.port.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(port));
    }

    std::thread::Builder::new()
        .name("vidbrief-view-server".into())
        .spawn(move || serve_forever(server))
        .context("无法启动查看服务线程")?;

    Ok(port)
}

/// **在本进程里把服务跑起来**(`vidbrief serve` 走这条),返回端口。
///
/// 与 [`ensure_running`] 的区别:这里**不 spawn**,当前进程就是服务进程 ——
/// 打印一行握手 JSON 之后一直服务,直到收到 `POST /@shutdown`。
/// `pid` 由调用方打印(见 CLI),这里只管把服务起好。
pub fn run_in_this_process(port: u16, mounts: Vec<(String, PathBuf)>) -> Result<u16> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("无法监听 {addr}:{e}"))?;
    let port = match server.server_addr() {
        tiny_http::ListenAddr::IP(addr) => addr.port(),
    };

    let mut table = BTreeMap::new();
    for (id, root) in mounts {
        let canonical = root
            .canonicalize()
            .with_context(|| format!("挂载的目录不存在:{}", root.display()))?;
        table.insert(id, canonical);
    }

    let running = Running {
        port: std::sync::atomic::AtomicU16::new(port),
        mounts: Mutex::new(table),
        // 独立进程:**关服务只关它自己**(这正是把服务挪出工具进程的全部意义)
        placement: Placement::Local { standalone: true },
    };
    if SERVER.set(running).is_err() {
        bail!("这个进程里已经有一个查看服务了");
    }
    write_registry(port)?;

    std::thread::Builder::new()
        .name("vidbrief-view-server".into())
        .spawn(move || serve_forever(server))
        .context("无法启动查看服务线程")?;
    Ok(port)
}

/// 把手握信息写给调用方(一行 JSON),并让它在管道里**立刻**可用。
///
/// 为什么要握手而不是"父进程先挑个端口":让子进程绑 `:0`(由 OS 分配)才是
/// 真正不撞车的做法;父进程只管读这一行。
pub fn print_handshake(port: u16) -> Result<()> {
    use std::io::Write as _;
    let line = format!("{{\"pid\":{},\"port\":{port}}}
", std::process::id());
    let mut out = std::io::stdout();
    out.write_all(line.as_bytes())?;
    out.flush()?;
    Ok(())
}

/// spawn 一个 `vidbrief serve` 子进程,等它把端口报回来。
///
/// **子进程的 stdout 只用来收这一行**,之后不再读也不再用:所以它绝不能往
/// stdout 写别的东西(协议通道那条规矩在 `serve` 上同样适用 —— 只不过这条通道
/// 的客户端是父进程)。
fn spawn_server_process(exe: PathBuf) -> Result<u16> {
    use std::io::{BufRead, BufReader};

    let mut child = std::process::Command::new(&exe)
        .args(["serve", "--port", "0", "--watch-stdin"])
        // stdin 留着当**心跳管道**:我们活着它就开着,我们一走(正常退出或被杀)
        // 它读到 EOF 就跟着收工 —— 不留孤儿服务占着端口与项目目录。
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("无法启动查看服务进程:{}", exe.display()))?;

    // 心跳管道要一直开着:句柄活到本进程结束(那时 OS 关掉它,子进程跟着走)
    let heartbeat = child.stdin.take().context("查看服务进程没有 stdin")?;
    HEARTBEAT.set(heartbeat).ok();

    let stdout = child.stdout.take().context("查看服务进程没有 stdout")?;
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .context("查看服务进程没报回端口")?;
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).with_context(|| format!("握手行不是 JSON:`{line}`"))?;
    let port = value
        .get("port")
        .and_then(|p| p.as_u64())
        .context("握手行里没有 port")? as u16;

    // 记在表里:后续 mount / shutdown 全走 HTTP 控制面(表在对面那个进程里)
    let running = Running {
        port: std::sync::atomic::AtomicU16::new(port),
        mounts: Mutex::new(BTreeMap::new()),
        placement: Placement::Remote,
    };
    if SERVER.set(running).is_err() {
        return Ok(SERVER
            .get()
            .map(|r| r.port.load(std::sync::atomic::Ordering::SeqCst))
            .unwrap_or(port));
    }
    Ok(port)
}

/// 与子进程之间的心跳管道(`--watch-stdin`)。**故意只存不读** ——
/// 它开着的时候子进程就知道我们还活着;本进程结束时 OS 关掉写端,子进程读到 EOF 自行收工。
static HEARTBEAT: OnceLock<std::process::ChildStdin> = OnceLock::new();

/// 要 spawn 的那个可执行文件:就是**当前这个**(CLI 与 MCP 共用一个二进制)。
/// 起一条心跳:stdin 读到 EOF 就是"父进程没了",收掉登记再退出。
///
/// 服务是独立进程,但**不该比会话活得久** —— 否则每跑一轮测试/每个崩溃的会话
/// 都会留下一个占着端口与项目目录的服务(实测踩过:上一次构建就被这些孤儿锁住了 exe)。
pub fn watch_parent_on_stdin() {
    std::thread::spawn(|| {
        let mut sink = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut sink);
        remove_registry_entry();
        std::process::exit(0);
    });
}

fn standalone_serve() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vidbrief"))
}

/// 收掉**我自己那一份**登记(`@shutdown` 与心跳两条退出路径都走它)。
///
/// 为什么不靠 Drop:退出走的是 `std::process::exit`,析构函数不会跑。
pub fn remove_registry_entry() {
    let path = registry_dir().join(format!("{}.json", std::process::id()));
    let _ = std::fs::remove_file(path);
}

/// 把"我在跑、我挂着什么"写进注册表(挂载变化时也要刷新)。
fn write_registry(port: u16) -> Result<()> {
    let Some(running) = SERVER.get() else {
        return Ok(());
    };
    let mounts = running
        .mounts
        .lock()
        .map_err(|_| anyhow::anyhow!("挂载表被毒化了"))?;
    let list: Vec<serde_json::Value> = mounts
        .iter()
        .map(|(id, root)| serde_json::json!({"id": id, "root": root.display().to_string()}))
        .collect();
    let body = serde_json::json!({
        "pid": std::process::id(),
        "port": port,
        "standalone": matches!(running.placement, Placement::Local { standalone: true }),
        "mounts": list,
    });
    let dir = registry_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("建注册表目录失败:{}", dir.display()))?;
    let path = dir.join(format!("{}.json", std::process::id()));
    vidbrief_project::write_atomic_str(&path, &format!("{body}
"))
        .with_context(|| format!("写注册表失败:{}", path.display()))
}

/// 把一个项目挂到 `/{id}/…` 下。
///
/// **独立进程那一档要走 HTTP 控制面**:挂载表在**服务进程**里,不在调用者进程里
/// (`POST /@mount`)。进程内那一档就是本地插表。
pub fn mount(id: &str, root: &Path) -> Result<()> {
    let port = ensure_running()?;
    let canonical = root
        .canonicalize()
        .with_context(|| format!("挂载的目录不存在:{}", root.display()))?;
    let running = SERVER.get().context("查看服务没起来")?;

    match running.placement {
        // 表在对面:交给服务进程自己插(它插完会刷自己的注册表)
        Placement::Remote => post_control(
            port,
            "@mount",
            &serde_json::json!({"id": id, "root": canonical.display().to_string()}),
        )
        .map(|_| ()),
        Placement::Local { standalone } => {
            running
                .mounts
                .lock()
                .map_err(|_| anyhow::anyhow!("挂载表被毒化了"))?
                .insert(id.to_string(), canonical);
            if standalone {
                // 本进程就是那个独立服务:挂载变了要刷注册表(--stop 靠它找项目)
                write_registry(port)?;
            }
            Ok(())
        }
    }
}

/// 打一个控制面请求(`POST /@…`),把返回的 JSON 拿回来。
pub fn post_control(port: u16, path: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
    use std::io::Write as _;
    let payload = body.to_string();
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("连不上查看服务(端口 {port})"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    // 行尾必须是 CRLF:HTTP 服务端(尤其 tiny_http)会等一个"完整的头部块",
    // 只给 LF 的话它一直等下去,调用方看到的就是"连上了却 10 秒没回音"。
    let request = format!(
        "POST /{path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        payload.len(),
        payload
    );
    stream.write_all(request.as_bytes())?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stream, &mut text)?;
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body.trim())
        .with_context(|| format!("控制面 `{path}` 的回应不是 JSON:`{body}`"))
}

/// 关掉这个进程里(或我们 spawn 的)那个服务。
///
/// **这是"关掉"的正门**:从前只能按端口找到进程再杀掉,而它跑在工具进程里时
/// 那一刀等于把 agent 会话一起关了。现在它独立成进程,这一下只关它自己。
pub fn shutdown() -> Result<()> {
    let running = SERVER.get().context("没有在跑的查看服务")?;
    match running.placement {
        // 服务在别的进程里:让它自己收工
        Placement::Remote => post_control(
            running.port.load(std::sync::atomic::Ordering::SeqCst),
            "@shutdown", &serde_json::json!({})).map(|_| ()),
        // 本进程就是那个独立服务 → 回应完就退(见控制面那条)
        Placement::Local { standalone: true } => std::process::exit(0),
        Placement::Local { standalone: false } => bail!(
            "这个查看服务跑在**当前进程的线程**里(前台命令那种),关掉它等于关掉这个进程本身:
               要么在那个终端按 Ctrl+C,要么用 `vidbrief serve` 起一份独立的"
        ),
    }
}

/// 项目内某个文件对应的 URL。
pub fn url_for(id: &str, rel: &str) -> Result<String> {
    let port = ensure_running()?;
    // rel 已由 RelPath 保证安全,这里再挡一次调用方直接塞进来的字符串
    let safe = RelPath::new(rel)?.as_str().to_string();
    Ok(format!("http://127.0.0.1:{port}/{id}/{safe}"))
}

/// 同时伺候几条连接。
///
/// **为什么不写成一个循环**:从前这里是
/// `for request in server.incoming_requests()` —— 一次只处理一个请求,而视频那种
/// 响应是几十 MB 的**流式写**。实测(2026-09-27,真 agent 那轮跑完看报告页):
/// 页面一打开,浏览器就去拉视频,同一个页面自己的其它请求(`report.html`、
/// `report.json`、截图)全排在它后面 —— `project.json` 八秒都拿不到;
/// 浏览器被切走时更糟:那条连接没人读、也没人关,服务端 `write` 就阻塞在那儿,
/// 整个服务看着像死了,直到那条连接被放掉才恢复(实测:关掉浏览器侧连接后
/// 0.02 秒就通了)。
///
/// 现在 N 条线程各自 `recv()`,谁手上堵住都不牵连别人。
const WORKERS: usize = 4;

fn serve_forever(server: Server) {
    let server = Arc::new(server);
    let mut workers = Vec::new();
    for i in 0..WORKERS {
        let server = Arc::clone(&server);
        let spawned = std::thread::Builder::new()
            .name(format!("vidbrief-view-worker-{i}"))
            .spawn(move || {
                loop {
                    // 单个请求出错不能让服务死掉
                    match server.recv() {
                        Ok(request) => {
                            if let Err(e) = handle(&server, request) {
                                eprintln!("查看服务:请求处理失败:{e:#}");
                            }
                        }
                        Err(e) => {
                            eprintln!("查看服务:收请求失败:{e}");
                            break;
                        }
                    }
                }
            });
        match spawned {
            Ok(worker) => workers.push(worker),
            Err(e) => eprintln!("查看服务:工作线程起不来:{e}"),
        }
    }
    // 工作线程活着,这个线程就活着 —— 调用方靠它来判断"服务在不在跑"
    for worker in workers {
        let _ = worker.join();
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

    // `@` 打头的是**控制面**(挂载 / 关掉 / 问状态),不是项目 id —— 先拦下来。
    // 项目 id 只允许 ASCII 小写字母数字与 `-`/`_`(`RelPath` 的规矩),所以不会撞。
    if let Some(control) = decoded.trim_start_matches('/').strip_prefix('@') {
        return handle_control(request, &decoded, control);
    }

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

    // **工作台这一页是现算的**(见 `html::workbench_page`):它就是"看材料"的那一页,
    // 而材料一直在变 —— 烤进文件的那份快照只在渲染那一瞬间是对的。实测(2026-09-27):
    // agent 在步 2 中途渲过一次,步 4 跑完打开看到的还是「全部未核验 / 还没有报告」。
    // 路要排在静态文件**前面**,否则拿到的是盘上那份旧快照。
    if rest == vidbrief_project::layout::WORKBENCH_FILE {
        return match crate::html::workbench_page(&root) {
            Ok(html) => respond_html(request, 200, &html),
            Err(e) => respond_text(request, 500, &format!("这一页渲不出来:{e:#}")),
        };
    }

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
    respond_with(request, code, "text/plain; charset=utf-8", text)
}

/// 回一页 HTML(现算的工作台走这条 —— 它和静态文件那条路的 mime 必须一样,
/// 否则浏览器会把它当纯文本显示出来)。
fn respond_html(request: Request, code: u16, html: &str) -> Result<()> {
    respond_with(request, code, "text/html; charset=utf-8", html)
}

fn respond_with(request: Request, code: u16, mime: &str, body: &str) -> Result<()> {
    request
        .respond(
            Response::from_string(body)
                .with_status_code(StatusCode(code))
                .with_header(header("Content-Type", mime))
                // 现算的页面**不许缓存**:不然浏览器拿旧的那份,又回到"看着像没干活"
                .with_header(header("Cache-Control", "no-store")),
        )
        .map_err(Into::into)
}

/// 控制面:`POST /@mount`、`POST /@shutdown`、`GET /@status`。
///
/// **为什么要这一层**:服务挪到独立进程之后,挂载表在**它**手里 —— 调用方
/// (MCP 会话)得有个办法把"再挂一个项目"告诉它;而"关掉"也从"按端口找进程杀"
/// 变成一次正经的请求。
///
/// 只收本机回环上的请求(服务本身就只绑回环),所以没有额外的鉴权层 ——
/// 能在本机上发请求的人,本来就能读这些材料。
fn handle_control(mut request: Request, decoded: &str, control: &str) -> Result<()> {
    let control = control.trim_end_matches('/');
    let running = SERVER.get().context("服务不在跑")?;
    match (control, request.method()) {
        ("status", &Method::Get) | ("status", &Method::Head) => {
            let mounts = running
                .mounts
                .lock()
                .map_err(|_| anyhow::anyhow!("挂载表中毒"))?;
            let list: Vec<serde_json::Value> = mounts
                .iter()
                .map(|(id, root)| serde_json::json!({"id": id, "root": root.display().to_string()}))
                .collect();
            respond_value(
                request,
                200,
                &serde_json::json!({
                    "pid": std::process::id(),
                    "port": running.port.load(std::sync::atomic::Ordering::SeqCst),
                    "standalone": matches!(running.placement, Placement::Local { standalone: true }),
                    "mounts": list,
                }),
            )
        }
        ("mount", &Method::Post) => {
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
                return respond_text(request, 400, "body 要是 JSON:{\"id\":…,\"root\":…}");
            };
            let (Some(id), Some(root)) = (
                value.get("id").and_then(|v| v.as_str()),
                value.get("root").and_then(|v| v.as_str()),
            ) else {
                return respond_text(request, 400, "缺 id 或 root");
            };
            let Ok(canonical) = Path::new(root).canonicalize() else {
                return respond_text(request, 400, &format!("挂载的目录不存在:{root}"));
            };
            running
                .mounts
                .lock()
                .map_err(|_| anyhow::anyhow!("挂载表中毒"))?
                .insert(id.to_string(), canonical);
            write_registry(running.port.load(std::sync::atomic::Ordering::SeqCst))?;
            respond_value(request, 200, &serde_json::json!({"ok": true}))
        }
        ("shutdown", &Method::Post) => {
            // **请求体一定要读掉**:不读干净,tiny_http 那边的连接状态会错乱,
            // 回应发不出去 —— 表现就是调用方"连上了却一直等",10 秒后超时。
            // 这一课在页面动作那条路上已经写过一次(见 `handle_action`)。
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            if !matches!(running.placement, Placement::Local { standalone: true }) {
                // 不连坐:进程内那一档关掉服务 = 关掉调用者
                return respond_value(
                    request,
                    409,
                    &serde_json::json!({
                        "ok": false,
                        "why": "这个服务跑在调用进程里,不能单独关掉 —— 用 `vidbrief serve` 起一份独立的"
                    }),
                );
            }
            // 先回话再退:让发起方知道自己成功了
            respond_value(
                request,
                200,
                &serde_json::json!({"ok": true, "pid": std::process::id()}),
            )?;
            remove_registry_entry();
            std::process::exit(0);
        }
        _ => respond_text(
            request,
            404,
            &format!("不认识的控制面 `{}`;有 @status / @mount / @shutdown", decoded),
        ),
    }
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
/// 原样回一个 JSON 值(控制面用)。
///
/// 与 [`respond_json`] 的区别:那个是**给页面看的话**(包一层 `{ok, message}`);
/// 控制面的调用方是程序(父进程 / `serve --stop`),它们要的是**数据本身**。
fn respond_value(request: Request, code: u16, value: &serde_json::Value) -> Result<()> {
    request
        .respond(
            Response::from_string(value.to_string())
                .with_status_code(StatusCode(code))
                .with_header(header("Content-Type", "application/json; charset=utf-8")),
        )
        .map_err(Into::into)
}

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
            "01_preprocess/workbench.html",
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

    /// **一条慢慢读(或者干脆不读)的连接,不能把别的请求堵住。**
    ///
    /// 这是实测踩出来的:报告页一打开,浏览器就去拉视频(几十 MB 的流式响应),
    /// 而服务当时一次只处理一个请求 —— 于是同一个页面的 `project.json`
    /// 八秒都拿不到,看着像死了。判据就是"另一条请求照样很快回话"。
    #[test]
    fn a_slow_reader_does_not_block_other_requests() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpStream;
        use std::time::{Duration, Instant};

        let dir = std::env::temp_dir().join(format!("vidbrief-slow-reader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 要大到"服务端一口气写不完":客户端不读,内核缓冲填满之后才有阻塞
        std::fs::write(dir.join("big.bin"), vec![7u8; 8 * 1024 * 1024]).unwrap();
        std::fs::write(dir.join("small.txt"), b"tinybody").unwrap();

        let port = ensure_running().unwrap();
        mount("slow", &dir).unwrap();

        // 慢客户端:要大文件,只读一小口就不读了(浏览器被切走时的样子)
        let mut slow = TcpStream::connect(("127.0.0.1", port)).unwrap();
        slow.write_all(b"GET /slow/big.bin HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut sip = [0u8; 64];
        let _ = slow.read(&mut sip);

        // 这条应当很快回来(从前它要一直等到上面那条连接被放掉)
        let mut fast = TcpStream::connect(("127.0.0.1", port)).unwrap();
        fast.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let started = Instant::now();
        fast.write_all(b"GET /slow/small.txt HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut answer = Vec::new();
        while started.elapsed() < Duration::from_secs(5) {
            let mut chunk = [0u8; 512];
            match fast.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    answer.extend_from_slice(&chunk[..n]);
                    if String::from_utf8_lossy(&answer).contains("tinybody") {
                        break;
                    }
                }
                Err(_) => break, // 读超时:没等到
            }
        }
        let took = started.elapsed();

        drop(slow);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            String::from_utf8_lossy(&answer).contains("tinybody"),
            "那条慢连接把这条请求堵住了(等了 {took:?},只收到 {} 字节)",
            answer.len()
        );
        assert!(took < Duration::from_secs(3), "不该等这么久:{took:?}");
    }
}
