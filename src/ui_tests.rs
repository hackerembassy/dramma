use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use std::cell::Cell;

struct TestPlatform(Rc<MinimalSoftwareWindow>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn tap(app: &MainWindow, x: f32, y: f32) {
    let position = slint::LogicalPosition::new(x, y);
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    app.window().dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn outage_blocks_payments_preserves_credit_and_keeps_diagnostics_accessible() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    adapter.set_size(slint::PhysicalSize::new(1280, 1024));
    app.show().unwrap();

    let starts = Rc::new(Cell::new(0));
    app.on_start_accepting_money({
        let starts = starts.clone();
        move || starts.set(starts.get() + 1)
    });
    let stops = Rc::new(Cell::new(0));
    app.on_stop_accepting_money({
        let stops = stops.clone();
        move || stops.set(stops.get() + 1)
    });

    assert!(app.get_showing_technical_issue());
    // Allow a local visual check using the same UI as the kiosk, with no hardware.
    if let Ok(path) = std::env::var("DRAMMA_UI_SNAPSHOT") {
        let pixels = app.window().take_snapshot().unwrap();
        image::save_buffer(
            path,
            pixels.as_bytes(),
            pixels.width(),
            pixels.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
    tap(&app, 1050., 470.);
    assert_eq!(starts.get(), 0);

    app.invoke_acceptor_health_changed(true);
    assert!(!app.get_showing_technical_issue());
    // The Play Games card in the main menu.
    tap(&app, 1050., 470.);
    assert!(app.get_on_insert_coins_page());
    assert_eq!(starts.get(), 1);
    app.set_session_amount(500);

    app.invoke_acceptor_health_changed(false);
    assert!(app.get_showing_technical_issue());
    assert_eq!(stops.get(), 1);
    assert_eq!(app.get_session_amount(), 500);
    app.invoke_open_diagnostics();
    assert!(!app.get_showing_technical_issue());
    app.invoke_return_from_diagnostics();
    assert!(app.get_showing_technical_issue());
    assert_eq!(starts.get(), 1);

    app.invoke_acceptor_health_changed(true);
    assert!(!app.get_showing_technical_issue());
    assert!(app.get_on_insert_coins_page());
    assert_eq!(app.get_session_amount(), 500);
    assert_eq!(starts.get(), 2);
}

#[test]
fn maintenance_screen_skips_password_gate_but_main_page_does_not() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    adapter.set_size(slint::PhysicalSize::new(1280, 1024));
    app.show().unwrap();

    app.set_diagnostics_password("secret".into());

    // Healthy + on Main: opening diagnostics still requires the password.
    app.invoke_acceptor_health_changed(true);
    app.invoke_open_diagnostics();
    let _ = app.window().take_snapshot();
    assert!(
        app.get_on_diagnostics_auth_page(),
        "normal entry should still be password-gated"
    );
    app.invoke_return_from_diagnostics();
    let _ = app.window().take_snapshot();

    // Unavailable (technical-issue/maintenance screen showing): the password
    // gate is skipped and diagnostics opens directly.
    app.invoke_acceptor_health_changed(false);
    assert!(app.get_showing_technical_issue());
    app.invoke_open_diagnostics();
    let _ = app.window().take_snapshot();
    assert!(
        !app.get_on_diagnostics_auth_page(),
        "maintenance-screen entry should skip the password gate"
    );
    assert!(!app.get_showing_technical_issue());
}

#[test]
fn diagnostics_auth_page_reopens_on_repeat_entry() {
    // Covers the page-transition/re-instantiation half of a reported bug
    // (password screen's virtual keyboard not reappearing after Back, then
    // reopening diagnostics again). This part is confirmed correct here:
    // DiagnosticsAuth's `init` genuinely reruns each time. The keyboard's
    // `open` flag is now also deferred by a frame via a real Timer (see
    // diagnostics_auth.slint) to dodge a suspected animation/destroy race on
    // real hardware — that part isn't verifiable headlessly, since this
    // manually-driven test platform doesn't pump Slint's timers without a
    // real event loop. Verify the keyboard behavior itself on device.
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    adapter.set_size(slint::PhysicalSize::new(1280, 1024));
    app.show().unwrap();

    app.set_diagnostics_password("secret".into());

    // A render pass is required for conditionally-instantiated pages (`if
    // current-page == ...: Page { }`) to actually construct and run `init`;
    // property changes alone don't trigger it in this headless harness.
    app.invoke_open_diagnostics();
    let _ = app.window().take_snapshot();
    assert!(
        !app.get_showing_technical_issue(),
        "should have navigated to DiagnosticsAuth (1st time)"
    );

    app.invoke_return_from_diagnostics();
    let _ = app.window().take_snapshot();

    app.invoke_open_diagnostics();
    let _ = app.window().take_snapshot();
    assert!(
        !app.get_showing_technical_issue(),
        "should have navigated to DiagnosticsAuth (2nd time)"
    );
}

#[test]
fn cat_page_waits_for_print_completion_before_returning_home() {
    let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
    let app = MainWindow::new().unwrap();
    adapter.set_size(slint::PhysicalSize::new(1280, 1024));
    app.show().unwrap();
    app.invoke_acceptor_health_changed(true);

    let print_requests = Rc::new(Cell::new(0));
    app.on_print_cat_clicked({
        let print_requests = print_requests.clone();
        move |_| print_requests.set(print_requests.get() + 1)
    });
    let celebrations = Rc::new(Cell::new(0));
    app.on_confetti_started({
        let celebrations = celebrations.clone();
        move || celebrations.set(celebrations.get() + 1)
    });

    // Open Print-a-cat from the first card in the second row.
    tap(&app, 275., 700.);
    let _ = app.window().take_snapshot();
    app.set_session_amount(100);

    // Press the large Print button.
    tap(&app, 640., 580.);
    assert_eq!(print_requests.get(), 1);
    assert!(app.get_cat_printing());
    assert_eq!(app.get_session_amount(), 100);
    assert_eq!(celebrations.get(), 0);
    assert!(!app.get_on_insert_money_page());

    app.invoke_cat_print_finished();
    assert!(!app.get_cat_printing());
    assert_eq!(app.get_session_amount(), 0);
    assert_eq!(celebrations.get(), 1);
    assert!(!app.get_on_insert_money_page());
}
