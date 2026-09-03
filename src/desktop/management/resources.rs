//! Typed resource-admission policy management for graphical frontends.

use super::{DesktopControlPlane, DesktopEntityMutationReceipt, DesktopError, control_error};
use crate::{config::XanaConfig, resource::ResourcePolicyV1};
use std::collections::BTreeMap;

const RESOURCE_POLICY_VERSION: u16 = 1;
const MAX_RESOURCE_EDITS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResourceLimit {
    pub key: String,
    pub group: String,
    pub label: String,
    pub unit: String,
    pub configured: u64,
    pub default: u64,
    pub hard_ceiling: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResourcePolicySnapshot {
    pub version: u16,
    pub limits: Vec<DesktopResourceLimit>,
    pub effective_note: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopResourcePolicyDraft {
    pub values: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopResourcePolicyPreview {
    pub snapshot: DesktopResourcePolicySnapshot,
    pub changed_keys: Vec<String>,
    pub effect_timing: String,
}

impl DesktopControlPlane {
    pub fn resource_policy_snapshot(&self) -> Result<DesktopResourcePolicySnapshot, DesktopError> {
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        Ok(project_policy(&registry.resources))
    }

    pub fn preview_resource_policy(
        &self,
        draft: &DesktopResourcePolicyDraft,
    ) -> Result<DesktopResourcePolicyPreview, DesktopError> {
        if draft.values.len() > MAX_RESOURCE_EDITS {
            return Err(control_error(format!(
                "resource policy draft exceeds {MAX_RESOURCE_EDITS} fields"
            )));
        }
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        let mut policy = registry.resources.clone();
        for (key, value) in &draft.values {
            apply_limit(&mut policy, key, *value)?;
        }
        policy.validate().map_err(control_error)?;
        let mut changed_keys = draft
            .values
            .iter()
            .filter_map(|(key, expected)| {
                value_for(&registry.resources, key)
                    .filter(|current| current != expected)
                    .map(|_| key.clone())
            })
            .collect::<Vec<_>>();
        changed_keys.sort();
        Ok(DesktopResourcePolicyPreview {
            snapshot: project_policy(&policy),
            changed_keys,
            effect_timing: "Applies to new acquisition and new turns; retained artifacts are unchanged. A route, provider, decoder, or platform may enforce a lower limit.".to_owned(),
        })
    }

    pub fn save_resource_policy(
        &self,
        draft: DesktopResourcePolicyDraft,
    ) -> Result<DesktopEntityMutationReceipt, DesktopError> {
        let preview = self.preview_resource_policy(&draft)?;
        let registry =
            XanaConfig::load_registry_from(self.paths.config_file()).map_err(control_error)?;
        let mut policy = registry.resources;
        for (key, value) in &draft.values {
            apply_limit(&mut policy, key, *value)?;
        }
        XanaConfig::set_resource_policy(self.paths.config_file(), policy).map_err(control_error)?;
        Ok(DesktopEntityMutationReceipt {
            semantic_code: "resource_policy.save.completed.v1".to_owned(),
            subject: "resources".to_owned(),
            effect: format!("{} field(s) changed", preview.changed_keys.len()),
            detail: "Configuration backup created; retained artifacts were preserved.".to_owned(),
        })
    }
}

macro_rules! resource_fields {
    ($macro:ident) => {
        $macro! {
            ("max_resources_per_turn", "All resources", "Resources per turn", "count", max_resources_per_turn),
            ("max_total_source_bytes", "All resources", "Source bytes per turn", "bytes", max_total_source_bytes),
            ("max_active_jobs", "All resources", "Concurrent transforms", "count", max_active_jobs),
            ("max_active_players", "All resources", "Concurrent players", "count", max_active_players),
            ("max_cache_bytes", "All resources", "Preview cache", "bytes", max_cache_bytes),
            ("max_in_memory_buffer_bytes", "All resources", "In-memory buffer", "bytes", max_in_memory_buffer_bytes),
            ("max_transform_millis", "All resources", "Transform time", "milliseconds", max_transform_millis),
            ("static_raster.max_source_bytes", "Static images", "Source bytes", "bytes", static_raster.max_source_bytes),
            ("static_raster.max_pixels", "Static images", "Decoded pixels", "pixels", static_raster.max_pixels),
            ("static_raster.max_edge", "Static images", "Longest edge", "pixels", static_raster.max_edge),
            ("static_raster.max_per_turn", "Static images", "Images per turn", "count", static_raster.max_per_turn),
            ("static_raster.max_total_bytes_per_turn", "Static images", "Image bytes per turn", "bytes", static_raster.max_total_bytes_per_turn),
            ("animated_raster.max_source_bytes", "Animated images", "Source bytes", "bytes", animated_raster.max_source_bytes),
            ("animated_raster.max_canvas_pixels", "Animated images", "Canvas pixels", "pixels", animated_raster.max_canvas_pixels),
            ("animated_raster.max_frames", "Animated images", "Frames", "count", animated_raster.max_frames),
            ("animated_raster.max_duration_millis", "Animated images", "Duration", "milliseconds", animated_raster.max_duration_millis),
            ("animated_raster.max_pixel_frames", "Animated images", "Pixel-frames", "pixel-frames", animated_raster.max_pixel_frames),
            ("svg.max_source_bytes", "SVG", "Source bytes", "bytes", svg.max_source_bytes),
            ("svg.max_elements", "SVG", "Elements", "count", svg.max_elements),
            ("svg.max_commands", "SVG", "Drawing commands", "count", svg.max_commands),
            ("svg.max_transform_millis", "SVG", "Transform time", "milliseconds", svg.max_transform_millis),
            ("lottie.max_source_bytes", "Lottie", "Source bytes", "bytes", lottie.max_source_bytes),
            ("lottie.max_depth", "Lottie", "Document depth", "count", lottie.max_depth),
            ("lottie.max_items", "Lottie", "Items", "count", lottie.max_items),
            ("lottie.max_layers_and_assets", "Lottie", "Layers and assets", "count", lottie.max_layers_and_assets),
            ("lottie.max_duration_millis", "Lottie", "Duration", "milliseconds", lottie.max_duration_millis),
            ("audio.max_source_bytes", "Audio", "Source bytes", "bytes", audio.max_source_bytes),
            ("audio.max_duration_millis", "Audio", "Duration", "milliseconds", audio.max_duration_millis),
            ("audio.max_sample_rate_hz", "Audio", "Sample rate", "hertz", audio.max_sample_rate_hz),
            ("audio.max_channels", "Audio", "Channels", "count", audio.max_channels),
            ("audio.max_metadata_entries", "Audio", "Metadata entries", "count", audio.max_metadata_entries),
            ("audio.max_cover_art_bytes", "Audio", "Cover-art bytes", "bytes", audio.max_cover_art_bytes),
            ("video.max_source_bytes", "Video", "Source bytes", "bytes", video.max_source_bytes),
            ("video.max_duration_millis", "Video", "Duration", "milliseconds", video.max_duration_millis),
            ("video.max_width", "Video", "Width", "pixels", video.max_width),
            ("video.max_height", "Video", "Height", "pixels", video.max_height),
            ("video.max_pixels", "Video", "Frame pixels", "pixels", video.max_pixels),
            ("video.max_tracks", "Video", "Tracks", "count", video.max_tracks),
            ("video.max_frames_per_second", "Video", "Frame rate", "frames/second", video.max_frames_per_second),
            ("unknown.max_source_bytes", "Other files", "Source bytes", "bytes", unknown.max_source_bytes)
        }
    };
}

fn project_policy(policy: &ResourcePolicyV1) -> DesktopResourcePolicySnapshot {
    let defaults = ResourcePolicyV1::default();
    let ceiling = ResourcePolicyV1::hard_ceiling();
    macro_rules! collect {
        ($(($key:literal, $group:literal, $label:literal, $unit:literal, $($field:ident).+)),+ $(,)?) => {
            vec![$(DesktopResourceLimit {
                key: $key.to_owned(),
                group: $group.to_owned(),
                label: $label.to_owned(),
                unit: $unit.to_owned(),
                configured: policy.$($field).+ as u64,
                default: defaults.$($field).+ as u64,
                hard_ceiling: ceiling.$($field).+ as u64,
            }),+]
        };
    }
    DesktopResourcePolicySnapshot {
        version: RESOURCE_POLICY_VERSION,
        limits: resource_fields!(collect),
        effective_note: "Configured values are soft upper bounds. Selected routes, providers, codecs, and platforms may lower them; unknown capability remains unknown.".to_owned(),
    }
}

fn value_for(policy: &ResourcePolicyV1, key: &str) -> Option<u64> {
    macro_rules! get {
        ($(($key:literal, $group:literal, $label:literal, $unit:literal, $($field:ident).+)),+ $(,)?) => {
            match key {
                $($key => Some(policy.$($field).+ as u64),)+
                _ => None,
            }
        };
    }
    resource_fields!(get)
}

fn apply_limit(policy: &mut ResourcePolicyV1, key: &str, value: u64) -> Result<(), DesktopError> {
    macro_rules! set {
        ($(($key:literal, $group:literal, $label:literal, $unit:literal, $($field:ident).+)),+ $(,)?) => {
            match key {
                $($key => {
                    policy.$($field).+ = value.try_into().map_err(|_| {
                        control_error(format!("{key} does not fit its integer type"))
                    })?;
                },)+
                _ => return Err(control_error(format!("unknown resource limit {key:?}"))),
            }
        };
    }
    resource_fields!(set);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{InitialConfig, InitialConnection, PermissionMode},
        shell::ShellConfig,
    };
    use std::fs;
    use tempfile::tempdir;

    fn control() -> (tempfile::TempDir, DesktopControlPlane) {
        let directory = tempdir().unwrap();
        let control =
            DesktopControlPlane::resolve(Some(directory.path().join("home").into_os_string()))
                .unwrap();
        let rendered = XanaConfig::render_initial(InitialConfig {
            connection: InitialConnection::Ollama {
                name: "local".to_owned(),
                base_url: "http://localhost:11434/v1".to_owned(),
            },
            model: "qwen".to_owned(),
            max_tool_rounds: 8,
            shell: ShellConfig::default(),
            permission_mode: PermissionMode::Ask,
            reasoning_effort: None,
        })
        .unwrap();
        fs::create_dir_all(control.paths.config_file().parent().unwrap()).unwrap();
        fs::write(control.paths.config_file(), rendered).unwrap();
        (directory, control)
    }

    #[test]
    fn every_resource_limit_has_default_and_hard_ceiling() {
        let (_directory, control) = control();
        let snapshot = control.resource_policy_snapshot().unwrap();
        assert_eq!(snapshot.limits.len(), 40);
        assert!(snapshot.limits.iter().all(|limit| {
            limit.configured > 0 && limit.default > 0 && limit.default <= limit.hard_ceiling
        }));
    }

    #[test]
    fn preview_rejects_zero_and_cross_field_inconsistency() {
        let (_directory, control) = control();
        let zero = DesktopResourcePolicyDraft {
            values: BTreeMap::from([("video.max_tracks".to_owned(), 0)]),
        };
        assert!(control.preview_resource_policy(&zero).is_err());

        let inconsistent = DesktopResourcePolicyDraft {
            values: BTreeMap::from([
                ("max_resources_per_turn".to_owned(), 2),
                ("static_raster.max_per_turn".to_owned(), 3),
            ]),
        };
        assert!(control.preview_resource_policy(&inconsistent).is_err());
    }

    #[test]
    fn save_is_atomic_and_visible_in_a_fresh_snapshot() {
        let (_directory, control) = control();
        let draft = DesktopResourcePolicyDraft {
            values: BTreeMap::from([("max_active_players".to_owned(), 2)]),
        };
        let receipt = control.save_resource_policy(draft).unwrap();
        assert_eq!(receipt.semantic_code, "resource_policy.save.completed.v1");
        let snapshot = control.resource_policy_snapshot().unwrap();
        assert_eq!(
            snapshot
                .limits
                .iter()
                .find(|limit| limit.key == "max_active_players")
                .unwrap()
                .configured,
            2
        );
    }
}
