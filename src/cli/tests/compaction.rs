use super::*;

fn arguments(extra: &[&str]) -> Vec<String> {
    [
        "xana",
        "session",
        "evaluate-compaction",
        "--connection",
        "fixture",
        "--model",
        "fixture-model",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}

#[test]
fn parses_normal_filtered_and_explicit_synthetic_inspection_without_changing_approval_flags() {
    for (extra, expected_case, expected_inspect, expected_yes, expected_enable, expected_disable) in [
        (vec![], None, false, false, false, false),
        (vec!["--yes"], None, false, true, false, false),
        (vec!["--yes", "--enable"], None, false, true, true, false),
        (vec!["--yes", "--disable"], None, false, true, false, true),
        (
            vec!["--yes", "--case-id", "en-6"],
            Some("en-6"),
            false,
            true,
            false,
            false,
        ),
        (
            vec!["--yes", "--case-id", "en-6", "--inspect-synthetic-summary"],
            Some("en-6"),
            true,
            true,
            false,
            false,
        ),
    ] {
        let cli = Cli::try_parse_from(arguments(&extra)).unwrap();
        assert_eq!(
            cli.command,
            Some(Command::Session(SessionArgs {
                command: SessionCommand::EvaluateCompaction {
                    connection: "fixture".into(),
                    model: "fixture-model".into(),
                    case_id: expected_case.map(str::to_owned),
                    inspect_synthetic_summary: expected_inspect,
                    yes: expected_yes,
                    enable: expected_enable,
                    disable: expected_disable,
                }
            }))
        );
    }
}

#[test]
fn synthetic_inspection_requires_case_id_and_conflicts_with_all_approval_mutation_flags() {
    let missing =
        Cli::try_parse_from(arguments(&["--yes", "--inspect-synthetic-summary"])).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::MissingRequiredArgument);
    for approval in ["--enable", "--disable"] {
        for inspection in [false, true] {
            let mut extra = vec!["--yes", "--case-id", "en-6", approval];
            if inspection {
                extra.push("--inspect-synthetic-summary");
            }
            assert_eq!(
                Cli::try_parse_from(arguments(&extra)).unwrap_err().kind(),
                ErrorKind::ArgumentConflict
            );
        }
        assert!(
            Cli::try_parse_from(arguments(&[
                "--yes",
                "--inspect-synthetic-summary",
                approval
            ]))
            .is_err()
        );
    }
    assert_eq!(
        Cli::try_parse_from(arguments(&["--yes", "--enable", "--disable"]))
            .unwrap_err()
            .kind(),
        ErrorKind::ArgumentConflict
    );
}
