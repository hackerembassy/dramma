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
