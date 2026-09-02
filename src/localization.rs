//! Presentation-owned localization for stable semantic codes.
//!
//! Runtime policy emits codes and bounded parameters. Local clients choose a
//! locale and layout; missing copy never changes the action or authority.

const MAX_DYNAMIC_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterfaceLocale {
    #[default]
    English,
    Spanish,
    Pseudo,
}

impl InterfaceLocale {
    pub const ALL: [Self; 3] = [Self::English, Self::Spanish, Self::Pseudo];

    pub const fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Spanish => "Español sample",
            Self::Pseudo => "Pseudolocale",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticMessage {
    SetupReady { profile: String },
    ApprovalRequired { action: String },
    AttentionRequired { count: u32 },
    ErrorReported { detail: String },
    RecoveryCompleted { count: u32 },
    CapabilityUnavailable { capability: String },
    ReceiptCompleted { receipt_id: String },
    Unknown { code: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalizedCopy {
    pub code: String,
    pub text: String,
    pub used_fallback: bool,
}

impl SemanticMessage {
    pub fn code(&self) -> String {
        match self {
            Self::SetupReady { .. } => "setup.ready".to_owned(),
            Self::ApprovalRequired { .. } => "approval.required".to_owned(),
            Self::AttentionRequired { .. } => "attention.required".to_owned(),
            Self::ErrorReported { .. } => "error.reported".to_owned(),
            Self::RecoveryCompleted { .. } => "recovery.completed".to_owned(),
            Self::CapabilityUnavailable { .. } => "capability.unavailable".to_owned(),
            Self::ReceiptCompleted { .. } => "receipt.completed".to_owned(),
            Self::Unknown { code } => bounded(code),
        }
    }

    pub fn localize(&self, locale: InterfaceLocale) -> LocalizedCopy {
        let code = self.code();
        let (text, used_fallback) = match (locale, self) {
            (InterfaceLocale::English, message) => (english(message), false),
            (InterfaceLocale::Pseudo, message) => (pseudo(message), false),
            (InterfaceLocale::Spanish, Self::SetupReady { profile }) => (
                format!("Listo para usar el perfil «{}».", bounded(profile)),
                false,
            ),
            (InterfaceLocale::Spanish, Self::ApprovalRequired { action }) => (
                format!("Se requiere aprobación: {}", bounded(action)),
                false,
            ),
            (InterfaceLocale::Spanish, Self::ReceiptCompleted { receipt_id }) => (
                format!("Trabajo completado. Recibo: {}", bounded(receipt_id)),
                false,
            ),
            (InterfaceLocale::Spanish, _) => (
                format!("Contenido no disponible en el idioma seleccionado. Código: {code}"),
                true,
            ),
        };
        LocalizedCopy {
            code,
            text,
            used_fallback,
        }
    }
}

pub fn representative_path(
    locale: InterfaceLocale,
    profile: impl Into<String>,
    action: impl Into<String>,
    receipt_id: impl Into<String>,
) -> [LocalizedCopy; 3] {
    [
        SemanticMessage::SetupReady {
            profile: profile.into(),
        }
        .localize(locale),
        SemanticMessage::ApprovalRequired {
            action: action.into(),
        }
        .localize(locale),
        SemanticMessage::ReceiptCompleted {
            receipt_id: receipt_id.into(),
        }
        .localize(locale),
    ]
}

pub fn catalog_messages() -> Vec<SemanticMessage> {
    vec![
        SemanticMessage::SetupReady {
            profile: "Personal".to_owned(),
        },
        SemanticMessage::ApprovalRequired {
            action: "Write the generated report to reports/weekly.md".to_owned(),
        },
        SemanticMessage::AttentionRequired { count: 3 },
        SemanticMessage::ErrorReported {
            detail: "The provider closed the response stream before a final message arrived."
                .to_owned(),
        },
        SemanticMessage::RecoveryCompleted { count: 2 },
        SemanticMessage::CapabilityUnavailable {
            capability: "native video understanding".to_owned(),
        },
        SemanticMessage::ReceiptCompleted {
            receipt_id: "run-2026-09-02-0017".to_owned(),
        },
        SemanticMessage::Unknown {
            code: "future.surface.signal".to_owned(),
        },
    ]
}

fn english(message: &SemanticMessage) -> String {
    match message {
        SemanticMessage::SetupReady { profile } => {
            format!("Ready to use the «{}» profile.", bounded(profile))
        }
        SemanticMessage::ApprovalRequired { action } => {
            format!("Approval required: {}", bounded(action))
        }
        SemanticMessage::AttentionRequired { count } => {
            format!("{} need your attention.", format_count(*count, ','))
        }
        SemanticMessage::ErrorReported { detail } => {
            format!("Something went wrong: {}", bounded(detail))
        }
        SemanticMessage::RecoveryCompleted { count } => format!(
            "Recovered {} interrupted operation(s) without replaying work.",
            format_count(*count, ',')
        ),
        SemanticMessage::CapabilityUnavailable { capability } => {
            format!("This connection does not support {}.", bounded(capability))
        }
        SemanticMessage::ReceiptCompleted { receipt_id } => {
            format!("Work completed. Receipt: {}", bounded(receipt_id))
        }
        SemanticMessage::Unknown { code } => {
            format!("Unsupported interface message. Code: {}", bounded(code))
        }
    }
}

fn pseudo(message: &SemanticMessage) -> String {
    match message {
        SemanticMessage::SetupReady { profile } => format!(
            "[!! Řéáďý ţø ůšé ţħé «{}» þřøƒïļé — êxţřá !!]",
            bounded(profile)
        ),
        SemanticMessage::ApprovalRequired { action } => {
            format!("[!! Áþþřøváļ řéǫůïřéď — {} — êxţřá !!]", bounded(action))
        }
        SemanticMessage::AttentionRequired { count } => format!(
            "[!! {} ïţéɱš ńééď ýöůř áţţéńţïøń — êxţřá !!]",
            format_count(*count, ',')
        ),
        SemanticMessage::ErrorReported { detail } => {
            format!("[!! Šøɱéţħïńğ ŵéńţ ŵřøńğ — {} — êxţřá !!]", bounded(detail))
        }
        SemanticMessage::RecoveryCompleted { count } => format!(
            "[!! Řéçøvéřéď {} øþéřáţïøńš ŵïţħøůţ řéþļáýïńğ ŵøřķ — êxţřá !!]",
            format_count(*count, ',')
        ),
        SemanticMessage::CapabilityUnavailable { capability } => format!(
            "[!! Ţħïš çøńńéçţïøń ďøéš ńøţ šůþþøřţ {} — êxţřá !!]",
            bounded(capability)
        ),
        SemanticMessage::ReceiptCompleted { receipt_id } => format!(
            "[!! Ŵøřķ çøɱþļéţéď — řéçéïþţ {} — êxţřá !!]",
            bounded(receipt_id)
        ),
        SemanticMessage::Unknown { code } => format!(
            "[!! Ůńšůþþøřţéď ïńţéřƒáçé ɱéššáğé — çøďé {} — êxţřá !!]",
            bounded(code)
        ),
    }
}

fn bounded(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_DYNAMIC_CHARS {
        return normalized;
    }
    normalized
        .chars()
        .take(MAX_DYNAMIC_CHARS.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn format_count(count: u32, separator: char) -> String {
    let digits = count.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            result.push(separator);
        }
        result.push(character);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_semantic_family_has_a_catalog_fixture() {
        let codes = catalog_messages()
            .iter()
            .map(SemanticMessage::code)
            .collect::<Vec<_>>();
        for code in [
            "setup.ready",
            "approval.required",
            "attention.required",
            "error.reported",
            "recovery.completed",
            "capability.unavailable",
            "receipt.completed",
            "future.surface.signal",
        ] {
            assert!(codes.iter().any(|candidate| candidate == code));
        }
    }

    #[test]
    fn representative_spanish_path_preserves_codes_and_dynamic_values() {
        let copy = representative_path(
            InterfaceLocale::Spanish,
            "Personal",
            "Escribir el informe",
            "run-17",
        );
        assert_eq!(
            copy.clone().map(|item| item.code),
            ["setup.ready", "approval.required", "receipt.completed"]
        );
        assert!(copy.iter().all(|item| !item.used_fallback));
        assert!(copy[0].text.contains("Personal"));
        assert!(copy[1].text.contains("Escribir el informe"));
        assert!(copy[2].text.contains("run-17"));
    }

    #[test]
    fn missing_spanish_copy_uses_a_safe_inspectable_fallback() {
        let copy =
            SemanticMessage::AttentionRequired { count: 2 }.localize(InterfaceLocale::Spanish);
        assert!(copy.used_fallback);
        assert!(copy.text.contains("attention.required"));
    }

    #[test]
    fn pseudo_copy_expands_the_english_fixture() {
        let message = SemanticMessage::SetupReady {
            profile: "Personal".to_owned(),
        };
        assert!(
            message
                .localize(InterfaceLocale::Pseudo)
                .text
                .chars()
                .count()
                > message
                    .localize(InterfaceLocale::English)
                    .text
                    .chars()
                    .count()
        );
    }

    #[test]
    fn unknown_codes_stay_inspectable_in_every_locale() {
        let unknown = SemanticMessage::Unknown {
            code: "future.action".to_owned(),
        };
        for locale in InterfaceLocale::ALL {
            let copy = unknown.localize(locale);
            assert_eq!(copy.code, "future.action");
            assert!(copy.text.contains("future.action"));
        }
    }

    #[test]
    fn dynamic_values_are_bounded_and_counts_are_locale_ready() {
        let id = "r".repeat(300);
        let localized =
            SemanticMessage::ReceiptCompleted { receipt_id: id }.localize(InterfaceLocale::Pseudo);
        assert!(localized.text.contains(&format!("{}…", "r".repeat(159))));
        assert_eq!(format_count(1_234_567, ','), "1,234,567");
        assert_eq!(format_count(1_234_567, '.'), "1.234.567");
    }
}
