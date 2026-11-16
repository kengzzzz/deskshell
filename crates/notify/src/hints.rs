//! Reads the `Notify` hints.

use std::collections::HashMap;
use std::fmt;

use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use tracing::warn;
use zbus::{Signature, Type, Variant};

use crate::model::{Pixels, Urgency};

/// A hint with an unexpected type is ignored instead of failing `Notify`, and image bytes
/// are read straight from the message instead of one `Variant` per byte.
#[derive(Debug, Default)]
pub(crate) struct Hints {
    pub urgency: Urgency,
    pub transient: bool,
    pub resident: bool,
    pub desktop_entry: String,
    pub category: String,
    pub pixels: Option<Pixels>,
    pub image_path: Option<String>,
    pixels_rank: u8,
    path_rank: u8,
}

const PIXEL_KEYS: [&str; 3] = ["image-data", "image_data", "icon_data"];
const PATH_KEYS: [&str; 2] = ["image-path", "image_path"];

type RawImage<'a> = (i32, i32, i32, bool, i32, i32, &'a [u8]);

fn rank(keys: &[&str], key: &str) -> u8 {
    keys.iter()
        .position(|k| *k == key)
        .map_or(0, |i| keys.len() as u8 - i as u8)
}

impl Hints {
    fn apply(&mut self, key: &str, value: &Variant<'_>) {
        let value = match value {
            Variant::Variant(inner) => inner,
            other => other,
        };
        match key {
            "urgency" => {
                if let Some(level) = integer(value) {
                    self.urgency = Urgency::from_level(level);
                }
            }
            "transient" => self.transient = boolean(value).unwrap_or(self.transient),
            "resident" => self.resident = boolean(value).unwrap_or(self.resident),
            "desktop-entry" => {
                if let Variant::Str(s) = value {
                    self.desktop_entry = s.as_str().to_owned();
                }
            }
            "category" => {
                if let Variant::Str(s) = value {
                    self.category = s.as_str().to_owned();
                }
            }
            _ => {
                let rank = rank(&PATH_KEYS, key);
                if rank > self.path_rank
                    && let Variant::Str(s) = value
                {
                    self.image_path = Some(s.as_str().to_owned());
                    self.path_rank = rank;
                }
            }
        }
    }
}

fn integer(value: &Variant<'_>) -> Option<i64> {
    Some(match *value {
        Variant::U8(n) => n.into(),
        Variant::I16(n) => n.into(),
        Variant::U16(n) => n.into(),
        Variant::I32(n) => n.into(),
        Variant::U32(n) => n.into(),
        Variant::I64(n) => n,
        Variant::U64(n) => i64::try_from(n).unwrap_or(i64::MAX),
        _ => return None,
    })
}

fn boolean(value: &Variant<'_>) -> Option<bool> {
    match value {
        Variant::Bool(b) => Some(*b),
        other => integer(other).map(|n| n != 0),
    }
}

impl Type for Hints {
    const SIGNATURE: &'static Signature = <HashMap<String, Variant<'static>> as Type>::SIGNATURE;
}

impl<'de> Deserialize<'de> for Hints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(HintsVisitor)
    }
}

struct HintsVisitor;

impl<'de> Visitor<'de> for HintsVisitor {
    type Value = Hints;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a{sv}")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Hints, A::Error> {
        let mut hints = Hints::default();
        while let Some(key) = map.next_key::<&str>()? {
            let rank = rank(&PIXEL_KEYS, key);
            if rank == 0 {
                let value: Variant<'de> = map.next_value()?;
                hints.apply(key, &value);
                continue;
            }
            let ImageHint(raw) = map.next_value()?;
            if rank > hints.pixels_rank
                && let Some(image) = raw.and_then(pixels)
            {
                hints.pixels = Some(image);
                hints.pixels_rank = rank;
            }
        }
        Ok(hints)
    }
}

/// Borrowed from the message, so only the winning key's bytes are copied.
struct ImageHint<'de>(Option<RawImage<'de>>);

impl<'de> Deserialize<'de> for ImageHint<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_struct("Variant", &["signature", "value"], ImageVisitor)
    }
}

struct ImageVisitor;

impl<'de> Visitor<'de> for ImageVisitor {
    type Value = ImageHint<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a variant")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<ImageHint<'de>, A::Error> {
        let missing = || serde::de::Error::custom("variant without a value");
        let signature: Signature = seq
            .next_element()?
            .ok_or_else(missing)?;
        if &signature != <RawImage<'de> as Type>::SIGNATURE {
            seq.next_element::<IgnoredAny>()?
                .ok_or_else(missing)?;
            warn!(%signature, "ignoring an image hint with the wrong type");
            return Ok(ImageHint(None));
        }
        let raw: RawImage<'de> = seq
            .next_element()?
            .ok_or_else(missing)?;
        Ok(ImageHint(Some(raw)))
    }
}

fn pixels(
    (width, height, rowstride, has_alpha, bits, channels, data): RawImage<'_>,
) -> Option<Pixels> {
    let expected_channels = if has_alpha { 4 } else { 3 };
    let fits = || -> Option<bool> {
        let width = usize::try_from(width)
            .ok()
            .filter(|&w| w > 0)?;
        let height = usize::try_from(height)
            .ok()
            .filter(|&h| h > 0)?;
        let rowstride = usize::try_from(rowstride).ok()?;
        let row = width.checked_mul(expected_channels)?;
        let needed = rowstride
            .checked_mul(height - 1)?
            .checked_add(row)?;
        Some(rowstride >= row && data.len() >= needed)
    };
    if bits != 8 || channels != expected_channels as i32 || fits() != Some(true) {
        warn!(
            width,
            height,
            rowstride,
            has_alpha,
            bits,
            channels,
            len = data.len(),
            "ignoring image data that does not fit its header"
        );
        return None;
    }
    Some(Pixels {
        width: width as u32,
        height: height as u32,
        rowstride: rowstride as u32,
        has_alpha,
        channels: channels as u32,
        data: data.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::Variant;
    use zbus::wire::serialized::Context;
    use zbus::wire::{LE, to_bytes};

    use super::*;

    fn decode(hints: &HashMap<&str, Variant<'_>>) -> Hints {
        let encoded = to_bytes(Context::new(LE, 0), hints).unwrap();
        encoded
            .deserialize::<Hints>()
            .unwrap()
            .0
    }

    fn image(
        width: i32,
        height: i32,
        rowstride: i32,
        alpha: bool,
        bytes: usize,
    ) -> Variant<'static> {
        let channels = if alpha { 4 } else { 3 };
        Variant::from((
            width,
            height,
            rowstride,
            alpha,
            8,
            channels,
            vec![7u8; bytes],
        ))
    }

    #[test]
    fn keeps_the_known_hints() {
        let hints = HashMap::from([
            ("urgency", Variant::U8(2)),
            ("transient", Variant::Bool(true)),
            ("resident", Variant::Bool(true)),
            ("desktop-entry", Variant::from("org.example.Chat")),
            ("category", Variant::from("im.received")),
            ("x-unknown", Variant::from(vec!["a", "b"])),
        ]);
        let hints = decode(&hints);
        assert_eq!(hints.urgency, Urgency::Critical);
        assert!(hints.transient);
        assert!(hints.resident);
        assert_eq!(hints.desktop_entry, "org.example.Chat");
        assert_eq!(hints.category, "im.received");
        assert!(hints.pixels.is_none());
        assert!(hints.image_path.is_none());
    }

    #[test]
    fn wrong_types_are_ignored_or_coerced() {
        let hints = HashMap::from([
            ("urgency", Variant::I32(0)),
            ("transient", Variant::U32(1)),
            ("desktop-entry", Variant::U8(3)),
            ("image-data", Variant::from("not an image")),
            ("image-path", Variant::U32(5)),
        ]);
        let hints = decode(&hints);
        assert_eq!(hints.urgency, Urgency::Low);
        assert!(hints.transient);
        assert_eq!(hints.desktop_entry, "");
        assert!(hints.pixels.is_none());
        assert!(hints.image_path.is_none());
    }

    #[test]
    fn reads_image_data_with_padded_rows() {
        // 3x2 RGB with rows padded to 12 bytes. The last row needs no padding.
        let hints = HashMap::from([("image-data", image(3, 2, 12, false, 12 + 9))]);
        let pixels = decode(&hints).pixels.expect("pixels");
        assert_eq!(
            (pixels.width, pixels.height, pixels.rowstride),
            (3, 2, 12)
        );
        assert_eq!(pixels.channels, 3);
        assert_eq!(pixels.data.len(), 21);
    }

    #[test]
    fn rejects_image_data_that_does_not_fit() {
        let short = HashMap::from([("image-data", image(4, 4, 16, true, 63))]);
        assert!(decode(&short).pixels.is_none());
        let narrow = HashMap::from([("image-data", image(4, 1, 8, true, 64))]);
        assert!(decode(&narrow).pixels.is_none());
        let bad_channels = HashMap::from([(
            "image-data",
            Variant::from((1, 1, 3, true, 8, 3, vec![0u8; 3])),
        )]);
        assert!(decode(&bad_channels).pixels.is_none());
    }

    #[test]
    fn image_keys_precedence() {
        let hints = HashMap::from([
            ("icon_data", image(1, 1, 4, true, 4)),
            ("image_data", image(2, 1, 8, true, 8)),
            ("image-data", image(3, 1, 12, true, 12)),
            ("image_path", Variant::from("second.png")),
            ("image-path", Variant::from("file:///first.png")),
        ]);
        let hints = decode(&hints);
        assert_eq!(hints.pixels.expect("pixels").width, 3);
        assert_eq!(
            hints.image_path.as_deref(),
            Some("file:///first.png")
        );

        let fallback = HashMap::from([
            ("icon_data", image(1, 1, 4, true, 4)),
            ("image_path", Variant::from("dialog-information")),
        ]);
        let fallback = decode(&fallback);
        assert_eq!(fallback.pixels.expect("pixels").width, 1);
        assert_eq!(
            fallback.image_path.as_deref(),
            Some("dialog-information")
        );
    }
}
