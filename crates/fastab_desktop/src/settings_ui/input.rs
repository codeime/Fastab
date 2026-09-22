//! Bounded single-line settings input. Password text is never shaped, copied,
//! cut, or returned through the platform's text-query interface.
use std::ops::Range;

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle, Focusable, KeyDownEvent,
    MouseButton, Pixels, Point, ShapedLine, SharedString, TextRun, UTF16Selection, Window, canvas, div, fill, point,
    px, rgb, size,
};

use super::theme::Chrome;

pub(super) struct Input {
    value: String,
    password: bool,
    limit: usize,
    focus: FocusHandle,
    anchor: usize,
    cursor: usize,
    marked: Option<Range<usize>>,
    layout: Option<ShapedLine>,
    origin: Point<Pixels>,
    scroll: Pixels,
    dragging: bool,
    edit_revision: u64,
    pub(super) enabled: bool,
}

impl Input {
    pub(super) fn new(value: String, password: bool, limit: usize, cx: &mut Context<'_, Self>) -> Self {
        Self {
            value: Self::bounded(value, limit),
            password,
            limit,
            focus: cx.focus_handle(),
            anchor: 0,
            cursor: 0,
            marked: None,
            layout: None,
            origin: point(px(0.), px(0.)),
            scroll: px(0.),
            dragging: false,
            edit_revision: 0,
            enabled: true,
        }
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }

    pub(super) fn edit_revision(&self) -> u64 {
        self.edit_revision
    }

    pub(super) fn set(&mut self, value: String, cx: &mut Context<'_, Self>) {
        self.value = Self::bounded(value, self.limit);
        self.cursor = self.value.len();
        self.anchor = self.cursor;
        self.marked = None;
        self.layout = None;
        self.scroll = px(0.);
        cx.notify();
    }

    pub(super) fn take(&mut self, cx: &mut Context<'_, Self>) -> String {
        let value = std::mem::take(&mut self.value);
        self.set(String::new(), cx);
        value
    }

    fn bounded(mut value: String, limit: usize) -> String {
        value.retain(|ch| !ch.is_control());
        let mut end = value.len().min(limit);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
        value
    }

    fn selection(&self) -> Range<usize> {
        self.anchor.min(self.cursor)..self.anchor.max(self.cursor)
    }

    fn utf8(text: &str, utf16: usize) -> usize {
        let mut offset = 0;
        for (byte, ch) in text.char_indices() {
            if offset >= utf16 {
                return byte;
            }
            offset += ch.len_utf16();
        }
        text.len()
    }

    fn utf16(&self, byte: usize) -> usize {
        self.value[..byte].encode_utf16().count()
    }

    fn range(&self, range: Range<usize>) -> Range<usize> {
        let start = Self::utf8(&self.value, range.start);
        start..Self::utf8(&self.value, range.end).max(start)
    }

    fn display_index(&self, byte: usize) -> usize {
        if self.password {
            self.value[..byte].chars().count()
        } else {
            byte
        }
    }

    fn actual_index(&self, display: usize) -> usize {
        if self.password {
            self.value
                .char_indices()
                .nth(display)
                .map_or(self.value.len(), |(byte, _)| byte)
        } else {
            let mut byte = display.min(self.value.len());
            while !self.value.is_char_boundary(byte) {
                byte -= 1;
            }
            byte
        }
    }

    fn mouse_index(&self, position: Point<Pixels>) -> usize {
        self.layout.as_ref().map_or(0, |line| {
            self.actual_index(line.closest_index_for_x(position.x - self.origin.x))
        })
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<'_, Self>) -> bool {
        if !self.enabled
            || text.chars().any(char::is_control)
            || self.value.len() - range.len() + text.len() > self.limit
        {
            return false;
        }
        self.value.replace_range(range.clone(), text);
        self.edit_revision = self.edit_revision.wrapping_add(1);
        self.cursor = range.start + text.len();
        self.anchor = self.cursor;
        self.marked = None;
        cx.notify();
        true
    }

    fn key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.enabled {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let mut handled = true;
        if modifiers.platform {
            match key {
                "a" => {
                    self.anchor = 0;
                    self.cursor = self.value.len();
                },
                "c" | "x" => {
                    if !self.password && !self.selection().is_empty() {
                        cx.write_to_clipboard(ClipboardItem::new_string(self.value[self.selection()].to_owned()));
                        if key == "x" {
                            self.replace(self.selection(), "", cx);
                        }
                    }
                },
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.replace(self.selection(), &text, cx);
                    }
                },
                "left" => {
                    self.cursor = 0;
                    if !modifiers.shift {
                        self.anchor = 0;
                    }
                },
                "right" => {
                    self.cursor = self.value.len();
                    if !modifiers.shift {
                        self.anchor = self.cursor;
                    }
                },
                _ => handled = false,
            }
        } else if !modifiers.control && !modifiers.alt {
            match key {
                "backspace" | "delete" => {
                    let mut range = self.selection();
                    if range.is_empty() {
                        if key == "backspace" {
                            range.start = self.value[..self.cursor]
                                .char_indices()
                                .next_back()
                                .map_or(0, |(byte, _)| byte);
                        } else {
                            range.end = self.value[self.cursor..]
                                .chars()
                                .next()
                                .map_or(self.cursor, |ch| self.cursor + ch.len_utf8());
                        }
                    }
                    self.replace(range, "", cx);
                },
                "left" | "right" | "home" | "end" => {
                    self.cursor = match key {
                        "home" => 0,
                        "end" => self.value.len(),
                        "left" if !modifiers.shift && !self.selection().is_empty() => self.selection().start,
                        "right" if !modifiers.shift && !self.selection().is_empty() => self.selection().end,
                        "left" => self.value[..self.cursor]
                            .char_indices()
                            .next_back()
                            .map_or(0, |(byte, _)| byte),
                        _ => self.value[self.cursor..]
                            .chars()
                            .next()
                            .map_or(self.cursor, |ch| self.cursor + ch.len_utf8()),
                    };
                    if !modifiers.shift {
                        self.anchor = self.cursor;
                    }
                    self.marked = None;
                },
                "enter" => {},
                _ => handled = false,
            }
        } else {
            handled = false;
        }
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Focusable for Input {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for Input {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<String> {
        if self.password {
            *adjusted = None;
            return None;
        }
        let range = self.range(range);
        *adjusted = Some(self.utf16(range.start)..self.utf16(range.end));
        Some(self.value[range].to_owned())
    }
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<'_, Self>) -> Option<UTF16Selection> {
        if !self.enabled {
            return None;
        }
        let range = self.selection();
        Some(UTF16Selection {
            range: self.utf16(range.start)..self.utf16(range.end),
            reversed: self.cursor < self.anchor,
        })
    }
    fn marked_text_range(&self, _: &mut Window, _: &mut Context<'_, Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|range| self.utf16(range.start)..self.utf16(range.end))
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) {
        self.marked = None;
        cx.notify();
    }
    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let range = range
            .map(|range| self.range(range))
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        self.replace(range, text, cx);
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let range = range
            .map(|range| self.range(range))
            .or(self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let start = range.start;
        if self.replace(range, text, cx) {
            self.marked = (!text.is_empty()).then_some(start..start + text.len());
            if let Some(selected) = selected {
                self.anchor = start + Self::utf8(text, selected.start);
                self.cursor = start + Self::utf8(text, selected.end);
            }
        }
    }
    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.layout.as_ref()?;
        let range = self.range(range);
        Some(Bounds::from_corners(
            point(
                self.origin.x + line.x_for_index(self.display_index(range.start)),
                bounds.top(),
            ),
            point(
                self.origin.x + line.x_for_index(self.display_index(range.end)),
                bounds.bottom(),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<'_, Self>,
    ) -> Option<usize> {
        self.layout.as_ref()?;
        Some(self.utf16(self.mouse_index(position)))
    }
}

impl Render for Input {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let chrome = Chrome::current();
        let entity = cx.entity();
        div()
            .id("jev-input")
            .w_full()
            .min_w(px(0.))
            .h(px(34.))
            .px(px(8.))
            .py(px(6.))
            .border_1()
            .border_color(rgb(chrome.separator))
            .rounded_md()
            .bg(rgb(chrome.bg))
            .text_size(px(13.))
            .text_color(rgb(chrome.text))
            .overflow_hidden()
            .track_focus(&self.focus)
            .tab_stop(true)
            .cursor(gpui::CursorStyle::IBeam)
            .focus(|style| style.border_color(rgb(chrome.accent)))
            .when(!self.enabled, |style| style.opacity(0.55))
            .on_key_down(cx.listener(Self::key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    if !this.enabled {
                        return;
                    }
                    this.focus.focus(window);
                    this.cursor = this.mouse_index(event.position);
                    if !event.modifiers.shift {
                        this.anchor = this.cursor;
                    }
                    this.dragging = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.dragging {
                    this.cursor = this.mouse_index(event.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.dragging = false))
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        entity.update(cx, |input, cx| {
                            let style = window.text_style();
                            let display: SharedString = if input.password {
                                "*".repeat(input.value.chars().count()).into()
                            } else {
                                input.value.clone().into()
                            };
                            let run = TextRun {
                                len: display.len(),
                                font: style.font(),
                                color: style.color,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let line = window.text_system().shape_line(
                                display,
                                style.font_size.to_pixels(window.rem_size()),
                                &[run],
                                None,
                            );
                            let cursor = line.x_for_index(input.display_index(input.cursor));
                            let width = bounds.size.width;
                            input.scroll = input.scroll.min(cursor).max((cursor - width + px(2.)).max(px(0.)));
                            let origin = point(bounds.left() - input.scroll, bounds.top());
                            let selection = input.selection();
                            if input.focus.is_focused(window) {
                                if !selection.is_empty() {
                                    window.paint_quad(fill(
                                        Bounds::from_corners(
                                            point(
                                                origin.x + line.x_for_index(input.display_index(selection.start)),
                                                bounds.top(),
                                            ),
                                            point(
                                                origin.x + line.x_for_index(input.display_index(selection.end)),
                                                bounds.bottom(),
                                            ),
                                        ),
                                        rgb(chrome.selection),
                                    ));
                                } else {
                                    window.paint_quad(fill(
                                        Bounds::new(
                                            point(origin.x + cursor, bounds.top()),
                                            size(px(1.), bounds.size.height),
                                        ),
                                        rgb(chrome.accent),
                                    ));
                                }
                            }
                            let _ = line.paint(origin, bounds.size.height, window, cx);
                            window.handle_input(&input.focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                            input.origin = origin;
                            input.layout = Some(line);
                        });
                    },
                )
                .w_full()
                .h_full(),
            )
    }
}
