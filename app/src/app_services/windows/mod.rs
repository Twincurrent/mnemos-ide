use registry::register_uri_handler;
use mnemos_ui::AppContext;
#[cfg(feature = "release_bundle")]
use {
    service_impl::forward_uri_to_sole_running_instance,
    single_instance_manager::SingleInstanceManager, thiserror::Error, url::Url,
};

mod registry;
#[cfg(feature = "release_bundle")]
mod service_impl;
#[cfg(feature = "release_bundle")]
mod single_instance_manager;

#[derive(Error, Debug)]
#[cfg(feature = "release_bundle")]
pub enum StartupArgsForwardingError {
    #[error("should not forward arguments after an auto-update")]
    IgnoredAfterAutoUpdate,
    #[error("there is no other instance of Mnemos")]
    NoExistingInstance,
    #[error("failed to construct url")]
    CouldNotCreateUrl(#[from] url::ParseError),
    #[error("IPC Client failed to send message")]
    IpcError(#[from] ipc::ClientError),
    #[error("Win32 error")]
    WindowsError(#[from] windows::core::Error),
}

#[cfg(feature = "release_bundle")]
pub fn pass_startup_args_to_existing_instance(
    args: &mnemos_cli::AppArgs,
) -> Result<(), StartupArgsForwardingError> {
    if args.finish_update {
        return Err(StartupArgsForwardingError::IgnoredAfterAutoUpdate);
    }
    if SingleInstanceManager::is_sole_running_instance()? {
        return Err(StartupArgsForwardingError::NoExistingInstance);
    }

    mnemos_ui::r#async::block_on(async {
        // Mnemos IDE: links this process builds itself use the local launch
        // scheme, which the running instance opens without asking.
        let mut urls = args.urls.clone();
        urls.extend(crate::uri::local_launch_urls(args));
        if urls.is_empty() {
            // If there are no URLs on the command line, send one to open a new
            // window using the same current working directory as this process.
            let url = match std::env::current_dir() {
                Ok(current_dir) => crate::uri::local_launch_url(true, &current_dir),
                Err(_) => None,
            };
            let url = match url {
                Some(url) => url,
                None => Url::parse(&format!(
                    "{}://action/new_window",
                    crate::uri::local_launch_scheme()
                ))?,
            };
            urls.push(url);
        }
        forward_uri_to_sole_running_instance(urls).await?;

        Ok(())
    })
}

pub(super) fn init(_ctx: &mut AppContext) {
    #[cfg(feature = "release_bundle")]
    _ctx.add_singleton_model(SingleInstanceManager::new);
    register_uri_handler();
}
