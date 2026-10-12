#![allow(deprecated)]

use std::ffi::CStr;

use cocoa::base::{id, nil};
use core_foundation::{
    base::TCFType,
    string::{CFString, CFStringRef},
};
use objc::{class, msg_send, sel, sel_impl};
use mnemos_core::channel::{Channel, ChannelState};

// Launch Services constants
type LSRolesMask = u32;
type OSStatus = i32;

// https://github.com/kornelski/core-services/blob/5572befea9fae3c31310d875240342229afa14ca/src/launch_services.rs#L33
const K_LS_ROLES_SHELL: LSRolesMask = 0x00000008;

extern "C" {
    // Launch Services bindings
    fn LSCopyDefaultRoleHandlerForContentType(
        in_content_type: CFStringRef,
        in_role: LSRolesMask,
    ) -> CFStringRef;

    fn LSSetDefaultRoleHandlerForContentType(
        in_content_type: CFStringRef,
        in_role: LSRolesMask,
        in_handler_bundle_id: CFStringRef,
    ) -> OSStatus;
}

pub fn can_become_default_terminal() -> bool {
    unsafe {
        let bundle_class = class!(NSBundle);
        let main_bundle: id = msg_send![bundle_class, mainBundle];
        let bundle_id: id = msg_send![main_bundle, bundleIdentifier];
        bundle_id != nil && ChannelState::channel() != Channel::Local
    }
}

pub fn is_mnemos_default_terminal() -> bool {
    let Some(current_handler) = current_default_terminal() else {
        return false;
    };
    let Some(mnemos_bundle_id) = get_mnemos_bundle_id() else {
        return false;
    };
    current_handler == mnemos_bundle_id
}

/// Mnemos IDE: the bundle id of the app that currently opens shell scripts and executables, or
/// `None` when none is set (macOS then uses Terminal).
pub fn current_default_terminal() -> Option<String> {
    unsafe {
        let unix_executable_content_type = CFString::new("public.unix-executable");
        let handler = LSCopyDefaultRoleHandlerForContentType(
            unix_executable_content_type.as_concrete_TypeRef(),
            K_LS_ROLES_SHELL,
        );

        if handler.is_null() {
            return None;
        }

        let handler_string = CFString::wrap_under_create_rule(handler);
        Some(handler_string.to_string())
    }
}

/// Mnemos IDE: the bundle id of the running Mnemos, if it runs as a bundle.
pub fn mnemos_bundle_id() -> Option<String> {
    get_mnemos_bundle_id()
}

pub fn set_mnemos_as_default_terminal() -> Result<(), String> {
    log::debug!("Setting Mnemos as default terminal");

    let bundle_id = get_mnemos_bundle_id().ok_or("No bundle ID".to_string())?;

    set_default_terminal(&bundle_id)
}

pub fn set_default_terminal(bundle_id: &str) -> Result<(), String> {
    log::debug!("Setting default terminal to bundle ID: {bundle_id}");

    unsafe {
        let unix_executable_content_type = CFString::new("public.unix-executable");

        let bundle_id_cf = CFString::new(bundle_id);

        let result = LSSetDefaultRoleHandlerForContentType(
            unix_executable_content_type.as_concrete_TypeRef(),
            K_LS_ROLES_SHELL,
            bundle_id_cf.as_concrete_TypeRef(),
        );

        match result {
            0 => Ok(()),
            _ => Err(format!(
                "LSSetDefaultRoleHandlerForContentType failed with stats: {result}"
            )),
        }
    }
}

/// Gets Mnemos's bundle identifier. This may be `None` if not running as a bundle, i.e. through
/// `cargo run` without `cargo bundle`.
fn get_mnemos_bundle_id() -> Option<String> {
    unsafe {
        let bundle_class = class!(NSBundle);
        let main_bundle: id = msg_send![bundle_class, mainBundle];
        let bundle_id: id = msg_send![main_bundle, bundleIdentifier];

        if bundle_id == nil {
            return None;
        }

        let bundle_id_str: *const i8 = msg_send![bundle_id, UTF8String];
        let bundle_id_cstr = CStr::from_ptr(bundle_id_str);
        String::from_utf8(bundle_id_cstr.to_bytes().into())
            .inspect_err(|err| log::error!("Error converting bundle ID to string: {err:#}"))
            .ok()
    }
}
