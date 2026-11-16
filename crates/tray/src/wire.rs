//! Typed readers for what items and menus send. A mistyped value only loses that value.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use zbus::{ObjectPath, Signature, Type};

use crate::pixmap::RawPixmap;

const PROPS: &Signature = &Signature::static_dict(&Signature::Str, &Signature::Variant);

pub(crate) struct Typed<T>(pub Option<T>);

impl<T> Type for Typed<T> {
    const SIGNATURE: &'static Signature = &Signature::Variant;
}

impl<'de, T: Deserialize<'de> + Type> Deserialize<'de> for Typed<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(TypedVisitor(PhantomData))
    }
}

struct TypedVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de> + Type> Visitor<'de> for TypedVisitor<T> {
    type Value = Typed<T>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a variant")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Typed<T>, A::Error> {
        let signature: Signature = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("a variant without a signature"))?;
        if signature == *T::SIGNATURE {
            Ok(Typed(seq.next_element::<T>()?))
        } else {
            seq.next_element::<IgnoredAny>()?;
            Ok(Typed(None))
        }
    }
}

/// Any integer or a boolean, as Qt's `canConvert<qint32>` accepts them.
pub(crate) struct LooseInt(pub Option<i64>);

impl Type for LooseInt {
    const SIGNATURE: &'static Signature = &Signature::Variant;
}

impl<'de> Deserialize<'de> for LooseInt {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(LooseIntVisitor)
    }
}

struct LooseIntVisitor;

impl<'de> Visitor<'de> for LooseIntVisitor {
    type Value = LooseInt;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a variant")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<LooseInt, A::Error> {
        let signature: Signature = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("a variant without a signature"))?;
        let value = match signature {
            Signature::U8 => seq.next_element::<u8>()?.map(i64::from),
            Signature::Bool => seq
                .next_element::<bool>()?
                .map(i64::from),
            Signature::I16 => seq
                .next_element::<i16>()?
                .map(i64::from),
            Signature::U16 => seq
                .next_element::<u16>()?
                .map(i64::from),
            Signature::I32 => seq
                .next_element::<i32>()?
                .map(i64::from),
            Signature::U32 => seq
                .next_element::<u32>()?
                .map(i64::from),
            Signature::I64 => seq.next_element::<i64>()?,
            Signature::U64 => seq
                .next_element::<u64>()?
                .map(|n| i64::try_from(n).unwrap_or(i64::MAX)),
            _ => {
                seq.next_element::<IgnoredAny>()?;
                None
            }
        };
        Ok(LooseInt(value))
    }
}

pub(crate) struct Skip<T>(PhantomData<T>);

impl<T: Type> Type for Skip<T> {
    const SIGNATURE: &'static Signature = T::SIGNATURE;
}

impl<'de, T> Deserialize<'de> for Skip<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer)?;
        Ok(Self(PhantomData))
    }
}

pub(crate) type ChangedInterface<'a> = (
    &'a str,
    Skip<std::collections::HashMap<&'a str, zbus::Variant<'a>>>,
    Skip<Vec<&'a str>>,
);

pub(crate) type Tooltip<'a> = (&'a str, Vec<RawPixmap<'a>>, &'a str, &'a str);

#[derive(Default)]
pub(crate) struct ItemProps<'a> {
    pub id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub status: Option<&'a str>,
    pub category: Option<&'a str>,
    pub icon_theme_path: Option<&'a str>,
    /// `IconThemePath` was in the set, whatever its type.
    pub icon_theme_path_seen: bool,
    pub icon_name: Option<&'a str>,
    pub icon_pixmap: Option<Vec<RawPixmap<'a>>>,
    pub overlay_icon_name: Option<&'a str>,
    pub overlay_icon_pixmap: Option<Vec<RawPixmap<'a>>>,
    pub attention_icon_name: Option<&'a str>,
    pub attention_icon_pixmap: Option<Vec<RawPixmap<'a>>>,
    pub tool_tip: Option<Tooltip<'a>>,
    pub menu: Option<ObjectPath<'a>>,
    pub item_is_menu: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prop {
    Id,
    Title,
    Status,
    Category,
    IconThemePath,
    IconName,
    IconPixmap,
    OverlayIconName,
    OverlayIconPixmap,
    AttentionIconName,
    AttentionIconPixmap,
    ToolTip,
    Menu,
    ItemIsMenu,
}

impl Prop {
    const ALL: [Self; 14] = [
        Self::Id,
        Self::Title,
        Self::Status,
        Self::Category,
        Self::IconThemePath,
        Self::IconName,
        Self::IconPixmap,
        Self::OverlayIconName,
        Self::OverlayIconPixmap,
        Self::AttentionIconName,
        Self::AttentionIconPixmap,
        Self::ToolTip,
        Self::Menu,
        Self::ItemIsMenu,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Id => "Id",
            Self::Title => "Title",
            Self::Status => "Status",
            Self::Category => "Category",
            Self::IconThemePath => "IconThemePath",
            Self::IconName => "IconName",
            Self::IconPixmap => "IconPixmap",
            Self::OverlayIconName => "OverlayIconName",
            Self::OverlayIconPixmap => "OverlayIconPixmap",
            Self::AttentionIconName => "AttentionIconName",
            Self::AttentionIconPixmap => "AttentionIconPixmap",
            Self::ToolTip => "ToolTip",
            Self::Menu => "Menu",
            Self::ItemIsMenu => "ItemIsMenu",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|prop| prop.name() == name)
    }
}

impl<'a> ItemProps<'a> {
    fn read<E>(&mut self, prop: Prop, next: impl Reader<'a, E>) -> Result<(), E> {
        match prop {
            Prop::Id => self.id = next.read()?,
            Prop::Title => self.title = next.read()?,
            Prop::Status => self.status = next.read()?,
            Prop::Category => self.category = next.read()?,
            Prop::IconThemePath => {
                self.icon_theme_path_seen = true;
                self.icon_theme_path = next.read()?;
            }
            Prop::IconName => self.icon_name = next.read()?,
            Prop::IconPixmap => self.icon_pixmap = next.read()?,
            Prop::OverlayIconName => self.overlay_icon_name = next.read()?,
            Prop::OverlayIconPixmap => self.overlay_icon_pixmap = next.read()?,
            Prop::AttentionIconName => self.attention_icon_name = next.read()?,
            Prop::AttentionIconPixmap => self.attention_icon_pixmap = next.read()?,
            Prop::ToolTip => self.tool_tip = next.read()?,
            Prop::Menu => self.menu = next.read()?,
            Prop::ItemIsMenu => self.item_is_menu = next.read()?,
        }
        Ok(())
    }

    pub(crate) fn from_get(prop: Prop, body: &'a zbus::message::Body) -> zbus::Result<Self> {
        let mut props = Self::default();
        props.read(prop, BodyReader(body))?;
        Ok(props)
    }
}

trait Reader<'a, E> {
    fn read<T: Deserialize<'a> + Type>(self) -> Result<Option<T>, E>;
}

struct BodyReader<'b>(&'b zbus::message::Body);

impl<'a> Reader<'a, zbus::Error> for BodyReader<'a> {
    fn read<T: Deserialize<'a> + Type>(self) -> zbus::Result<Option<T>> {
        Ok(self.0.deserialize::<Typed<T>>()?.0)
    }
}

struct MapReader<'m, M>(&'m mut M);

impl<'a, M: MapAccess<'a>> Reader<'a, M::Error> for MapReader<'_, M> {
    fn read<T: Deserialize<'a> + Type>(self) -> Result<Option<T>, M::Error> {
        Ok(self.0.next_value::<Typed<T>>()?.0)
    }
}

impl Type for ItemProps<'_> {
    const SIGNATURE: &'static Signature = PROPS;
}

impl<'de> Deserialize<'de> for ItemProps<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(ItemPropsVisitor)
    }
}

struct ItemPropsVisitor;

impl<'de> Visitor<'de> for ItemPropsVisitor {
    type Value = ItemProps<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a property map")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<ItemProps<'de>, M::Error> {
        let mut props = ItemProps::default();
        while let Some(key) = map.next_key::<&str>()? {
            match Prop::from_name(key) {
                Some(prop) => props.read(prop, MapReader(&mut map))?,
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(props)
    }
}

#[derive(Default)]
pub(crate) struct MenuProps<'a> {
    /// How many properties the map held, known or not. An empty map means no change.
    pub count: usize,
    pub kind: Option<&'a str>,
    pub label: Option<&'a str>,
    pub enabled: Option<bool>,
    pub visible: Option<bool>,
    pub icon_name: Option<&'a str>,
    pub icon_data: Option<&'a [u8]>,
    pub toggle_type: Option<&'a str>,
    pub toggle_state: Option<i64>,
    pub children_display: Option<&'a str>,
    pub shortcut: Option<Vec<Vec<&'a str>>>,
}

impl Type for MenuProps<'_> {
    const SIGNATURE: &'static Signature = PROPS;
}

impl<'de> Deserialize<'de> for MenuProps<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(MenuPropsVisitor)
    }
}

struct MenuPropsVisitor;

impl<'de> Visitor<'de> for MenuPropsVisitor {
    type Value = MenuProps<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a property map")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<MenuProps<'de>, M::Error> {
        let mut props = MenuProps::default();
        while let Some(key) = map.next_key::<&str>()? {
            props.count += 1;
            match key {
                "type" => props.kind = map.next_value::<Typed<_>>()?.0,
                "label" => props.label = map.next_value::<Typed<_>>()?.0,
                "enabled" => props.enabled = map.next_value::<Typed<_>>()?.0,
                "visible" => props.visible = map.next_value::<Typed<_>>()?.0,
                "icon-name" => props.icon_name = map.next_value::<Typed<_>>()?.0,
                "icon-data" => props.icon_data = map.next_value::<Typed<_>>()?.0,
                "toggle-type" => props.toggle_type = map.next_value::<Typed<_>>()?.0,
                "toggle-state" => props.toggle_state = map.next_value::<LooseInt>()?.0,
                "children-display" => props.children_display = map.next_value::<Typed<_>>()?.0,
                "shortcut" => props.shortcut = map.next_value::<Typed<_>>()?.0,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(props)
    }
}

// Children that are not themselves a layout node are dropped.
pub(crate) struct Layout<'a> {
    pub id: i32,
    pub props: MenuProps<'a>,
    pub children: Vec<Layout<'a>>,
}

static VARIANTS: Signature = Signature::static_array(&Signature::Variant);
static LAYOUT: Signature = Signature::static_structure(&[&Signature::I32, PROPS, &VARIANTS]);

impl Type for Layout<'_> {
    const SIGNATURE: &'static Signature = &LAYOUT;
}

impl<'de> Deserialize<'de> for Layout<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_tuple(3, LayoutVisitor)
    }
}

struct LayoutVisitor;

impl<'de> Visitor<'de> for LayoutVisitor {
    type Value = Layout<'de>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a menu layout")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Layout<'de>, A::Error> {
        let missing = || de::Error::custom("a short menu layout");
        let id = seq
            .next_element::<i32>()?
            .ok_or_else(missing)?;
        let props = seq
            .next_element::<MenuProps<'de>>()?
            .ok_or_else(missing)?;
        let children = seq
            .next_element::<Vec<Typed<Layout<'de>>>>()?
            .ok_or_else(missing)?
            .into_iter()
            .filter_map(|child| child.0)
            .collect();
        Ok(Layout { id, props, children })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::Variant;
    use zbus::wire::serialized::Context;
    use zbus::wire::{LE, to_bytes};

    use super::*;

    fn encode<T: serde::Serialize + zbus::DynamicType>(
        value: &T,
    ) -> zbus::wire::serialized::Data<'static, 'static> {
        to_bytes(Context::new(LE, 0), value).expect("encode")
    }

    #[test]
    fn item_props_keep_each_well_typed_value() {
        let pixmap = vec![(2i32, 1i32, vec![0xffu8, 1, 2, 3, 0, 4, 5, 6])];
        let mut map: HashMap<&str, Variant<'_>> = HashMap::new();
        map.insert("Id", Variant::from("app"));
        map.insert("Title", Variant::from(7u32));
        map.insert("IconThemePath", Variant::from(true));
        map.insert("IconPixmap", Variant::new(pixmap));
        map.insert(
            "Menu",
            Variant::from(ObjectPath::try_from("/Menu").unwrap()),
        );
        map.insert("ItemIsMenu", Variant::from(true));
        map.insert("Unknown", Variant::from(1.5f64));
        let data = encode(&map);
        let (props, _): (ItemProps<'_>, _) = data.deserialize().expect("decode");
        assert_eq!(props.id, Some("app"));
        assert_eq!(props.title, None, "a u32 title is dropped");
        assert!(props.icon_theme_path_seen);
        assert_eq!(props.icon_theme_path, None);
        let pixmaps = props.icon_pixmap.unwrap();
        assert_eq!(pixmaps.len(), 1);
        assert_eq!(pixmaps[0].0, 2);
        assert_eq!(pixmaps[0].2, &[0xff, 1, 2, 3, 0, 4, 5, 6]);
        assert_eq!(
            props.menu.as_ref().map(|p| p.as_str()),
            Some("/Menu")
        );
        assert_eq!(props.item_is_menu, Some(true));
    }

    #[test]
    fn loose_int_takes_integers_and_bools() {
        for (value, expected) in [
            (Variant::from(1i32), Some(1)),
            (Variant::from(0u32), Some(0)),
            (Variant::from(-1i64), Some(-1)),
            (Variant::from(true), Some(1)),
            (Variant::from(u64::MAX), Some(i64::MAX)),
            (Variant::from("on"), None),
        ] {
            let mut map: HashMap<&str, Variant<'_>> = HashMap::new();
            map.insert("toggle-state", value);
            let data = encode(&map);
            let (props, _): (MenuProps<'_>, _) = data.deserialize().expect("decode");
            assert_eq!(props.toggle_state, expected);
            assert_eq!(props.count, 1);
        }
    }
}
