//! **写操作的两条路要走同一道门。**
//!
//! 第五轮实测:agent 把自己的工具进程一起关掉了,之后写报告、记结论全都没路可走 ——
//! 它只好手写文件、手改进度。这就是这几条命令存在的理由:每个写动作都得有一条
//! 不经过 MCP 的入口,而且走的是**同一套 `ops` 代码、同一把锁、同一个门**。
//!
//! 这里验的就是"同一道门":命令行版照样会被审核门挡住,照样把结论落到同一个地方。

use std::io::Write;
use std::process::{Command, Stdio};

use vidbrief_ops::testkit::Fake;

/// 跑一条命令,把 `input` 喂给它的 stdin。
fn run(args: &[&str], input: &str) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vidbrief"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("无法启动 vidbrief");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("写 stdin");
    let out = child.wait_with_output().expect("等子进程");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn the_command_line_twin_writes_through_the_same_door() {
    let fake = Fake::new("twin-write")
        .with_transcript(&[
            (0.0, 2.0, "这是国内派森教程的第一小节"),
            (2.0, 4.0, "各位好,看这里的数字"),
        ])
        // 一张很远的图:让这一组(两段连成一气)成为候选,门才有东西可拦。
        // 候选的判据是**指代**(「看这里」),不再是「附近没图」——
        // 后者实测把 95~99% 的段都报成了候选、而 agent 一条都没补([260927-16])。
        .with_shot(1, 30.0);
    let proj = fake.layout().root().to_string_lossy().into_owned();
    let pending = vidbrief_ops::find_supplement_candidates(&mut fake.open(), 3.0, false)
        .unwrap()
        .pending()
        .count();
    assert!(pending > 0, "这段材料该出候选,不然门测不出东西");

    // ① 写回一条结论 —— 形状和 MCP 工具那份一模一样
    let body = r#"{
      "corrections": [
        {
          "segment_id": 1,
          "outcome": "corrected",
          "text": "这是国内 Python 教程的第一小节",
          "type": "terminology",
          "note": "派森 → Python",
          "reason": "画面上的标题写着「Python 教程」"
        }
      ],
      "reviewed": [2]
    }"#;
    let (ok, stdout, stderr) = run(&["correct", &proj, "--json", "-"], body);
    assert!(ok, "correct 失败了:\n{stderr}");
    assert!(stdout.contains("改了 1 段"), "{stdout}");
    assert!(stdout.contains("合计动过 1 段"), "{stdout}");

    let fixed = vidbrief_project::Transcript::load(&fake.layout().transcript_fixed()).unwrap();
    assert_eq!(
        fixed.segments[0].effective_text(),
        "这是国内 Python 教程的第一小节"
    );
    assert_eq!(fixed.stats().corrected, 1);
    assert_eq!(fixed.stats().clean, 1, "第 2 段是 reviewed 报上来的");

    // ② 门:命令行版**照样**拦 —— 这正是"同一条路"的意思
    let (ok, _, stderr) = run(
        &["summary", &proj, "--title", "总结", "--md", "-"],
        "# 总结\n",
    );
    assert!(!ok, "门没拦:命令行版绕过去了?");
    assert!(stderr.contains("补图候选"), "门该把三类入口都说出来:{stderr}");

    // ③ 报告也有门 —— 它是**步 3 那道**:总结还没写,报告不许出
    let (ok, _, stderr) = run(
        &["report", &proj, "--json", "-"],
        r#"{"style":{"preset":"quick","granularity":"standard","tone":"大白话"}}"#,
    );
    assert!(!ok, "总结都没写,报告不该出得来");
    assert!(stderr.contains("还没有总结"), "{stderr}");
    assert!(!fake.layout().report_json().is_file(), "被挡住就不该落文件");

    // ④ 形式错误当场退回,不留半成品
    let (ok, _, stderr) = run(
        &["correct", &proj, "--json", "-"],
        r#"{"corrections":[{"segment_id":2,"outcome":"corrected","text":"各位好,看这里的数字","reason":"x"}]}"#,
    );
    assert!(!ok);
    assert!(stderr.contains("一字不差"), "{stderr}");

    let (ok, _, stderr) = run(&["correct", &proj, "--json", "-"], r#"{"corrections":[]}"#);
    assert!(!ok, "什么都没给也该退回");
    assert!(
        stderr.contains("至少给一个") || stderr.contains("没有要写的东西"),
        "{stderr}"
    );
}

/// 出报告那条命令行入口:数据落在同一个地方,而且**过同一道门**。
#[test]
fn the_command_line_twin_builds_a_report_through_the_same_gate() {
    let fake = Fake::new("twin-report")
        .with_transcript(&[(0.0, 2.0, "各位好,今天讲 Python 列表"), (2.0, 4.0, "看这一段结构图")])
        .with_curve(5)
        .with_shot(1, 2.0);
    let proj = fake.layout().root().to_string_lossy().into_owned();

    // 步 2 的活走命令行干完:看图结论 + 转录核验(审过、没问题的也要报)
    let (ok, _, stderr) = run(
        &["shot-check", &proj, "--json", "-"],
        r#"{"shot_id":"s001","match":true,"audio_visual_sync":true,
            "type_read":"Python 列表 结构","confidence":"high",
            "evidence_quote":"各位好,今天讲 Python 列表"}"#,
    );
    assert!(ok, "shot-check 失败了:\n{stderr}");
    let (ok, _, stderr) = run(&["correct", &proj, "--json", "-"], r#"{"reviewed":[1,2]}"#);
    assert!(ok, "correct 失败了:\n{stderr}");

    // 写总结 —— 走工具(命令行版),一段一段交,底账也就记上了
    let (ok, _, stderr) = run(
        &["summary", &proj, "--title", "总述", "--md", "-"],
        "# 总述\n\n两句话讲完。\n",
    );
    assert!(ok, "summary 失败了:\n{stderr}");
    let (ok, _, stderr) = run(
        &["summary", &proj, "--title", "切到解释器主窗口", "--at", "2", "--md", "-"],
        "讲者从大纲切到解释器主窗口。\n",
    );
    assert!(ok, "summary(带锚点的段)失败了:\n{stderr}");

    // 出报告:只有口味 —— 内容由步 3 的段落组装
    let body = r#"{
      "style": {"preset": "teaching", "granularity": "detailed", "tone": "大白话", "notes": "给刚入门的人看"}
    }"#;
    let (ok, stdout, stderr) = run(&["report", &proj, "--json", "-"], body);
    assert!(ok, "report 失败了:\n{stderr}");
    assert!(stdout.contains("知识点   1 条"), "{stdout}");
    assert!(stdout.contains("风格     教学版 · 详细 · 大白话"), "{stdout}");
    assert!(stdout.contains("summary.md"), "总结该取步 3 那一份:{stdout}");
    assert!(stdout.contains("--view report"), "该指出去哪看:{stdout}");
    assert!(fake.layout().report_json().is_file());
    assert!(fake.layout().report_md().is_file());
    assert!(fake.layout().report_html().is_file());

    let report = vidbrief_project::Report::load(&fake.layout().report_json()).unwrap();
    assert_eq!(report.topics.len(), 1);
    assert!(report.summary.contains("两句话讲完"), "{}", report.summary);
    // 段号是工具**就近取的**(这条没给 `segments`)—— t=2.0 正落在第 2 段上
    assert_eq!(report.topics[0].segments, vec![2]);
    // 口味落在数据里(与 MCP 工具同一个字段、同一个必填口径)
    assert_eq!(
        report.style.as_ref().map(vidbrief_project::ReportStyle::describe),
        Some("教学版 · 详细 · 大白话".to_string())
    );

    // 同一条路:总结被绕过工具改过,命令行版照样拦
    std::fs::write(fake.layout().summary_md(), "# 手改的\n").unwrap();
    let (ok, _, stderr) = run(&["report", &proj, "--json", "-"], body);
    assert!(!ok, "手改过的总结该被拦下");
    assert!(stderr.contains("不是工具写出去的那一份"), "{stderr}");
}

/// 候选处置那条命令行入口:形状一样,结论一样落在材料上。
#[test]
fn the_command_line_twin_disposes_supplement_candidates() {
    let fake = Fake::new("twin-supp")
        .with_transcript(&[(0.0, 2.0, "大家看一下这张图"), (2.0, 4.0, "还有这里")]);
    let proj = fake.layout().root().to_string_lossy().into_owned();

    // 先让算法列出候选(那一步的入口在 MCP 那边;项目文件是共享的)
    let list = vidbrief_ops::find_supplement_candidates(&mut fake.open(), 3.0, false).unwrap();
    assert!(!list.requests.is_empty());
    let ids: Vec<String> = list.requests.iter().map(|r| format!("#{}", r.id)).collect();

    let items: Vec<String> = list
        .requests
        .iter()
        .map(|r| {
            format!(
                r#"{{"id":{},"decision":"no_shot","note":"旁边就有图,判定半径太紧"}}"#,
                r.id
            )
        })
        .collect();
    let body = format!(r#"{{"items":[{}]}}"#, items.join(","));

    let (ok, stdout, stderr) = run(&["supplements", &proj, "--json", "-"], &body);
    assert!(ok, "supplements 失败了:\n{stderr}");
    assert!(stdout.contains("还剩 0 条没结论"), "{stdout}");
    assert!(stdout.contains("不用补"), "该把判过「不补」的号报出来:{stdout}\n{ids:?}");

    let after = vidbrief_ops::supplements(&fake.open()).unwrap().unwrap();
    assert_eq!(after.pending().count(), 0);
}

// ── 参数扫描 ─────────────────────────────────────────────────────────

/// **用法说明里写着的开关,参数扫描这一关必须认得。**
///
/// 实测撞见:`--open` 写在 `vidbrief report -h` 里,却在参数扫描时被当成未知参数
/// 打回去(`report 不认识参数 --open`)—— 文档说有、实际没有,是最坏的一种不一致。
/// 这组测试按"开关该被认下来"验:给一个**读不动的输入**,报错也必须换一条
/// (换到解析 JSON / 找项目去了),而不是"不认识参数"。
#[test]
fn the_report_command_accepts_its_documented_switches() {
    for flag in ["--open", "--self-contained"] {
        let (ok, _out, err) = run(&["report", "no-such-project", "--json", "-", flag], "{");
        assert!(!ok, "输入是坏的,命令该失败");
        assert!(
            !err.contains("不认识参数"),
            "`{flag}` 是用法说明里写着的东西,不该被参数扫描拒掉:{err}"
        );
    }
}
