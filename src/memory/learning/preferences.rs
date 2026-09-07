//! Closed automatic preference values. The helper interprets language; these
//! values bound what can become active without an owner review.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrdinaryPreference {
    ConciseResponses,
    DetailedResponses,
    Examples,
    MetricUnits,
    ImperialUnits,
    DarkTheme,
    LightTheme,
    Rust,
    Python,
    Typescript,
}

impl OrdinaryPreference {
    pub(super) fn statement(self) -> &'static str {
        match self {
            Self::ConciseResponses => "I prefer concise responses",
            Self::DetailedResponses => "I prefer detailed responses",
            Self::Examples => "I prefer examples",
            Self::MetricUnits => "I prefer metric units",
            Self::ImperialUnits => "I prefer imperial units",
            Self::DarkTheme => "I prefer dark mode",
            Self::LightTheme => "I prefer light mode",
            Self::Rust => "I use Rust",
            Self::Python => "I use Python",
            Self::Typescript => "I use TypeScript",
        }
    }
}
