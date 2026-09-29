use std::ffi::c_void;
use std::time::{Duration, Instant};

use accessibility::util::{ax_call, bool_ax_call};
use accessibility::{AXAttribute, AXUIElement};
use accessibility_sys::{
    _AXUIElementGetWindow, AXUIElementCopyParameterizedAttributeValue, AXUIElementGetPid,
    AXUIElementSetMessagingTimeout, AXValue, AXValueCreate, AXValueGetValue, AXValueRef,
    kAXBoundsForRangeParameterizedAttribute, kAXFocusedWindowAttribute, kAXTextAreaRole, kAXValueTypeCFRange,
    kAXValueTypeCGRect, pid_t,
};
use core_foundation::base::{CFRange, CFType, CFTypeRef, TCFType};
use core_foundation::string::CFString;
use core_graphics::geometry::CGRect;
use core_graphics::window::CGWindowID;
use tracing::debug;

#[derive(Debug)]
pub struct CaretPosition {
    pub valid: bool,
    pub x: f64,
    pub y: f64,
    pub height: f64,
}

const INVALID_CARET_POSITION: CaretPosition = CaretPosition {
    valid: false,
    x: 0.0,
    y: 0.0,
    height: 0.0,
};

#[allow(clippy::missing_safety_doc)]
pub unsafe fn get_caret_position(extend_range: bool) -> CaretPosition {
    let system_wide_element: AXUIElement = AXUIElement::system_wide();

    // Get the focused element
    let focused_element: AXUIElement = match system_wide_element.attribute(&AXAttribute::focused_ui()) {
        Ok(focused_element) => focused_element,
        Err(err) => {
            debug!(%err, "focused UI is not available for caret tracking");

            return INVALID_CARET_POSITION;
        },
    };

    caret_for_element(&focused_element, extend_range, false, None)
}

/// Read a terminal text area's actual insertion point, never a window-relative
/// estimate. The caller must check that `pid` is still frontmost after this call.
#[allow(clippy::missing_safety_doc)]
pub unsafe fn get_terminal_caret_position(pid: pid_t, window_id: CGWindowID) -> CaretPosition {
    let deadline = Instant::now() + Duration::from_millis(250);
    let application = AXUIElement::application(pid);
    macro_rules! query {
        ($element:expr, $work:expr) => {{
            if !prepare_ax_query($element, Some(deadline)) {
                return INVALID_CARET_POSITION;
            }
            match $work {
                Ok(value) => value,
                Err(_) => return INVALID_CARET_POSITION,
            }
        }};
    }
    let focused_window = query!(
        &application,
        application.attribute(&AXAttribute::new(&CFString::new(kAXFocusedWindowAttribute)))
    );
    let Some(focused_window) = focused_window.downcast_into::<AXUIElement>() else {
        return INVALID_CARET_POSITION;
    };
    let focused_element = query!(&application, application.attribute(&AXAttribute::focused_ui()));
    let actual_window_id = query!(
        &focused_window,
        ax_call(|id| _AXUIElementGetWindow(focused_window.as_concrete_TypeRef(), id))
    );
    let element_pid = query!(
        &focused_element,
        ax_call(|id| AXUIElementGetPid(focused_element.as_concrete_TypeRef(), id))
    );
    let element_window = query!(&focused_element, focused_element.attribute(&AXAttribute::window()));
    let role = query!(&focused_element, focused_element.attribute(&AXAttribute::role()));
    if actual_window_id != window_id
        || element_pid != pid
        || element_window != focused_window
        || role != kAXTextAreaRole
    {
        return INVALID_CARET_POSITION;
    }
    // Zero-length AXBoundsForRange addresses the insertion point. Expanding it
    // to the next character can fail at the viewport end or read another line.
    let caret = caret_for_element(&focused_element, false, true, Some(deadline));
    let still_focused = query!(&application, application.attribute(&AXAttribute::focused_ui()));
    if still_focused != focused_element {
        return INVALID_CARET_POSITION;
    }
    caret
}

fn prepare_ax_query(element: &AXUIElement, deadline: Option<Instant>) -> bool {
    let Some(deadline) = deadline else {
        return true;
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    // One budget covers all remote AX reads, not a fresh 250 ms per attribute.
    unsafe {
        AXUIElementSetMessagingTimeout(element.as_concrete_TypeRef(), remaining.as_secs_f32())
            == accessibility_sys::kAXErrorSuccess
    }
}

fn valid_terminal_insertion_range(range: CFRange) -> bool {
    range.location >= 0 && range.length == 0
}

unsafe fn caret_for_element(
    focused_element: &AXUIElement,
    extend_range: bool,
    require_insertion_point: bool,
    deadline: Option<Instant>,
) -> CaretPosition {
    if !prepare_ax_query(focused_element, deadline) {
        return INVALID_CARET_POSITION;
    }

    // Get the selected range value
    let selected_range_value: CFType = match focused_element.attribute(&AXAttribute::selected_range()) {
        Ok(selected_range_value) => selected_range_value,
        Err(err) => {
            debug!(%err, "selected range is not available for caret tracking");

            return INVALID_CARET_POSITION;
        },
    };

    // `ax_call` is necessary for the value ptr to actually change
    let selected_range_result: Result<CFRange, bool> = bool_ax_call(|x: *mut CFRange| {
        AXValueGetValue(
            selected_range_value.as_concrete_TypeRef() as AXValueRef,
            kAXValueTypeCFRange,
            x as *mut _ as *mut c_void,
        )
    });

    let selected_text_range: CFRange = match selected_range_result {
        Ok(selected_text_range) => selected_text_range,
        Err(err) => {
            debug!("Couldn't get selected text range, did types match {:?}", err);
            return INVALID_CARET_POSITION;
        },
    };

    if require_insertion_point && !valid_terminal_insertion_range(selected_text_range) {
        return INVALID_CARET_POSITION;
    }

    // https://linear.app/fig/issue/ENG-109/ - autocomplete-popup-shows-when-copying-and-pasting-in-terminal
    if selected_text_range.length > 1 {
        debug!("selectedRange length > 1");
        return INVALID_CARET_POSITION;
    }

    // Owned so the `AXValueCreate` (+1) is released with the frame. Runs once per
    // keystroke in every AX terminal; unreleased it leaked one value per key.
    let extended_range: Option<AXValue> = if extend_range {
        let updated_range = CFRange::init(selected_text_range.location, 1);
        let created = AXValueCreate(kAXValueTypeCFRange, &updated_range as *const _ as *const c_void);
        if created.is_null() {
            debug!("Couldn't build the one-character range for caret bounds");
            return INVALID_CARET_POSITION;
        }
        Some(AXValue::wrap_under_create_rule(created))
    } else {
        None
    };
    let range_parameter: CFTypeRef = match &extended_range {
        Some(range) => range.as_CFTypeRef(),
        None => selected_range_value.as_concrete_TypeRef(),
    };

    if !prepare_ax_query(focused_element, deadline) {
        return INVALID_CARET_POSITION;
    }
    let select_bounds_result = ax_call(|x: *mut CFTypeRef| {
        AXUIElementCopyParameterizedAttributeValue(
            focused_element.as_concrete_TypeRef(),
            CFString::new(kAXBoundsForRangeParameterizedAttribute).as_concrete_TypeRef(),
            range_parameter,
            x,
        )
    });

    // `Copy` rule again: take the +1 so the bounds value is released on return.
    let select_bounds: AXValue = match select_bounds_result {
        Ok(select_bounds) => AXValue::wrap_under_create_rule(select_bounds as AXValueRef),
        Err(err) => {
            debug!("Selected bounds error, error code {:?}", err);
            return INVALID_CARET_POSITION;
        },
    };

    let selected_rect_result = bool_ax_call(|x: *mut CGRect| {
        AXValueGetValue(select_bounds.as_concrete_TypeRef(), kAXValueTypeCGRect, x.cast())
    });

    let select_rect = match selected_rect_result {
        Ok(select_rect) => select_rect,
        Err(err) => {
            debug!("Couldn't get selected range, did types match {:?}", err);
            return INVALID_CARET_POSITION;
        },
    };
    // Sanity check: prevents flashing autocomplete in bottom corner
    if (require_insertion_point && !valid_caret_bounds(select_rect))
        || (select_rect.size.width == 0.0 && select_rect.size.height == 0.0)
    {
        debug!("Prevents flashing autocomplete in bottom corner");
        return INVALID_CARET_POSITION;
    }

    // Tauri uses Quartz coordinates (don't need to convert coordinates to Cocoa like macos)
    let result = CaretPosition {
        valid: true,
        x: select_rect.origin.x,
        y: select_rect.origin.y,
        height: select_rect.size.height,
    };
    debug!("Got position {result:?}");
    result
}

fn valid_caret_bounds(rect: CGRect) -> bool {
    rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
        && rect.size.width.is_finite()
        && rect.size.height.is_finite()
        && rect.size.width >= 0.0
        && rect.size.height > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_graphics::geometry::{CGPoint, CGSize};

    #[test]
    fn terminal_caret_requires_an_insertion_point_not_selected_text() {
        assert!(valid_terminal_insertion_range(CFRange::init(0, 0)));
        assert!(valid_terminal_insertion_range(CFRange::init(100, 0)));
        assert!(!valid_terminal_insertion_range(CFRange::init(-1, 0)));
        assert!(!valid_terminal_insertion_range(CFRange::init(100, 1)));
        assert!(!valid_terminal_insertion_range(CFRange::init(100, 20)));
    }

    #[test]
    fn caret_bounds_accept_zero_width_and_external_screens_but_reject_invalid_geometry() {
        let rect = |x, y, width, height| CGRect::new(&CGPoint::new(x, y), &CGSize::new(width, height));
        assert!(valid_caret_bounds(rect(-1920.0, -450.0, 0.0, 18.0)));
        assert!(!valid_caret_bounds(rect(100.0, 200.0, 1.0, 0.0)));
        assert!(!valid_caret_bounds(rect(f64::NAN, 200.0, 1.0, 18.0)));
        assert!(!valid_caret_bounds(rect(100.0, 200.0, -1.0, 18.0)));
    }
}
