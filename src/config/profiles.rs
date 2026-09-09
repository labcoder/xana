//! Shared profile draft construction and lifecycle invariants.

use super::*;
use toml_edit::{DocumentMut, Item, value};

/// An explicit default change invalidates its old model override in the same atomic file.
/// Deterministic revision derivation keeps settings previews stable; no second-file race.
pub(crate) fn reconcile_selection_revision(
    before: &ConnectionRegistry,
    document: &mut DocumentMut,
) -> Result<(), ConfigError> {
    let after = XanaConfig::parse_registry(&document.to_string())?;
    let prior = &before.profiles[&before.default_profile];
    let next = &after.profiles[&after.default_profile];
    if prior.profile_id != next.profile_id {
        let revision = uuid::Uuid::new_v5(
            &before
                .model_selection_revision
                .unwrap_or(uuid::Uuid::NAMESPACE_URL),
            next.profile_id.as_bytes(),
        );
        document["model_selection_revision"] = value(revision.to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProfileRetirement {
    pub(crate) name: String,
    pub(crate) archive: bool,
    pub(crate) replacement: Option<String>,
    pub(crate) candidates: Vec<String>,
    pub(crate) removed_routes: Vec<String>,
    revision: String,
}

impl XanaConfig {
    pub(crate) fn create_profile_from_defaults(
        path: &Path,
        name: &str,
        connection: Option<&str>,
        model: Option<&str>,
        make_default: bool,
    ) -> Result<(), ConfigError> {
        let mut transaction = ConfigEditTransaction::begin(path)?;
        let document = transaction.document_mut();
        if profiles_table_mut(document)?.contains_key(name) {
            return Err(ConfigError::Edit(format!(
                "profile {name:?} already exists"
            )));
        }
        ensure_profile(document, name)?;
        let profile = profile_table_mut(document, name)?;
        if let Some(connection) = connection {
            if model.is_none()
                && profile.get("connection").and_then(Item::as_str) != Some(connection)
            {
                return Err(ConfigError::Edit("changing the connection requires an explicit model for that connection; omit both to reuse the current default binding".into()));
            }
            if profile.get("connection").and_then(Item::as_str) != Some(connection) {
                profile.remove("reasoning_effort");
                profile.remove("reasoning_summary");
            }
            profile["connection"] = value(connection);
        }
        if let Some(model) = model {
            profile["model"] = value(model);
        }
        if make_default {
            document["default_profile"] = value(name);
        }
        transaction.commit(true)
    }
    /// Preview consequences without changing credentials, history, or any project file.
    pub(crate) fn plan_profile_retirement(
        path: &Path,
        name: &str,
        archive: bool,
        replacement: Option<&str>,
    ) -> Result<ProfileRetirement, ConfigError> {
        let source = read_config(path)?;
        let registry = Self::parse_registry(&source)?;
        if !registry.profiles.contains_key(name) {
            return Err(ConfigError::Edit(format!("unknown profile {name:?}")));
        }
        let mut candidates = registry
            .profiles
            .values()
            .filter(|profile| profile.id != name && profile.can_be_default())
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        // Lists are name-sorted; choose the next eligible row and wrap once.
        let pivot = candidates.partition_point(|candidate| candidate.as_str() < name);
        candidates.rotate_left(pivot);
        if candidates.is_empty() {
            return Err(ConfigError::Edit("keep at least one active primary profile; create another profile before removing this one".into()));
        }
        let replacement = if registry.default_profile == name {
            let selected = replacement.unwrap_or(&candidates[0]);
            if !candidates.iter().any(|candidate| candidate == selected) {
                return Err(ConfigError::Edit(format!(
                    "replacement {selected:?} must be another active primary profile"
                )));
            }
            Some(selected.to_owned())
        } else {
            if replacement.is_some() {
                return Err(ConfigError::Edit(
                    "a replacement is only needed when removing the default profile".into(),
                ));
            }
            None
        };
        Ok(ProfileRetirement {
            name: name.to_owned(),
            archive,
            replacement,
            candidates,
            removed_routes: registry
                .routes
                .values()
                .filter(|route| route.profile == name)
                .map(|route| route.id.clone())
                .collect(),
            revision: blake3::hash(source.as_bytes()).to_hex().to_string(),
        })
    }

    /// Applying a reviewed plan is one validated config replacement under the writer lock.
    pub(crate) fn retire_profile(path: &Path, plan: &ProfileRetirement) -> Result<(), ConfigError> {
        let mut transaction = ConfigEditTransaction::begin(path)?;
        if blake3::hash(read_config(path)?.as_bytes())
            .to_hex()
            .as_str()
            != plan.revision
        {
            return Err(ConfigError::Edit(
                "configuration changed since the profile review; review again before applying"
                    .into(),
            ));
        }
        let document = transaction.document_mut();
        if let Some(replacement) = &plan.replacement {
            document["default_profile"] = value(replacement);
        }
        if plan.archive {
            profile_table_mut(document, &plan.name)?["archived"] = value(true);
        } else {
            profiles_table_mut(document)?.remove(&plan.name);
        }
        if plan
            .removed_routes
            .iter()
            .any(|route| document.get("default_child_route").and_then(Item::as_str) == Some(route))
        {
            document.remove("default_child_route");
        }
        if let Some(routes) = document.get_mut("routes").and_then(Item::as_table_mut) {
            for route in &plan.removed_routes {
                routes.remove(route);
            }
        }
        transaction.commit(true)
    }

    pub(crate) fn set_default_profile(path: &Path, name: &str) -> Result<(), ConfigError> {
        let mut transaction = ConfigEditTransaction::begin(path)?;
        if !transaction
            .registry
            .profiles
            .get(name)
            .is_some_and(ProfileConfig::can_be_default)
        {
            return Err(ConfigError::Edit(format!(
                "default {name:?} must name an active primary profile"
            )));
        }
        transaction.document_mut()["default_profile"] = value(name);
        transaction.commit(true)
    }
}

impl ProfileConfig {
    /// Eligibility is structural, not dependent on credentials or network availability.
    pub(crate) fn can_be_default(&self) -> bool {
        !self.archived && self.applies_to.contains(&ProfileUse::Primary)
    }
}

pub(crate) fn validate_profile_name(name: &str) -> Result<(), ConfigError> {
    validate_name("profile", name)
}

/// A new preset is a value copy, with its own identity, never a live inheritance link.
pub(crate) fn ensure_profile(document: &mut DocumentMut, name: &str) -> Result<(), ConfigError> {
    validate_profile_name(name)?;
    let default = document["default_profile"]
        .as_str()
        .ok_or_else(|| ConfigError::Edit("configuration has no default profile".into()))?
        .to_owned();
    let profiles = profiles_table_mut(document)?;
    if !profiles.contains_key(name) {
        let mut profile = profiles.get(&default).cloned().ok_or_else(|| {
            ConfigError::Edit("configuration has no complete default profile".into())
        })?;
        profile["profile_id"] = value(uuid::Uuid::new_v4().to_string());
        let table = profile
            .as_table_mut()
            .ok_or_else(|| ConfigError::Edit("profile must be a table".into()))?;
        table.remove("archived");
        if let Some(provider) = table.remove("provider") {
            table.insert("connection", provider);
        }
        profiles.insert(name, profile);
    }
    Ok(())
}

/// Rename the single generated setup profile, before it has any external references.
pub(crate) fn name_initial_profile(source: &str, name: &str) -> Result<String, ConfigError> {
    validate_profile_name(name)?;
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| ConfigError::Edit(error.to_string()))?;
    let old = document["default_profile"]
        .as_str()
        .ok_or_else(|| ConfigError::Edit("missing default profile".into()))?
        .to_owned();
    let profiles = profiles_table_mut(&mut document)?;
    if profiles.len() != 1 {
        return Err(ConfigError::Edit(
            "initial profile naming requires exactly one profile".into(),
        ));
    }
    let mut profile = profiles
        .remove(&old)
        .ok_or_else(|| ConfigError::Edit("missing initial profile".into()))?;
    profile["profile_id"] = value(uuid::Uuid::new_v4().to_string());
    profiles.insert(name, profile);
    document["default_profile"] = value(name);
    if let Some(routes) = document.get_mut("routes").and_then(Item::as_table_mut) {
        for (_, route) in routes.iter_mut() {
            if route.get("profile").and_then(Item::as_str) == Some(&old) {
                route["profile"] = value(name);
            }
        }
    }
    let rendered = document.to_string();
    XanaConfig::parse_registry(&rendered)?;
    Ok(rendered)
}
