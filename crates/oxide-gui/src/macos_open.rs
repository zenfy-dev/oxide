//! Receives files opened from Finder (double-click, "Open With", dropping on the Dock icon).
//!
//! macOS delivers these as an `odoc` Apple Event rather than as command-line arguments, and
//! winit 0.30 (under eframe) does not handle it. Without this, AppKit's default handler
//! answers instead, with "Oxide cannot open files in the Archive format".
//!
//! Timing matters: files chosen when Oxide isn't running arrive *during* launch, before eframe
//! creates the app. Apple's documented hook is `applicationWillFinishLaunching`, which comes
//! after AppKit installs its default handlers. winit owns the app delegate, so [`register`]
//! (called before eframe starts) observes the equivalent notification and installs the
//! handler then.
//!
//! This is the only `unsafe` code in the app: defining an Objective-C class and registering
//! it with `NSAppleEventManager` cannot be expressed in safe Rust.
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{ClassType, define_class, msg_send, sel};
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString};

/// Paths received but not yet picked up by the UI thread.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "OxideOpenDocumentsHandler"]
    struct OpenDocumentsHandler;

    impl OpenDocumentsHandler {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            self.install_event_handler();
        }

        #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
        fn handle_open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            // keyDirectObject holds a list of file references.
            let Some(list) = event.paramDescriptorForKeyword(four_cc(b"----")) else {
                return;
            };
            let paths: Vec<PathBuf> = (1..=list.numberOfItems())
                .filter_map(|i| list.descriptorAtIndex(i)?.fileURLValue()?.path())
                .map(|p| PathBuf::from(p.to_string()))
                .collect();
            PENDING.lock().unwrap_or_else(|e| e.into_inner()).extend(paths);
            if let Some(ctx) = CONTEXT.get() {
                ctx.request_repaint();
            }
        }
    }
);

impl OpenDocumentsHandler {
    fn install_event_handler(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: this class implements the selector with the signature the event manager
        // expects (`-(void)handle:(NSAppleEventDescriptor*)event withReplyEvent:(…)reply`).
        unsafe {
            manager.setEventHandler_andSelector_forEventClass_andEventID(
                self,
                sel!(handleOpenDocuments:withReplyEvent:),
                four_cc(b"aevt"),
                four_cc(b"odoc"),
            );
        }
    }
}

/// Arranges for Finder "open" events to reach Oxide. Call once, before eframe starts.
pub fn register() {
    // SAFETY: `new` on an NSObject subclass returns a retained, initialised instance.
    let handler: Retained<OpenDocumentsHandler> = unsafe { msg_send![OpenDocumentsHandler::class(), new] };
    let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
    // SAFETY: the handler implements `applicationWillFinishLaunching:` taking an NSNotification.
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &handler,
            sel!(applicationWillFinishLaunching:),
            Some(&name),
            None,
        );
    }
    // Neither the notification center nor the event manager retains the handler; it must
    // live as long as the app.
    std::mem::forget(handler);
}

/// Lets the handler wake the UI when files arrive. Call once the egui context exists.
pub fn attach(ctx: &egui::Context) {
    let _ = CONTEXT.set(ctx.clone());
    ctx.request_repaint();
}

/// Files opened from Finder since the last call.
pub fn take_opened() -> Vec<PathBuf> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()))
}
