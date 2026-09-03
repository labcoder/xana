//! Desktop projection of Xana's shared, presentation-only semantic copy.

pub(crate) use xana::localization::{InterfaceLocale, catalog_messages};

pub(crate) fn semantic_code_label(code: &str, fallback: &str) -> String {
    let mut label = code
        .split(['.', '_'])
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        return fallback.to_owned();
    }
    let first = label.remove(0).to_uppercase().to_string();
    label.insert_str(0, &first);
    format!("{label} ({code})")
}

#[cfg(test)]
mod tests {
    use super::semantic_code_label;

    #[test]
    fn unknown_semantic_label_keeps_the_original_code_inspectable() {
        assert_eq!(
            semantic_code_label("future.signal", "Unavailable"),
            "Future signal (future.signal)"
        );
        assert_eq!(semantic_code_label("...", "Unavailable"), "Unavailable");
    }
}
