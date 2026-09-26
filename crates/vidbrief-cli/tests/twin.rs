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
            (2.0, 4.0, "各位好"),
        ])
        // 一张很远的图:让前两段都成为「附近没图」的候选,门才有东西可拦
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
          "reason": "画面上的标题写着「Python 列表」"
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
    let (ok, _, stderr) = run(&["summary", &proj, "--md", "-"], "# 总结\n");
    assert!(!ok, "门没拦:命令行版绕过去了?");
    assert!(stderr.contains("补图候选"), "门该把三类入口都说出来:{stderr}");

    // ③ 报告不控门(它是最后一步),该写得进去
    let (ok, stdout, stderr) = run(&["report", &proj, "--md", "-"], "# 报告\n");
    assert!(ok, "report 失败了:\n{stderr}");
    assert!(stdout.contains("report.md"), "{stdout}");
    assert!(fake.layout().report_md().is_file());

    // ④ 形式错误当场退回,不留半成品
    let (ok, _, stderr) = run(
        &["correct", &proj, "--json", "-"],
        r#"{"corrections":[{"segment_id":2,"outcome":"corrected","text":"各位好","reason":"x"}]}"#,
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
