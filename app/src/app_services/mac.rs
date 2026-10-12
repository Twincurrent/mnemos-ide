#[allow(deprecated)]
use cocoa::base::id;
use mnemos_ui::platform::mac::make_nsstring;


extern "C" {
    /// ObjC function to create and register the NSServices provider for the
    /// application.
    fn mnemos_register_services_provider();
}

/// Initializes application services.
pub fn init() {
    unsafe {
        mnemos_register_services_provider();
    }
}

/// Returns an NSString containing the URL scheme for the links the Finder
/// Services build. Mnemos IDE: that is [`crate::uri::local_launch_scheme`],
/// not the registered mnemos:// scheme, so a Services request opens the
/// folder at once instead of asking like a link from a web page.
///
/// Called synchronously from the NSServices dispatch path in
/// `services.m::forFilesFromPasteboard:performAction:`, which wraps the body in
/// an `@autoreleasepool` block. That ambient pool owns the returned NSString.
#[allow(deprecated)]
#[no_mangle]
extern "C-unwind" fn mnemos_services_provider_custom_url_scheme() -> id {
    make_nsstring(&crate::uri::local_launch_scheme())
}
