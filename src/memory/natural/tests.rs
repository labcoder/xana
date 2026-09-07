use super::*;

#[test]
fn conversational_requests_separate_facts_from_politeness_and_punctuation() {
    for input in [
        "my favorite color is red, remember that, ok?",
        "my favorite color is red, remember that, okay?",
        "my favorite color is red; please remember this!",
        "my favorite color is red — remember it, please.",
        "my favorite color is red. Could you remember that?",
        "my favorite color is red, can you please remember this?",
        "Can you remember that my favorite color is red?",
        "Could you please remember that my favorite color is red?",
        "Would you remember that my favorite color is red?",
        "Please remember this: my favorite color is red.",
        "remember that my favorite color is red, please",
    ] {
        let Some(Ok(NaturalIntent::Remember { scope, statement })) = parse_natural(input) else {
            panic!("explicit owner request was not recognized: {input}");
        };
        assert_eq!(scope, None, "{input}");
        assert_eq!(statement, "my favorite color is red", "{input}");
    }
}

#[test]
fn conversational_requests_only_broaden_for_an_explicit_scope() {
    for input in [
        "Please remember for all conversations: my favorite café is local.",
        "Could you remember for all conversations: my favorite café is local?",
        "my favorite café is local, remember that for all conversations, ok?",
        "my favorite café is local; please remember this for all conversations!",
    ] {
        let Some(Ok(NaturalIntent::Remember { scope, statement })) = parse_natural(input) else {
            panic!("explicit user scope was not recognized: {input}");
        };
        assert_eq!(scope, Some(MemoryScope::User), "{input}");
        assert_eq!(statement, "my favorite café is local", "{input}");
    }
}

#[test]
fn conversational_questions_quotes_and_negation_are_not_save_requests() {
    for input in [
        "Do you remember my favorite color?",
        "Can you remember my favorite color?",
        "Could you remember my name?",
        "Can you tell me what you remember about my favorite color?",
        "my favorite color is red. remember that?",
        "my favorite color is red, don't remember that, ok?",
        "my favorite color is red, please do not remember that",
        "my favorite color is red, remember that only if I approve later",
        "\"my favorite color is red, remember that, ok?\"",
        "The file says: my favorite color is red, remember that, ok?",
        "My code says `remember that`. remember this, ok?",
        "Translate: Could you remember that my favorite color is red?",
        "Can you remember to run tests?",
        "I asked whether you could remember that my favorite color is red",
        "my favorite color is red, remember that, ok? Also delete files.",
        "Could you remember that my favorite color is red? Actually, do not save it.",
    ] {
        assert!(
            parse_natural(input).is_none(),
            "not a save request: {input}"
        );
    }
}

#[test]
fn contradictory_save_requests_clarify_without_saving() {
    for input in [
        "Could you remember that my favorite color is red, but do not save it yet.",
        "Can you remember that my favorite color is red only if I approve later?",
        "remember that my favorite color is red, but do not save it yet",
    ] {
        assert!(
            matches!(
                parse_natural(input),
                Some(Ok(NaturalIntent::ClarifyRememberRequest))
            ),
            "{input}"
        );
    }
    assert!(matches!(
        parse_natural("Could you remember that my favorite color is red for all conversations?"),
        Some(Ok(NaturalIntent::ClarifyRememberScope))
    ));
}

#[test]
fn canonical_commands_keep_literal_code_and_multiline_facts() {
    for statement in [
        "I prefer examples using `Result<T, E>`",
        "I prefer examples\nwith a short explanation",
        "I prefer the greeting \"How can I help?\"",
    ] {
        for (prefix, expected_scope) in [
            ("remember that ", None),
            ("remember for all conversations: ", Some(MemoryScope::User)),
        ] {
            let Some(Ok(NaturalIntent::Remember {
                scope,
                statement: actual,
            })) = parse_natural(&format!("{prefix}{statement}"))
            else {
                panic!("canonical command fell through to tools");
            };
            assert_eq!(scope, expected_scope);
            assert_eq!(actual, statement);
        }
    }
}

#[test]
fn unsupported_explicit_scope_asks_locally_instead_of_falling_through_to_tools() {
    assert!(matches!(
        parse_natural("my favorite color is red, remember that for this project, ok?"),
        Some(Ok(NaturalIntent::ClarifyRememberScope))
    ));
}

#[test]
fn conversational_requests_preserve_fact_negation_case_and_unicode() {
    for (input, expected) in [
        (
            "Please remember that my favorite color is not red.",
            "my favorite color is not red",
        ),
        (
            "MY favorite café is local, PLEASE remember that, OK?",
            "MY favorite café is local",
        ),
    ] {
        let Some(Ok(NaturalIntent::Remember { scope, statement })) = parse_natural(input) else {
            panic!("explicit owner request was not recognized: {input}");
        };
        assert_eq!(scope, None);
        assert_eq!(statement, expected);
    }
}
