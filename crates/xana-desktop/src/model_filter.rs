//! Shared bounded model-catalog filtering for graphical setup and management.

use xana::desktop::DesktopModelOption;

pub(crate) fn matches(model: &DesktopModelOption, normalized_query: &str) -> bool {
    normalized_query.is_empty()
        || model.id.to_lowercase().contains(normalized_query)
        || model.display_name.to_lowercase().contains(normalized_query)
        || model
            .input_modalities
            .iter()
            .any(|value| value.to_lowercase().contains(normalized_query))
        || model
            .output_modalities
            .iter()
            .any(|value| value.to_lowercase().contains(normalized_query))
        || model
            .reasoning_efforts
            .iter()
            .any(|value| value.to_lowercase().contains(normalized_query))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(index: usize) -> DesktopModelOption {
        DesktopModelOption {
            id: format!("provider/model-{index:04}"),
            display_name: format!("Reasoning Model {index:04}"),
            input_modalities: vec!["text".to_owned(), "image".to_owned()],
            output_modalities: vec!["text".to_owned()],
            tools: Some(true),
            reasoning: Some(true),
            reasoning_efforts: vec!["low".to_owned(), "high".to_owned()],
            default_reasoning_effort: Some("high".to_owned()),
            context_tokens: Some(128_000),
            max_output_tokens: Some(16_384),
            pricing: None,
            source: "fixture".to_owned(),
        }
    }

    #[test]
    fn four_hundred_ten_model_fixture_filters_deterministically() {
        let models = (0..410).map(fixture).collect::<Vec<_>>();
        let matching = models
            .iter()
            .enumerate()
            .filter(|(_, model)| matches(model, "model-0409"))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        assert_eq!(matching, [409]);
    }

    #[test]
    fn capability_terms_participate_without_changing_catalog_order() {
        let models = (0..32).map(fixture).collect::<Vec<_>>();
        let matching = models
            .iter()
            .enumerate()
            .filter(|(_, model)| matches(model, "image"))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(matching, (0..32).collect::<Vec<_>>());
    }
}
