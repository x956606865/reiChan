use serde::{Deserialize, Serialize};

use super::{edge_texture::EdgeTextureConfig, SplitThresholdOverrides};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitPrimaryMode {
    EdgeTexture,
    Projection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitModeSelector {
    EdgeTextureOnly,
    ProjectionOnly,
    Hybrid {
        #[serde(default = "SplitModeSelector::default_primary")]
        primary: SplitPrimaryMode,
        #[serde(default = "SplitModeSelector::default_fallback")]
        fallback: SplitPrimaryMode,
    },
}

impl SplitModeSelector {
    const fn default_primary() -> SplitPrimaryMode {
        SplitPrimaryMode::EdgeTexture
    }

    const fn default_fallback() -> SplitPrimaryMode {
        SplitPrimaryMode::Projection
    }
}

impl Default for SplitModeSelector {
    fn default() -> Self {
        Self::Hybrid {
            primary: Self::default_primary(),
            fallback: Self::default_fallback(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitConfig {
    pub min_aspect_ratio: f32,
    pub padding_ratio: f32,
    pub confidence_threshold: f32,
    pub cover_content_ratio: f32,
    pub min_foreground_ratio: f32,
    pub max_center_offset_ratio: f32,
    pub mode: SplitModeSelector,
    pub edge_texture: EdgeTextureConfig,
    pub projection: ProjectionConfig,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            min_aspect_ratio: 1.2,
            padding_ratio: 0.015,
            confidence_threshold: 0.1,
            cover_content_ratio: 0.45,
            min_foreground_ratio: 0.01,
            max_center_offset_ratio: 0.05,
            mode: SplitModeSelector::default(),
            edge_texture: EdgeTextureConfig::default(),
            projection: ProjectionConfig::default(),
        }
    }
}

impl SplitConfig {
    pub fn with_overrides(mut self, overrides: &SplitThresholdOverrides) -> Self {
        if let Some(mode) = overrides.mode {
            self.mode = mode;
        }

        self.padding_ratio = overrides.padding_ratio.unwrap_or(self.padding_ratio);
        self.confidence_threshold = overrides
            .confidence_threshold
            .unwrap_or(self.confidence_threshold);
        self.cover_content_ratio = overrides
            .cover_content_ratio
            .unwrap_or(self.cover_content_ratio);
        self.min_foreground_ratio = overrides
            .min_foreground_ratio
            .unwrap_or(self.min_foreground_ratio);
        self.max_center_offset_ratio = overrides
            .max_center_offset_ratio
            .unwrap_or(self.max_center_offset_ratio);

        if let Some(edge_overrides) = overrides.edge_texture.as_ref() {
            self.edge_texture = self.edge_texture.apply_overrides(edge_overrides);
        }

        if let Some(projection_overrides) = overrides.projection.as_ref() {
            self.projection = self.projection.apply_overrides(projection_overrides);
        }

        if let Some(edge_exclusion_ratio) = overrides.edge_exclusion_ratio {
            self.projection.edge_exclusion_ratio = edge_exclusion_ratio;
        }

        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionConfig {
    pub edge_exclusion_ratio: f32,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        Self {
            edge_exclusion_ratio: 0.12,
        }
    }
}

impl ProjectionConfig {
    pub fn apply_overrides(mut self, overrides: &ProjectionThresholdOverrides) -> Self {
        if let Some(ratio) = overrides.edge_exclusion_ratio {
            self.edge_exclusion_ratio = ratio;
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTextureThresholdOverrides {
    pub gamma: Option<f32>,
    pub gaussian_kernel: Option<u32>,
    pub entropy_window: Option<u32>,
    pub entropy_bins: Option<u32>,
    pub white_threshold: Option<f32>,
    pub brightness_thresholds: Option<[f32; 2]>,
    pub brightness_weight: Option<f32>,
    pub enable_dual_brightness: Option<bool>,
    pub left_search_ratio: Option<f32>,
    pub right_search_ratio: Option<f32>,
    pub center_search_ratio: Option<f32>,
    pub min_margin_ratio: Option<f32>,
    pub center_max_ratio: Option<f32>,
    pub score_weights: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProjectionThresholdOverrides {
    pub edge_exclusion_ratio: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_config_applies_nested_overrides() {
        let overrides = super::SplitThresholdOverrides {
            cover_content_ratio: None,
            confidence_threshold: None,
            edge_exclusion_ratio: None,
            min_foreground_ratio: None,
            padding_ratio: None,
            max_center_offset_ratio: None,
            edge_texture: Some(EdgeTextureThresholdOverrides {
                white_threshold: Some(0.55),
                score_weights: Some([0.2, 0.3, 0.5]),
                gaussian_kernel: Some(7),
                ..Default::default()
            }),
            projection: Some(ProjectionThresholdOverrides {
                edge_exclusion_ratio: Some(0.2),
            }),
            mode: Some(SplitModeSelector::ProjectionOnly),
        };

        let updated = SplitConfig::default().with_overrides(&overrides);

        assert!(matches!(updated.mode, SplitModeSelector::ProjectionOnly));
        assert!((updated.edge_texture.white_threshold - 0.55).abs() < f32::EPSILON);
        assert_eq!(updated.edge_texture.score_weights, [0.2, 0.3, 0.5]);
        assert_eq!(updated.edge_texture.gaussian_kernel, 7);
        assert!((updated.projection.edge_exclusion_ratio - 0.2).abs() < f32::EPSILON);
    }
}
