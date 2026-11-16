//! An application icon in a square box.

use iced::widget::{container, image};
use iced::{ContentFit, Element};

use crate::{Theme, icon};

pub const FALLBACK_SYMBOL: &str = "apps";

pub fn app_icon<'a, Message: 'a>(
    theme: &Theme,
    icon_image: Option<&image::Handle>,
    fallback: Option<&image::Handle>,
    size: f32,
) -> Element<'a, Message> {
    match icon_image.or(fallback) {
        Some(handle) => image(handle.clone())
            .width(size)
            .height(size)
            .content_fit(ContentFit::Contain)
            .into(),
        None => container(
            icon(FALLBACK_SYMBOL)
                .size((size * 0.75).round())
                .color(theme.colors.text_secondary),
        )
        .center(size)
        .into(),
    }
}
