//! `vidbrief` —— 独立命令行入口(脱离 agent 也能测试与使用)。
//!
//! 子命令见 `docs/DESIGN.md` §10.5。**这里只做三件事**:解析参数、调 `vidbrief-ops`、
//! 把结果说给人听。任何"该做什么"的判断都属于动作层 —— 否则 MCP 那边就得再写一遍。
//!
//! 唯一的例外是 `mcp`:它把进程交给 `vidbrief-mcp` 的 stdio 服务,之后 stdout 归协议所有。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use vidbrief_ops::{OpenProject, PreprocessOptions, RunStage, Stage, TranscriptOutcome};
use vidbrief_project::{SourceLink, human_size};

const USAGE: &str = "\
vidbrief —— 视频图文总结 MCP 工具集

Usage: vidbrief <command> [args]

Commands:
  init <video> [options]    建项目(目录骨架 + 探测元数据 + 搬入源视频)
  preprocess <project>      步 1:差帧图 / 密集区 / 截图 / 转录
  status <project>          查看项目状态与健康检查
  approve <project>         记下「校准这一步用户看过了」—— 打开通往步 3 的门
  correct <project> -j F    把转录结论写回纠错稿(apply_corrections)
  shot-check <project> -j F 记一张图的看图结论(record_shot_check)
  supplements <project> -j F 处置一批补图候选(disposition_supplements)
  summary <project> -m F    写总结(write_summary,过校准的门)
  report <project> -j F     出报告(build_report:数据 + 页面 + 导出)
  upgrade <project>         把项目对齐到当前版本(旧格式 / 状态文件读不动时用)
  ui <project>              渲染时间线视图 + 起本地服务 + 打开浏览器
  mcp [--stdio]             启动 MCP 服务(stdio 传输,供 agent 接入)
  help                      显示本条用法

Run `vidbrief <command> -h` for the options of a command.
";

const CORRECT_USAGE: &str = "\
vidbrief correct —— 把转录结论写回纠错稿(apply_corrections 的命令行版)

Usage: vidbrief correct <project> --json <FILE|->

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  -j, --json <FILE|->      输入(`-` = 从 stdin 读)。形状与 MCP 工具 apply_corrections
                           的入参一致(少了 project_id):
                           {\"corrections\":[{\"segment_id\":1,\"outcome\":\"corrected\",
                             \"text\":\"改后的整句\",\"type\":\"terminology\",
                             \"note\":\"派森 → Python\",\"reason\":\"画面文字\"},
                            {\"segment_id\":2,\"outcome\":\"punctuated\",
                             \"text\":\"加了标点之后的整段。\"}],
                            \"reviewed\":[3,4],
                            \"reviewed_ranges\":[{\"from\":5,\"to\":300}]}

说明:
  outcome 四种:corrected(改了字,必须给 text)/ punctuated(**只加标点 / 断句**,必须给 text,
  字一个不动:去掉标点后要跟原文一字不差)/ kept(**这一段按原文算**)/ uncertain(定不了,留给人)。
  **改成跟原文一字不差会被拒收** —— 要保留原句、**或要把这一段退回原文(撤掉前面落的改动)**,
  都用 kept —— kept 不给 text,报下去那一段就按原文算;跟原文**只差标点**也会被拒 —— 那是 punctuated。
  撤回之后还想补标点,再单独报一条 punctuated(那时现读法已经回到原文了)。
  reason 除 punctuated 外都要给(依据)。
  reviewed / reviewed_ranges 是「审过、没问题」的段:逐段点名,或整片报一个闭区间,两种取并集。
  不报等于没审,审核门会拦。
  走的是和 MCP 工具同一套代码、同一把锁、同一个门。
";

const SHOT_CHECK_USAGE: &str = "\
vidbrief shot-check —— 记一张图的看图结论(record_shot_check 的命令行版)

Usage: vidbrief shot-check <project> --json <FILE|->

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  -j, --json <FILE|->      输入(`-` = 从 stdin 读)。形状与 MCP 工具一致:
                           {\"shot_id\":\"s007\",\"match\":true,\"audio_visual_sync\":true,
                            \"type_read\":\"从画面读到的内容\",\"issues\":[],
                            \"confidence\":\"high\",\"evidence_quote\":\"照抄的转录原文\"}

说明:
  evidence_quote 要**照抄**一句转录原文(落在一段之内)。工具拿它去材料里搜:
  搜不到就拒收,搜得到就连段号一起记下 —— 它是这条结论唯一的凭据。
";

const SUPPLEMENTS_USAGE: &str = "\
vidbrief supplements —— 处置一批补图候选(disposition_supplements 的命令行版)

Usage: vidbrief supplements <project> --json <FILE|->

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  -j, --json <FILE|->      输入(`-` = 从 stdin 读)。形状与 MCP 工具一致:
                           {\"items\":[{\"id\":1,\"decision\":\"no_shot\",
                             \"note\":\"旁边就有图,判定半径太紧\"},
                            {\"id\":126,\"decision\":\"need_shot\",\"note\":\"这一段缺图\"}]}

说明:
  decision 两种:need_shot(工具去抽帧;那一刻附近已有留用的图就沿用)/ no_shot。
  **每条都要给理由**,门数的是「还有几条没有结论」。
";

const SUMMARY_USAGE: &str = "\
vidbrief summary —— 交**一段**总结(write_summary 的命令行版)

Usage: vidbrief summary <project> --title <标题> --md <FILE|-> [--at <秒>] [--verify <一句>]...

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  --title <标题>           这一段叫什么(报告页上那条的标题;改稿按它认段:同名 = 改写)
  -m, --md <FILE|->        Markdown 正文(`-` = 从 stdin 读)
      --at <秒>            这一节讲的是视频里哪一刻;**不给 = 总述**(不上时间轴)
      --verify <一句>      待核验的点,可给多次

说明:
  一次交一段,写完一段落一段:段落落在 03_summary/sections.json,
  summary.md 是拼好的文章(总述在前、其余按时刻)。
  报告页直接由这些段组装 —— 出报告时**不用再交内容**。
  **这一步要过校准的门**:还有没核验的、或者有等用户拍板的,会被挡住并说清是哪些。
  门和 MCP 那边是同一道。
";

const REPORT_USAGE: &str = "\
vidbrief report —— 出报告(build_report 的命令行版)

Usage: vidbrief report <project> --json <FILE|-> [options]

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  -j, --json <FILE|->      输入(`-` = 从 stdin 读)。形状与 MCP 工具 build_report
                           的入参一致(少了 project_id)—— **只有口味**:
                           {\"style\":{\"preset\":\"teaching\",\"granularity\":\"detailed\",
                                     \"tone\":\"大白话\",\"notes\":\"给刚入门的人看(可选)\"},
                            \"format\":\"both\",\"open_browser\":true}
                           **内容不从这里进**:报告页由步 3 的分段
                           (03_summary/sections.json)组装 —— 一条 = 一个带时刻的段。
                           `style` 是必填的:preset 是 quick(快扫版)/ teaching(教学版)/
                           story(故事版)/ review(复盘版)/ exec(汇报版)/ custom,
                           granularity 只能是 brief(概括)/ standard(标准)/ detailed(详细),
                           tone 是自由文本(大白话 / 诙谐 / 讲故事 / 行业腔……)。
      --format <both|html|md>
                           出哪几份:both(默认,页面 + Markdown 导出)/ html / md。
                           给了就以它为准(压过输入里的 format)
      --open               渲染之后起本地查看服务并打开浏览器,**然后常驻**
                           (与 `vidbrief ui` 一样,按 Ctrl+C 结束)。
                           不给就只渲染:页面已经写好,要看用
                           `vidbrief ui <项目> --view report`
      --self-contained     **单文件分享**:图内嵌成 base64 塞进页面/导出,
                           产物能单独发出去(对方不需要项目目录)。
                           代价是体积(一张图 ×1.37);**视频不内嵌** ——
                           单文件版的图能带走,点锚点回跳那一半仍要本机素材 + 服务
  -h, --help               显示本条用法

说明:
  报告**以数据为源**:`04_report/report.json` 是判据,`report.html`(时间轴上挂知识点、
  可回跳视频)与 `report.md` 是它的两个投影。重做就是再跑一遍 —— **整份替换**。
  口味(`style`)落在数据里:页面的标题栏有个小标签、导出的头一行写着它 ——
  交付时能一句话说清「这份是按什么做的」。

  **这里默认不起服务**:命令行进程一退出,服务就没了,印出来的地址就是个死链。
  要边看边拖时间轴(页面要播视频,`file://` 下拖不动),用 `vidbrief ui --view report`
  —— 那条命令本来就是常驻的。输入里给 `\"open_browser\": true` 等同于 `--open`。

  **每条内容都来自步 3 的段** —— 一条 = 一个带时刻的段(标题 + 正文 + 锚点),
  它那一刻的转录段与截图由工具就近挂上 —— 来源与截图不用你在这给。
  没给 `--at` 的那段是总述:进报告,但不上时间轴。

  **这道门前置着步 3**:没写过总结、或总结被绕过工具改过,会被挡住并说清怎么办。
";

const APPROVE_USAGE: &str = "\
vidbrief approve —— 记下「这一步用户看过了」

Usage: vidbrief approve <project> [options]

Arguments:
  <project>                项目:项目目录的路径,或 projects/ 下的项目名

Options:
  -n, --note <TEXT>        带着未处理项通过时的说明(用户的原话最好)。
                           该处理的都处理完了就别给;给了会原样存进项目状态,事后能查

说明:
  校准(步 2)通往总结(步 3)有一道门:没被确认过、或者确认之后材料又变过,
  写总结会被挡住。这个命令就是开门那一下。
  服务端会记下**确认当时的材料状态**;之后你再改任何东西,这次确认就作废,
  得重新看一遍再确认。
";

const UPGRADE_USAGE: &str = "\
vidbrief upgrade —— 把项目对齐到当前版本(状态照材料重新起一份)

Usage: vidbrief upgrade <project> [options]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
      --fresh              什么都不收,起一份**空**状态:已有的核验记录清零,材料原样保留。
                           步 1 的完成标记保留 —— 它答的是「那趟算法活儿跑完了没有」,
                           材料没动,答案就没变(清掉它会让状态页报「① 前处理 — 未做」而材料齐全)
      --accept-material    把**现在这份**材料当成新底账(连跟底账对不上的那些也重记)。
                           只有在你知道材料为什么变了、并且认了它的时候才用
  -h, --help               显示本条用法

说明:
  两种情况用得上它:

  ① **旧格式(v1)的项目** —— 进度、核验戳、看图结论、拍板记录当年写在明文里,
     现在要收进 state.bin(非明文)。老项目直接打开会报「旧格式」,跑一次这个就行。
  ② **状态文件不见了 / 读不动** —— 照明文材料里还读得到的那部分重新起一份。

  材料(转录、截图、候选、总结、报告)**一个字节都不动**。
  收回来的东西会一条条报出来,免得你不知道自己那几轮活还在不在。

  它是**最保守**的那条路:读得到什么就收什么,读不到的就是「还没核验」——
  拿它绕门是绕不过去的,只会让门拦得更死。

  另有一个内容底账(见 docs/DESIGN.md §4.10):材料**内容**的手改会被门拦下,
  而门数是「还有哪些没核验」的,所以拿掉行、活就看不见了 —— 底账挡的就是这个。
  默认**不换**对不上的底账;认了现在这份才加 `--accept-material`。
";

const INIT_USAGE: &str = "\
vidbrief init —— 建项目

Usage: vidbrief init <video> [options]

Arguments:
  <video>                  源视频路径(任何 ffmpeg 认得的格式)

Options:
  -n, --name <ID>          项目 id:ASCII 小写字母/数字/`-`/`_`。默认由视频文件名净化;
                           净化不出来(如纯中文名)且未指定 -p 时会报错
  -t, --title <TEXT>       人类可读名,可含中文。默认取视频文件名
  -p, --project-dir <DIR>  项目目录本身,直接把项目建在 <DIR> 里;该目录已存在则报错,不覆盖。
                           不给 -p 时用默认位置 projects/<id>。
                           两种写法撞上已存在的目录都是**报错** —— 工具不会替你改名
  -h, --help               显示本条用法

Examples:
  vidbrief init D:/videos/demo.mp4
  vidbrief init D:/videos/lecture.mp4 -n lecture -t \"第一讲 开场\"
  vidbrief init demo.mp4 -p D:/proj/demo

说明:
  - 只建目录与元数据,不跑任何算法;源视频按 硬链接 → 软链接 → 复制 的顺序引入
  - 步 1 用 `vidbrief preprocess <project>`
";

const STATUS_USAGE: &str = "\
vidbrief status —— 查看项目状态与健康检查

Usage: vidbrief status <project> [--verify]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
  -V, --verify             重算源视频 sha256 并与记录比对。能发现\"源文件被就地改写\",
                           但要把整个视频读一遍(几百 MB 要几秒)
  -h, --help               显示本条用法

健康检查查什么(依 source.link 而定):
  hardlink   两边是否仍是同一份数据。源文件被移动/删除/替换都**不影响**项目,只提示
  symlink    链接目标是否还在。**失效即项目不可用**,会以 ✗ 报出来
  copy       无需检查
";

const PREPROCESS_USAGE: &str = "\
vidbrief preprocess —— 步 1 前处理(纯算法,一次做完)

Usage: vidbrief preprocess <project> [options]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
  -f, --fps <N>            采样帧率,默认 1.0(每秒一个点)
  -w, --width <N>          参与打分的缩小宽度,默认 480
      --no-transcribe      跳过语音转文字(只出差帧图与截图)
      --no-shots           只出差帧图与密集区,不抽截图
      --force              **无条件重跑**:不沿用上次的任何成果。默认相反 ——
                           输入没变(源视频 sha256 + 参数 + 模型 + 口径版本逐项相同、
                           材料还在、还是工具写出去的那一份)就沿用,不重算、也不作废
                           那批看图结论与核验戳。怀疑上次那批材料时走这条。
      --run-id <ID>        给这一趟起个名字(内部契约:MCP 起作业时用,好让等它的人
                           在 run.json 里认得出「就是这一趟」)。不给就自己生成。
  -h, --help               显示本条用法

产出(全部落在 01_preprocess/):
  change_curve.json   差帧图
  dense_regions.json  变化密集区
  shots.json + shots/ 预筛选截图
  transcript.json     + .srt/.txt/.vtt(源视频没有音轨时跳过)
  inputs.json         这一趟的**输入指纹**(增量重跑比的就是它:源视频 sha256、
                      fps/width、模型/语言、口径版本)
  run.json            **运行记录**:谁在跑、跑到第几步、跑完的交代或失败原因
                      (给「别的会话想知道跑到哪儿了」用的;MCP 那三件套读的就是它)

说明:
  - 会先取项目写锁;若另一个会话正开着这个项目,这里会直接拒绝而不是硬写
    (`docs/DESIGN.md` §4.7)。查看视图不受影响。
  - 这个项目上已经有一趟在跑时**拒绝再开一趟**(两趟同时写会互相盖)。
    记录超过 2 分钟没有心跳就算那一趟断了,那时重跑不会撞上这条。
  - **画面与转录分开判**:改 `--fps` 重跑只重算画面那一半,转录照旧留着
    (它跟抽帧参数没关系);反过来也一样。判不准(没记录、材料动过)就重算 ——
    重算只是多花几分钟,拿旧材料配新视频才是真出错。
";

const UI_USAGE: &str = "\
vidbrief ui —— 本地视图 + 视频服务

Usage: vidbrief ui <project> [options]

Arguments:
  <project>                项目目录,或 projects/ 下的项目名

Options:
      --view <name>        渲染哪一页:workbench(工作台:视频 + 五层共轴时间轴 +
                           轴上浮层,右边抽屉里截图/转录/报告三块 —— 一页看全部,
                           步 2 起可用)或 report(纯图文报告页,能单独保存/发给别人;
                           数据来自 04_report/report.json,得先跑过步 4)
      --no-open            只起服务并打印 URL,不打开浏览器
  -h, --help               显示本条用法

说明:
  - 渲染 01_preprocess/workbench.html(--view report 是 04_report/report.html)
  - 起一个绑在 127.0.0.1 的静态服务把项目目录发布出去 —— **必须有它浏览器才能 seek**
    (`file://` 打开时对本地媒体有跨域限制,轴上点了不跳)
  - 端口由操作系统分配,不会和别的东西撞车;只监听回环地址,局域网访问不到
  - 不取写锁:它写的是**派生视图**不是材料,两个会话同时渲染也不会坏事
";

const MCP_USAGE: &str = "\
vidbrief mcp —— 启动 MCP 服务

Usage: vidbrief mcp [--stdio]

Options:
      --stdio              用 stdio 传输(默认,也是唯一已实现的)
  -h, --help               显示本条用法

说明:
  - **这是给 agent 用的入口**:客户端把本进程拉起来,用 stdin/stdout 走 JSON-RPC
  - stdout 从此归协议所有 —— 本命令不会往 stdout 打印任何东西(诊断都在 stderr)
  - 工具清单见 docs/DESIGN.md §11;提示模板 P1–P5 见 §10.4
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some((cmd, rest)) = args.split_first() else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    let result = match cmd.as_str() {
        "init" => cmd_init(rest),
        "status" => cmd_status(rest),
        "preprocess" => cmd_preprocess(rest),
        "approve" => cmd_approve(rest),
        "upgrade" => cmd_upgrade(rest),
        "correct" => cmd_correct(rest),
        "shot-check" => cmd_shot_check(rest),
        "supplements" => cmd_supplements(rest),
        "summary" => cmd_summary(rest),
        "report" => cmd_report(rest),
        "ui" => cmd_ui(rest),
        "serve" => cmd_serve(rest),
        "mcp" => cmd_mcp(rest),
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => {
            eprintln!("未知子命令:`{other}`\n");
            print!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` 打印 anyhow 的整条因果链,"为什么会失败"比"失败了"有用得多
            eprintln!("错误:{err:#}");
            ExitCode::FAILURE
        }
    }
}

// ── 通用的参数解析小工具 ─────────────────────────────────────────────

/// 看起来像选项(以 `-` 开头且不止一个字符)。单独的 `-` 视为普通参数。
fn is_option(arg: &str) -> bool {
    arg.starts_with('-') && arg.len() > 1
}

/// 取选项的值:`-k=v` / `--k=v` 用等号右边,`-k v` / `--k v` 吃下一个参数。
fn take_value(args: &[String], i: &mut usize, inline: Option<String>, what: &str) -> Result<String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    *i += 1;
    args.get(*i)
        .cloned()
        .with_context(|| format!("{what} 后面缺少取值"))
}

/// 把 `--k=v` 拆成 `("--k", Some("v"))`。
fn split_option(arg: &str) -> (&str, Option<String>) {
    match arg.split_once('=') {
        Some((k, v)) if is_option(k) => (k, Some(v.to_string())),
        _ => (arg, None),
    }
}

/// 收集位置参数(最多一个),顺便把不认识的选项骂回去。
fn one_positional(args: &[String], cmd: &str, what: &str) -> Result<String> {
    let mut found: Option<String> = None;
    for arg in args {
        if is_option(arg) {
            bail!("{cmd} 不认识参数 `{arg}`(见 `vidbrief {cmd} -h`)");
        }
        if found.is_some() {
            bail!("{cmd} 只接受一个{what},多余的参数:`{arg}`");
        }
        found = Some(arg.clone());
    }
    found.with_context(|| format!("用法:vidbrief {cmd} <{what}> [options](见 `vidbrief {cmd} -h`)"))
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "-h" || a == "--help")
}

// ── init ─────────────────────────────────────────────────────────────

struct InitArgs {
    video: PathBuf,
    name: Option<String>,
    title: Option<String>,
    /// 项目目录本身。`None` = 用默认位置 `projects/<id>`。
    project_dir: Option<PathBuf>,
}

fn parse_init(args: &[String]) -> Result<InitArgs> {
    let mut video: Option<PathBuf> = None;
    let mut name = None;
    let mut title = None;
    let mut project_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = split_option(arg);
        match key {
            "-n" | "--name" => name = Some(take_value(args, &mut i, inline, key)?),
            "-t" | "--title" => title = Some(take_value(args, &mut i, inline, key)?),
            "-p" | "--project-dir" => {
                project_dir = Some(PathBuf::from(take_value(args, &mut i, inline, key)?))
            }
            other if is_option(other) => {
                bail!("init 不认识参数 `{other}`(见 `vidbrief init -h`)")
            }
            _ => {
                if video.is_some() {
                    bail!("init 只接受一个视频路径,多余的参数:`{arg}`");
                }
                video = Some(PathBuf::from(arg));
            }
        }
        i += 1;
    }

    let video = video.context("用法:vidbrief init <video> [options](见 `vidbrief init -h`)")?;
    Ok(InitArgs {
        video,
        name,
        title,
        project_dir,
    })
}

fn cmd_init(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{INIT_USAGE}");
        return Ok(());
    }
    let parsed = parse_init(args)?;

    // 跨卷复制要好几秒,**在动手之前**先说出来
    let mut on_notice = |notice: vidbrief_ops::Notice| match notice {
        vidbrief_ops::Notice::CrossVolumeCopy { size_bytes, hint } => {
            println!(
                "  预判     素材与项目不在同一个卷(盘),硬链接用不了 —— 即将复制 {}",
                human_size(size_bytes)
            );
            if let Some(hint) = hint {
                println!("           想省下这份空间,把项目建到素材同卷:{hint}");
            }
        }
    };

    let created = vidbrief_ops::create(
        &vidbrief_ops::CreateOptions {
            video: parsed.video,
            name: parsed.name,
            title: parsed.title,
            project_dir: parsed.project_dir,
        },
        &mut on_notice,
    )?;

    print_summary(&created);
    Ok(())
}

fn print_summary(created: &vidbrief_ops::Created) {
    let meta = &created.meta;
    let fps = match meta.fps {
        Some(fps) => format!(" @ {fps:.3} fps"),
        None => String::new(),
    };
    let size = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "分辨率未知".to_string(),
    };
    let codec = meta.codec.as_deref().unwrap_or("编码未知");

    println!("项目已建立:{}", created.project.id);
    println!("  目录     {}", display_dir(created.layout.root()));
    println!("  入口     {}", created.layout.project_json().display());
    println!(
        "  源视频   {} → {}",
        created.original.display(),
        created.project.source.path
    );
    println!(
        "  引入     {}",
        link_note(created.project.source.link, meta.size_bytes)
    );
    for reason in &created.fallbacks {
        println!("           └ {reason}");
    }
    println!("  时长     {:.1}s  {size}{fps}  {codec}", meta.duration);
    println!("  指纹     sha256:{}", meta.sha256);
    if !created.project.source.link.is_self_contained() {
        println!("  警告     项目**不**自包含:源文件一旦移动或删除即失效,打包时还可能变成坏链接");
    }
    println!();
    println!("下一步:");
    println!(
        "  vidbrief preprocess {}    # 步 1:差帧图 / 转录 / 预筛选截图",
        created.layout.root().display()
    );
}

/// 一句话说清"源视频是怎么进来的",以及这件事的**实际后果**。
///
/// 措辞在 `vidbrief_project::SourceLink::describe`,那里能不起进程就单测。
fn link_note(link: SourceLink, size_bytes: u64) -> String {
    link.describe(size_bytes)
}

// ── preprocess ───────────────────────────────────────────────────────

fn parse_preprocess(args: &[String]) -> Result<(String, PreprocessOptions, Option<String>)> {
    let mut project: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut opts = PreprocessOptions::default();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (key, inline) = split_option(arg);
        match key {
            "-f" | "--fps" => {
                let raw = take_value(args, &mut i, inline, key)?;
                opts.fps = raw
                    .parse()
                    .with_context(|| format!("--fps 不是数字:`{raw}`"))?;
            }
            "-w" | "--width" => {
                let raw = take_value(args, &mut i, inline, key)?;
                opts.width = raw
                    .parse()
                    .with_context(|| format!("--width 不是整数:`{raw}`"))?;
            }
            "--no-transcribe" => opts.transcribe = false,
            "--no-shots" => opts.shots = false,
            // 默认「能省则省」:输入没变就沿用上次那半成果(见 `vidbrief_ops::inputs`)。
            // 想无条件重来一遍(怀疑材料 / 换了算法)走这条。
            "--force" => opts.force = true,
            // 内部契约:MCP 起作业时用它给这一趟起名,好让等它的人认得出「就是这一趟」
            "--run-id" => run_id = Some(take_value(args, &mut i, inline, key)?),
            other if is_option(other) => {
                bail!("preprocess 不认识参数 `{other}`(见 `vidbrief preprocess -h`)")
            }
            _ => {
                if project.is_some() {
                    bail!("preprocess 只接受一个项目,多余的参数:`{arg}`");
                }
                project = Some(arg.clone());
            }
        }
        i += 1;
    }

    if opts.fps <= 0.0 || !opts.fps.is_finite() {
        bail!("--fps 必须为正数,实际 {}", opts.fps);
    }
    if opts.width == 0 {
        bail!("--width 必须为正数");
    }

    let project =
        project.context("用法:vidbrief preprocess <project> [options]")?;
    Ok((project, opts, run_id))
}

/// `vidbrief approve` —— 记下"这一步用户看过了",打开步 2 通往步 3 的门。
///
// ── 写操作的命令行孪生入口 ────────────────────────────────────────────
//
// 为什么要有这几条:MCP 是**一条**通往写操作的路,而它会断(第五轮实测:
// agent 把自己的工具进程一起关掉了,之后写报告、记结论全都没路可走,
// 它只好手写文件、手改进度)。每个写动作都该有一条不经过 MCP 的入口 ——
// 走的是**同一套 `ops` 代码、同一把锁、同一个门**,不是"另一条捷径"。
//
// 输入统一从文件或 stdin 读(`--json -` / `--md -`),因为结论的正文往往很长,
// 塞进命令行参数既别扭又有长度上限。

/// 读一份输入:`<文件>` 或 `-`(stdin)。
fn read_input(path: &str) -> Result<String> {
    if path == "-" {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).context("读 stdin 失败")?;
        return Ok(buf);
    }
    std::fs::read_to_string(path).with_context(|| format!("读 {path} 失败"))
}

/// 从 `--json` / `--md` 这类选项里取"从哪读",并读进来。
fn take_input(args: &[String], cmd: &str, flag: &str) -> Result<String> {
    let mut value: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = split_option(&args[i]);
        if key == flag || (flag == "--json" && key == "-j") || (flag == "--md" && key == "-m") {
            value = Some(take_value(args, &mut i, inline, flag)?);
        }
        i += 1;
    }
    let path = value.with_context(|| {
        format!("用法:vidbrief {cmd} <项目> {flag} <文件|->(见 `vidbrief {cmd} -h`)")
    })?;
    read_input(&path)
}

/// 项目名:去掉选项之后的那个位置参数。
///
/// `extra` 是这条命令**自己**认识的选项(它们和标志位一样要跳过,值也不能被当成项目名)。
/// 从参数里挑出"项目"那个位置参数。
///
/// `value_flags` = **会吃掉下一个参数**的选项(如 `--format v`);
/// `bare_flags` = 不带值的开关(如 `--open`)。
/// **两者都要列全** —— 没列出来的会被当成未知参数打回去。
/// 这一条踩过:`--open` 写在用法说明里,却在参数扫描这一关就被拒了
/// (`report 不认识参数 --open`),而那正是"文档说能用的东西不能用"。
fn positional_project(
    args: &[String],
    cmd: &str,
    flag: &str,
    value_flags: &[&str],
    bare_flags: &[&str],
) -> Result<String> {
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = split_option(&args[i]);
        if key == flag
            || (flag == "--json" && key == "-j")
            || (flag == "--md" && key == "-m")
            || value_flags.contains(&key)
        {
            take_value(args, &mut i, inline, flag)?;
        } else if bare_flags.contains(&key) {
            // 开关:不取值,也不当项目名
        } else if is_option(&args[i]) {
            bail!("{cmd} 不认识参数 `{}`(见 `vidbrief {cmd} -h`)", args[i]);
        } else {
            positional.push(args[i].clone());
        }
        i += 1;
    }
    one_positional(&positional, cmd, "项目")
}

/// `vidbrief correct` —— 把转录结论写回(`apply_corrections`)。
fn cmd_correct(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{CORRECT_USAGE}");
        return Ok(());
    }
    let name = positional_project(args, "correct", "--json", &[], &[])?;
    let raw = take_input(args, "correct", "--json")?;
    let body: CorrectBody = serde_json::from_str(&raw).context("--json 解析失败")?;

    let mut items = Vec::with_capacity(body.corrections.len());
    for c in body.corrections {
        let segment_id = c.segment_id;
        let conclusion = c.into_conclusion()?;
        items.push(vidbrief_ops::CorrectionInput {
            segment_id,
            conclusion,
        });
    }

    let (id, outcome) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let ranges: Vec<vidbrief_ops::ReviewedRange> = body
            .reviewed_ranges
            .iter()
            .map(|r| vidbrief_ops::ReviewedRange {
                from: r.from,
                to: r.to,
            })
            .collect();
        let outcome = vidbrief_ops::apply_corrections(opened, &items, &body.reviewed, &ranges)?;
        Ok((opened.id().to_string(), outcome))
    })?;

    println!("项目     {id}");
    println!("本次写入 {} 条", outcome.applied);
    if outcome.folded > 0 {
        println!(
            "           其中 {} 条本来就断过句(折成「看过、没问题」,没落结论)",
            outcome.folded
        );
    }
    println!(
        "全稿     {} 段:改了 {} 段 / 加了断句 {} 段 / 决定不改 {} 段 / 存疑未改 {} 段 / 合计动过 {} 段",
        outcome.total_segments,
        outcome.corrected,
        outcome.punctuated,
        outcome.kept,
        outcome.uncertain,
        outcome.corrected + outcome.punctuated + outcome.kept + outcome.uncertain
    );
    println!("核验过   {} 段", outcome.reviewed);
    println!("纠错稿   {}", outcome.fixed_path.display());
    println!("对照表   {}", outcome.diff_path.display());
    // 部分落实:好的已经落盘,坏的逐条退回 —— 说清哪一条为什么,再以非零退出
    if !outcome.rejected.is_empty() {
        eprintln!();
        eprintln!(
            "退回     {} 条(其余 {} 条已经落盘,不用再发):",
            outcome.rejected.len(),
            outcome.applied
        );
        for item in &outcome.rejected {
            // `why` 里本来就带着段号(「第 N 段…」),别再拼一遍
            eprintln!("           {}", item.why);
        }
        bail!(
            "这批没全落下:{} 条被退(原因见上)。改完**只把这几条**再交一次",
            outcome.rejected.len()
        );
    }
    Ok(())
}

/// `vidbrief shot-check` —— 记一张图的看图结论(`record_shot_check`)。
fn cmd_shot_check(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{SHOT_CHECK_USAGE}");
        return Ok(());
    }
    let name = positional_project(args, "shot-check", "--json", &[], &[])?;
    let raw = take_input(args, "shot-check", "--json")?;
    let body: ShotCheckBody = serde_json::from_str(&raw).context("--json 解析失败")?;
    let shot_id = parse_shot_id(&body.shot_id)?;

    let (id, check) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let input = vidbrief_ops::ShotCheckInput {
            r#match: body.r#match,
            audio_visual_sync: body.audio_visual_sync,
            type_read: body.type_read.clone(),
            issues: body.issues.clone(),
            confidence: parse_confidence(&body.confidence),
            neighbor_text: body.neighbor_text.clone(),
            evidence_quote: body.evidence_quote.clone(),
        };
        let check = vidbrief_ops::record_shot_check(opened, shot_id.clone(), &input)?;
        Ok((opened.id().to_string(), check))
    })?;

    println!("项目     {id}");
    println!("截图     {}", body.shot_id);
    println!(
        "凭据     {}",
        match &check.evidence {
            Some(e) => format!("seg {}「{}」", e.segment, e.quote),
            None => "(没给 —— 门会要求补上)".to_string(),
        }
    );
    println!("记于     {}", check.checked_at.format("%Y-%m-%d %H:%M:%S"));
    Ok(())
}

/// `vidbrief supplements` —— 处置一批补图候选(`disposition_supplements`)。
fn cmd_supplements(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{SUPPLEMENTS_USAGE}");
        return Ok(());
    }
    let name = positional_project(args, "supplements", "--json", &[], &[])?;
    let raw = take_input(args, "supplements", "--json")?;
    let body: SupplementsBody = serde_json::from_str(&raw).context("--json 解析失败")?;

    let mut items = Vec::with_capacity(body.items.len());
    for it in body.items {
        let decision = match it.decision.as_str() {
            "need_shot" => vidbrief_ops::SupplementDecisionKind::NeedShot,
            "no_shot" => vidbrief_ops::SupplementDecisionKind::NoShot,
            other => bail!(
                "候选 #{} 的 decision 只能是 `need_shot` / `no_shot`,收到 `{other}`",
                it.id
            ),
        };
        items.push(vidbrief_ops::SupplementDecision {
            id: it.id,
            decision,
            note: it.note,
            t: it.t,
        });
    }

    let (id, outcome) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let outcome = vidbrief_ops::disposition_supplements(opened, &items)?;
        Ok((opened.id().to_string(), outcome))
    })?;

    println!("项目     {id}");
    println!("处置     {} 条,还剩 {} 条没结论", outcome.handled, outcome.pending_left);
    if !outcome.extracted.is_empty() {
        println!("新抽     {}", outcome.extracted.join("、"));
    }
    if !outcome.reused.is_empty() {
        println!("沿用     {}", outcome.reused.join("、"));
    }
    if !outcome.rejected.is_empty() {
        let ids: Vec<String> = outcome.rejected.iter().map(|i| format!("#{i}")).collect();
        println!("不用补   {}", ids.join("、"));
    }
    Ok(())
}

/// `vidbrief summary` —— 写总结(`write_summary`,**过步 2 的门**)。
fn cmd_summary(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{SUMMARY_USAGE}");
        return Ok(());
    }
    // 这一段自己认识的选项:`--title` / `--at` / `--verify`(值)与 `--md`(值)
    let mut title: Option<String> = None;
    let mut at: Option<f64> = None;
    let mut verify: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = split_option(&args[i]);
        match key {
            "--title" => title = Some(take_value(args, &mut i, inline, key)?),
            "--at" => {
                let raw = take_value(args, &mut i, inline, key)?;
                at = Some(
                    raw.parse::<f64>()
                        .with_context(|| format!("--at 要一个秒数,收到 `{raw}`"))?,
                );
            }
            "--verify" => verify.push(take_value(args, &mut i, inline, key)?),
            _ => {}
        }
        i += 1;
    }
    let name = positional_project(args, "summary", "--md", &["--title", "--at", "--verify"], &[])?;
    let title = title.context("要给 `--title`(这一段叫什么;报告页上那条的标题)")?;
    let md = take_input(args, "summary", "--md")?;

    let (id, outcome) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let input = vidbrief_ops::SectionInput {
            t: at,
            title: title.clone(),
            md: md.clone(),
            verify: verify.clone(),
        };
        let outcome = vidbrief_ops::write_summary(opened, &input)?;
        Ok((opened.id().to_string(), outcome))
    })?;
    println!("项目     {id}");
    println!(
        "{}   「{}」",
        if outcome.added { "新增一段" } else { "改写一段" },
        title
    );
    println!(
        "现有     {} 段(其中 {} 段上时间轴,{} 段是总述)",
        outcome.sections,
        outcome.anchored,
        outcome.sections - outcome.anchored
    );
    println!("段落     {}", outcome.sections_path.display());
    println!("文章     {}", outcome.article_path.display());
    Ok(())
}

/// `vidbrief report` —— 出报告(`build_report`:数据 + 两个投影)。
fn cmd_report(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{REPORT_USAGE}");
        return Ok(());
    }
    // `--format` / `--open` / `--self-contained` 是命令行上的便利;
    // 输入里也能给(形状与 MCP 工具一致)
    let mut format_flag: Option<String> = None;
    let mut open_flag = false;
    let mut contained_flag = false;
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = split_option(&args[i]);
        match key {
            "--format" => format_flag = Some(take_value(args, &mut i, inline, key)?),
            "--open" => open_flag = true,
            "--self-contained" => contained_flag = true,
            _ => {}
        }
        i += 1;
    }

    let name = positional_project(args, "report", "--json", &["--format"], &["--open", "--self-contained"])?;
    let raw = take_input(args, "report", "--json")?;
    let body: ReportBody = serde_json::from_str(&raw).context("--json 解析失败")?;

    let format = match format_flag.as_deref().or(body.format.as_deref()) {
        None => vidbrief_ops::ReportFormat::Both,
        Some(raw) => vidbrief_ops::ReportFormat::parse(raw)?,
    };
    // **命令行默认不起服务**:这个进程一退出服务就没了,印出来的地址就是死链。
    // 要那套"页面 + 视频回跳"的体验,显式 `--open`(常驻),或者用 `vidbrief ui`。
    let serving = if open_flag || body.open_browser.unwrap_or(false) {
        vidbrief_ops::Serving::On { open_browser: true }
    } else {
        vidbrief_ops::Serving::Off
    };
    // 单文件分享:图内嵌进页面,产物能单独发出去(视频那一半仍然要素材 + 服务)
    let images = if contained_flag || body.self_contained.unwrap_or(false) {
        vidbrief_report::ImageMode::Embedded
    } else {
        vidbrief_report::ImageMode::Relative
    };

    let input = vidbrief_ops::ReportInput {
        style: body.style.into_style()?,
    };

    let (id, outcome) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let outcome = vidbrief_ops::build_report(opened, &input, format, images, serving)?;
        Ok((opened.id().to_string(), outcome))
    })?;

    println!("项目     {id}");
    println!("知识点   {} 条", outcome.topics);
    if let Some(style) = &outcome.style {
        println!("风格     {style}");
    }
    println!(
        "总结     来自 {}",
        match outcome.summary_from {
            "input" => "输入里给的",
            _ => "03_summary/summary.md",
        }
    );
    println!("数据     {}", outcome.report_json.display());
    if let Some(md) = &outcome.report_md {
        println!("导出     {}", md.display());
    }
    if let Some(html) = &outcome.report_html {
        println!("页面     {}", html.display());
    }
    if outcome.self_contained {
        println!("单文件   图已内嵌(页面可单独发出去;视频那一半仍要素材 + 本机服务)");
    }
    print_stale_anchors(&outcome.stale_anchors);
    if !outcome.warnings.is_empty() {
        println!();
        println!(
            "留意     这页有 {} 处乱码字符(页面上是方块)—— 不拦落盘,列出来给人核:",
            outcome.warnings.len()
        );
        for one in &outcome.warnings {
            println!("         · {one}");
        }
    }
    if outcome.derived_segments > 0 {
        println!(
            "证据     {} 条的转录段是按锚点时刻就近取的(那一刻他说了什么)",
            outcome.derived_segments
        );
    }
    match &outcome.url {
        Some(url) => {
            println!("打开     {url}");
            println!();
            println!("按 Ctrl+C 结束(这个进程要一直活着,服务才能响应浏览器)。");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3600));
            }
        }
        None => {
            println!();
            println!("要看这一页(带视频回跳):`vidbrief ui {id} --view report`。");
        }
    }
    Ok(())
}

/// 报告引的图 / 段 / 话在当前材料里找不到了 —— 印一段,并说清怎么修。
///
/// **不是报错**:报告照渲染(页面是发现这件事的窗口),但读者要能看到它 ——
/// 一句"讲者原话"回去核却在转录里找不到,人就没法判断该信谁。
fn print_stale_anchors(stale: &[String]) {
    if stale.is_empty() {
        return;
    }
    println!();
    println!(
        "⚠ 这份报告有 {} 处锚点在**当前材料**里找不到了(材料在报告之后被改过):",
        stale.len()
    );
    for line in stale {
        println!("  · {line}");
    }
    println!(
        "  修法:让 agent 重出一份(`build_report` 整份替换)——\
         页面上这些地方也已经标出来了。"
    );
}

// ── 这几条命令的输入形状 ─────────────────────────────────────────────
//
// 形状和 MCP 工具的入参一致(少了 `project_id`,项目是位置参数)——
// 同一份 JSON,两条路都吃得下,agent 换路时不用重学。

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectBody {
    #[serde(default)]
    corrections: Vec<CorrectItem>,
    #[serde(default)]
    reviewed: Vec<u32>,
    #[serde(default)]
    reviewed_ranges: Vec<CorrectRange>,
}

/// 「审过、没问题」的一个区间(闭区间,按段 id)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectRange {
    from: u32,
    to: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectItem {
    segment_id: u32,
    outcome: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    note: Option<String>,
    /// 依据。**除 `punctuated` 外都要给** —— 那条判在工具那边(`check_items`),
    /// 这里不设成必填,免得断句条目得硬塞一句空话进去。
    #[serde(default)]
    reason: String,
}

impl CorrectItem {
    fn into_conclusion(self) -> Result<vidbrief_project::Conclusion> {
        use vidbrief_project::{Conclusion, CorrectionType};
        let kind = match self.kind.as_deref() {
            None => CorrectionType::Other,
            Some("homophone") => CorrectionType::Homophone,
            Some("terminology") => CorrectionType::Terminology,
            Some("number") => CorrectionType::Number,
            Some("other") => CorrectionType::Other,
            Some(other) => bail!(
                "第 {} 段的 `type` 只能是 homophone / terminology / number / other,收到 `{other}`",
                self.segment_id
            ),
        };
        Ok(match self.outcome.as_str() {
            // **缺件不在这里挡** —— 交给 `apply_corrections` **逐条**退(见那儿的注释):
            // 在这里一挡,整批几十条就都被拦在门口,而好的那些本来可以照落。
            "corrected" => Conclusion::Corrected {
                text: self.text.unwrap_or_default(),
                kind,
                note: self.note.unwrap_or_default(),
                reason: self.reason,
            },
            "punctuated" => Conclusion::Punctuated {
                text: self.text.unwrap_or_default(),
            },
            "kept" => Conclusion::Kept {
                note: self.note.unwrap_or_default(),
                reason: self.reason,
            },
            "uncertain" => Conclusion::Uncertain {
                note: self.note.unwrap_or_default(),
                reason: self.reason,
            },
            other => bail!(
                "第 {} 段的 `outcome` 只能是 corrected / punctuated / kept / uncertain,收到 `{other}`",
                self.segment_id
            ),
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ShotCheckBody {
    shot_id: String,
    r#match: bool,
    audio_visual_sync: bool,
    type_read: String,
    #[serde(default)]
    issues: Vec<String>,
    confidence: String,
    #[serde(default)]
    neighbor_text: Option<String>,
    #[serde(default)]
    evidence_quote: Option<String>,
}

/// `high` / `medium` / 其它(读作 `low`)。
fn parse_confidence(raw: &str) -> vidbrief_project::Confidence {
    match raw {
        "high" => vidbrief_project::Confidence::High,
        "medium" => vidbrief_project::Confidence::Medium,
        _ => vidbrief_project::Confidence::Low,
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplementsBody {
    items: Vec<SupplementItem>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SupplementItem {
    id: u32,
    decision: String,
    note: String,
    #[serde(default)]
    t: Option<f64>,
}

/// 报告数据。形状与 MCP 工具 `build_report` 的入参一致(少了 `project_id`)——
/// 同一份 JSON,两条路都吃得下。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportBody {
    /// **这份报告按什么口味做** —— 用户提示词里那一份要求的落点。必填。
    ///
    /// **内容不从这里进**:报告页由步 3 的分段(`03_summary/sections.json`)组装。
    style: ReportStyleBody,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    open_browser: Option<bool>,
    /// 单文件分享:图内嵌 base64(与 MCP 工具的 `self_contained` 同一个开关)。
    #[serde(default)]
    self_contained: Option<bool>,
}

/// 口味:风格轮廓 + 粒度(闭集合)+ 语气(自由文本)+ 补充要求。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportStyleBody {
    /// `quick` / `teaching` / `story` / `review` / `exec` / `custom`
    /// —— 轮廓怎么填见 `skill/styles.md`(或 `get_guide(step="styles")`)。
    preset: String,
    /// `brief` / `standard` / `detailed`。
    granularity: String,
    tone: String,
    #[serde(default)]
    notes: Option<String>,
}

impl ReportStyleBody {
    fn into_style(self) -> anyhow::Result<vidbrief_project::ReportStyle> {
        let preset = match self.preset.trim() {
            "quick" => vidbrief_project::ReportPreset::Quick,
            "teaching" => vidbrief_project::ReportPreset::Teaching,
            "story" => vidbrief_project::ReportPreset::Story,
            "review" => vidbrief_project::ReportPreset::Review,
            "exec" => vidbrief_project::ReportPreset::Exec,
            "custom" => vidbrief_project::ReportPreset::Custom,
            other => anyhow::bail!(
                "style.preset 只能是 quick(快扫版)/ teaching(教学版)/ story(故事版)/                  review(复盘版)/ exec(汇报版)/ custom(自定义),收到 `{other}`                 
轮廓怎么填看 skill/styles.md,或 get_guide(step=\"styles\")"
            ),
        };
        let granularity = match self.granularity.trim() {
            "brief" => vidbrief_project::Granularity::Brief,
            "standard" => vidbrief_project::Granularity::Standard,
            "detailed" => vidbrief_project::Granularity::Detailed,
            other => anyhow::bail!(
                "style.granularity 只能是 brief(概括)/ standard(标准)/ detailed(详细),收到 `{other}`"
            ),
        };
        Ok(vidbrief_project::ReportStyle {
            preset: Some(preset),
            granularity,
            tone: self.tone,
            notes: self.notes,
        })
    }
}



/// 截图 id:`s007` 或 `7`。
///
/// 解析只有一处(`vidbrief_ops::parse_shot_id`)—— 几个入口各写一份的话,
/// 同一个输入迟早得到几种说法。
fn parse_shot_id(raw: &str) -> Result<vidbrief_project::ShotId> {
    vidbrief_ops::parse_shot_id(raw)
}

/// 这个命令是**人**跑的,所以它本身就是"人签的字" —— 不需要谁来转达。
fn cmd_upgrade(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{UPGRADE_USAGE}");
        return Ok(());
    }
    let mut opts = vidbrief_ops::UpgradeOptions::default();
    let mut positional: Vec<String> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--fresh" => opts.fresh = true,
            "--accept-material" => opts.accept_material = true,
            other => positional.push(other.to_string()),
        }
    }
    let name = one_positional(&positional, "upgrade", "项目")?;

    let outcome = vidbrief_ops::upgrade(&name, &opts)?;

    println!("项目     {}", outcome.project_id);
    println!(
        "版本     {} → {}",
        outcome.from_version,
        vidbrief_project::SCHEMA_VERSION
    );
    println!("修订号   {}", outcome.rev);
    if outcome.note.is_none() {
        println!("收回来");
        println!("  看图结论   {} 张截图", outcome.shot_checks);
        println!("  转录核验   {} 段", outcome.segments);
        println!("  候选处置   {} 条", outcome.supplements);
        println!(
            "  用户拍板   {}",
            if outcome.approval { "有,搬过来了" } else { "无" }
        );
        if outcome.progress_moved {
            println!("  四步进度   从旧文件里搬过来了");
        }
    }
    if !outcome.content.is_empty() {
        println!("内容底账");
        for line in &outcome.content {
            println!("  {line}");
        }
    }
    if let Some(note) = &outcome.note {
        println!();
        println!("{note}");
    }
    println!();
    println!("材料一个字节都没动。接着跑 `vidbrief status {}` 看看现在是什么样。", name);
    Ok(())
}

fn cmd_approve(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{APPROVE_USAGE}");
        return Ok(());
    }

    let mut note: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--note" => {
                let value = args.get(i + 1).context("--note 缺参数值")?;
                note = Some(value.clone());
                i += 2;
            }
            other => {
                positional.push(other.to_string());
                i += 1;
            }
        }
    }
    let name = one_positional(&positional, "approve", "项目")?;

    // 开 → 取锁 → 做,这条序列走 ops 那一个入口(见 `vidbrief_ops::with_write`)。
    // 写锁降级放行的说明会交给闭包 —— 必须在动手**之前**说出去。
    let (id, approval) = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        if let Some(reason) = lock_note {
            eprintln!("警告:{reason}");
        }
        let approval = vidbrief_ops::approve(opened, note)?;
        Ok((opened.id().to_string(), approval))
    })?;
    let c = approval.counts;

    println!("项目     {id}");
    println!("确认于   {}", approval.approved_at.format("%Y-%m-%d %H:%M:%S"));
    println!(
        "确认时   未校准 {} 张 / 待裁决 {} 张 / 未处理补图候选 {} 条 / 存疑转录 {} 段",
        c.unchecked, c.needing_review, c.supplement_pending, c.uncertain
    );
    if let Some(n) = &approval.note {
        println!("理由     {n}");
    }
    println!();

    // 把"还欠着什么"摆在眼前 —— 门开是开了,但用户该知道自己是带着什么过的
    if c.unchecked > 0 || c.needing_review > 0 || c.supplement_pending > 0 || c.uncertain > 0 {
        println!("注意:上面这些还没处理完,你确认的是「就这样往下走」—— 理由已经记进项目状态。");
    }
    println!("门开了,可以写总结了。**之后任何一次改动都会让这次确认作废。**");
    Ok(())
}

fn cmd_preprocess(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{PREPROCESS_USAGE}");
        return Ok(());
    }
    let (name, opts, run_id) = parse_preprocess(args)?;

    // 运行记录(`vidbrief_ops::run`)那只手。它在**拿到写锁之后**才开 ——
    // 顺序有讲究:锁先到、记录后到,「一个项目同一时刻只有一份记录在写」就是
    // 锁保证的,不是靠运气。两趟同时起步时,输的那趟在锁上就被挡了,连记录都写不出来。
    let mut recorder: Option<vidbrief_ops::RunRecorder> = None;

    // 开 → 取锁 → 干活:这条序列走 ops 那**一个**入口(`vidbrief_ops::with_write`)。
    let outcome = vidbrief_ops::with_write(&name, None, |opened, lock_note| {
        let started = match &run_id {
            Some(id) => vidbrief_ops::RunRecorder::start_named(opened.layout(), &opts, id),
            None => vidbrief_ops::RunRecorder::start(opened.layout(), &opts),
        };
        match started {
            Ok(hand) => recorder = Some(hand),
            // 这个项目上已经有一趟在跑:不许开新的,把话说清楚
            Err(vidbrief_ops::StartError::Busy(why)) => bail!("{why}"),
            // 记录写不动**不许拦作业**:它是给别人看的一双眼睛
            Err(vidbrief_ops::StartError::Storage(e)) => {
                eprintln!("警告:这趟的运行记录写不了({e:#})—— 不影响跑,只是别人看不到进度。");
                recorder = None;
            }
        }

        // 写锁降级(文件系统不支持锁)那句话说在开工**之前**,记录里也留一份 ——
        // 作业跑在独立进程里时,agent 只看得到记录,看不到这行终端输出。
        if let (Some(recorder), Some(note)) = (&recorder, lock_note) {
            recorder.note_warning(note);
        }
        run_preprocess(opened, &name, &opts, lock_note, recorder.as_ref())
    });
    if let Err(error) = &outcome {
        // 还没收尾的(取锁失败、转录失败……)在这里收:记录里要留一句为什么。
        // 已经收过尾的(报告已经拿到手)不动 —— 那一趟跑到了终点,交代在报告里。
        if let Some(recorder) = &recorder {
            recorder.finish_err(&format!("{error:#}"));
        }
    }
    outcome
}

/// 真正的活。从 [`vidbrief_ops::with_write`] 的闭包里调 —— **锁已经拿在手上**。
///
/// `recorder` 是**运行记录**那只手:每换一步往上写一句。它是 `Option` ——
/// 记录开不出来(磁盘满 / 目录不可写)时作业照跑,只是没有记录可看。
fn run_preprocess(
    opened: &mut OpenProject,
    name: &str,
    opts: &PreprocessOptions,
    lock_note: Option<&str>,
    recorder: Option<&vidbrief_ops::RunRecorder>,
) -> Result<()> {
    // 文件系统不支持锁时它**不失败**,而是带着一句说明放行 —— 那句话必须说出来,
    // 否则用户就在不知情的情况下失去了"两个人同时改会互相覆盖"的保护。
    if let Some(reason) = lock_note {
        eprintln!("警告:{reason}");
    }

    println!("项目     {}", opened.id());
    println!(
        "源视频   {}  ({:.1}s)",
        opened.project().source.path,
        opened.meta().duration
    );
    println!();

    let mut last_print = Instant::now();
    let mut on_progress = |stage: Stage| {
        // ① 记录:换一步就往上写一句(抽帧那张数报细的)。
        //    `Stage` 是库里对外的进度语言,记录里的 `RunStage` 是**给别人读**的那套;
        //    两边在这一个 match 里对齐,别处不许再翻译一遍。
        if let Some(recorder) = recorder {
            match stage {
                Stage::Analyzing => recorder.stage(RunStage::Analyzing, "开始算差帧图", None),
                Stage::Analyzed(a) => recorder.stage(
                    RunStage::Shots,
                    format!("密集区 {} 个,开始抽关键截图", a.regions),
                    None,
                ),
                Stage::Shot { done, total } => recorder.stage(
                    RunStage::Shots,
                    format!("抽关键截图 {done}/{total}"),
                    Some((done, total)),
                ),
                Stage::Transcribing => {
                    recorder.stage(RunStage::Transcribing, "语音转文字(最久的一段)", None)
                }
                Stage::Rendering => recorder.stage(RunStage::Rendering, "渲染时间线", None),
            }
        }

        // ② 终端:人就在跟前,照旧打细的
        match stage {
            Stage::Analyzing => eprint!("  分析     抽帧 + 逐帧评分..."),
            Stage::Analyzed(a) => {
                eprintln!();
                println!(
                    "差帧图   {} 个采样点({}fps),分数最高 {:.3}",
                    a.curve_points, a.fps, a.max_score
                );
                println!(
                    "密集区   {} 个  阈值 {:.3}(中位数 {:.3} × {} 与下限 {} 取大)",
                    a.regions, a.mass_threshold, a.mass_median, a.mass_k, a.mass_floor
                );
            }
            Stage::Shot { done, total } => {
                // 抽帧是有 IO 的,长视频上几十张要好几秒 —— 给点进度别让人以为卡住了
                if done == total || last_print.elapsed().as_millis() > 300 {
                    eprint!("\r  抽帧     {done}/{total}");
                    last_print = Instant::now();
                }
            }
            // 转录是最久的一段,而它**没有细粒度进度**可报 —— 说清这一点,
            // 免得人对着不动的屏幕以为卡死了
            Stage::Transcribing => {
                eprintln!("\n  转录     正在跑 whisper(最久的一段,没有中间进度)")
            }
            Stage::Rendering => eprintln!("  视图     渲染时间线..."),
        }
    };

    let report = vidbrief_ops::preprocess(opened, opts, &mut on_progress)?;
    if opts.shots {
        eprintln!();
    }

    // 报告一拿到手就写进记录:**它是这一趟的交代**,MCP 那边的「取结果」靠它。
    // 注意 `stage = done` 说的是**这一趟跑完了**,不是「步 1 完成了」——
    // 后者看报告里的 `done`(转录失败时这里是 true、那里是 false)。
    if let Some(recorder) = recorder {
        recorder.finish_ok(&report);
    }

    match &report.shots {
        // 沿用上次那批:说清"一张没重抽" —— 它意味着那批看图结论一条没作废
        Some(count) if report.reuse.visual => {
            println!("截图     {count} 张(沿用上次那批,没有重抽)")
        }
        Some(count) => {
            println!("截图     {count} 张 → {}", opened.layout().shots_dir().display());
            if !report.shots_skipped.is_empty() {
                println!("         其中 {} 张失败(已跳过):", report.shots_skipped.len());
                for item in report.shots_skipped.iter().take(3) {
                    println!("           t={:.1}s  {}", item.t, item.reason);
                }
            }
        }
        None => println!("截图     已跳过(--no-shots)"),
    }

    match &report.transcript {
        TranscriptOutcome::Done {
            segments,
            model,
            language,
            ..
        } => {
            if report.reuse.transcript {
                println!("转录     {segments} 段,模型 {model} ({language})(沿用上次那份,没有重跑)")
            } else {
                println!("转录     {segments} 段,模型 {model} ({language})")
            }
        }
        // 没有音轨是**合法**的空结果,不算失败
        TranscriptOutcome::NoAudio => println!("转录     源视频没有音轨,跳过"),
        TranscriptOutcome::Skipped => println!("转录     已跳过(--no-transcribe)"),
        TranscriptOutcome::Failed { error } => {
            eprintln!("转录     失败:{error}");
            // **为什么这一趟要重转** —— 失败的时候最需要这句话,而它平时挂在下面
            // (成功路径里才打)。实测踩过:CLI 少带了 `VIDBRIEF_WHISPER_MODEL`,
            // 解析到默认候选(medium)、判定"要重转录",随后失败 —— 而屏幕上只有
            // "转录失败",那句"模型 small → medium"压根没显示出来。
            if let Some(why) = &report.reuse.transcript_why_not {
                eprintln!("         转录重跑:{why}");
            }
            // 差帧图与截图保留,但进度不标完成 —— 重跑一次就能补上
            eprintln!();
            eprintln!("差帧图与截图已保留;转录未完成,**未**标记步 1 为完成。");
            eprintln!("修复后重跑:`vidbrief preprocess {name}`");
            bail!("步 1 未完成:转录失败");
        }
    }

    match (&report.workbench, &report.workbench_error) {
        (Some(path), _) => println!("工作台   {}", path.display()),
        (None, Some(error)) => eprintln!("工作台   渲染失败(材料不受影响):{error}"),
        (None, None) => {}
    }

    // 增量重跑:没沿用哪半、为什么。静默作废别人的活最不该干 —— 静默复用也一样,
    // 所以两种结局都要摆出来。显式跳过(`--no-shots` / `--no-transcribe`)不必重复说。
    if opts.shots {
        if let Some(why) = &report.reuse.visual_why_not {
            println!("         画面重算:{why}");
        }
    }
    if opts.transcribe {
        if let Some(why) = &report.reuse.transcript_why_not {
            println!("         转录重跑:{why}");
        }
    }
    if report.reuse.source_changed {
        let short = report
            .reuse
            .source_sha256
            .get(..16)
            .unwrap_or(&report.reuse.source_sha256);
        println!(
            "         ⚠ 源视频内容跟 project.json 里记的对不上(现在算出来 sha256 {short}…)——\
             硬链接的源文件被就地改过?材料该重算的都重算了,但这事得让你知道"
        );
    }

    println!();
    println!("完成。下一步:`vidbrief ui {name}` 打开时间线,或让 agent 看图校准");
    Ok(())
}

// ── status ───────────────────────────────────────────────────────────

struct StatusArgs {
    project: String,
    verify: bool,
}

fn parse_status(args: &[String]) -> Result<StatusArgs> {
    let mut project: Option<String> = None;
    let mut verify = false;
    for arg in args {
        match arg.as_str() {
            "-V" | "--verify" => verify = true,
            other if is_option(other) => bail!("status 不认识参数 `{other}`(见 `vidbrief status -h`)"),
            _ => {
                if project.is_some() {
                    bail!("status 只接受一个项目,多余的参数:`{arg}`");
                }
                project = Some(arg.clone());
            }
        }
    }
    let project =
        project.context("用法:vidbrief status <project> [--verify](见 `vidbrief status -h`)")?;
    Ok(StatusArgs { project, verify })
}

fn cmd_status(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{STATUS_USAGE}");
        return Ok(());
    }
    let parsed = parse_status(args)?;
    let report = vidbrief_ops::inspect(
        &parsed.project,
        &vidbrief_ops::InspectOptions {
            verify: parsed.verify,
        },
    )?;

    let project = &report.project;
    let meta = &report.meta;
    println!("项目     {}", project.id);
    println!("  名称   {}", project.name);
    println!("  目录   {}", display_dir(&report.dir));
    println!("  建立   {}", project.created_at);
    // **报读到的那一版**(不是本工具认的那一版):旧项目要如实说自己是旧项目
    let version_note = if report.schema_version == vidbrief_project::SCHEMA_VERSION {
        String::new()
    } else {
        format!("(本工具认 v{})", vidbrief_project::SCHEMA_VERSION)
    };
    println!(
        "  格式   schema v{} / rev {}{version_note}",
        report.schema_version, report.rev
    );
    if let Some(glossary) = &project.glossary {
        println!("  术语表 {} 条", glossary.len());
    }

    println!();
    println!("源视频   {}", project.source.path);
    println!("  原始   {}", project.source.original);
    println!(
        "  引入   {}",
        project.source.link.describe(meta.size_bytes)
    );
    println!("  大小   {}", human_size(meta.size_bytes));
    let res = match (meta.width, meta.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "未知".to_string(),
    };
    let fps = meta
        .fps
        .map(|f| format!("{f:.3}"))
        .unwrap_or_else(|| "?".into());
    println!(
        "  规格   {:.1}s  {res} @ {fps} fps  {}",
        meta.duration,
        meta.codec.as_deref().unwrap_or("编码未知")
    );
    println!("  指纹   sha256:{}", meta.sha256);

    println!();
    println!("健康");
    print_health(&report.link_health);
    if let Some(health) = &report.content_health {
        print_health(health);
    }

    println!();
    println!("进度");
    let p = &report.progress;
    println!(
        "  ① 前处理   {}",
        step_text(p.preprocess.done, p.preprocess.at.map(|t| t.to_string()))
    );
    println!("  ② 校准     {} 轮", p.calibrate.rounds);
    println!(
        "  ③ 总结     {}",
        step_text(p.summarize.done, p.summarize.at.map(|t| t.to_string()))
    );
    println!(
        "  ④ 报告     {}",
        step_text(p.report.done, p.report.at.map(|t| t.to_string()))
    );

    println!();
    println!("材料");
    print_materials(&report.materials);

    // 外挂那一栏**不进硬伤判定**(见 `InspectReport::unhealthy` 的注释):
    // 没装 ffmpeg 不该让"看项目状态"这件事失败 —— 只读材料照样能读。
    println!();
    println!("外挂");
    print_tools(&report.tools);

    // 进度与产物对不上时点一句:④ 标着完成、而 04_report/ 里一份产物都没有。
    //
    // M6 联调实测的真场面:agent 看到这个组合,先说了句"报告 done 但文件不在",
    // 然后**多花了一步** glob 去确认。进度记的是"这一步做过"(见 §4.10),
    // 产物在不在看文件 —— 两件事都对,但摆在一起才看得出发生了什么。
    let m = &report.materials;
    if p.report.done && !(m.report_json || m.report_md || m.report_html) {
        println!();
        println!(
            "⚠️ ④ 报告标着完成,但 04_report/ 下没有产物 ——\
             进度记的是'这一步做过',产物在不在看文件(可能被拿掉了,或者在别处出的)。"
        );
    }

    // 有硬伤时用非零退出码,方便脚本和 agent 判断
    if report.unhealthy() {
        bail!("项目存在问题(见上面标 ✗ 的行)");
    }
    Ok(())
}

/// 三个外挂(ffmpeg / ffprobe / whisper)现在什么样。
///
/// **为什么要打这一栏**:环境齐不齐,从前只有跑到 `init` / `preprocess` 才知道,
/// 而"看项目现在什么状态"这条命令正是该说话的地方 ——
/// 没装、装了个不认识的版本,是两条不同的修法。
///
/// 教训来自实测:一个过期二进制 / 没配好的 whisper,能让一整轮联调白跑掉,
/// 而当时 `status` 一声不吭(它只看项目,不看环境)。
fn print_tools(tools: &[vidbrief_ops::ToolView]) {
    for tool in tools {
        match &tool.version {
            Some(version) => println!("  ✓ {version}"),
            // 用 ⚠ 而不是 ✗:那是**环境缺件**,不是项目硬伤 —— 退出码也不因它变
            //(只读材料照样读得动)。两个符号混着用,看的人会以为项目坏了。
            None => println!(
                "  ⚠ {} 跑不起来(会跑的是 `{}`)—— 设 {} 指向它,或把它放进 PATH",
                tool.name, tool.path, tool.env
            ),
        }
        if let Some(note) = &tool.note {
            println!("      {note}");
        }
    }
}

fn print_health(health: &vidbrief_ops::HealthView) {    let mut lines = health.message.lines();
    if let Some(first) = lines.next() {
        println!("  {} {first}", health.marker());
    }
    // Problem 的消息可能带换行(如哈希对照),续行缩进对齐
    for extra in lines {
        println!("    {extra}");
    }
}

/// 材料盘点。**给人看的是一眼能看出缺什么**,所以"没做"也要列出来。
fn print_materials(m: &vidbrief_ops::Materials) {
    let count = |v: Option<usize>| match v {
        Some(n) => n.to_string(),
        None => "—".to_string(),
    };
    println!("  差帧图   {} 个采样点", count(m.curve_points));
    println!("  密集区   {} 个", count(m.regions));
    match m.shots {
        // 「值得再看」是**视图口径**(把握低 / 报了问题 / 图文不符),不是门那个
        // 「待裁决」(模型自己说没把握)—— 同名不同义,这里绝不能写「待裁决」,
        // 否则读的人会以为门是关着的(M5 联调实测:门开着,这里却报「待裁决 1」)。
        Some(s) => println!(
            "  截图     {} 张(留用 {},未校准 {},值得再看 {})",
            s.total, s.keep, s.unchecked, s.needs_look
        ),
        None => println!("  截图     —"),
    }
    println!("  转录     {} 段", count(m.transcript_segments));
    if let Some(fixed) = m.concluded_segments {
        println!("  纠错     {fixed} 段有标记");
    }
    if let Some(total) = m.supplement_total {
        println!(
            "  补图     {total} 条候选({} 条待判断)",
            m.supplement_pending.unwrap_or(0)
        );
    }
    println!(
        "  工作台   {}",
        if m.workbench_html { "已渲染" } else { "—" }
    );

    // 报告:判据(report.json)与两个投影分开数 —— "出过报告"与"文件还在不在"是两件事
    // (M6 联调:agent 看到 ④✓ 而目录空着,只能再花一步自己去 glob)
    let have: Vec<&str> = [
        ("json", m.report_json),
        ("html", m.report_html),
        ("md", m.report_md),
    ]
    .into_iter()
    .filter(|(_, present)| *present)
    .map(|(name, _)| name)
    .collect();
    println!(
        "  报告     {}",
        if have.is_empty() {
            "—".to_string()
        } else {
            format!("已出({})", have.join("、"))
        }
    );

    // 状态文件那两栏:**读不动要说出来**,不能让它混在"一切正常"里
    if let Some(note) = &m.state_note {
        println!();
        println!("⚠️ 状态");
        for line in note.lines() {
            println!("  {line}");
        }
    }
    if !m.content_stale.is_empty() {
        println!();
        println!("✗ 材料内容跟底账对不上的地方({} 处)", m.content_stale.len());
        for line in m.content_stale.iter().take(8) {
            println!("  {line}");
        }
        println!("  (门会为此拦下来 —— 门的『还有哪些没核验』是从这几份文件里数的)");
    }
    if !m.projection_stale.is_empty() {
        println!();
        println!("⚠️ 明文材料与状态对不上的地方({} 处)", m.projection_stale.len());
        for line in m.projection_stale.iter().take(5) {
            println!("  {line}");
        }
        println!("  (明文那几栏只是投影,门读的是状态,所以骗不过去;下一次写这份材料的工具调用会把它盖回去)");
    }
}

fn step_text(done: bool, at: Option<String>) -> String {
    match (done, at) {
        (true, Some(at)) => format!("✓ {at}"),
        (true, None) => "✓".to_string(),
        (false, _) => "— 未做".to_string(),
    }
}

fn display_dir(path: &Path) -> String {
    let mut text = path.display().to_string();
    if !text.ends_with(['/', '\\']) {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

// ── ui ───────────────────────────────────────────────────────────────

fn cmd_ui(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{UI_USAGE}");
        return Ok(());
    }
    let mut open_browser = true;
    let mut view = "workbench";
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--no-open" => open_browser = false,
            "--view" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    bail!("--view 后面要跟视图名(workbench / report)");
                };
                view = match v.as_str() {
                    "workbench" => "workbench",
                    "report" => "report",
                    other => bail!("不认识视图 `{other}` —— 可选:workbench(工作台,一页看全部)/ report(纯图文报告页)"),
                };
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }
    let name = one_positional(&positional, "ui", "项目")?;

    let opened = OpenProject::open(&name)?;
    let (path, url, stale) = match view {
        "report" => vidbrief_ops::serve_and_open_report(&opened, open_browser)?,
        _ => {
            let (path, url) = vidbrief_ops::serve_and_open_workbench(&opened, open_browser)?;
            (path, url, Vec::new())
        }
    };

    println!("视图     {}", path.display());
    println!("服务     {url}");
    if !open_browser {
        println!("         (--no-open:请手动打开上面的地址)");
    }
    print_stale_anchors(&stale);
    println!();
    match view {
        "report" => println!(
            "纯图文总结:每条知识点带时刻、正文、来源与关联的转录原话 —— \
             这一页可以单独保存 / 发给别人(视频那一半带不走)。"
        ),
        _ => println!("时间轴上点/拖任意位置即可跳转视频;空格播放暂停,←→ ±1 秒(Shift ±10 秒)。"),
    }

    // 服务是后台线程,主线程不能立刻退出 —— 否则服务跟着进程一起没了
    println!();
    println!("按 Ctrl+C 结束(这个进程要一直活着,服务才能响应浏览器)。");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

// ── serve ────────────────────────────────────────────────────────────

const SERVE_USAGE: &str = "vidbrief serve —— 本地查看服务(独立进程)

Usage:
  vidbrief serve [--port <N>] [--mount <id>=<目录>]...     # 起一份,前台跑着
  vidbrief serve --stop [<项目>]                          # 关掉在跑的服务

Options:
  --port <N>          监听端口。默认 0 = 由操作系统分配(不撞车)。
                      起好之后 stdout 只打印**一行**握手 JSON:{\"pid\":…,\"port\":…}
                      —— 打印完 stdout 就不再用了(调用方拿它当协议通道)。
  --mount <id>=<目录> 起手就挂一个项目(可重复)。之后还能通过控制面
                      `POST /@mount` 动态挂 —— MCP 会话就是这么做的。
      --stop [<项目>] 关掉在跑的服务:**这是「关掉」的正门**。
                      不给项目 = 关掉本机所有在跑的服务;
                      给了项目 = 只关挂着它的那些(按目录或项目 id 认)。
                      走的是 `POST /@shutdown`,不是「按端口找进程杀」。
  -h, --help          显示本条用法

说明:
  **为什么服务要能独立成一个进程**:从前它跑在工具进程里,「关掉页面」等于关掉
  那个进程 —— 而它可能就是 agent 的会话进程。独立之后,`--stop`(或页面上的关闭)
  只关服务自己,**不连坐**。

  在跑的服务各自登记在 `<临时目录>/vidbrief-view/<pid>.json`(端口、挂着哪些项目),
  `--stop` 靠它发现。登记表不进项目目录 —— 项目里只该有材料。
";

fn cmd_serve(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{SERVE_USAGE}");
        return Ok(());
    }

    let mut port: u16 = 0;
    let mut mounts: Vec<(String, std::path::PathBuf)> = Vec::new();
    let mut stop: Option<String> = None;
    let mut watch_stdin = false;
    let mut i = 0;
    while i < args.len() {
        let (key, inline) = split_option(&args[i]);
        match key {
            "--port" => {
                let value = take_value(args, &mut i, inline, key)?;
                port = value
                    .parse()
                    .with_context(|| format!("--port 要是端口号,收到 `{value}`"))?;
            }
            "--mount" => {
                let value = take_value(args, &mut i, inline, key)?;
                let (id, dir) = value
                    .split_once('=')
                    .with_context(|| format!("--mount 的写法是 <id>=<目录>,收到 `{value}`"))?;
                mounts.push((id.to_string(), std::path::PathBuf::from(dir)));
            }
            "--watch-stdin" => watch_stdin = true,
            "--stop" => {
                // `--stop` 后面**可以**跟一个项目(也可以不跟):用 inline 值,
                // 否则看下一个参数像不像选项再决定吃不吃
                stop = Some(match inline {
                    Some(v) => v,
                    None => match args.get(i + 1) {
                        Some(next) if !is_option(next) => {
                            i += 1;
                            next.clone()
                        }
                        _ => String::new(),
                    },
                });
            }
            other => bail!("serve 不认识参数 `{other}`(见 `vidbrief serve -h`)"),
        }
        i += 1;
    }

    if let Some(project) = stop {
        return stop_servers(&project);
    }

    let port = vidbrief_viz::run_in_this_process(port, mounts)?;
    vidbrief_viz::print_handshake(port)?;

    if watch_stdin {
        // 父进程活着 = 这条管道开着。它一走(或自己退出),我们这边读到 EOF,跟着收工
        // (顺手把登记表里自己那一份收掉 —— 退出走的是 `process::exit`,析构不会跑)。
        vidbrief_viz::watch_parent_on_stdin();
    }

    // 握手行发出去之后 stdout 不再使用(调用方把它当协议通道读一行就丢)
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// `serve --stop`:按登记表找到在跑的服务,走 `POST /@shutdown` 让它们自己收工。
fn stop_servers(project: &str) -> Result<()> {
    let dir = vidbrief_viz::registry_dir();
    if !dir.is_dir() {
        println!("没有在跑的查看服务(登记目录 {} 不在)", dir.display());
        return Ok(());
    }
    // 给了项目就只关挂着它的那些:按目录或项目 id 认
    let want = if project.is_empty() {
        None
    } else {
        Some(
            std::path::Path::new(project)
                .canonicalize()
                .unwrap_or_else(|_| std::path::PathBuf::from(project)),
        )
    };

    let mut stopped = 0usize;
    let mut skipped = 0usize;
    let mut stale = 0usize;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(status) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let port = status.get("port").and_then(|p| p.as_u64()).unwrap_or(0) as u16;
        let pid = status.get("pid").and_then(|p| p.as_u64()).unwrap_or(0);
        if port == 0 {
            let _ = std::fs::remove_file(&path);
            stale += 1;
            continue;
        }

        if let Some(want) = &want {
            let hit = status["mounts"].as_array().is_some_and(|list| {
                list.iter().any(|m| {
                    let root = m.get("root").and_then(|r| r.as_str()).unwrap_or("");
                    let id = m.get("id").and_then(|r| r.as_str()).unwrap_or("");
                    std::path::Path::new(root)
                        .canonicalize()
                        .map(|c| &c == want)
                        .unwrap_or(false)
                        || std::path::Path::new(id) == want.as_path()
                })
            });
            if !hit {
                skipped += 1;
                continue;
            }
        }

        match vidbrief_viz::post_control(port, "@shutdown", &serde_json::json!({})) {
            Ok(_) => {
                println!("已关掉     pid {pid} · 端口 {port}");
                stopped += 1;
            }
            Err(e) => {
                // 端口没人应:那一份是**陈旧登记**(进程早没了),清掉它
                println!("已经不在   pid {pid} · 端口 {port}({e:#})");
                let _ = std::fs::remove_file(&path);
                stale += 1;
            }
        }
    }

    match (stopped, skipped, stale) {
        (0, 0, 0) => println!("没有在跑的查看服务"),
        (0, n, _) if n > 0 => println!("没有挂着这个项目的服务(另有 {n} 份在跑,没动它们)"),
        _ => println!("关掉 {stopped} 份(stale {stale} 份已清账)"),
    }
    Ok(())
}

// ── mcp ──────────────────────────────────────────────────────────────

fn cmd_mcp(args: &[String]) -> Result<()> {
    if wants_help(args) {
        print!("{MCP_USAGE}");
        return Ok(());
    }
    for arg in args {
        match arg.as_str() {
            // 现在只有 stdio 一种;写出来是为了将来加 HTTP 时不用改调用方的脚本
            "--stdio" => {}
            other => bail!("mcp 不认识参数 `{other}`(见 `vidbrief mcp -h`)"),
        }
    }
    // 从这里开始 stdout 归 JSON-RPC 协议所有
    vidbrief_mcp::serve_stdio()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vidbrief_project::SourceLink;

    #[test]
    fn human_size_picks_a_readable_unit() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(1_953_106), "1.9 MB");
        assert_eq!(human_size(3_221_225_472), "3.00 GB");
    }

    #[test]
    fn link_note_does_not_call_a_hardlink_a_copy() {
        // 这条文案曾经让人以为"硬链接"等于"复制了一份文件",措辞必须区分清楚
        let hard = link_note(SourceLink::Hardlink, 1000);
        assert!(hard.contains("硬链接"), "{hard}");
        assert!(hard.contains("不占额外磁盘"), "{hard}");
        assert!(!hard.contains("占用"), "硬链接不该说'占用空间':{hard}");

        let copy = link_note(SourceLink::Copy, 1_953_106);
        assert!(copy.contains("复制") && copy.contains("1.9 MB"), "{copy}");

        let sym = link_note(SourceLink::Symlink, 0);
        assert!(sym.contains("失效"), "软链接必须说清风险:{sym}");
    }

    #[test]
    fn step_text_distinguishes_never_done_from_done_without_a_time() {
        assert_eq!(step_text(false, None), "— 未做");
        assert_eq!(step_text(true, None), "✓");
        assert_eq!(step_text(true, Some("2026-09-24".into())), "✓ 2026-09-24");
    }

    #[test]
    fn option_splitting_only_treats_leading_dashes_as_options() {
        assert_eq!(split_option("--fps=2"), ("--fps", Some("2".into())));
        assert_eq!(split_option("-f=2"), ("-f", Some("2".into())));
        assert_eq!(split_option("--fps"), ("--fps", None));
        // 单独的 `-` 是普通参数(有人真会传它当"标准输入")
        assert_eq!(split_option("-"), ("-", None));
        // 等号左边不是选项时,整串当位置参数
        assert_eq!(split_option("a=b"), ("a=b", None));
    }
}
