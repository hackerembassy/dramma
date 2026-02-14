// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT

slint::include_modules!();

mod cashcode;
mod config;
mod donation;
mod error;
mod funds;
mod home_assistant;

use cashcode::{BillEvent, CashCode};
use config::Config;
use log::{error, info, warn};
use slint::Model;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

pub fn main() {
    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("Starting :3");

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

    pub fn init(
        app: &MainWindow,
        config: &Config,
        cashcode_tx: Sender<bill_acceptor::CashCodeCommand>,
    ) {
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
                            Ok(_) => info!("✅ Donation sent successfully!"),
                            Err(e) => error!("❌ Failed to send donation: {}", e),
                        }
                    })
                    .unwrap();
                } else {
                    warn!("⚠️  No token loaded, donation not sent to server");
                }
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
