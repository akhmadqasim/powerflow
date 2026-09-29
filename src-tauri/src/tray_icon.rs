use std::process;

use tauri::{
    menu::{MenuBuilder, MenuItemBuilder},
    tray::{MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, Runtime,
};
use tauri_plugin_nspopover::{AppExt, WindowExt as _};
use tauri_specta::Event;

use crate::{event::PowerUpdatedEvent, ext::WebviewWindowExt, show_main_window};

pub fn setup_tray_icon<R: Runtime>(app: &impl Manager<R>) -> tauri::Result<()> {
    let show = MenuItemBuilder::new("Show Window").build(app)?;
    let quit = MenuItemBuilder::new("Quit").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .separator()
        .item(&quit)
        .build()?;

    let tray_icon = TrayIconBuilder::with_id("main")
        .title(PowerUpdatedEvent::new(0.0).0)
        .menu_on_left_click(false)
        .menu(&menu)
        .build(app)?;

    // SAFETY: `setup` runs on the main thread, right after the status item
    // has been created.
    unsafe { use_monospaced_digits_in_status_bar() };

    tray_icon.on_menu_event(move |app, event| match event.id() {
        val if val == show.id() => show_main_window(app),
        val if val == quit.id() => {
            app.cleanup_before_exit();
            process::exit(0);
        }
        _ => {}
    });

    tray_icon.on_tray_icon_event(move |tray_handle, event| {
        tauri_plugin_positioner::on_tray_event(tray_handle.app_handle(), &event);
        if let TrayIconEvent::Click {
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let handle = tray_handle.app_handle();
            if handle.is_popover_shown() {
                handle.hide_popover();
            } else {
                handle.show_popover();
            }
        }
    });

    PowerUpdatedEvent::listen(app.app_handle(), move |event| {
        if let Err(e) = tray_icon.set_title(Some(event.payload.0)) {
            log::error!("failed to update tray title: {e}");
        }
    });

    if let Some(popover) = app.popover_window() {
        popover.to_popover();
    }

    Ok(())
}

/// The default menu bar font has proportional digits, so the status item
/// changes width on every update. Switch our status item button to the
/// monospaced-digit variant of the menu bar font.
///
/// # Safety
/// Must be called on the main thread.
unsafe fn use_monospaced_digits_in_status_bar() {
    use cocoa::base::{id, nil, BOOL, NO};
    use objc::{class, msg_send, runtime::Class, sel, sel_impl};

    let (Some(status_window_class), Some(button_class)) =
        (Class::get("NSStatusBarWindow"), Class::get("NSButton"))
    else {
        return;
    };

    let app: id = msg_send![class!(NSApplication), sharedApplication];
    let windows: id = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    let font_size: f64 = {
        let menu_font: id = msg_send![class!(NSFont), menuBarFontOfSize: 0.0f64];
        msg_send![menu_font, pointSize]
    };
    // NSFontWeightRegular
    let font: id = msg_send![class!(NSFont), monospacedDigitSystemFontOfSize: font_size weight: 0.0f64];
    if font == nil {
        return;
    }

    for i in 0..count {
        let window: id = msg_send![windows, objectAtIndex: i];
        let is_status_window: BOOL = msg_send![window, isKindOfClass: status_window_class];
        if is_status_window == NO {
            continue;
        }
        let view: id = msg_send![window, contentView];
        if view == nil {
            continue;
        }
        let is_button: BOOL = msg_send![view, isKindOfClass: button_class];
        if is_button != NO {
            let _: () = msg_send![view, setFont: font];
        }
    }
}
