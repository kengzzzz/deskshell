//! How long Hyprland animates a closed layer out, from its animation tree.

use std::time::Duration;

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::{Client, Error};

/// Hyprland 0.57 `LayerFadeout.cpp` fades a closed layer out with this leaf. Its `layersOut`
/// move happens under the same fade, so it never shows longer.
const CLOSE: &str = "fadeLayersOut";

/// The part of `AnimationTree.cpp` above [`CLOSE`]. `global` is the root.
const PARENTS: [(&str, &str); 3] =
    [("fadeLayersOut", "fadeLayers"), ("fadeLayers", "fade"), ("fade", "global")];

/// hyprutils runs a curve with this prefix as a spring, which ignores `speed`.
const SPRING: &str = "spring:";

/// A cap, so a broken speed cannot stall a caller for minutes.
const LONGEST: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Node {
    name: String,
    overridden: bool,
    enabled: bool,
    /// In deciseconds.
    speed: f64,
    bezier: String,
}

#[derive(Debug, Deserialize)]
struct Toggle {
    #[serde(rename = "bool")]
    on: bool,
}

/// From a `j/animations` reply. `None` when the fade's length cannot be told: a spring curve
/// or a tree without the leaf.
fn layer_close(animations: &str) -> Result<Option<Duration>, Error> {
    let (nodes, _beziers): (Vec<Node>, IgnoredAny) = serde_json::from_str(animations)?;
    Ok(
        resolve(&nodes, CLOSE).and_then(|node| match node {
            Node { enabled: false, .. } => Some(Duration::ZERO),
            Node { bezier, .. } if bezier.starts_with(SPRING) => None,
            Node { speed, .. } => Some(duration(*speed)),
        }),
    )
}

/// A node that is not overridden takes its config from the nearest overridden ancestor.
fn resolve<'a>(nodes: &'a [Node], leaf: &str) -> Option<&'a Node> {
    let mut name = leaf;
    loop {
        let node = nodes
            .iter()
            .find(|node| node.name == name)?;
        let parent = PARENTS
            .iter()
            .find(|(child, _)| *child == name)
            .map(|(_, parent)| *parent);
        match parent {
            Some(parent) if !node.overridden => name = parent,
            _ => return Some(node),
        }
    }
}

fn duration(deciseconds: f64) -> Duration {
    Duration::try_from_secs_f64(deciseconds / 10.0)
        .unwrap_or_default()
        .min(LONGEST)
}

impl Client {
    /// How long a closed layer stays on screen, zero when animations are off. `None` when
    /// the length cannot be told. A missing `animations:enabled` counts as on.
    pub async fn layer_close_time(&self) -> Result<Option<Duration>, Error> {
        let batch = self
            .batch(&["j/getoption animations:enabled", "j/animations"])
            .await?;
        let mut parts = batch.parts();
        let enabled = serde_json::from_str::<Toggle>(parts.next().unwrap_or_default())
            .map_or(true, |toggle| toggle.on);
        if !enabled {
            return Ok(Some(Duration::ZERO));
        }
        layer_close(parts.next().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(nodes: &[(&str, bool, bool, f64, &str)]) -> String {
        let nodes: Vec<_> = nodes
            .iter()
            .map(|(name, overridden, enabled, speed, bezier)| {
                serde_json::json!({
                    "name": name, "overridden": overridden, "bezier": bezier,
                    "enabled": enabled, "speed": speed, "style": ""
                })
            })
            .collect();
        serde_json::json!([nodes, []]).to_string()
    }

    #[test]
    fn the_fade_inherits_from_the_nearest_overridden_ancestor() {
        let json = tree(&[
            ("global", true, true, 8.0, "default"),
            ("fade", true, true, 3.0, "easeOutBack"),
            ("fadeLayers", false, true, 0.0, ""),
            ("fadeLayersOut", false, true, 0.0, ""),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Some(Duration::from_millis(300))
        );
    }

    #[test]
    fn an_overridden_leaf_wins() {
        let json = tree(&[
            ("global", true, true, 8.0, "default"),
            ("fade", true, true, 3.0, "default"),
            ("fadeLayers", true, true, 4.0, "default"),
            ("fadeLayersOut", true, true, 6.0, "default"),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Some(Duration::from_millis(600))
        );
    }

    #[test]
    fn the_root_counts_even_when_not_overridden() {
        let json = tree(&[
            ("global", false, true, 8.0, "default"),
            ("fade", false, true, 0.0, ""),
            ("fadeLayers", false, true, 0.0, ""),
            ("fadeLayersOut", false, true, 0.0, ""),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Some(Duration::from_millis(800))
        );
    }

    #[test]
    fn a_turned_off_fade_takes_no_time() {
        let json = tree(&[
            ("global", true, true, 8.0, "default"),
            ("fade", true, false, 5.0, "spring:soft"),
            ("fadeLayers", false, true, 0.0, ""),
            ("fadeLayersOut", false, true, 0.0, ""),
        ]);
        assert_eq!(layer_close(&json).unwrap(), Some(Duration::ZERO));
    }

    #[test]
    fn a_spring_has_no_known_length() {
        let json = tree(&[
            ("global", true, true, 8.0, "default"),
            ("fade", true, true, 5.0, "spring:soft"),
            ("fadeLayers", false, true, 0.0, ""),
            ("fadeLayersOut", false, true, 0.0, ""),
        ]);
        assert_eq!(layer_close(&json).unwrap(), None);
    }

    #[test]
    fn a_broken_speed_is_capped_or_zero() {
        let json = tree(&[("fadeLayersOut", true, true, 1e9, "default")]);
        assert_eq!(layer_close(&json).unwrap(), Some(LONGEST));
        let json = tree(&[("fadeLayersOut", true, true, -4.0, "default")]);
        assert_eq!(layer_close(&json).unwrap(), Some(Duration::ZERO));
    }

    #[test]
    fn a_missing_leaf_has_no_known_length() {
        assert_eq!(layer_close(&tree(&[])).unwrap(), None);
        let json = tree(&[("fadeLayersOut", false, true, 0.0, "")]);
        assert_eq!(layer_close(&json).unwrap(), None);
    }

    #[test]
    fn a_reply_of_another_shape_is_an_error() {
        assert!(layer_close("no such option").is_err());
        assert!(layer_close(r#"[[{"name": "fade"}], []]"#).is_err());
    }
}
