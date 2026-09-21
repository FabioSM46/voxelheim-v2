//! Bounded built-in shadow configuration; celestial motion and brightness stay in sky.
use bevy::light::{
    CascadeShadowConfig, CascadeShadowConfigBuilder, DirectionalLightShadowMap, PointLightShadowMap,
};
use bevy::prelude::*;

pub(super) fn install_shadow_maps(app: &mut App) {
    app.insert_resource(DirectionalLightShadowMap { size: 2048 });
    app.insert_resource(PointLightShadowMap { size: 512 });
}

pub(super) fn sun_cascades() -> CascadeShadowConfig {
    CascadeShadowConfigBuilder {
        num_cascades: 2,
        minimum_distance: 0.1,
        maximum_distance: 64.0,
        first_cascade_far_bound: 16.0,
        overlap_proportion: 0.2,
    }
    .build()
}

/// Directional maps use a separate texture array. Reserve existing point shadow cubes
/// before handing remaining layers to the candle selector; never change other lights.
pub(super) fn remaining_point_layers(device_layers: u32, other_shadowed_points: usize) -> u32 {
    let occupied = u32::try_from(other_shadowed_points)
        .unwrap_or(u32::MAX)
        .saturating_mul(6);
    device_layers.saturating_sub(occupied)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn other_point_shadows_are_reserved_without_overflow_or_underflow() {
        assert_eq!(remaining_point_layers(48, 2), 36);
        assert_eq!(remaining_point_layers(5, 1), 0);
        assert_eq!(remaining_point_layers(2048, usize::MAX), 0);
    }
}
