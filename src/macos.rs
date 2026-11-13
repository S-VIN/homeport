//! AppKit delivers Dock/Finder reopen to the running app, without a second main.
//! Extend winit's delegate only with the missing event; keep its ownership intact.
use objc2::{encode::Encode, runtime::{AnyClass, AnyObject, Bool, Imp, Sel}, MainThreadMarker, sel};
use objc2_app_kit::NSApplication;
use std::sync::Mutex;
static CONTEXT: Mutex<Option<egui::Context>> = Mutex::new(None);

unsafe extern "C-unwind" fn reopen(_: &AnyObject, _: Sel, _: &NSApplication, _: Bool) -> Bool {
    if let Ok(context) = CONTEXT.lock() {
        if let Some(ctx) = context.as_ref() { crate::native_tray::show(ctx); }
    }
    Bool::NO
}
pub fn install_reopen(ctx: &egui::Context) {
    let Some(main) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(main);
    let Some(delegate) = app.delegate() else { return };
    if let Ok(mut context) = CONTEXT.lock() { *context = Some(ctx.clone()); }
    let object: &AnyObject = delegate.as_ref();
    let class = object.class();
    let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    if class.instance_method(selector).is_some() { return; }
    // Objective-C BOOL is signed char on Intel macOS and bool on ARM64.
    let encoding = std::ffi::CString::new(format!("{}@:@{}", Bool::ENCODING, Bool::ENCODING)).expect("method encoding");
    // SAFETY: the signature and encoding exactly implement the AppKit delegate
    // method. Runtime copies encoding; class/method and callback live forever.
    // Called on the main thread before AppKit can deliver another reopen event.
    unsafe {
        let implementation = std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel, &NSApplication, Bool) -> Bool, Imp>(reopen);
        objc2::ffi::class_addMethod(class as *const AnyClass as *mut AnyClass, selector, implementation, encoding.as_ptr());
    }
}
