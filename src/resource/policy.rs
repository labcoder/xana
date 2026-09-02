//! Configurable resource admission limits beneath immutable ceilings.

use super::{
    DEFAULT_STATIC_RASTER_BYTES, DEFAULT_STATIC_RASTER_PIXELS, DEFAULT_STATIC_RASTER_TURN_BYTES,
    DEFAULT_STATIC_RASTERS_PER_TURN, ResourceKindV1,
};
use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResourcePolicyV1 {
    pub(crate) max_resources_per_turn: u16,
    pub(crate) max_total_source_bytes: u64,
    pub(crate) max_active_jobs: u16,
    pub(crate) max_active_players: u16,
    pub(crate) max_cache_bytes: u64,
    pub(crate) max_in_memory_buffer_bytes: u64,
    pub(crate) max_transform_millis: u64,
    pub(crate) static_raster: StaticRasterPolicyV1,
    pub(crate) animated_raster: AnimatedRasterPolicyV1,
    pub(crate) svg: StructuredVisualPolicyV1,
    pub(crate) lottie: LottiePolicyV1,
    pub(crate) audio: AudioPolicyV1,
    pub(crate) video: VideoPolicyV1,
    pub(crate) unknown: UnknownResourcePolicyV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StaticRasterPolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_pixels: u64,
    pub(crate) max_edge: u32,
    pub(crate) max_per_turn: u16,
    pub(crate) max_total_bytes_per_turn: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AnimatedRasterPolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_canvas_pixels: u64,
    pub(crate) max_frames: u32,
    pub(crate) max_duration_millis: u64,
    pub(crate) max_pixel_frames: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct StructuredVisualPolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_elements: u32,
    pub(crate) max_commands: u32,
    pub(crate) max_transform_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LottiePolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_depth: u16,
    pub(crate) max_items: u32,
    pub(crate) max_layers_and_assets: u32,
    pub(crate) max_duration_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AudioPolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_duration_millis: u64,
    pub(crate) max_sample_rate_hz: u32,
    pub(crate) max_channels: u16,
    pub(crate) max_metadata_entries: u16,
    pub(crate) max_cover_art_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct VideoPolicyV1 {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_duration_millis: u64,
    pub(crate) max_width: u32,
    pub(crate) max_height: u32,
    pub(crate) max_pixels: u64,
    pub(crate) max_tracks: u16,
    pub(crate) max_frames_per_second: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UnknownResourcePolicyV1 {
    pub(crate) max_source_bytes: u64,
}

macro_rules! defaults {
    ($type:ty, $value:expr) => {
        impl Default for $type {
            fn default() -> Self {
                $value
            }
        }
    };
}

defaults!(
    StaticRasterPolicyV1,
    Self {
        max_source_bytes: DEFAULT_STATIC_RASTER_BYTES as u64,
        max_pixels: DEFAULT_STATIC_RASTER_PIXELS,
        max_edge: 16_384,
        max_per_turn: DEFAULT_STATIC_RASTERS_PER_TURN as u16,
        max_total_bytes_per_turn: DEFAULT_STATIC_RASTER_TURN_BYTES,
    }
);
defaults!(
    AnimatedRasterPolicyV1,
    Self {
        max_source_bytes: 4 * 1024 * 1024,
        max_canvas_pixels: 20_000_000,
        max_frames: 120,
        max_duration_millis: 30_000,
        max_pixel_frames: 80_000_000,
    }
);
defaults!(
    StructuredVisualPolicyV1,
    Self {
        max_source_bytes: 2 * 1024 * 1024,
        max_elements: 50_000,
        max_commands: 250_000,
        max_transform_millis: 5_000,
    }
);
defaults!(
    LottiePolicyV1,
    Self {
        max_source_bytes: 2 * 1024 * 1024,
        max_depth: 64,
        max_items: 100_000,
        max_layers_and_assets: 256,
        max_duration_millis: 120_000,
    }
);
defaults!(
    AudioPolicyV1,
    Self {
        max_source_bytes: 25 * 1024 * 1024,
        max_duration_millis: 30 * 60 * 1_000,
        max_sample_rate_hz: 96_000,
        max_channels: 8,
        max_metadata_entries: 16,
        max_cover_art_bytes: 4 * 1024 * 1024,
    }
);
defaults!(
    VideoPolicyV1,
    Self {
        max_source_bytes: 64 * 1024 * 1024,
        max_duration_millis: 10 * 60 * 1_000,
        max_width: 3_840,
        max_height: 2_160,
        max_pixels: 8_294_400,
        max_tracks: 8,
        max_frames_per_second: 60,
    }
);
defaults!(
    UnknownResourcePolicyV1,
    Self {
        max_source_bytes: 4 * 1024 * 1024,
    }
);

impl Default for ResourcePolicyV1 {
    fn default() -> Self {
        Self {
            max_resources_per_turn: 8,
            max_total_source_bytes: 64 * 1024 * 1024,
            max_active_jobs: 2,
            max_active_players: 1,
            max_cache_bytes: 128 * 1024 * 1024,
            max_in_memory_buffer_bytes: 8 * 1024 * 1024,
            max_transform_millis: 10_000,
            static_raster: StaticRasterPolicyV1::default(),
            animated_raster: AnimatedRasterPolicyV1::default(),
            svg: StructuredVisualPolicyV1::default(),
            lottie: LottiePolicyV1::default(),
            audio: AudioPolicyV1::default(),
            video: VideoPolicyV1::default(),
            unknown: UnknownResourcePolicyV1::default(),
        }
    }
}

impl ResourcePolicyV1 {
    pub(crate) fn max_source_bytes_for(&self, kind: &ResourceKindV1) -> u64 {
        match kind {
            ResourceKindV1::StaticRaster => self.static_raster.max_source_bytes,
            ResourceKindV1::AnimatedRaster => self.animated_raster.max_source_bytes,
            ResourceKindV1::Svg => self.svg.max_source_bytes,
            ResourceKindV1::Lottie => self.lottie.max_source_bytes,
            ResourceKindV1::Audio => self.audio.max_source_bytes,
            ResourceKindV1::Video => self.video.max_source_bytes,
            ResourceKindV1::Binary | ResourceKindV1::Unknown(_) => self.unknown.max_source_bytes,
        }
    }

    pub(crate) fn hard_ceiling() -> Self {
        Self {
            max_resources_per_turn: 32,
            max_total_source_bytes: 512 * 1024 * 1024,
            max_active_jobs: 4,
            max_active_players: 2,
            max_cache_bytes: 512 * 1024 * 1024,
            max_in_memory_buffer_bytes: 64 * 1024 * 1024,
            max_transform_millis: 120_000,
            static_raster: StaticRasterPolicyV1 {
                max_source_bytes: 32 * 1024 * 1024,
                max_pixels: 64_000_000,
                max_edge: 32_768,
                max_per_turn: 32,
                max_total_bytes_per_turn: 128 * 1024 * 1024,
            },
            animated_raster: AnimatedRasterPolicyV1 {
                max_source_bytes: 32 * 1024 * 1024,
                max_canvas_pixels: 64_000_000,
                max_frames: 600,
                max_duration_millis: 5 * 60 * 1_000,
                max_pixel_frames: 1_000_000_000,
            },
            svg: StructuredVisualPolicyV1 {
                max_source_bytes: 8 * 1024 * 1024,
                max_elements: 250_000,
                max_commands: 1_000_000,
                max_transform_millis: 30_000,
            },
            lottie: LottiePolicyV1 {
                max_source_bytes: 8 * 1024 * 1024,
                max_depth: 128,
                max_items: 500_000,
                max_layers_and_assets: 1_024,
                max_duration_millis: 10 * 60 * 1_000,
            },
            audio: AudioPolicyV1 {
                max_source_bytes: 128 * 1024 * 1024,
                max_duration_millis: 2 * 60 * 60 * 1_000,
                max_sample_rate_hz: 192_000,
                max_channels: 32,
                max_metadata_entries: 64,
                max_cover_art_bytes: 16 * 1024 * 1024,
            },
            video: VideoPolicyV1 {
                max_source_bytes: 512 * 1024 * 1024,
                max_duration_millis: 2 * 60 * 60 * 1_000,
                max_width: 8_192,
                max_height: 8_192,
                max_pixels: 64_000_000,
                max_tracks: 32,
                max_frames_per_second: 240,
            },
            unknown: UnknownResourcePolicyV1 {
                max_source_bytes: 32 * 1024 * 1024,
            },
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ResourcePolicyError> {
        let ceiling = Self::hard_ceiling();
        macro_rules! check {
            ($field:expr, $value:expr, $ceiling:expr) => {
                check_limit($field, u128::from($value), u128::from($ceiling))?
            };
        }
        check!(
            "max_resources_per_turn",
            self.max_resources_per_turn,
            ceiling.max_resources_per_turn
        );
        check!(
            "max_total_source_bytes",
            self.max_total_source_bytes,
            ceiling.max_total_source_bytes
        );
        check!(
            "max_active_jobs",
            self.max_active_jobs,
            ceiling.max_active_jobs
        );
        check!(
            "max_active_players",
            self.max_active_players,
            ceiling.max_active_players
        );
        check!(
            "max_cache_bytes",
            self.max_cache_bytes,
            ceiling.max_cache_bytes
        );
        check!(
            "max_in_memory_buffer_bytes",
            self.max_in_memory_buffer_bytes,
            ceiling.max_in_memory_buffer_bytes
        );
        check!(
            "max_transform_millis",
            self.max_transform_millis,
            ceiling.max_transform_millis
        );

        macro_rules! fields {
            ($prefix:literal, $ours:expr, $cap:expr, [$($field:ident),+ $(,)?]) => {
                $(check!(concat!($prefix, ".", stringify!($field)), $ours.$field, $cap.$field);)+
            };
        }
        fields!(
            "static_raster",
            self.static_raster,
            ceiling.static_raster,
            [
                max_source_bytes,
                max_pixels,
                max_edge,
                max_per_turn,
                max_total_bytes_per_turn
            ]
        );
        fields!(
            "animated_raster",
            self.animated_raster,
            ceiling.animated_raster,
            [
                max_source_bytes,
                max_canvas_pixels,
                max_frames,
                max_duration_millis,
                max_pixel_frames
            ]
        );
        fields!(
            "svg",
            self.svg,
            ceiling.svg,
            [
                max_source_bytes,
                max_elements,
                max_commands,
                max_transform_millis
            ]
        );
        fields!(
            "lottie",
            self.lottie,
            ceiling.lottie,
            [
                max_source_bytes,
                max_depth,
                max_items,
                max_layers_and_assets,
                max_duration_millis
            ]
        );
        fields!(
            "audio",
            self.audio,
            ceiling.audio,
            [
                max_source_bytes,
                max_duration_millis,
                max_sample_rate_hz,
                max_channels,
                max_metadata_entries,
                max_cover_art_bytes
            ]
        );
        fields!(
            "video",
            self.video,
            ceiling.video,
            [
                max_source_bytes,
                max_duration_millis,
                max_width,
                max_height,
                max_pixels,
                max_tracks,
                max_frames_per_second
            ]
        );
        check!(
            "unknown.max_source_bytes",
            self.unknown.max_source_bytes,
            ceiling.unknown.max_source_bytes
        );

        if self.static_raster.max_per_turn > self.max_resources_per_turn {
            return Err(ResourcePolicyError::Inconsistent {
                field: "static_raster.max_per_turn",
                reason: "cannot exceed max_resources_per_turn",
            });
        }
        Ok(())
    }

    pub(crate) fn effective_with(&self, route: &Self) -> Result<Self, ResourcePolicyError> {
        self.validate()?;
        route.validate()?;
        let mut effective = self.clone();
        macro_rules! lower {
            ($target:expr, $route:expr, [$($field:ident),+ $(,)?]) => {
                $($target.$field = $target.$field.min($route.$field);)+
            };
        }
        lower!(
            effective,
            route,
            [
                max_resources_per_turn,
                max_total_source_bytes,
                max_active_jobs,
                max_active_players,
                max_cache_bytes,
                max_in_memory_buffer_bytes,
                max_transform_millis
            ]
        );
        lower!(
            effective.static_raster,
            route.static_raster,
            [
                max_source_bytes,
                max_pixels,
                max_edge,
                max_per_turn,
                max_total_bytes_per_turn
            ]
        );
        lower!(
            effective.animated_raster,
            route.animated_raster,
            [
                max_source_bytes,
                max_canvas_pixels,
                max_frames,
                max_duration_millis,
                max_pixel_frames
            ]
        );
        lower!(
            effective.svg,
            route.svg,
            [
                max_source_bytes,
                max_elements,
                max_commands,
                max_transform_millis
            ]
        );
        lower!(
            effective.lottie,
            route.lottie,
            [
                max_source_bytes,
                max_depth,
                max_items,
                max_layers_and_assets,
                max_duration_millis
            ]
        );
        lower!(
            effective.audio,
            route.audio,
            [
                max_source_bytes,
                max_duration_millis,
                max_sample_rate_hz,
                max_channels,
                max_metadata_entries,
                max_cover_art_bytes
            ]
        );
        lower!(
            effective.video,
            route.video,
            [
                max_source_bytes,
                max_duration_millis,
                max_width,
                max_height,
                max_pixels,
                max_tracks,
                max_frames_per_second
            ]
        );
        effective.unknown.max_source_bytes = effective
            .unknown
            .max_source_bytes
            .min(route.unknown.max_source_bytes);
        effective.validate()?;
        Ok(effective)
    }

    pub(crate) fn admit_source_lengths<I>(&self, lengths: I) -> Result<(), ResourcePolicyError>
    where
        I: IntoIterator<Item = u64>,
    {
        self.validate()?;
        let mut count = 0_u16;
        let mut bytes = 0_u128;
        for length in lengths {
            count = count
                .checked_add(1)
                .ok_or(ResourcePolicyError::ArithmeticOverflow("resource count"))?;
            bytes = bytes
                .checked_add(u128::from(length))
                .ok_or(ResourcePolicyError::ArithmeticOverflow("resource bytes"))?;
        }
        if count > self.max_resources_per_turn {
            return Err(ResourcePolicyError::TurnLimit {
                field: "resource count",
                actual: u128::from(count),
                limit: u128::from(self.max_resources_per_turn),
            });
        }
        if bytes > u128::from(self.max_total_source_bytes) {
            return Err(ResourcePolicyError::TurnLimit {
                field: "resource bytes",
                actual: bytes,
                limit: u128::from(self.max_total_source_bytes),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourcePolicyError {
    Zero(&'static str),
    AboveCeiling {
        field: &'static str,
        value: u128,
        ceiling: u128,
    },
    Inconsistent {
        field: &'static str,
        reason: &'static str,
    },
    ArithmeticOverflow(&'static str),
    TurnLimit {
        field: &'static str,
        actual: u128,
        limit: u128,
    },
}

impl fmt::Display for ResourcePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero(field) => write!(formatter, "resource policy {field} must not be zero"),
            Self::AboveCeiling {
                field,
                value,
                ceiling,
            } => write!(
                formatter,
                "resource policy {field} is {value}; compiled ceiling is {ceiling}"
            ),
            Self::Inconsistent { field, reason } => {
                write!(formatter, "resource policy {field} {reason}")
            }
            Self::ArithmeticOverflow(operation) => {
                write!(formatter, "resource policy {operation} overflowed")
            }
            Self::TurnLimit {
                field,
                actual,
                limit,
            } => write!(
                formatter,
                "resource turn {field} is {actual}; effective limit is {limit}"
            ),
        }
    }
}

impl Error for ResourcePolicyError {}

fn check_limit(field: &'static str, value: u128, ceiling: u128) -> Result<(), ResourcePolicyError> {
    if value == 0 {
        return Err(ResourcePolicyError::Zero(field));
    }
    if value > ceiling {
        return Err(ResourcePolicyError::AboveCeiling {
            field,
            value,
            ceiling,
        });
    }
    Ok(())
}
