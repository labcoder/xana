//! Fixed lexical/citation evaluation: not model-generated semantic accuracy.
use super::*;

#[test]
fn recall_forty_case_multilingual_citation_and_scope_evaluation() {
    let fixture = Fixture::new();
    let project = fixture.project();
    let private = fixture.conversation("profile-private", Some(project), "Private baseline");
    let (mut active, _) =
        DurableSession::resume_protected(fixture.owner.store.clone(), fixture.owner.conversation)
            .unwrap();
    let (mut hidden, _) =
        DurableSession::resume_protected(fixture.owner.store.clone(), private).unwrap();
    let languages = [
        ("en", "Recheck the source before deployment"),
        ("es", "Revisar Cancún antes de publicar"),
        ("fr", "Le café nécessite une vérification"),
        ("ja", "再確認が必要です"),
        ("de", "Überprüfung vor Veröffentlichung"),
    ];
    let mut cases = Vec::new();
    for (language, wording) in languages {
        for variant in 0..4 {
            let marker = format!("evidence-{language}-{variant}");
            let text = format!(
                "{marker}: {wording}. Windows source C:\\work\\reports\\{language}_{variant}.csv; Unix source reports/{language}_{variant}.csv. Corrected target beta-{language}-{variant}; unresolved review-{language}-{variant}."
            );
            let entry = active
                .append_message(Message::text(Role::User, &text))
                .unwrap();
            let private_marker = format!("private-{language}-{variant}");
            hidden
                .append_message(Message::text(
                    Role::User,
                    format!("{private_marker}: confidential cross-Profile canary"),
                ))
                .unwrap();
            let query = match variant {
                0 => marker.clone(),
                1 => format!("{marker} beta-{language}-{variant}"),
                2 => format!("{language}_{variant}.csv {marker}"),
                _ => format!("review-{language}-{variant}"),
            };
            cases.push((query, Some((entry, text))));
            cases.push((private_marker, None));
        }
    }
    drop(active);
    drop(hidden);
    // Build the hidden index through its own legitimate owner; exclusion must
    // occur at selection/materialization, not merely because no canary was indexed.
    RecallOwner {
        conversation: private,
        ..fixture.owner.clone()
    }
    .refresh_history(&CancellationToken::new())
    .unwrap();
    fixture
        .owner
        .refresh_history(&CancellationToken::new())
        .unwrap();
    let mut answered = 0;
    let mut abstained = 0;
    for (query, expected) in &cases {
        let hits = fixture.owner.search(query, None).unwrap();
        if let Some((entry, source)) = expected {
            assert_eq!(hits.len(), 1, "query {query}");
            let hit = &hits[0];
            assert_eq!(
                hit.citation.source,
                Source::Conversation {
                    conversation: fixture.owner.conversation,
                    entry: *entry
                }
            );
            let original = history::source_text(&Message::text(Role::User, source)).unwrap();
            assert_eq!(
                hit.citation.source_hash,
                blake3::hash(original.as_bytes()).to_hex().to_string()
            );
            assert_eq!(hit.text, original[hit.citation.start..hit.citation.end]);
            answered += 1;
        } else {
            assert!(hits.is_empty(), "forbidden query {query}");
            abstained += 1;
        }
    }
    assert_eq!((cases.len(), answered, abstained), (40, 20, 20));
    eprintln!(
        "recall evaluation:40 cases;20 exact source/hash/range answers;20 cross-Profile abstentions;0 model calls"
    );
}
