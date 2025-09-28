use serde::{Deserialize, Serialize};

use super::SplitThresholdOverrides;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplitConfig {
    pub min_aspect_ratio: f32,
    pub padding_ratio: f32,
    pub confidence_threshold: f32,
    pub cover_content_ratio: f32,
    pub edge_exclusion_ratio: f32,
    pub min_foreground_ratio: f32,
    pub max_center_offset_ratio: f32,
}

impl Default for SplitConfig {
    fn default() -> Self {
        Self {
            min_aspect_ratio: 1.2,
            padding_ratio: 0.015,
            confidence_threshold: 0.1,
            cover_content_ratio: 0.45,
            edge_exclusion_ratio: 0.12,
            min_foreground_ratio: 0.01,
            max_center_offset_ratio: 0.05,
        }
    }
}

impl SplitConfig {
    pub fn with_overrides(self, overrides: &SplitThresholdOverrides) -> Self {
        Self {
            min_aspect_ratio: self.min_aspect_ratio,
            padding_ratio: overrides.padding_ratio.unwrap_or(self.padding_ratio),
            confidence_threshold: overrides
                .confidence_threshold
                .unwrap_or(self.confidence_threshold),
            cover_content_ratio: overrides
                .cover_content_ratio
                .unwrap_or(self.cover_content_ratio),
            edge_exclusion_ratio: overrides
                .edge_exclusion_ratio
                .unwrap_or(self.edge_exclusion_ratio),
            min_foreground_ratio: overrides
                .min_foreground_ratio
                .unwrap_or(self.min_foreground_ratio),
            max_center_offset_ratio: overrides
                .max_center_offset_ratio
                .unwrap_or(self.max_center_offset_ratio),
        }
    }
}
