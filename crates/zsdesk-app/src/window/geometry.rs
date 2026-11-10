//! Geometry shared by custom-frame message handling and local regressions.
use windows::Win32::{Foundation::{POINT, RECT}, UI::WindowsAndMessaging::{
    HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
    SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_MINIMIZE, SW_SHOWMINNOACTIVE, WPF_RESTORETOMAXIMIZED,
}};

pub(super) fn resize_hit(window: RECT, point: POINT, border: (i32, i32), maximized: bool, resizable: bool) -> Option<u32> {
    if maximized || !resizable || window.right <= window.left || window.bottom <= window.top
        || point.x < window.left || point.x >= window.right || point.y < window.top || point.y >= window.bottom { return None; }
    let x = border.0.max(1).min((window.right - window.left) / 2);
    let y = border.1.max(1).min((window.bottom - window.top) / 2);
    let left = point.x < window.left + x;
    let right = point.x >= window.right - x;
    let top = point.y < window.top + y;
    let bottom = point.y >= window.bottom - y;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(HTTOPLEFT), (_, true, true, _) => Some(HTTOPRIGHT),
        (true, _, _, true) => Some(HTBOTTOMLEFT), (_, true, _, true) => Some(HTBOTTOMRIGHT),
        (true, _, _, _) => Some(HTLEFT), (_, true, _, _) => Some(HTRIGHT),
        (_, _, true, _) => Some(HTTOP), (_, _, _, true) => Some(HTBOTTOM), _ => None,
    }
}

pub(super) fn is_resize_hit(value: u32) -> bool {
    [HTLEFT,HTRIGHT,HTTOP,HTBOTTOM,HTTOPLEFT,HTTOPRIGHT,HTBOTTOMLEFT,HTBOTTOMRIGHT].contains(&value)
}

pub(super) fn captionless_client(window: RECT, mut native_client: RECT, maximized: bool, frame_y: i32) -> RECT {
    native_client.top = window.top.saturating_add(if maximized { frame_y.max(0) } else { 0 }).min(native_client.bottom);
    native_client
}

pub(super) fn restore_maximized(show: u32, flags: u32) -> bool {
    show == SW_SHOWMAXIMIZED.0 as u32 ||
        ([SW_SHOWMINIMIZED, SW_MINIMIZE, SW_SHOWMINNOACTIVE].iter().any(|value| show == value.0 as u32)
            && flags & WPF_RESTORETOMAXIMIZED.0 != 0)
}

/// Origin of an additional window: a fixed step down and right from the window
/// opened before it, restarting near the work-area corner when the next step no
/// longer fits.
pub(super) fn cascaded_origin(previous: Option<(i32, i32)>, anchor: (i32, i32), size: (i32, i32),
    step: i32, margin: i32, work: RECT) -> (i32, i32) {
    let step = step.max(0);
    let (mut left, mut top) = match previous {
        Some((last_x, last_y)) => (last_x.saturating_add(step), last_y.saturating_add(step)),
        None => (anchor.0.saturating_add(step), anchor.1.saturating_add(step)),
    };
    if left.saturating_add(size.0) > work.right || top.saturating_add(size.1) > work.bottom {
        left = work.left.saturating_add(margin.max(0));
        top = work.top.saturating_add(margin.max(0));
    }
    (left, top)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_edges_and_corners_resize_at_each_dpi_and_negative_origin() {
        for dpi in [96, 120, 144, 192] {
            let border = 8 * dpi / 96;
            let r = RECT { left: -1600, top: -120, right: -600, bottom: 580 };
            for (point, expected) in [
                (POINT{x:-1599,y:-119}, HTTOPLEFT), (POINT{x:-601,y:-119}, HTTOPRIGHT),
                (POINT{x:-1599,y:579}, HTBOTTOMLEFT), (POINT{x:-601,y:579}, HTBOTTOMRIGHT),
                (POINT{x:-1599,y:230}, HTLEFT), (POINT{x:-601,y:230}, HTRIGHT),
                (POINT{x:-1100,y:-119}, HTTOP), (POINT{x:-1100,y:579}, HTBOTTOM),
            ] { assert_eq!(resize_hit(r, point, (border,border), false, true), Some(expected)); }
            assert_eq!(resize_hit(r, POINT{x:-1100,y:230}, (border,border), false, true), None);
            assert_eq!(resize_hit(r, POINT{x:-601,y:230}, (border,border), true, true), None);
            assert_eq!(resize_hit(r, POINT{x:-601,y:230}, (border,border), false, false), None);
            assert_eq!(resize_hit(r, POINT{x:-600,y:230}, (border,border), false, true), None);
        }
    }
    #[test]
    fn restore_does_not_accumulate_the_maximized_top_inset() {
        let normal = RECT { left: 100, top: 200, right: 1100, bottom: 900 };
        let normal_client = RECT { left: 108, top: 231, right: 1092, bottom: 892 };
        let restored = captionless_client(normal, normal_client, false, 8);
        assert_eq!((restored.left, restored.top, restored.right, restored.bottom), (108,200,1092,892));
        let zoomed = captionless_client(RECT { left:-8,top:-8,right:1928,bottom:1048 },
            RECT { left:0,top:23,right:1920,bottom:1040 }, true, 8);
        assert_eq!((zoomed.left,zoomed.top,zoomed.right,zoomed.bottom),(0,0,1920,1040));
        assert_eq!(captionless_client(normal, normal_client, false, 8), restored);
    }
    #[test]
    fn closing_a_minimized_maximized_window_keeps_its_restore_state() {
        assert!(restore_maximized(SW_SHOWMAXIMIZED.0 as u32, 0));
        assert!(restore_maximized(SW_SHOWMINIMIZED.0 as u32, WPF_RESTORETOMAXIMIZED.0));
        assert!(!restore_maximized(SW_SHOWMINIMIZED.0 as u32, 0));
        assert!(!restore_maximized(1, WPF_RESTORETOMAXIMIZED.0));
    }
    #[test]
    fn additional_windows_cascade_by_a_fixed_step_and_wrap_before_leaving_the_screen() {
        let work = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };
        let size = (1180, 760);
        let first = cascaded_origin(None, (60, 52), size, 28, 24, work);
        assert_eq!(first, (88, 80));
        let mut origin = first;
        let mut origins = vec![origin];
        for _ in 0..2 {
            origin = cascaded_origin(Some(origin), (60, 52), size, 28, 24, work);
            origins.push(origin);
        }
        // Every step is the same size until the work area is exhausted.
        assert_eq!(origins, vec![(88, 80), (116, 108), (144, 136)]);
        // The last step that still fits stays; the next one restarts the cascade.
        let last_fit = cascaded_origin(Some((712, 250)), (60, 52), size, 28, 24, work);
        assert_eq!(last_fit, (740, 278));
        assert_eq!(cascaded_origin(Some(last_fit), (60, 52), size, 28, 24, work), (24, 24));
    }
}
