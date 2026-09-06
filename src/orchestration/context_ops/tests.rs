use super::*;

#[test]
fn all_closed_operations_preserve_exact_citations_without_model_calls() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().join("artifacts"));
    let text = "first\n日本語 marker\nlast\n";
    let (artifact, _) = store
        .put(text.as_bytes(), "text/plain", PrincipalId::new())
        .unwrap();
    let input = EvidenceRange {
        artifact: artifact.reference.clone(),
        offset: 0,
        length: text.len(),
    };
    for operation in [
        ContextOperation::Search {
            inputs: vec![input.clone()],
            query: "marker".into(),
        },
        ContextOperation::Slice {
            input: input.clone(),
        },
        ContextOperation::Filter {
            inputs: vec![input.clone()],
            contains: "last".into(),
        },
        ContextOperation::Map {
            inputs: vec![input.clone()],
            transform: Transform::Uppercase,
        },
        ContextOperation::Reduce {
            inputs: vec![input.clone()],
            reducer: Reducer::CountLines,
        },
        ContextOperation::Derive {
            inputs: vec![input.clone()],
            label: "selected evidence".into(),
        },
        ContextOperation::Cite {
            inputs: vec![input.clone()],
        },
    ] {
        operation.validate().unwrap();
        let output = materialize(
            &store,
            &operation,
            std::slice::from_ref(&artifact),
            &CancellationToken::new(),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        for fragment in value["fragments"].as_array().unwrap() {
            for citation in fragment["citations"].as_array().unwrap() {
                let citation: EvidenceRange = serde_json::from_value(citation.clone()).unwrap();
                assert_eq!(citation.artifact, artifact.reference);
                assert!(citation.offset + citation.length as u64 <= artifact.byte_len);
            }
        }
        if matches!(operation, ContextOperation::Search { .. }) {
            assert_eq!(value["fragments"][0]["citations"][0]["offset"], 6);
            assert_eq!(value["fragments"][0]["text"], "日本語 marker\n");
        }
    }
    assert!(serde_json::from_value::<ContextOperation>(serde_json::json!({"operation":"map","inputs":[input],"transform":"eval","code":"arbitrary"})).is_err());
}

#[test]
fn cancelled_corrupt_and_oversize_context_never_returns_success() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().join("artifacts"));
    let (artifact, _) = store
        .put(b"one\ntwo\n", "text/plain", PrincipalId::new())
        .unwrap();
    let input = EvidenceRange {
        artifact: artifact.reference.clone(),
        offset: 0,
        length: artifact.byte_len as usize,
    };
    let op = ContextOperation::Slice {
        input: input.clone(),
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(materialize(&store, &op, std::slice::from_ref(&artifact), &cancel).is_err());
    let mut false_record = artifact.clone();
    false_record.byte_len += 1;
    assert!(materialize(&store, &op, &[false_record], &CancellationToken::new()).is_err());
    let path = store.verified_path(&artifact, 64 * 1024).unwrap();
    std::fs::write(path, b"bad\nblob").unwrap();
    assert!(
        materialize(
            &store,
            &op,
            std::slice::from_ref(&artifact),
            &CancellationToken::new()
        )
        .is_err()
    );
    assert!(
        ContextOperation::Cite {
            inputs: vec![input; 17]
        }
        .validate()
        .is_err()
    );
}

#[test]
fn cancellation_between_map_inputs_never_publishes_partial_derived_evidence() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().join("artifacts"));
    let (artifact, _) = store
        .put(b"first source\n", "text/plain", PrincipalId::new())
        .unwrap();
    let input = EvidenceRange {
        artifact: artifact.reference.clone(),
        offset: 0,
        length: artifact.byte_len as usize,
    };
    let cancel = CancellationToken::new();
    let mut visited = 0;
    let result = materialize_observed(
        &store,
        &ContextOperation::Map {
            inputs: vec![input; 3],
            transform: Transform::Uppercase,
        },
        &[artifact.clone(), artifact.clone(), artifact],
        &cancel,
        |index| {
            visited += 1;
            if index == 1 {
                cancel.cancel();
            }
        },
    );
    assert!(result.is_err());
    assert_eq!(visited, 2);
}

#[test]
fn direct_retrieval_remains_the_smaller_path_for_a_small_exact_answer() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().join("artifacts"));
    let (artifact, _) = store.put(b"42", "text/plain", PrincipalId::new()).unwrap();
    let direct = store.read_bounded(&artifact, 64).unwrap();
    let operation = ContextOperation::Slice {
        input: EvidenceRange {
            artifact: artifact.reference.clone(),
            offset: 0,
            length: 2,
        },
    };
    let derived = materialize(&store, &operation, &[artifact], &CancellationToken::new()).unwrap();
    assert!(direct.len() < derived.len());
    println!(
        "equal source/model budget (zero model calls): direct={} bytes; typed derived envelope={} bytes; direct exact retrieval wins",
        direct.len(),
        derived.len()
    );
}

#[test]
fn fixed_large_tasks_compare_direct_retrieval_and_native_selection() {
    let home = tempfile::tempdir().unwrap();
    let store = ArtifactStore::new(home.path().join("artifacts"));
    let text = format!("{}needle=42\n", "unrelated evidence\n".repeat(1500));
    let (artifact, _) = store
        .put(text.as_bytes(), "text/plain", PrincipalId::new())
        .unwrap();
    let range = EvidenceRange {
        artifact: artifact.reference.clone(),
        offset: 0,
        length: text.len(),
    };
    for (name, operation, expected) in [
        (
            "find_one_fact",
            ContextOperation::Search {
                inputs: vec![range.clone()],
                query: "needle=".into(),
            },
            "needle=42\n",
        ),
        (
            "count_lines",
            ContextOperation::Reduce {
                inputs: vec![range],
                reducer: Reducer::CountLines,
            },
            "1501",
        ),
    ] {
        let mut direct_times = Vec::new();
        let mut operation_times = Vec::new();
        let mut derived_bytes = 0;
        for _ in 0..5 {
            let began = std::time::Instant::now();
            let direct = store.read_bounded(&artifact, 64 * 1024).unwrap();
            direct_times.push(began.elapsed().as_micros());
            let began = std::time::Instant::now();
            let derived = materialize(
                &store,
                &operation,
                std::slice::from_ref(&artifact),
                &CancellationToken::new(),
            )
            .unwrap();
            operation_times.push(began.elapsed().as_micros());
            derived_bytes = derived.len();
            let value: serde_json::Value = serde_json::from_slice(&derived).unwrap();
            assert_eq!(value["fragments"][0]["text"], expected);
            assert!(derived.len() < direct.len());
        }
        direct_times.sort_unstable();
        operation_times.sort_unstable();
        println!(
            "fixed task={name}; same source={} bytes; direct median={}us; native operation median={}us; derived envelope={derived_bytes} bytes; zero model calls; each path verifies the complete source",
            text.len(),
            direct_times[2],
            operation_times[2]
        );
    }
}
