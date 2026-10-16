//! AppKit: status item, menu, action target, application delegate. Main thread only.
//! Spec sections 3.1, 4.3, 4.4, 4.5, 8.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationDelegate, NSControlStateValueOff, NSControlStateValueOn, NSImage,
    NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    NSWorkspace,
};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol, NSString, ns_string};

use crate::app::{App, MenuModel, WARNING_NO_HELPER};
use crate::config;
use crate::login_item;
use crate::power::SystemPower;
use crate::session::{EndReason, PRESET_MINUTES};
use crate::sleep::RealSleep;

pub type RealApp = App<RealSleep, SystemPower>;

const TAG_TOGGLE: isize = 1;
const TAG_SCREEN_OFF: isize = 2;
const TAG_LOCK: isize = 3;
const TAG_BATTERY_GUARD: isize = 4;
const TAG_LOGIN_ITEM: isize = 5;
const TAG_REVEAL_LOG: isize = 6;
const TAG_QUIT: isize = 7;
/// Timer presets carry `TAG_TIMER_BASE + minutes`.
const TAG_TIMER_BASE: isize = 1000;

const ICON_IDLE: &str = "moon.zzz";
const ICON_ACTIVE: &str = "sun.max.fill";

struct TargetIvars {
    app: Arc<RealApp>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and MenuTarget has no Drop impl.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    struct MenuTarget;

    unsafe impl NSObjectProtocol for MenuTarget {}

    impl MenuTarget {
        #[unsafe(method(menuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            handle_action(&self.ivars().app, sender.tag(), self.mtm());
        }
    }

    unsafe impl NSMenuDelegate for MenuTarget {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, _menu: &NSMenu) {
            refresh_now();
        }
    }

    unsafe impl NSApplicationDelegate for MenuTarget {
        #[unsafe(method(applicationWillTerminate:))]
        fn application_will_terminate(&self, _notification: &NSNotification) {
            tracing::info!("application terminating");
            self.ivars().app.quit();
        }
    }
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker, app: Arc<RealApp>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { app });
        // SAFETY: NSObject's `init` has the standard signature.
        unsafe { msg_send![super(this), init] }
    }
}

fn handle_action(app: &Arc<RealApp>, tag: isize, mtm: MainThreadMarker) {
    let model = app.menu_model(Instant::now());
    match tag {
        TAG_TOGGLE => {
            if model.active {
                app.stop_session(EndReason::User);
            } else {
                app.start_session(None);
            }
        }
        t if t > TAG_TIMER_BASE => app.start_session(Some((t - TAG_TIMER_BASE) as u64)),
        TAG_SCREEN_OFF => app.screen_off(),
        TAG_LOCK => app.lock_screen(),
        TAG_BATTERY_GUARD => app.set_battery_guard(!model.battery_guard),
        TAG_LOGIN_ITEM => {
            let on = !model.launch_at_login;
            match login_item::set_enabled(on) {
                Ok(()) => app.set_launch_at_login(on),
                Err(e) => tracing::warn!("launch at login change failed: {e}"),
            }
        }
        TAG_REVEAL_LOG => reveal_log(),
        TAG_QUIT => {
            app.quit();
            NSApplication::sharedApplication(mtm).terminate(None);
        }
        other => tracing::warn!("unknown menu tag {other}"),
    }
    refresh_now();
}

fn reveal_log() {
    let dir = config::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = NSString::from_str(&dir.to_string_lossy());
    let _ = NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(None, &path);
}

pub fn preset_title(minutes: u64) -> String {
    match minutes {
        m if m < 60 => format!("{m} minutes"),
        60 => "1 hour".to_string(),
        m => format!("{} hours", m / 60),
    }
}

fn action_item(mtm: MainThreadMarker, title: &str, tag: isize, target: &MenuTarget) -> Retained<NSMenuItem> {
    // SAFETY: `menuAction:` is defined on MenuTarget with the matching (id) signature.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel!(menuAction:)),
            ns_string!(""),
        )
    };
    // SAFETY: the target is retained by `Ui` for as long as the menu exists.
    unsafe { item.setTarget(Some(target.as_ref() as &AnyObject)) };
    item.setTag(tag);
    item
}

fn label_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // SAFETY: a nil selector is allowed.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            ns_string!(""),
        )
    };
    item.setEnabled(false);
    item
}

fn symbol(name: &str) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    image.setTemplate(true);
    Some(image)
}

/// Handles to the items whose text or state changes. Lives in a main-thread thread_local.
struct Ui {
    app: Arc<RealApp>,
    status_item: Retained<NSStatusItem>,
    _target: Retained<MenuTarget>,
    _menu: Retained<NSMenu>,
    status_line: Retained<NSMenuItem>,
    warning: Retained<NSMenuItem>,
    note: Retained<NSMenuItem>,
    toggle: Retained<NSMenuItem>,
    helper_line: Retained<NSMenuItem>,
    battery_guard: Retained<NSMenuItem>,
    login_item: Retained<NSMenuItem>,
    icon_idle: Option<Retained<NSImage>>,
    icon_active: Option<Retained<NSImage>>,
}

impl Ui {
    fn apply(&self, m: &MenuModel) {
        let mtm = MainThreadMarker::new().expect("menu refresh must run on the main thread");
        self.status_line.setTitle(&NSString::from_str(&m.status_line()));
        self.warning.setHidden(m.warning().is_none());
        match m.note {
            Some(text) => {
                self.note.setTitle(&NSString::from_str(text));
                self.note.setHidden(false);
            }
            None => self.note.setHidden(true),
        }
        self.toggle.setTitle(&NSString::from_str(if m.active { "Stop session" } else { "Start session" }));
        self.helper_line.setTitle(&NSString::from_str(m.helper_line()));
        self.battery_guard.setState(if m.battery_guard { NSControlStateValueOn } else { NSControlStateValueOff });
        self.login_item.setState(if m.launch_at_login { NSControlStateValueOn } else { NSControlStateValueOff });
        if let Some(button) = self.status_item.button(mtm) {
            let icon = if m.active { &self.icon_active } else { &self.icon_idle };
            match icon {
                Some(image) => button.setImage(Some(image)),
                None => button.setTitle(&NSString::from_str(if m.active { "On" } else { "Zz" })),
            }
        }
    }
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Build the status item and menu, and install the app delegate. Main thread, before `run`.
pub fn install(mtm: MainThreadMarker, app: Arc<RealApp>) {
    let target = MenuTarget::new(mtm, app.clone());
    NSApplication::sharedApplication(mtm).setDelegate(Some(ProtocolObject::from_ref(&*target)));

    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));

    let status_line = label_item(mtm, "Awake: Idle");
    let warning = label_item(mtm, WARNING_NO_HELPER);
    let note = label_item(mtm, "");
    menu.addItem(&status_line);
    menu.addItem(&warning);
    menu.addItem(&note);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let toggle = action_item(mtm, "Start session", TAG_TOGGLE, &target);
    menu.addItem(&toggle);
    let timer = label_item(mtm, "Start with timer");
    timer.setEnabled(true);
    let presets = NSMenu::new(mtm);
    presets.setAutoenablesItems(false);
    for minutes in PRESET_MINUTES {
        presets.addItem(&action_item(mtm, &preset_title(minutes), TAG_TIMER_BASE + minutes as isize, &target));
    }
    timer.setSubmenu(Some(&presets));
    menu.addItem(&timer);
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    menu.addItem(&action_item(mtm, "Screen off", TAG_SCREEN_OFF, &target));
    menu.addItem(&action_item(mtm, "Lock", TAG_LOCK, &target));
    menu.addItem(&NSMenuItem::separatorItem(mtm));

    let battery_guard = action_item(mtm, "Battery guard (end at 10%)", TAG_BATTERY_GUARD, &target);
    let login_item = action_item(mtm, "Launch at login", TAG_LOGIN_ITEM, &target);
    let helper_line = label_item(mtm, "Helper: not running");
    menu.addItem(&battery_guard);
    menu.addItem(&login_item);
    menu.addItem(&helper_line);
    menu.addItem(&action_item(mtm, "Reveal log", TAG_REVEAL_LOG, &target));
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&action_item(mtm, "Quit Awake", TAG_QUIT, &target));

    let status_item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    status_item.setMenu(Some(&menu));
    let icon_idle = symbol(ICON_IDLE);
    let icon_active = symbol(ICON_ACTIVE);

    let ui = Ui {
        app,
        status_item,
        _target: target,
        _menu: menu,
        status_line,
        warning,
        note,
        toggle,
        helper_line,
        battery_guard,
        login_item,
        icon_idle,
        icon_active,
    };
    UI.with(|slot| *slot.borrow_mut() = Some(ui));
    refresh_now();
}

/// Re-render from the current model. Main thread only.
pub fn refresh_now() {
    UI.with(|slot| {
        if let Some(ui) = slot.borrow().as_ref() {
            ui.apply(&ui.app.menu_model(Instant::now()));
        }
    });
}

/// Ask the main thread to re-render. Safe from any thread; this is `App`'s refresh callback.
pub fn post_refresh() {
    dispatch2::DispatchQueue::main().exec_async(refresh_now);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_titles() {
        assert_eq!(preset_title(30), "30 minutes");
        assert_eq!(preset_title(60), "1 hour");
        assert_eq!(preset_title(120), "2 hours");
        assert_eq!(preset_title(480), "8 hours");
    }

    #[test]
    fn timer_tags_do_not_collide_with_fixed_tags() {
        for m in PRESET_MINUTES {
            assert!(TAG_TIMER_BASE + m as isize > TAG_QUIT);
        }
    }
}
