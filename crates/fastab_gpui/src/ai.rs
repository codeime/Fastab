//! Status presentation only; Jev recommendations use the normal local rows.

use gpui::prelude::*;
use gpui::{AnyElement, div, px, rgb};

use crate::list::OverlayTheme;

#[derive(Clone, Debug)]
pub struct AiPreview {
    pub message: String,
}

impl AiPreview {
    pub fn height(&self, row_height: f32) -> f32 {
        row_height
    }
}

pub(crate) fn preview(preview: &AiPreview, theme: OverlayTheme, row_height: f32) -> AnyElement {
    div()
        .id("jev-preview")
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(5.))
        .w_full()
        .h(px(preview.height(row_height)))
        .flex_shrink_0()
        .overflow_hidden()
        .text_color(rgb(theme.text))
        .child(crate::icons::ai_icon_image_element(row_height * 0.75, theme.accent))
        .child(div().min_w(px(0.)).truncate().child(preview.message.clone()))
        .into_any_element()
}
