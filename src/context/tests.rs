use super::*;
use std::fs;
use tempfile::tempdir;

fn source(id: &str, content: impl Into<String>, max_tokens: usize) -> ContextSource {
    ContextSource {
        id: TransientSourceId::new(id),
        provenance: SourceProvenance {
            display_name: id.to_owned(),
            path: Some(PathBuf::from("AGENTS.md")),
            origin: SourceOrigin::ProjectFile,
        },
        trust: TrustClass::Project,
        content: content.into(),
        max_tokens,
    }
}

#[test]
fn missing_project_files_produce_no_sources() {
    let workspace = tempdir().expect("temporary workspace");

    assert!(
        load_project_sources(workspace.path())
            .expect("missing source is allowed")
            .is_empty()
    );
}

#[test]
fn root_agents_is_the_only_phase_two_project_instruction_source() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("AGENTS.md"), "root guidance").expect("root instructions");
    fs::create_dir(workspace.path().join("nested")).expect("nested directory");
    fs::write(workspace.path().join("nested/AGENTS.md"), "nested guidance")
        .expect("nested instructions");
    fs::create_dir(workspace.path().join(".agents")).expect("agents directory");
    fs::write(workspace.path().join(".agents/notes.md"), "not discovered")
        .expect("unrelated agent file");

    let sources = load_project_sources(workspace.path()).expect("project sources");

    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].id.as_str(), "project:AGENTS.md");
    assert_eq!(sources[0].content, "root guidance");
}

#[test]
fn xana_md_is_not_discovered() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("XANA.md"), "ignored").expect("Xana-specific file");

    assert!(
        load_project_sources(workspace.path())
            .expect("project sources")
            .is_empty()
    );
}

#[test]
fn source_provenance_retains_relative_path_and_project_trust() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("AGENTS.md"), "guidance").expect("root instructions");

    let source = load_project_sources(workspace.path())
        .expect("project source")
        .pop()
        .expect("root source");

    assert_eq!(source.trust, TrustClass::Project);
    assert_eq!(source.provenance.origin, SourceOrigin::ProjectFile);
    assert_eq!(source.provenance.path, Some(PathBuf::from("AGENTS.md")));
}

#[test]
fn non_regular_source_is_rejected() {
    let workspace = tempdir().expect("temporary workspace");
    fs::create_dir(workspace.path().join("AGENTS.md")).expect("directory fixture");

    assert!(matches!(
        load_project_sources(workspace.path()),
        Err(ContextError::InvalidSourceKind { .. })
    ));
}

#[cfg(unix)]
#[test]
fn symlink_source_is_rejected() {
    use std::os::unix::fs::symlink;

    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("instructions.md"), "guidance").expect("target");
    symlink(
        workspace.path().join("instructions.md"),
        workspace.path().join("AGENTS.md"),
    )
    .expect("symlink fixture");

    assert!(matches!(
        load_project_sources(workspace.path()),
        Err(ContextError::InvalidSourceKind { .. })
    ));
}

#[test]
fn oversized_source_is_rejected_before_unbounded_allocation() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(
        workspace.path().join("AGENTS.md"),
        vec![b'x'; source::MAX_PROJECT_SOURCE_BYTES + 1],
    )
    .expect("oversized source");

    assert!(matches!(
        load_project_sources(workspace.path()),
        Err(ContextError::SourceTooLarge { limit, .. })
            if limit == source::MAX_PROJECT_SOURCE_BYTES
    ));
}

#[test]
fn invalid_utf8_is_a_typed_source_error() {
    let workspace = tempdir().expect("temporary workspace");
    fs::write(workspace.path().join("AGENTS.md"), [0xff, 0xfe]).expect("invalid UTF-8");

    assert!(matches!(
        load_project_sources(workspace.path()),
        Err(ContextError::InvalidUtf8 { .. })
    ));
}

#[test]
fn line_preview_uses_inclusive_one_based_bounds() {
    let preview = preview(
        &source("project:lines", "one\ntwo\nthree\nfour", 100),
        PreviewSelector::Lines { start: 2, end: 3 },
    )
    .expect("line preview");

    assert_eq!(preview.text, "two\nthree");
    assert_eq!(
        preview.selector,
        PreviewSelector::Lines { start: 2, end: 3 }
    );
}

#[test]
fn literal_search_preserves_line_order_and_match_cap() {
    let preview = preview(
        &source("project:search", "hit one\nno\nhit two\nhit three", 100),
        PreviewSelector::LiteralSearch {
            query: "hit".to_owned(),
            max_matches: 2,
        },
    )
    .expect("search preview");

    assert_eq!(preview.text, "hit one\nhit two");
    assert!(preview.truncated);
}

#[test]
fn unicode_preview_never_splits_a_scalar_value() {
    let preview = preview(
        &source("project:unicode", "é🦀漢字-more", 1),
        PreviewSelector::Head,
    )
    .expect("Unicode preview");

    assert_eq!(preview.text, "é🦀漢");
    assert!(preview.truncated);
    assert_eq!(preview.estimated_tokens, 1);
}

#[test]
fn every_preview_retains_provenance_trust_and_selector() {
    let source = source("project:proof", "first\nsecond", 100);
    let selector = PreviewSelector::Lines { start: 1, end: 1 };
    let preview = preview(&source, selector.clone()).expect("preview");

    assert_eq!(preview.source_id, source.id);
    assert_eq!(preview.provenance, source.provenance);
    assert_eq!(preview.trust, TrustClass::Project);
    assert_eq!(preview.selector, selector);
}

#[test]
fn duplicate_and_reserved_source_ids_are_rejected() {
    let budget = ContextBudget {
        total_tokens: 100,
        conversation_reserve_tokens: 10,
    };
    let duplicate = ContextPlanner::new(
        vec![
            source("project:same", "one", 10),
            source("project:same", "two", 10),
        ],
        budget,
    );
    let reserved = ContextPlanner::new(vec![source("builtin:identity", "fake", 10)], budget);

    assert!(matches!(
        duplicate,
        Err(ContextError::DuplicateSourceId { .. })
    ));
    assert!(matches!(
        reserved,
        Err(ContextError::ReservedSourceId { .. })
    ));
}

#[test]
fn selection_is_deterministic_and_never_exceeds_budget() {
    struct Case {
        name: &'static str,
        budget: usize,
        source_sizes: &'static [usize],
        expected_ids: &'static [&'static str],
        expected_omitted: &'static [&'static str],
    }

    let cases = [
        Case {
            name: "exact fit",
            budget: 4,
            source_sizes: &[6, 6],
            expected_ids: &["project:0", "project:1"],
            expected_omitted: &[],
        },
        Case {
            name: "one token short",
            budget: 3,
            source_sizes: &[6, 6],
            expected_ids: &["project:0"],
            expected_omitted: &["project:1"],
        },
        Case {
            name: "oversized first does not starve later",
            budget: 2,
            source_sizes: &[9, 3],
            expected_ids: &["project:1"],
            expected_omitted: &["project:0"],
        },
        Case {
            name: "empty source",
            budget: 1,
            source_sizes: &[0, 3],
            expected_ids: &["project:0", "project:1"],
            expected_omitted: &[],
        },
        Case {
            name: "Unicode scalar boundary",
            budget: 1,
            source_sizes: &[3, 3],
            expected_ids: &["project:0"],
            expected_omitted: &["project:1"],
        },
    ];

    for case in cases {
        let sources = case
            .source_sizes
            .iter()
            .enumerate()
            .map(|(index, size)| {
                let content = if case.name == "Unicode scalar boundary" {
                    "🦀".repeat(*size)
                } else {
                    "x".repeat(*size)
                };
                source(&format!("project:{index}"), content, 100)
            })
            .collect();
        let planner = ContextPlanner::new(
            sources,
            ContextBudget {
                total_tokens: case.budget + 1,
                conversation_reserve_tokens: 1,
            },
        )
        .expect(case.name);
        let plan = planner.plan(0).expect(case.name);

        assert_eq!(
            plan.selected
                .iter()
                .map(|item| item.source_id.as_str())
                .collect::<Vec<_>>(),
            case.expected_ids,
            "{}",
            case.name
        );
        assert_eq!(
            plan.omitted_sources
                .iter()
                .map(TransientSourceId::as_str)
                .collect::<Vec<_>>(),
            case.expected_omitted,
            "{}",
            case.name
        );
        assert!(plan.used_tokens <= case.budget, "{}", case.name);
    }
}

#[test]
fn context_plan_report_never_includes_source_content() {
    let planner = ContextPlanner::new(
        vec![source("project:secret", "do-not-print-this", 100)],
        ContextBudget {
            total_tokens: 20,
            conversation_reserve_tokens: 1,
        },
    )
    .expect("planner");
    let plan = planner.plan(0).expect("plan");
    let report = ContextPlanReport::render(&plan);

    assert!(report.as_str().contains("project:secret"));
    assert!(!report.as_str().contains("do-not-print-this"));
}
