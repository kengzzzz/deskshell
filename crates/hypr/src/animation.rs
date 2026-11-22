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

/// A cap, so a broken speed cannot stall a caller for minutes.
const LONGEST: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Node {
    name: String,
    overridden: bool,
    enabled: bool,
    /// In deciseconds.
    speed: f64,
}

#[derive(Debug, Deserialize)]
struct Toggle {
    #[serde(rename = "bool")]
    on: bool,
}

/// From a `j/animations` reply. An animation that is turned off takes no time.
fn layer_close(animations: &str) -> Result<Duration, Error> {
    let (nodes, _beziers): (Vec<Node>, IgnoredAny) = serde_json::from_str(animations)?;
    Ok(resolve(&nodes, CLOSE)
        .filter(|node| node.enabled)
        .map_or(Duration::ZERO, |node| duration(node.speed)))
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
    /// How long a closed layer stays on screen. Zero when animations are off.
    pub async fn layer_close_time(&self) -> Result<Duration, Error> {
        let batch = self
            .batch(&["j/getoption animations:enabled", "j/animations"])
            .await?;
        let mut parts = batch.parts();
        let enabled: Toggle = serde_json::from_str(parts.next().unwrap_or_default())?;
        if !enabled.on {
            return Ok(Duration::ZERO);
        }
        layer_close(parts.next().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(nodes: &[(&str, bool, bool, f64)]) -> String {
        let nodes: Vec<_> = nodes
            .iter()
            .map(|(name, overridden, enabled, speed)| {
                serde_json::json!({
                    "name": name, "overridden": overridden, "bezier": "",
                    "enabled": enabled, "speed": speed, "style": ""
                })
            })
            .collect();
        serde_json::json!([nodes, []]).to_string()
    }

    #[test]
    fn the_fade_inherits_from_the_nearest_overridden_ancestor() {
        let json = tree(&[
            ("global", true, true, 8.0),
            ("fade", true, true, 3.0),
            ("fadeLayers", false, true, 0.0),
            ("fadeLayersOut", false, true, 0.0),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Duration::from_millis(300)
        );
    }

    #[test]
    fn an_overridden_leaf_wins() {
        let json = tree(&[
            ("global", true, true, 8.0),
            ("fade", true, true, 3.0),
            ("fadeLayers", true, true, 4.0),
            ("fadeLayersOut", true, true, 6.0),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Duration::from_millis(600)
        );
    }

    #[test]
    fn the_root_counts_even_when_not_overridden() {
        let json = tree(&[
            ("global", false, true, 8.0),
            ("fade", false, true, 0.0),
            ("fadeLayers", false, true, 0.0),
            ("fadeLayersOut", false, true, 0.0),
        ]);
        assert_eq!(
            layer_close(&json).unwrap(),
            Duration::from_millis(800)
        );
    }

    #[test]
    fn a_turned_off_fade_takes_no_time() {
        let json = tree(&[
            ("global", true, true, 8.0),
            ("fade", true, false, 5.0),
            ("fadeLayers", false, true, 0.0),
            ("fadeLayersOut", false, true, 0.0),
        ]);
        assert_eq!(layer_close(&json).unwrap(), Duration::ZERO);
    }

    #[test]
    fn a_broken_speed_is_capped_or_zero() {
        let mut json = tree(&[
            ("global", true, true, 1e9),
            ("fade", false, true, 0.0),
            ("fadeLayers", false, true, 0.0),
            ("fadeLayersOut", false, true, 0.0),
        ]);
        assert_eq!(layer_close(&json).unwrap(), LONGEST);
        json = tree(&[("fadeLayersOut", true, true, -4.0)]);
        assert_eq!(layer_close(&json).unwrap(), Duration::ZERO);
    }

    #[test]
    fn a_missing_leaf_counts_as_instant() {
        assert_eq!(layer_close(&tree(&[])).unwrap(), Duration::ZERO);
    }
}
