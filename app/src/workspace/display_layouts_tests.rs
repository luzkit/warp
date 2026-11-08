use super::*;

fn rect(x: f32, y: f32, width: f32, height: f32) -> RectF {
    RectF::new(vec2f(x, y), vec2f(width, height))
}

#[test]
fn display_key_ignores_enumeration_order_and_distinguishes_arrangement() {
    let primary = rect(0., 0., 1200., 800.);
    let right = rect(1200., 0., 900., 700.);
    let left = rect(-900., 0., 900., 700.);

    assert_eq!(
        display_key(&[primary, right]),
        display_key(&[right, primary])
    );
    assert_ne!(
        display_key(&[primary, right]),
        display_key(&[primary, left])
    );
}

#[test]
fn recovers_an_offscreen_window_and_returns_it_when_the_display_reconnects() {
    let mut state = DisplayLayoutState::default();
    let id = WindowId::from_usize(1);
    let primary = rect(0., 0., 1200., 800.);
    let external = rect(1200., 0., 1600., 1000.);
    let original = rect(1300., 30., 1400., 900.);

    assert!(
        state
            .observe(vec![primary, external], HashMap::from([(id, original)]))
            .is_empty()
    );
    let expected = rect(24., 24., 1152., 752.);
    assert_eq!(
        state.observe(vec![primary], HashMap::from([(id, original)])),
        vec![(id, expected)]
    );
    assert_eq!(
        state.observe(vec![primary, external], HashMap::from([(id, expected)])),
        vec![(id, original)]
    );
}

#[test]
fn remembers_a_window_the_os_already_moved_to_the_primary_display() {
    let mut state = DisplayLayoutState::default();
    let id = WindowId::from_usize(1);
    let primary = rect(0., 0., 1200., 800.);
    let external = rect(1200., 0., 900., 700.);
    let original = rect(1300., 50., 500., 500.);
    let moved = rect(25., 50., 500., 500.);

    state.observe(vec![primary, external], HashMap::from([(id, original)]));
    assert!(
        state
            .observe(vec![primary], HashMap::from([(id, moved)]))
            .is_empty()
    );
    assert_eq!(
        state.observe(vec![primary, external], HashMap::from([(id, moved)])),
        vec![(id, original)]
    );
}

#[test]
fn respects_a_window_repositioned_after_recovery() {
    let mut state = DisplayLayoutState::default();
    let id = WindowId::from_usize(1);
    let primary = rect(0., 0., 1200., 800.);
    let external = rect(1200., 0., 900., 700.);
    let original = rect(1300., 50., 500., 500.);
    let repositioned = rect(55., 55., 500., 500.);

    state.observe(vec![primary, external], HashMap::from([(id, original)]));
    state.observe(vec![primary], HashMap::from([(id, original)]));
    state.observe(vec![primary], HashMap::from([(id, repositioned)]));
    assert!(
        state
            .observe(vec![primary, external], HashMap::from([(id, repositioned)]))
            .is_empty()
    );
}

#[test]
fn keeps_reachable_windows_in_place_when_the_display_setup_changes() {
    let mut state = DisplayLayoutState::default();
    let id = WindowId::from_usize(1);
    let primary = rect(0., 0., 1200., 800.);
    let external = rect(1200., 0., 900., 700.);
    let partly_visible = rect(1090., 50., 500., 500.);

    state.observe(
        vec![primary, external],
        HashMap::from([(id, partly_visible)]),
    );
    assert!(
        state
            .observe(vec![primary], HashMap::from([(id, partly_visible)]))
            .is_empty()
    );
}
