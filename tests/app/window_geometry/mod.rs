use super::{centered_outer_position, maximized_geometry_needs_repair};

#[test]
fn repairs_large_maximized_geometry_mismatch() {
    assert!(maximized_geometry_needs_repair(604, 1384, 1080, 1501));
    assert!(maximized_geometry_needs_repair(1920, 1000, 3840, 2160));
}

#[test]
fn accepts_taskbar_sized_maximized_work_area() {
    assert!(!maximized_geometry_needs_repair(1920, 1040, 1920, 1080));
    assert!(!maximized_geometry_needs_repair(2560, 1400, 2560, 1440));
}

#[test]
fn centers_window_in_monitor_work_area() {
    assert_eq!(
        centered_outer_position(0, 0, 1920, 1080, 800, 600),
        (560, 240)
    );
    // Secondary monitor whose origin is not (0,0).
    assert_eq!(
        centered_outer_position(1920, 0, 1920, 1080, 800, 600),
        (2480, 240)
    );
}

#[test]
fn centering_clamps_when_window_is_larger_than_the_area() {
    assert_eq!(centered_outer_position(0, 0, 1280, 720, 1440, 900), (0, 0));
}
