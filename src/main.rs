// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

slint::include_modules!();

mod cashcode;
mod config;
mod donation;
mod error;
mod funds;
mod home_assistant;
mod sound;

use cashcode::{BillEvent, CashCode};
use config::Config;
use log::{error, info, warn};
use slint::Model;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub fn main() {
    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("Starting :3");

    sound::init();

    // Test
    for _ in 0..5 {
        sound::play_yippee();
    }

    // Load config
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            error!(
                "Failed to load configuration, falling back to defaults: {}",
                e
            );
            Config::default()
        }
    };

    let main_window = MainWindow::new().unwrap();

    // Enable fullscreen mode for kiosk deployment
    main_window.window().set_fullscreen(true);

    virtual_keyboard::init(&main_window);
    autocomplete_handler::init(&main_window);
    let cashcode_tx = bill_acceptor::init(&main_window, &config);
    fund_fetcher::init(&main_window, &config);
    donation_handler::init(&main_window, &config, cashcode_tx);
    home_assistant_handler::init(&main_window, &config);

    main_window.run().unwrap();
}

mod bill_acceptor {
    use super::*;
    use slint::*;
    use std::sync::mpsc::channel;

    /// Commands to control the CashCode bill acceptor
    #[derive(Debug, Clone)]
    pub enum CashCodeCommand {
        Enable,
        Disable,
    }

    pub fn init(app: &MainWindow, config: &Config) -> Sender<CashCodeCommand> {
        let weak = app.as_weak();

        // Create a channel for bill events (from CashCode to UI)
        let (event_tx, event_rx) = channel::<BillEvent>();

        // Create a channel for control commands (from UI to CashCode)
        let (cmd_tx, cmd_rx) = channel::<CashCodeCommand>();

        // Start CashCode driver in a separate thread
        thread::spawn({
            let config = config.clone();
            move || match init_cashcode(&config, event_tx, cmd_rx) {
                Ok(_) => info!("CashCode driver stopped"),
                Err(e) => error!("CashCode driver error: {}", e),
            }
        });

        // Set up callbacks for page transitions
        let cmd_tx_start = cmd_tx.clone();
        app.on_start_accepting_money(move || {
            info!("📥 UI: Start accepting money");
            if cmd_tx_start.send(CashCodeCommand::Enable).is_err() {
                error!("Failed to send enable command to CashCode");
            }
        });

        let cmd_tx_stop = cmd_tx.clone();
        app.on_stop_accepting_money(move || {
            info!("📤 UI: Stop accepting money");
            if cmd_tx_stop.send(CashCodeCommand::Disable).is_err() {
                error!("Failed to send disable command to CashCode");
            }
        });

        // Poll for bill events and update UI
        let timer = Timer::default();
        timer.start(
            TimerMode::Repeated,
            std::time::Duration::from_millis(100),
            move || {
                if let Some(window) = weak.upgrade() {
                    // Process all pending events
                    while let Ok(event) = event_rx.try_recv() {
                        match event {
                            BillEvent::Accepted(nominal) => {
                                info!("💵 Bill accepted in UI: {} dram", nominal as i32);
                                let current = window.get_session_amount();
                                window.set_session_amount(current + nominal as i32);
                            }
                            BillEvent::Rejected(reason) => {
                                info!("❌ Bill rejected: {}", reason);
                            }
                            BillEvent::StackerRemoved => {
                                error!("⚠️  Stacker removed!");
                            }
                            BillEvent::StackerReplaced => {
                                info!("✅ Stacker replaced");
                            }
                            BillEvent::Jam(msg) => {
                                error!("🚫 Jam: {}", msg);
                            }
                            BillEvent::Error(msg) => {
                                error!("⚠️  Error: {}", msg);
                            }
                        }
                    }
                }
            },
        );
        // Keep the timer alive for the lifetime of the application
        // Otherwise the timer is dropped, the closure is dropped, and the channel receiver is dropped
        std::mem::forget(timer);

        cmd_tx
    }
}

fn init_cashcode(
    config: &Config,
    tx: Sender<BillEvent>,
    cmd_rx: std::sync::mpsc::Receiver<bill_acceptor::CashCodeCommand>,
) -> Result<(), cashcode::CashCodeError> {
    use bill_acceptor::CashCodeCommand;

    info!("Initializing CashCode driver...");
    let mut cashcode = CashCode::new(&config.cashcode_serial_port, &config.stats_db_path)?;

    info!("Resetting bill acceptor...");
    cashcode.reset()?;
    thread::sleep(Duration::from_secs(5));

    info!("Polling for initializing status...");
    cashcode.poll()?;
    thread::sleep(Duration::from_millis(200));

    info!("Polling for disabled status...");
    cashcode.poll()?;
    thread::sleep(Duration::from_millis(200));

    // Keep bill acceptor disabled until UI requests to enable it
    info!("Bill acceptor initialized, waiting for enable command...");
    info!("Starting polling loop...");
    loop {
        // Check for enable/disable commands from UI
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                CashCodeCommand::Enable => {
                    info!("📥 Enabling bill acceptor...");
                    if let Err(e) = cashcode.enable() {
                        error!("Failed to enable bill acceptor: {}", e);
                    } else {
                        info!("✅ Bill acceptor enabled");
                    }
                }
                CashCodeCommand::Disable => {
                    info!("📤 Disabling bill acceptor...");
                    if let Err(e) = cashcode.disable() {
                        error!("Failed to disable bill acceptor: {}", e);
                    } else {
                        info!("✅ Bill acceptor disabled");
                    }
                }
            }
        }

        match cashcode.poll() {
            Ok(Some(event)) => {
                // Send event to UI thread
                if tx.send(event.clone()).is_err() {
                    error!("Failed to send event to UI thread");
                    break;
                }

                // Also log for debugging
                if let BillEvent::Accepted(_nominal) = event
                    && let Ok(total) = cashcode.get_total_amount()
                {
                    info!("Total collected in DB: {} dram", total);
                }
            }
            Ok(_none) => {
                // No event, continue polling
            }
            Err(e) => {
                error!("poll error: {}", e);
                thread::sleep(Duration::from_secs(1));
            }
        }

        thread::sleep(Duration::from_millis(400));
    }

    Ok(())
}

mod virtual_keyboard {
    use super::*;
    use slint::platform::Key;
    use slint::*;

    pub fn init(app: &MainWindow) {
        let weak = app.as_weak();
        app.global::<VirtualKeyboardHandler>().on_key_pressed({
            move |key| {
                let window = weak.unwrap();

                // Check if the right arrow was pressed - trigger autocomplete
                if key == SharedString::from(Key::RightArrow) {
                    let handler = window.global::<AutocompleteHandler>();
                    let current = handler.get_trigger_autocomplete_toggle();
                    handler.set_trigger_autocomplete_toggle(!current);
                }

                window
                    .window()
                    .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
                window
                    .window()
                    .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
            }
        });
    }
}

mod autocomplete_handler {
    use super::*;

    pub fn init(app: &MainWindow) {
        app.global::<AutocompleteHandler>()
            .on_find_suggestion(|input, suggestions| {
                if input.is_empty() {
                    return slint::SharedString::default();
                }

                let input_lower = input.to_lowercase();

                // Find the first suggestion that starts with the input (case-insensitive)
                for suggestion in suggestions.iter() {
                    let suggestion_lower = suggestion.to_lowercase();
                    if suggestion_lower.starts_with(&input_lower) && suggestion_lower != input_lower
                    {
                        return suggestion;
                    }
                }

                slint::SharedString::default()
            });

        app.global::<AutocompleteHandler>()
            .on_get_suggestion_suffix(|typed, suggestion| {
                if suggestion.is_empty() || typed.is_empty() {
                    return slint::SharedString::default();
                }

                // Get the suffix after the typed text
                let typed_len = typed.chars().count();
                let suffix: String = suggestion.chars().skip(typed_len).collect();
                slint::SharedString::from(suffix)
            });

        app.global::<AutocompleteHandler>()
            .on_is_valid_input(|input, suggestions| {
                if input.is_empty() {
                    return false;
                }

                let input_lower = input.to_lowercase();

                // Check if input exactly matches any suggestion (case-insensitive)
                suggestions.iter().any(|s| s.to_lowercase() == input_lower)
            });
    }
}

mod fund_fetcher {
    use super::*;
    use crate::funds;
    use slint::*;

    pub fn init(app: &MainWindow, config: &Config) {
        let app_handle = app.clone_strong();

        let Some(ref token) = config.token else {
            warn!("⚠️  No token loaded, donation functions unavailable");
            app_handle.set_available_funds(slint::ModelRc::new(slint::VecModel::<
                slint::SharedString,
            >::default()));
            app_handle
                .set_available_fund_ids(slint::ModelRc::new(slint::VecModel::<i32>::default()));

            return;
        };

        let token = token.clone();
        let token_usernames = token.clone();
        app.on_fetch_funds(move || {
            info!("🔍 Fetching funds from API...");
            let app = app_handle.clone_strong();
            let token = token.clone();

            slint::spawn_local(async move {
                match funds::fetch_funds(&token).await {
                    Ok(value) => {
                        info!("✅ Fetched {} funds", value.len());

                        // Convert funds to string array for ComboBox
                        let model_data: Vec<slint::SharedString> = value
                            .iter()
                            .map(|fund| {
                                slint::SharedString::from(std::format!(
                                    "{} (ID: {})",
                                    fund.name,
                                    fund.id
                                ))
                            })
                            .collect();

                        // Also store fund IDs separately for lookup
                        let fund_ids: Vec<i32> = value.iter().map(|f| f.id).collect();

                        // Set the properties on MainWindow
                        app.set_available_funds(slint::ModelRc::new(slint::VecModel::from(
                            model_data,
                        )));
                        app.set_available_fund_ids(slint::ModelRc::new(slint::VecModel::from(
                            fund_ids,
                        )));
                    }
                    Err(e) => {
                        error!("❌ Failed to fetch funds: {}", e);
                        app.set_available_funds(slint::ModelRc::new(slint::VecModel::<
                            slint::SharedString,
                        >::default(
                        )));
                        app.set_available_fund_ids(slint::ModelRc::new(
                            slint::VecModel::<i32>::default(),
                        ));
                    }
                }
            })
            .unwrap();
        });

        let app_handle = app.clone_strong();
        app.on_fetch_usernames(move || {
            info!("🔍 Fetching usernames from API...");
            let app = app_handle.clone_strong();
            let token = token_usernames.clone();

            slint::spawn_local(async move {
                match donation::fetch_usernames(&token).await {
                    Ok(value) => {
                        info!("✅ Fetched {} usernames", value.len());

                        // Convert usernames to string array for the input autocomplete
                        let model_data: Vec<slint::SharedString> = value
                            .iter()
                            .map(|username| slint::SharedString::from(username.to_string()))
                            .collect();

                        // Set the properties on MainWindow
                        app.set_usernames(slint::ModelRc::new(slint::VecModel::from(model_data)));
                    }
                    Err(e) => {
                        error!("❌ Failed to fetch usernames: {}", e);
                        app.set_available_funds(slint::ModelRc::new(slint::VecModel::<
                            slint::SharedString,
                        >::default(
                        )));
                    }
                }
            })
            .unwrap();
        });
    }
}

mod donation_handler {
    use super::*;

    const INACTIVITY_TIMEOUT_SECS: u64 = 180; // 3 minutes

    /// Spawns a single-shot inactivity timer. Returns the Timer (must be kept alive).
    fn spawn_inactivity_timer(
        weak: slint::Weak<MainWindow>,
        cashcode_tx: Sender<bill_acceptor::CashCodeCommand>,
        token: Option<String>,
    ) -> slint::Timer {
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_secs(INACTIVITY_TIMEOUT_SECS),
            move || {
                if let Some(window) = weak.upgrade() {
                    let amount = window.get_session_amount();
                    if amount == 0 {
                        // No money inserted — auto-cancel
                        info!("⏱️  Inactivity timeout: auto-cancelling (no money inserted)");
                        if cashcode_tx
                            .send(bill_acceptor::CashCodeCommand::Disable)
                            .is_err()
                        {
                            error!("Failed to send disable command on inactivity cancel");
                        }
                        window.set_session_amount(0);
                        window.set_session_username(slint::SharedString::default());
                        window.invoke_cancel_insert_money();
                    } else {
                        // Money inserted — auto-approve
                        info!("⏱️  Inactivity timeout: auto-approving {} AMD", amount);
                        if cashcode_tx
                            .send(bill_acceptor::CashCodeCommand::Disable)
                            .is_err()
                        {
                            error!("Failed to send disable command on inactivity approve");
                        }
                        if let Some(ref tok) = token {
                            let username = window.get_session_username().to_string();
                            let fund_id = window.get_session_fund_id();
                            let tok = tok.clone();
                            slint::spawn_local(async move {
                                match donation::send_donation(&tok, fund_id, &username, amount)
                                    .await
                                {
                                    Ok(_) => {
                                        sound::play_yippee();
                                        info!("✅ Auto-approved donation sent successfully!");
                                    }
                                    Err(e) => {
                                        error!("❌ Auto-approve: failed to send donation: {}", e)
                                    }
                                }
                            })
                            .unwrap();
                        } else {
                            warn!("⚠️  No token — auto-approved donation not sent to server");
                        }
                        window.set_session_amount(0);
                        window.set_session_username(slint::SharedString::default());
                        window.set_session_fund_id(0);
                        window.invoke_show_confetti_after_auto_approve();
                    }
                }
            },
        );
        timer
    }

    pub fn init(
        app: &MainWindow,
        config: &Config,
        cashcode_tx: Sender<bill_acceptor::CashCodeCommand>,
    ) {
        // Shared timer slots — replaced on each entry to InsertMoney page or bill insertion
        // Using Rc<RefCell<>> because all callbacks run on the single Slint event-loop thread.
        let inactivity_timer: Rc<RefCell<Option<slint::Timer>>> = Rc::new(RefCell::new(None));
        let countdown_ticker: Rc<RefCell<Option<slint::Timer>>> = Rc::new(RefCell::new(None));

        app.on_done_clicked({
            let cashcode_tx = cashcode_tx.clone();
            let token = config.token.clone();
            move |username, fund_id, amount| {
                info!(
                    "💰 Processing donation: {} AMD from {} to fund {}",
                    amount, username, fund_id
                );

                // Stop accepting money immediately
                if cashcode_tx
                    .send(bill_acceptor::CashCodeCommand::Disable)
                    .is_err()
                {
                    error!("Failed to send disable command to CashCode on done click");
                }
                if let Some(ref token) = token {
                    // Send donation asynchronously using slint::spawn_local
                    let token = token.clone();
                    let username_str = username.to_string();
                    slint::spawn_local(async move {
                        match donation::send_donation(&token, fund_id, &username_str, amount).await
                        {
                            Ok(_) => {
                                sound::play_yippee();
                                info!("✅ Donation sent successfully!");
                            }
                            Err(e) => error!("❌ Failed to send donation: {}", e),
                        }
                    })
                    .unwrap();
                } else {
                    warn!("⚠️  No token loaded, donation not sent to server");
                }
            }
        });

        // enter-insert-money: start 3-minute inactivity timer + countdown ticker
        let weak_enter = app.as_weak();
        let cashcode_tx_enter = cashcode_tx.clone();
        let token_enter = config.token.clone();
        let timer_enter = inactivity_timer.clone();
        let ticker_enter = countdown_ticker.clone();
        app.on_enter_insert_money(move || {
            info!("⏱️  InsertMoney entered — starting {INACTIVITY_TIMEOUT_SECS}s inactivity timer");
            // Reset the countdown display
            if let Some(w) = weak_enter.upgrade() {
                w.set_inactivity_seconds_left(INACTIVITY_TIMEOUT_SECS as i32);
            }
            // Main timeout timer
            let timer = spawn_inactivity_timer(
                weak_enter.clone(),
                cashcode_tx_enter.clone(),
                token_enter.clone(),
            );
            *timer_enter.borrow_mut() = Some(timer);
            // Countdown ticker (1-second decrement)
            let weak_tick = weak_enter.clone();
            let ticker = slint::Timer::default();
            ticker.start(
                slint::TimerMode::Repeated,
                Duration::from_secs(1),
                move || {
                    if let Some(w) = weak_tick.upgrade() {
                        let current = w.get_inactivity_seconds_left();
                        if current > 0 {
                            w.set_inactivity_seconds_left(current - 1);
                        }
                    }
                },
            );
            *ticker_enter.borrow_mut() = Some(ticker);
        });

        // activity-on-insert-money: reset both timers when a bill is inserted
        let weak_activity = app.as_weak();
        let cashcode_tx_activity = cashcode_tx.clone();
        let token_activity = config.token.clone();
        let timer_activity = inactivity_timer.clone();
        let ticker_activity = countdown_ticker.clone();
        app.on_activity_on_insert_money(move || {
            info!("⏱️  Bill inserted — resetting inactivity timer");
            // Reset countdown display
            if let Some(w) = weak_activity.upgrade() {
                w.set_inactivity_seconds_left(INACTIVITY_TIMEOUT_SECS as i32);
            }
            // Replace main timeout timer
            let timer = spawn_inactivity_timer(
                weak_activity.clone(),
                cashcode_tx_activity.clone(),
                token_activity.clone(),
            );
            *timer_activity.borrow_mut() = Some(timer);
            // Replace countdown ticker
            let weak_tick = weak_activity.clone();
            let ticker = slint::Timer::default();
            ticker.start(
                slint::TimerMode::Repeated,
                Duration::from_secs(1),
                move || {
                    if let Some(w) = weak_tick.upgrade() {
                        let current = w.get_inactivity_seconds_left();
                        if current > 0 {
                            w.set_inactivity_seconds_left(current - 1);
                        }
                    }
                },
            );
            *ticker_activity.borrow_mut() = Some(ticker);
        });

        // Drive confetti animation from Rust with a two-step approach:
        // 1. show-confetti is already set to true by the Slint side (overlay is created)
        // 2. After a brief delay, set confetti-falling = true (triggers the animations)
        // 3. After animation completes, reset both properties
        let weak = app.as_weak();
        app.on_confetti_started(move || {
            crate::sound::play_yippee();
            // Step 1: trigger falling after a short delay so the component is fully rendered
            let weak_fall = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
                if let Some(window) = weak_fall.upgrade() {
                    window.set_confetti_falling(true);
                }
            });

            // Step 2: dismiss everything after animations complete
            let weak_dismiss = weak.clone();
            slint::Timer::single_shot(std::time::Duration::from_millis(2500), move || {
                if let Some(window) = weak_dismiss.upgrade() {
                    window.set_confetti_falling(false);
                    window.set_show_confetti(false);
                }
            });
        });

        // Warmup: run the animation once at startup (no sound) so all SVGs are
        // rasterized and cached before the first real donation triggers it.
        let weak_warmup = app.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(500), move || {
            if let Some(window) = weak_warmup.upgrade() {
                info!("🎉 Warming up confetti cache...");
                window.set_show_confetti(true);
                window.set_confetti_falling(true);

                let weak_done = weak_warmup.clone();
                slint::Timer::single_shot(std::time::Duration::from_millis(1000), move || {
                    if let Some(window) = weak_done.upgrade() {
                        window.set_confetti_falling(false);
                        window.set_show_confetti(false);
                    }
                });
            }
        });
    }
}

mod home_assistant_handler {
    use super::*;
    use crate::home_assistant::ChromiumManager;
    use std::sync::Arc;

    pub fn init(app: &MainWindow, config: &Config) {
        let chromium = Arc::new(ChromiumManager::new());
        info!(
            "Home Assistant URL configured: {}",
            config.home_assistant_url
        );

        // Launch Chromium when showing Home Assistant page
        let chromium_show = chromium.clone();
        let url_for_launch = config.home_assistant_url.clone();
        app.on_show_home_assistant(move || {
            info!("Showing Home Assistant page, launching Chromium");
            if let Err(e) = chromium_show.launch(&url_for_launch) {
                error!("Failed to launch Chromium: {}", e);
            }
        });

        // Close Chromium when hiding Home Assistant page
        let chromium_hide = chromium.clone();
        app.on_hide_home_assistant(move || {
            info!("Hiding Home Assistant page, closing Chromium");
            chromium_hide.close();
        });
    }
}
