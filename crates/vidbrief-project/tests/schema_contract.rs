//! 三方锚定测试:`docs/schema/examples/*.json` ↔ `docs/schema/*.schema.json` ↔ Rust 类型。
//!
//! **示例文件不是文档,是契约。** 每个示例都要同时满足三件事:
//!
//! 1. 被对应的 JSON Schema 校验通过;
//! 2. 能被对应的 Rust 类型反序列化(`deny_unknown_fields` 生效 = 拼错字段名会红);
//! 3. 再序列化回去与示例**逐字节相同**(= 示例必须是规范格式,含字段顺序、显式 `null`、缩进)。
//!
//! 任何一环漂移都会在这里失败。改了 schema 或类型却忘了改示例,跑测试就会知道。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vidbrief_project::{
    ChangeCurve, DenseRegions, Project, Shot, ShotOrigin, Shots, SupplementRequests,
    Transcript, VideoMeta,
};

/// 示例文件 → 校验它的 schema。
const CASES: &[(&str, &str)] = &[
    ("project.json", "project.schema.json"),
    ("video.meta.json", "video_meta.schema.json"),
    ("change_curve.json", "change_curve.schema.json"),
    ("dense_regions.json", "dense_regions.schema.json"),
    ("transcript.json", "transcript.schema.json"),
    ("transcript.fixed.json", "transcript.schema.json"),
    ("shots.json", "shots.schema.json"),
    ("supplement_requests.json", "supplement_requests.schema.json"),
];

/// 仓库根(`crates/vidbrief-project/../..`)。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("无法定位仓库根目录")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取失败 {}:{e}", path.display()))
}

fn parse_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|e| panic!("不是合法 JSON {}:{e}", path.display()))
}

fn example_path(name: &str) -> PathBuf {
    repo_root().join("docs").join("schema").join("examples").join(name)
}

fn schema_path(name: &str) -> PathBuf {
    assert!(
        name.ends_with(".schema.json"),
        "`{name}` 不是 schema 文件名 —— 这里要的是 docs/schema/ 下的 schema,不是 examples/ 下的示例"
    );
    repo_root().join("docs").join("schema").join(name)
}

/// 比对规范序列化结果,只报第一处差异(否则 100 行的文件会刷屏)。
fn assert_canonical(example: &str, expected: &str, actual: &str) {
    if expected == actual {
        return;
    }
    let mut detail = String::new();
    for (i, (want, got)) in expected.lines().zip(actual.lines()).enumerate() {
        if want != got {
            detail = format!("第 {} 行不同\n  示例   : {want}\n  序列化 : {got}\n", i + 1);
            break;
        }
    }
    if detail.is_empty() {
        detail = format!(
            "行数不同:示例 {} 行,序列化 {} 行\n",
            expected.lines().count(),
            actual.lines().count()
        );
    }
    panic!("{example} 与规范序列化结果不一致:\n{detail}");
}

/// 一个示例的完整三连检查。
fn anchor<T>(example: &str, schema: &str)
where
    T: DeserializeOwned + Serialize,
{
    let example_file = example_path(example);
    let schema_file = schema_path(schema);
    let text = read(&example_file);

    // 1) schema 校验
    let instance: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{example} 不是合法 JSON:{e}"));
    let schema_value = parse_json(&schema_file);
    let validator = jsonschema::validator_for(&schema_value)
        .unwrap_or_else(|e| panic!("{schema} 本身不合法:{e}"));
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| format!("  - {e}  (位置: {})", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "{example} 未通过 {schema} 校验:\n{}",
        errors.join("\n")
    );

    // 2) Rust 类型能读
    let typed: T = serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!("{example} 无法反序列化为 {}:{e}", std::any::type_name::<T>())
    });

    // 3) 写回去逐字节相同
    let rendered = vidbrief_project::to_pretty_string(&typed).unwrap();
    assert_canonical(example, &text, &rendered);
}

#[test]
fn every_example_matches_its_schema_and_type() {
    anchor::<Project>("project.json", "project.schema.json");
    anchor::<VideoMeta>("video.meta.json", "video_meta.schema.json");
    anchor::<ChangeCurve>("change_curve.json", "change_curve.schema.json");
    anchor::<DenseRegions>("dense_regions.json", "dense_regions.schema.json");
    anchor::<Transcript>("transcript.json", "transcript.schema.json");
    anchor::<Transcript>("transcript.fixed.json", "transcript.schema.json");
    anchor::<Shots>("shots.json", "shots.schema.json");
    anchor::<SupplementRequests>("supplement_requests.json", "supplement_requests.schema.json");
}

#[test]
fn every_example_and_schema_is_accounted_for() {
    let dir = repo_root().join("docs").join("schema");

    let mut example_files: Vec<String> = std::fs::read_dir(dir.join("examples"))
        .expect("examples 目录不存在")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    example_files.sort();

    let mut expected: Vec<String> = CASES.iter().map(|(e, _)| (*e).to_string()).collect();
    expected.sort();
    assert_eq!(
        example_files, expected,
        "examples/ 下的文件与 CASES 列表不一致 —— 新增示例请一并登记(否则它没有被校验)"
    );

    let mut schema_files: Vec<String> = std::fs::read_dir(&dir)
        .expect("docs/schema 目录不存在")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".schema.json"))
        .collect();
    schema_files.sort();

    let mut used: Vec<String> = CASES.iter().map(|(_, s)| (*s).to_string()).collect();
    used.sort();
    used.dedup();
    assert_eq!(
        schema_files, used,
        "docs/schema/ 下的 schema 与 CASES 列表不一致 —— 有没有人用的 schema 或漏登记的 schema"
    );
}

#[test]
fn every_schema_declares_draft_2020_12() {
    let dir = repo_root().join("docs").join("schema");
    for entry in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".schema.json") {
            continue;
        }
        let value = parse_json(&entry.path());
        assert_eq!(
            value.get("$schema").and_then(|v| v.as_str()),
            Some("https://json-schema.org/draft/2020-12/schema"),
            "{name} 没有声明 Draft 2020-12"
        );
        assert!(
            value.get("title").is_some(),
            "{name} 缺少 title(它会显示在编辑器的提示里)"
        );
    }
}

/// 示例之间必须自洽 —— 否则它们只是 9 个各自合法的 JSON 片段,不能当夹具用。
///
/// 这是"schema 定稿"最容易漏掉的一半:字段对不对由 schema 管,**引用对不对**只能靠这里。
#[test]
fn examples_are_mutually_consistent() {
    let regions: DenseRegions =
        serde_json::from_str(&read(&example_path("dense_regions.json"))).unwrap();
    let shots: Shots = serde_json::from_str(&read(&example_path("shots.json"))).unwrap();
    let transcript: Transcript = serde_json::from_str(&read(&example_path("transcript.json"))).unwrap();
    let fixed: Transcript =
        serde_json::from_str(&read(&example_path("transcript.fixed.json"))).unwrap();
    let curve: ChangeCurve = serde_json::from_str(&read(&example_path("change_curve.json"))).unwrap();
    let meta: VideoMeta = serde_json::from_str(&read(&example_path("video.meta.json"))).unwrap();
    let project: Project = serde_json::from_str(&read(&example_path("project.json"))).unwrap();
    let reqs: SupplementRequests =
        serde_json::from_str(&read(&example_path("supplement_requests.json"))).unwrap();

    // 时长在四个文件里必须是同一个数
    assert_eq!(meta.duration, curve.duration, "video.meta 与 change_curve 的时长不一致");
    assert_eq!(meta.duration, transcript.duration, "video.meta 与 transcript 的时长不一致");
    assert_eq!(project.source.path, meta.path, "project.source.path 与 video.meta.path 不一致");
    assert_eq!(curve.source, meta.path, "change_curve.source 与 video.meta.path 不一致");
    assert_eq!(transcript.source, meta.path, "transcript.source 与 video.meta.path 不一致");

    // 差帧图与密集区统计量对得上
    assert_eq!(
        regions.stats.sampled_points as usize,
        curve.changes.len(),
        "sampled_points 与 change_curve.changes 长度不一致"
    );
    assert_eq!(
        regions.stats.mass_threshold,
        regions.params.mass_threshold(regions.stats.mass_median),
        "mass_threshold 与公式不符"
    );

    // 每个区域的时间范围都要落在视频内,且峰值确实在区间里
    for r in &regions.regions {
        assert!(r.end >= r.start, "区域 {} 的 end < start", r.id);
        assert!(r.end <= meta.duration, "区域 {} 超出视频时长", r.id);
        assert!(r.contains(r.peak_t), "区域 {} 的 peak_t 不在区间内", r.id);
    }

    // 截图:文件名与 id/t 一致、落在来源目录下、region_id 指向真实区域、时间在时长内
    let region_ids: Vec<u32> = regions.regions.iter().map(|r| r.id).collect();
    for shot in &shots.shots {
        assert!(shot.t <= meta.duration, "截图 {} 超出视频时长", shot.id);
        assert_eq!(
            shot.file.file_name(),
            Shot::file_name(shot.id, shot.t).unwrap(),
            "截图 {} 的文件名与 id/t 不符",
            shot.id
        );
        let expected = Shot::default_rel_path(shot.origin, shot.id, shot.t).unwrap();
        assert_eq!(shot.file, expected, "截图 {} 的存放目录与来源不符", shot.id);

        match shot.origin {
            ShotOrigin::DenseRegion | ShotOrigin::IsolatedPeak => {
                let rid = shot.region_id.unwrap_or_else(|| {
                    panic!("截图 {} 来源是算法却没记 region_id", shot.id)
                });
                assert!(region_ids.contains(&rid), "截图 {} 指向不存在的区域 {rid}", shot.id);
            }
            ShotOrigin::AgentRequest | ShotOrigin::Manual => {
                assert!(shot.region_id.is_none(), "截图 {} 不是算法产物,不该有 region_id", shot.id);
                assert!(shot.reason.is_some(), "截图 {} 是补图,必须写理由", shot.id);
            }
        }

        // 校准结论只能由 agent 写,且写了的必须自洽
        if let Some(check) = &shot.check {
            assert!(!check.type_read.trim().is_empty(), "截图 {} 的 type_read 为空", shot.id);
        }
    }

    // 转录:区间合法、id 连续、fixed 是 raw 的全量副本且只多了纠错
    assert_segments_well_formed(&transcript, "transcript.json");
    assert_segments_well_formed(&fixed, "transcript.fixed.json");
    assert_eq!(
        transcript.segments.len(),
        fixed.segments.len(),
        "transcript.fixed.json 必须是全量副本,不能增删段"
    );
    for (raw, fix) in transcript.segments.iter().zip(&fixed.segments) {
        assert_eq!(raw.id, fix.id);
        assert_eq!(raw.text, fix.text, "段 {} 的原文被改写了 —— 原始转录永不就地修改", raw.id);
        assert!(raw.correction.is_none(), "原始 transcription 不该带纠错说明(段 {})", raw.id);
        if fix.correction.as_ref().is_some_and(|c| matches!(c.kind, vidbrief_project::CorrectionType::Uncertain)) {
            assert!(
                fix.corrected_text.is_none(),
                "段 {} 标为 uncertain 就不能同时改文本 —— 存疑不等于可以臆造",
                raw.id
            );
        }
    }

    // 补图请求:指向真实转录段;accepted 必须给出真实存在的截图
    let segment_ids: Vec<u32> = transcript.segments.iter().map(|s| s.id).collect();
    for r in &reqs.requests {
        assert!(segment_ids.contains(&r.segment_id), "补图请求 {} 指向不存在的转录段", r.id);
        assert!(r.t <= meta.duration, "补图请求 {} 超出视频时长", r.id);
        assert_eq!(
            r.nearest_shot.is_none(),
            r.gap.is_none(),
            "补图请求 {} 的 nearest_shot 与 gap 必须同时有值或同时为 null",
            r.id
        );
        match r.status {
            vidbrief_project::SupplementStatus::Accepted => {
                let sid = r.shot_id.unwrap_or_else(|| panic!("请求 {} 已接受却没给 shot_id", r.id));
                let shot = shots.get(sid).unwrap_or_else(|| panic!("请求 {} 指向不存在的截图 {sid}", r.id));
                assert_eq!(
                    shot.origin,
                    ShotOrigin::AgentRequest,
                    "{} 是补图产物,origin 应为 agent_request",
                    sid
                );
            }
            vidbrief_project::SupplementStatus::Pending => {
                assert!(r.shot_id.is_none(), "请求 {} 还没处理,不该有 shot_id", r.id);
            }
            vidbrief_project::SupplementStatus::Rejected => {
                assert!(r.shot_id.is_none(), "请求 {} 被拒绝,不该有 shot_id", r.id);
                assert!(r.note.is_some(), "请求 {} 被拒绝,必须写理由", r.id);
            }
        }
    }

    // 项目进度与文件存在性一致
    assert!(project.progress.preprocess.done, "示例里已经跑完步 1,progress 应标记完成");
    assert!(project.progress.preprocess.at.is_some());
    assert!(project.progress.calibrate.rounds > 0);
    assert!(!project.progress.summarize.done, "示例还没到总结步");
}

fn assert_segments_well_formed(t: &Transcript, name: &str) {
    let mut prev_end = -1.0_f64;
    for (i, seg) in t.segments.iter().enumerate() {
        assert_eq!(seg.id as usize, i + 1, "{name}:第 {} 段的 id 不是 {} —— 应与 SRT 序号一致", i + 1, i + 1);
        assert!(seg.end >= seg.start, "{name}:段 {} 的 end < start", seg.id);
        assert!(seg.start >= prev_end, "{name}:段 {} 与前一段重叠", seg.id);
        assert!(!seg.text.trim().is_empty(), "{name}:段 {} 文本为空", seg.id);
        prev_end = seg.end;
    }
}

#[test]
fn writer_output_validates_against_the_schemas() {
    // 示例文件是手写的,只能证明"人能写对";这里证明"工具自己能写对" ——
    // 特别是时间戳这种由代码生成、手写示例覆盖不到的字段。
    use vidbrief_project::{
        ChangePoint, Confidence, Correction, CorrectionType, ProjectId, Region, RegionParams,
        RegionReason, RegionStats, RelPath, Segment, ShotCheck, ShotStatus, SourceLink, now,
    };

    let rel = RelPath::new("source/video.mp4").unwrap();
    let region = Region {
        id: 1,
        start: 9.0,
        end: 15.0,
        mass: 2.26,
        peak_t: 9.0,
        peak_score: 0.62,
        reason: RegionReason::HighDensity,
    };
    let params = RegionParams::default();
    let curve = ChangeCurve {
        source: rel.clone(),
        duration: 24.0,
        fps_sampled: vidbrief_project::curve::DEFAULT_FPS_SAMPLED,
        width: vidbrief_project::curve::DEFAULT_WIDTH,
        height: 270,
        metric: vidbrief_project::curve::DEFAULT_METRIC.to_string(),        generated_at: now(),
        changes: vec![ChangePoint { t: 0.0, score: 0.0 }],
    };
    let regions = DenseRegions::new(
        params,
        RegionStats::new(1, 0.1, &params),
        vec![region],
    );
    let transcript = Transcript {
        source: rel.clone(),
        model: "ggml-medium".into(),
        language: "zh".into(),
        duration: 24.0,
        generated_at: now(),
        segments: vec![Segment {
            id: 1,
            start: 0.0,
            end: 2.5,
            text: "各位好".into(),
            corrected_text: Some("各位好,开始".into()),
            correction: Some(Correction {
                kind: CorrectionType::Homophone,
                note: "好 → 好,开始".into(),
                reason: "上下文".into(),
            }),
            checked_at: Some(now()),
        }],
    };
    let mut shots = Shots::new();
    let sid = shots.next_id().unwrap();
    shots.shots.push(Shot {
        id: sid,
        t: 9.0,
        file: Shot::default_rel_path(ShotOrigin::DenseRegion, sid, 9.0).unwrap(),
        origin: ShotOrigin::DenseRegion,
        region_id: Some(1),
        status: ShotStatus::Keep,
        reason: None,
        created_at: now(),
        check: Some(ShotCheck {
            r#match: true,
            audio_visual_sync: true,
            type_read: "Python 列表 结构图".into(),
            neighbor_text: Some("好我们切到主图了".into()),
            issues: vec![],
            confidence: Confidence::High,
            checked_at: now(),
        }),
    });
    let reqs = SupplementRequests::new(3.0);
    let meta = VideoMeta {
        path: rel.clone(),
        duration: 24.0,
        width: Some(1280),
        height: Some(720),
        fps: Some(30.0),
        codec: Some("h264".into()),
        size_bytes: 1024,
        sha256: "0".repeat(64),
        probed_at: now(),
        prober: "ffprobe test".into(),
    };
    let mut project = Project::new(
        ProjectId::new("demo").unwrap(),
        "样例",
        rel,
        r"E:\videos\demo.mp4",
        SourceLink::Copy,
    );
    project.progress.preprocess.mark_done();
    project.progress.calibrate.bump();
    project.glossary = Some(vec!["样本".into()]);

    // 顺便验证时间戳确实没有小数秒 —— 这一条是这次运行真实踩出来的
    let meta_json = serde_json::to_value(&meta).unwrap();
    let probed_at = meta_json["probed_at"].as_str().unwrap();
    assert!(!probed_at.contains('.'), "probed_at 不该带小数秒:{probed_at}");

    macro_rules! validate {
        ($value:expr, $schema:expr) => {{
            let instance = serde_json::to_value(&$value).unwrap();
            let schema_value = parse_json(&schema_path($schema));
            let validator = jsonschema::validator_for(&schema_value).unwrap();
            let errors: Vec<String> = validator
                .iter_errors(&instance)
                .map(|e| format!("  - {e}  (位置: {})", e.instance_path()))
                .collect();
            assert!(
                errors.is_empty(),
                "工具自己写出的 {} 未通过 {} 校验:\n{}\n实际内容:\n{}",
                stringify!($value),
                $schema,
                errors.join("\n"),
                serde_json::to_string_pretty(&instance).unwrap()
            );
        }};
    }

    validate!(project, "project.schema.json");
    validate!(meta, "video_meta.schema.json");
    validate!(curve, "change_curve.schema.json");
    validate!(regions, "dense_regions.schema.json");
    validate!(transcript, "transcript.schema.json");
    validate!(shots, "shots.schema.json");
    validate!(reqs, "supplement_requests.schema.json");
}

#[test]
fn shots_example_exercises_every_origin_and_status_shape() {
    // 示例夹具的价值在于"覆盖到形状"。少了一种形状,测试就漏了一类。
    let shots: Shots = serde_json::from_str(&read(&example_path("shots.json"))).unwrap();

    let origins: Vec<ShotOrigin> = shots.shots.iter().map(|s| s.origin).collect();
    assert!(origins.contains(&ShotOrigin::DenseRegion), "示例缺少密集区截图");
    assert!(origins.contains(&ShotOrigin::IsolatedPeak), "示例缺少孤立峰截图");
    assert!(origins.contains(&ShotOrigin::AgentRequest), "示例缺少补图");

    assert!(
        shots.shots.iter().any(|s| s.check.is_none()),
        "示例里必须有一张未校准的图,否则 `check: null` 这条路没被验证"
    );
    assert!(
        shots.shots.iter().any(|s| s.check.as_ref().is_some_and(|c| c.needs_review())),
        "示例里必须有一张需要人工裁决的图,否则审查节点这条路没被验证"
    );
    assert!(
        shots.shots.iter().any(|s| s.check.as_ref().is_some_and(|c| !c.needs_review())),
        "示例里必须有一张干净通过的图"
    );
}
