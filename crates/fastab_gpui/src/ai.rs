//! Status presentation only; Jev recommendations use the normal local rows.

use std::time::Duration;

use gpui::prelude::*;
use gpui::{Animation, AnimationExt as _, AnyElement, div, px, rgb};

use crate::list::OverlayTheme;

#[derive(Clone, Debug)]
pub enum AiPreview {
    /// The request revision gives each new request a fresh animation clock.
    Loading(u64),
    Status(String),
}

impl AiPreview {
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading(_))
    }

    pub fn height(&self, row_height: f32) -> f32 {
        row_height
    }
}

/// Two gentle breathing cycles cover the request timeout without leaving a
/// perpetual redraw loop if completion delivery ever stalls.
pub(crate) fn loading_badge(preview: &AiPreview, row_height: f32) -> AnyElement {
    let AiPreview::Loading(revision) = preview else {
        return div().into_any_element();
    };
    div()
        .id("jev-loading")
        .flex()
        .items_center()
        .flex_shrink_0()
        .mr(px(6.))
        .child(crate::icons::ai_icon_element(row_height * 0.65).with_animation(
            ("jev-loading-pulse", *revision),
            Animation::new(Duration::from_millis(3200)),
            |icon, progress| {
                let brightness = (1.0 + (progress * std::f32::consts::TAU * 2.0).cos()) * 0.5;
                icon.opacity(0.55 + brightness * 0.45)
            },
        ))
        .into_any_element()
}

pub(crate) fn preview(preview: &AiPreview, theme: OverlayTheme, row_height: f32) -> AnyElement {
    let AiPreview::Status(message) = preview else {
        return div()
            .id("jev-loading-footer")
            .flex()
            .items_center()
            .justify_start()
            .w_full()
            .h(px(preview.height(row_height)))
            .flex_shrink_0()
            .overflow_hidden()
            .px(px(5.))
            .border_t_1()
            .border_color(rgb(theme.border))
            .child(loading_badge(preview, row_height))
            .into_any_element();
    };
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
        .child(crate::icons::ai_icon_element(row_height * 0.75))
        .child(div().min_w(px(0.)).truncate().child(message.clone()))
        .into_any_element()
}
