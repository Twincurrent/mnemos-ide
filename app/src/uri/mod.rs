pub mod parse_url_paths;
pub mod web_intent_parser;

#[cfg(target_family = "wasm")]
pub mod browser_url_handler;

use crate::ai::agent::api::ServerConversationToken;
use crate::drive::OpenMnemosDriveObjectSettings;
use crate::launch_configs::launch_config::LaunchConfig;
use crate::root_view::{open_new_window_get_handles, OpenLaunchConfigArg};
use crate::server::ids::ServerId;
use crate::server::telemetry::{LaunchConfigUiLocation, TelemetryEvent};
use crate::util::openable_file_type::{is_file_openable_in_mnemos, is_markdown_file};
use crate::workspace::{Workspace, WorkspaceAction, WorkspaceRegistry};
use crate::{cloud_object::ObjectType, workspace::ToastStack};
use crate::{
    drive::OpenMnemosDriveObjectArgs,
    view_components::{DismissibleToast, ToastLink},
};
use crate::{features::FeatureFlag, workspace::active_terminal_in_window};

use crate::settings_view::SettingsSection;
use crate::user_config::load_launch_configs;
use crate::{
    quake_mode_window_id, quake_mode_window_is_open, safe_info, send_telemetry_from_app_ctx,
    ChannelState, OpenPath,
};
use anyhow::{anyhow, ensure, Result};
use itertools::Itertools;
use session_sharing_protocol::common::SessionId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use url::Url;
use mnemos_ui::notification::UserNotification;
use mnemos_ui::{platform::TerminationMode, SingletonEntity as _, TypedActionView};

use mnemos_ui::{AppContext, WindowId};

const DESKTOP_REDIRECT_URI_PATH: &str = "/desktop_redirect";

/// Args for opening the MCP settings page via deeplink, with optional auto-install.
/// The `autoinstall` value is the raw query param string; it is matched case-insensitively
/// against gallery titles in `autoinstall_from_gallery`.
pub struct OpenMCPSettingsArgs {
    pub autoinstall: Option<String>,
}

/// Source query parameter value indicating auth was initiated from cloud agent setup.
/// Used to skip opening settings page after GitHub auth completes.
/// A settings page that a `mnemos://settings/<page>` link can open.
#[derive(Debug, PartialEq, Eq)]
enum SettingsLink {
    /// `mnemos://settings/mcp?autoinstall=<name>`; `autoinstall` is matched
    /// case-insensitively against the gallery titles.
    Mcp { autoinstall: Option<String> },
    /// `mnemos://settings/appearance` (themes, fonts, etc.).
    Appearance,
}

/// Mnemos IDE: only the MCP and Appearance pages are linkable. Upstream's
/// teams, billing_and_usage, environments and platform links are gone, since
/// those pages are not in Mnemos IDE.
fn parse_settings_link(url: &Url) -> Option<SettingsLink> {
    let sub_page = url.path_segments().into_iter().flatten().last()?;
    match sub_page {
        "mcp" => Some(SettingsLink::Mcp {
            autoinstall: url
                .query_pairs()
                .find(|(key, _)| key == "autoinstall")
                .map(|(_, value)| value.into_owned()),
        }),
        "appearance" => Some(SettingsLink::Appearance),
        _ => None,
    }
}

pub const CLOUD_SETUP_SOURCE: &str = "cloud_setup";

#[derive(Debug, PartialEq, Eq)]
pub enum UriHost {
    Auth,
    Team,
    /// A host prefix for all actions (e.g.: new tab, new window).
    Action,
    /// A host prefix for all actions that involve launch configurations
    Launch,
    /// Supports joining shared sessions via a mnemos:// URI.
    SharedSession,
    /// Supports viewing AI conversations via a mnemos:// URI.
    Conversation,
    /// Supports WD object actions
    Drive,
    /// Supports opening mnemos's settings panel via URI
    Settings,
    /// A host prefix for a general-purpose home/landing page. Unlike other intent URIs, the home
    /// page behavior may change over time and vary from platform to platform.
    Home,
    /// Actions related to MCP servers (e.g.: oauth callbacks).
    Mcp,
    /// Mnemos IDE: `mnemos://agents/focus?id=<claude session id>` (or `?project=<name>`)
    /// brings a running agent's tab to the front. Used by Iris. Focus only.
    Agents,
}

impl FromStr for UriHost {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "auth" => Ok(Self::Auth),
            "team" => Ok(Self::Team),
            "action" => Ok(Self::Action),
            "launch" => Ok(Self::Launch),
            "shared_session" if FeatureFlag::ViewingSharedSessions.is_enabled() => {
                Ok(Self::SharedSession)
            }
            "conversation" => Ok(Self::Conversation),
            "drive" => Ok(Self::Drive),
            "settings" => Ok(Self::Settings),
            "home" => Ok(Self::Home),
            "mcp" => Ok(Self::Mcp),
            "agents" => Ok(Self::Agents),
            _ => Err(anyhow!("Received url with unexpected host: {}", s)),
        }
    }
}

impl UriHost {
    fn handle(&self, primary_window_id: Option<WindowId>, url: &Url, ctx: &mut AppContext) {
        // Handle host
        match self {
            UriHost::Auth => {
                ctx.window_ids()
                    .collect_vec()
                    .into_iter()
                    .for_each(|window_id| {
                        let Some(root_view_id) = ctx.root_view_id(window_id) else {
                            return;
                        };
                        safe_info!(
                            safe: ("Dispatched auth url to window {window_id}"),
                            full: ("Dispatched auth url {url} to window {window_id}")
                        );
                        ctx.dispatch_action(
                            window_id,
                            &[root_view_id],
                            "root_view:handle_incoming_auth_url",
                            &url.clone(),
                            log::Level::Info,
                        );
                    });
            }
            UriHost::Team => {
                match url.path_segments().into_iter().flatten().last() {
                    // If the last segment of the URL is "settings", open the team settings page.
                    Some("settings") => {
                        open_window_with_action(
                            primary_window_id,
                            "root_view:open_team_settings_page",
                            ctx,
                        );
                    }
                    // Otherwise default to previous behavior.
                    _ => {
                        // TODO: Parse URL to ensure the user is logged into the right account
                        // Shows the user the settings view of their newly joined team within the app.
                        open_window_with_action(
                            primary_window_id,
                            "root_view:handle_team_intent_link_action",
                            ctx,
                        );
                    }
                };
                send_telemetry_from_app_ctx!(TelemetryEvent::OpenTeamFromURI, ctx);
            }
            UriHost::Action => {
                match Action::parse(url) {
                    Ok(action) => action.handle(LinkOrigin::External, primary_window_id, url, ctx),
                    Err(err) => {
                        log::warn!("{err}");
                    }
                };
            }
            UriHost::Launch => {
                if let Some(desired_config_path) = get_launch_config_path(url.path()) {
                    let configs = load_launch_configs(&crate::user_config::launch_configs_dir());
                    if let Some(config) =
                        find_matching_config(desired_config_path.as_str(), &configs)
                    {
                        ctx.dispatch_global_action(
                            "root_view:open_launch_config",
                            &OpenLaunchConfigArg {
                                launch_config: config.clone(),
                                ui_location: LaunchConfigUiLocation::Uri,
                                open_in_active_window: false,
                            },
                        )
                    } else {
                        log::warn!(
                            "couldn't find a matching file path for '{}'",
                            desired_config_path.as_str()
                        );
                    }
                } else {
                    log::warn!("couldn't turn launch link '{}' into path", url.path());
                }
            }
            UriHost::SharedSession => {
                // We expect the uri to have the ID of the session to join as the last segment.
                // e.g. mnemos://shared_session/{id}
                let session_id = url
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .last()
                    .and_then(|id| SessionId::from_str(id).ok());
                if let Some(session_id) = session_id {
                    // If there's an existing window, join the session inc a new tab. Otherwise, open a new window.
                    match primary_window_id.and_then(|window_id| {
                        ctx.root_view_id(window_id)
                            .map(|view_id| (window_id, view_id))
                    }) {
                        Some((primary_window_id, root_view_id)) => {
                            ctx.dispatch_action(
                                primary_window_id,
                                &[root_view_id],
                                "root_view:join_shared_session_in_existing_window",
                                &session_id,
                                log::Level::Info,
                            );
                        }
                        None => {
                            ctx.dispatch_global_action("root_view:join_shared_session", &session_id)
                        }
                    }
                } else {
                    log::warn!("Failed to join shared session with uri={url}");
                }
            }
            UriHost::Conversation => {
                // We expect the uri to have the conversation ID as the last segment.
                // e.g. mnemos://conversation/{conversation_id}
                let conversation_id: Option<ServerConversationToken> = url
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .last()
                    .map(|s| ServerConversationToken::new(s.to_owned()));

                if let Some(conversation_id) = conversation_id {
                    // If there's an existing window, open the conversation in a new tab. Otherwise, open a new window.
                    match primary_window_id.and_then(|window_id| {
                        ctx.root_view_id(window_id)
                            .map(|view_id| (window_id, view_id))
                    }) {
                        Some((primary_window_id, root_view_id)) => {
                            ctx.dispatch_action(
                                primary_window_id,
                                &[root_view_id],
                                "root_view:open_cloud_conversation_in_existing_window",
                                &conversation_id,
                                log::Level::Info,
                            );
                        }
                        None => ctx.dispatch_global_action(
                            "root_view:open_conversation_viewer",
                            &conversation_id,
                        ),
                    }
                } else {
                    log::warn!("Failed to open conversation with uri={url}");
                }
            }
            UriHost::Drive => {
                // We expect the uri to have the ID of the object we are trying to open and the object_type.
                // e.g. mnemos://drive/{object_type}?id={UID}
                // For folder links, we expect an additional query parameter primary_object_id which refers to the id object
                // that should be opened
                // When the user is directed here via the request access flow, we expect an additional query parameter invitee_email
                // If this paramter is present, we will open the sharing dialog with the email filled in.
                let object_type = url
                    .path_segments()
                    .into_iter()
                    .flatten()
                    .last()
                    .and_then(|object_type| ObjectType::from_str(object_type).ok());

                let query_string: HashMap<_, _> = url.query_pairs().collect();
                let object_server_id: Option<ServerId> =
                    query_string.get("id").map(ServerId::from_string_lossy);

                let focused_folder_id: Option<ServerId> = query_string
                    .get("focused_folder_id")
                    .map(ServerId::from_string_lossy);

                let invitee_email: Option<String> =
                    query_string.get("invitee_email").map(|s| s.to_string());

                if let Some((object_type, server_id)) = object_type.zip(object_server_id) {
                    let primary_window_and_view = primary_window_id.and_then(|window_id| {
                        ctx.root_view_id(window_id)
                            .map(|view_id| (window_id, view_id))
                    });
                    let args = OpenMnemosDriveObjectArgs {
                        object_type,
                        server_id,
                        settings: OpenMnemosDriveObjectSettings {
                            focused_folder_id,
                            invitee_email,
                        },
                    };
                    // If there's an existing window, open the object in that window, otherwise open a new window
                    if let Some((primary_window_id, root_view_id)) = primary_window_and_view {
                        // `args` may contain user-identifiable fields
                        // (e.g. `invitee_email`), so avoid writing the full
                        // debug representation to `mnemos.log` on non-dogfood
                        // release channels.
                        safe_info!(
                            safe: (
                                "Opening drive object in existing window: object_type={:?} server_id={}",
                                args.object_type, args.server_id,
                            ),
                            full: ("Opening drive object in existing window: {args:?}")
                        );
                        ctx.dispatch_action(
                            primary_window_id,
                            &[root_view_id],
                            "root_view:open_drive_object_existing_window",
                            &args,
                            log::Level::Info,
                        );
                    } else {
                        ctx.dispatch_global_action("root_view:open_drive_object_new_window", &args)
                    }
                } else {
                    log::warn!("Failed to open drive object with uri={url}");
                }
            }
            UriHost::Agents => focus_agent_from_url(url, ctx),
            UriHost::Settings => {
                match parse_settings_link(url) {
                    Some(SettingsLink::Mcp { autoinstall }) => {
                        let args = OpenMCPSettingsArgs { autoinstall };
                        dispatch_action_in_new_or_existing_window(
                            primary_window_id,
                            "root_view:open_mcp_settings_in_existing_window",
                            "root_view:open_mcp_settings_in_new_window",
                            &args,
                            ctx,
                        );
                    }
                    Some(SettingsLink::Appearance) => {
                        dispatch_action_in_new_or_existing_window(
                            primary_window_id,
                            "root_view:open_settings_page_in_existing_window",
                            "root_view:open_settings_page_in_new_window",
                            &SettingsSection::Appearance,
                            ctx,
                        );
                    }
                    None => {
                        log::warn!("Failed to open settings pane with uri={url}");
                    }
                }
            }
            UriHost::Home => {
                ctx.dispatch_global_action("root_view::open_new", &());
            }
            UriHost::Mcp => {
                #[cfg(not(target_family = "wasm"))]
                {
                    let result = crate::ai::mcp::TemplatableMCPServerManager::handle(ctx)
                        .update(ctx, |manager, _ctx| manager.handle_oauth_callback(url));
                    if let Err(e) = result {
                        log::error!("Failed to handle MCP OAuth callback: {e:?}");
                    }
                }
            }
        }
    }

    /// When handling this URI action, determine which window(s) should be focused.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn window_behavior_hint(&self) -> WindowBehaviorHint {
        use WindowBehaviorHint as W;
        match self {
            Self::Auth => W::ShowPrimaryWindow(WindowActivationFallbackBehavior::NewWindow {
                replace_existing: true,
            }),
            Self::Team | Self::Drive | Self::Settings => W::default(),
            // These URLs always open new windows.
            Self::Launch | Self::SharedSession | Self::Conversation | Self::Home => W::Nothing,
            // This will actually be handled by [`Action::window_behavior_hint`].
            Self::Action => W::Nothing,
            // TODO(vorporeal): probably want to focus the window with the MCP pane open
            Self::Mcp => W::Nothing,
            // The handler focuses the window that hosts the agent itself.
            Self::Agents => W::Nothing,
        }
    }
}

/// This determines which windows, if any, will become visible on handling a URI. This is a "hint"
/// because it is platform-dependent, and not all platforms can conform. For example, MacOS
/// automatically shows the frontmost window, and so the Nothing variant of this is impossible on
/// MacOS.
#[derive(Clone, Debug)]
enum WindowBehaviorHint {
    /// Determined by the [`get_primary_window`] function.
    ShowPrimaryWindow(WindowActivationFallbackBehavior),
    Nothing,
}

impl Default for WindowBehaviorHint {
    fn default() -> Self {
        Self::ShowPrimaryWindow(WindowActivationFallbackBehavior::NewWindow {
            replace_existing: false,
        })
    }
}

impl WindowBehaviorHint {
    /// Perform the desired window focus behavior for the URI being handled. This may change the
    /// "primary window" if a new one has to be created. Return the new primary WindowId.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn resolve(
        self,
        primary_window_id: Option<WindowId>,
        ctx: &mut AppContext,
    ) -> Option<WindowId> {
        match self {
            Self::ShowPrimaryWindow(fallback_behavior) => {
                if let Some(window_id) = primary_window_id {
                    match ctx.windows().windowing_system() {
                        Some(windowing_system)
                            if windowing_system.allows_programmatic_window_activation() =>
                        {
                            ctx.windows().show_window_and_focus_app(window_id);
                        }
                        _ => {
                            return fallback_behavior.resolve(window_id, ctx);
                        }
                    }
                }
            }
            Self::Nothing => {}
        };
        primary_window_id
    }
}

/// If we're in an environment where we can't fulfill [`WindowBehaviorHint`], and the OS default
/// behavior isn't acceptable/reliable, e.g. Wayland doesn't allow windows to programmatically show
/// themselves, try this fallback behavior instead.
#[derive(Clone, Debug)]
enum WindowActivationFallbackBehavior {
    /// If the primary window picked to handle the URL is not the active one, send a native push
    /// notification.
    Notify { title: String, description: String },
    /// Create a new window to handle the URI.
    NewWindow {
        /// Close the former "primary window" as determined by [`get_primary_window`]. This should
        /// generally default to `false` to avoid closing a window with information that the user
        /// may still want. One exception is the Auth route where the old window just showed the
        /// auth page.
        replace_existing: bool,
    },
}

impl WindowActivationFallbackBehavior {
    /// Perform the desired window fallback behavior for the URI being handled. This may change the
    /// "primary window" if a new one has to be created. Return the new primary WindowId.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn resolve(self, primary_window_id: WindowId, ctx: &mut AppContext) -> Option<WindowId> {
        match self {
            WindowActivationFallbackBehavior::Notify { title, description } => {
                if ctx
                    .windows()
                    .active_window()
                    .is_some_and(|active_window| active_window == primary_window_id)
                {
                    return Some(primary_window_id);
                }
                if let Some(view_handle) = ctx
                    .views_of_type::<Workspace>(primary_window_id)
                    .filter(|views| !views.is_empty())
                    .map(|mut views| views.swap_remove(0))
                {
                    view_handle.update(ctx, |_, ctx| {
                        ctx.send_desktop_notification(
                            UserNotification::new(title, description, None),
                            |_, err, ctx| {
                                log::warn!(
                                    "Error showing URL intent notification on {:?}: {err:?}",
                                    ctx.window_id()
                                )
                            },
                        );
                    });
                }
                Some(primary_window_id)
            }
            WindowActivationFallbackBehavior::NewWindow { replace_existing } => {
                let new_window_id = open_new_window_get_handles(None, ctx).0;
                if replace_existing {
                    ctx.windows()
                        .close_window(primary_window_id, TerminationMode::Cancellable);
                }
                Some(new_window_id)
            }
        }
    }
}

/// Turn the launch config URL into a filename.
/// "/hello%20world" --> "hello world"
fn get_launch_config_path(path: &str) -> Option<String> {
    // Remove the leading slash before the filename.
    let (_, config_path) = path.split_once('/')?;

    // URL-decode the filename to recover spaces and
    // other non-URL-friendly characters
    let decoded = serde_urlencoded::from_str::<Vec<(String, String)>>(config_path).ok()?;

    // serde_urlencoded::from_str tries to find a vector key-value pairs,
    // so we'll take the first tuple in the vector...
    let decoded_config_name = decoded.first()?;

    // ... and read the first member of the tuple.
    let validated_path = validate_launch_config_path(decoded_config_name.0.as_str())?;

    // Finally, return the validated path.
    Some(validated_path.to_string())
}

/// Remove file extension, which consists of the last '.' in the filename
/// and whatever characters follow it.
fn remove_extension(full_path: &str) -> Option<&str> {
    let (no_extension, _) = full_path.rsplit_once('.')?;
    Some(no_extension)
}

/// Ensure that a path is relative and doesn't contain '/../',
/// to prevent launch config links from escaping the launch config directory.
fn validate_launch_config_path(path: &str) -> Option<&str> {
    if path.starts_with('/')
        || path.starts_with("../")
        || path.contains("/../")
        || path.ends_with("/..")
    {
        None
    } else {
        Some(path)
    }
}

/// Given a config path, find a matching launch config file
fn find_matching_config<'a>(
    target_path: &str,
    configs: &'a [LaunchConfig],
) -> Option<&'a LaunchConfig> {
    // first, try to match the exact filename.
    if let Some(matched_config) = find_matching_config_name(target_path, configs) {
        return Some(matched_config);
    }

    // next, try to match the filename without the extension
    let no_extension = remove_extension(target_path)?;
    find_matching_config_name(no_extension, configs)
}

/// Case-insensitive matching on the config's name
/// (field in the YAML file).
fn find_matching_config_name<'a>(
    target_name: &str,
    configs: &'a [LaunchConfig],
) -> Option<&'a LaunchConfig> {
    let target_name_lower = target_name.to_lowercase();
    configs
        .iter()
        .find(|&config| config.name.to_lowercase() == target_name_lower)
}

/// Extract the `path` query parameter, expanding a leading `~` to the
/// user's home directory.
fn parse_tab_path(url: &Url) -> Option<PathBuf> {
    let raw = url.query_pairs().find(|(k, _)| k == "path")?.1;
    Some(PathBuf::from(shellexpand::tilde(&raw).into_owned()))
}

/// Mnemos IDE: who built a link, which decides how far it is trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkOrigin {
    /// Came through the OS URL handler, so any web page or app can send it.
    External,
    /// Mnemos built it for a local launch (see [`local_launch_scheme`]).
    LocalLaunch,
}

/// Mnemos IDE: the scheme of the links Mnemos builds itself when the user
/// opens it on a folder: the macOS Finder Services, the Windows Explorer
/// verbs (`--new-tab` / `--new-window`, see [`local_launch_urls`]) and a
/// second launch forwarded to the running instance. It is never registered
/// with the OS, so a browser or another app cannot open one; such links skip
/// the confirmation and the home-directory rule that `mnemos://` links get.
pub fn local_launch_scheme() -> String {
    format!("{}-local", ChannelState::url_scheme())
}

/// Mnemos IDE: a [`local_launch_scheme`] link that opens a terminal at `path`
/// in a new tab, or a new window when `new_window`. `None` for a path that is
/// not valid UTF-8.
pub fn local_launch_url(new_window: bool, path: &Path) -> Option<Url> {
    let action = if new_window { "new_window" } else { "new_tab" };
    let mut url = Url::parse(&format!("{}://action/{action}", local_launch_scheme())).ok()?;
    url.query_pairs_mut().append_pair("path", path.to_str()?);
    Some(url)
}

/// Mnemos IDE: the local launch links for the `--new-tab` / `--new-window`
/// flags the Windows Explorer verbs pass.
pub fn local_launch_urls(args: &mnemos_cli::AppArgs) -> Vec<Url> {
    [(false, &args.new_tab), (true, &args.new_window)]
        .into_iter()
        .filter_map(|(new_window, path)| {
            local_launch_url(new_window, &explorer_path(path.as_deref()?))
        })
        .collect()
}

/// Explorer quotes a folder as `"%V"`, so the drive root `C:\` arrives as
/// `C:"` (a backslash before a quote escapes it). Undo that.
fn explorer_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let trimmed = raw.trim_end_matches('"');
    if trimmed.len() == raw.len() {
        return path.to_path_buf();
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        PathBuf::from(format!("{trimmed}\\"))
    } else {
        PathBuf::from(trimmed)
    }
}

#[derive(Debug)]
enum Action {
    NewTab,
    NewWindow,
    OpenRepo,
    CloudAgentSetup,
    NewCloudAgentConversation,
    NewAgentConversation,
}

impl Action {
    fn parse(url: &Url) -> Result<Self> {
        match url.path() {
            "/new_tab" => Ok(Self::NewTab),
            "/new_window" => Ok(Self::NewWindow),
            "/open-repo" => Ok(Self::OpenRepo),
            "/cloud_agent_setup" => Ok(Self::CloudAgentSetup),
            "/new_cloud_agent_conversation" => Ok(Self::NewCloudAgentConversation),
            "/new_agent_conversation" => Ok(Self::NewAgentConversation),
            _ => Err(anyhow!(
                "Received \"action\" intent with unexpected action: {}",
                url.path()
            )),
        }
    }

    fn handle(
        &self,
        origin: LinkOrigin,
        primary_window_id: Option<WindowId>,
        url: &Url,
        ctx: &mut AppContext,
    ) {
        #[cfg(target_os = "linux")]
        let primary_window_id = self.window_behavior_hint().resolve(primary_window_id, ctx);
        match self {
            Self::NewTab | Self::NewWindow => {
                let window_id = if let Self::NewTab = self {
                    primary_window_id
                } else {
                    None
                };
                let Some(path) = parse_tab_path(url) else {
                    log::warn!("Could not parse path to open a new tab/window");
                    return;
                };
                // Mnemos IDE: only links from outside must stay under home.
                let checked = match origin {
                    LinkOrigin::External => dirs::home_dir()
                        .ok_or_else(|| anyhow!("no home directory"))
                        .and_then(|home| validate_link_path(&path, &home)),
                    LinkOrigin::LocalLaunch => validate_local_launch_path(&path),
                };
                let path = match checked {
                    Ok(path) => path,
                    Err(err) => {
                        log::warn!("Refusing a new tab/window link: {err}");
                        return;
                    }
                };
                // Mnemos IDE: a link opens a terminal; it never runs the file.
                open_file(window_id, path, false, ctx);
            }
            Action::OpenRepo => {
                let window_id =
                    primary_window_id.or_else(|| Some(open_new_window_get_handles(None, ctx).0));

                let Some(window_id) = window_id else {
                    log::warn!("unable to determine window for open repo action");
                    return;
                };

                let Some(mut workspaces) = ctx.views_of_type::<Workspace>(window_id) else {
                    log::warn!("no workspace found in window {window_id} for open repo action");
                    return;
                };

                if let Some(workspace) = workspaces.pop() {
                    workspace.update(ctx, |workspace, ctx| {
                        workspace
                            .handle_action(&WorkspaceAction::OpenRepository { path: None }, ctx);
                    });
                } else {
                    log::warn!("no workspace views in window {window_id} for open repo action");
                }
            }
            Action::CloudAgentSetup => {
                let window_id =
                    primary_window_id.or_else(|| Some(open_new_window_get_handles(None, ctx).0));

                let Some(window_id) = window_id else {
                    log::warn!("unable to determine window for cloud agent setup action");
                    return;
                };

                let Some(mut workspaces) = ctx.views_of_type::<Workspace>(window_id) else {
                    log::warn!(
                        "no workspace found in window {window_id} for cloud agent setup action"
                    );
                    return;
                };

                if let Some(workspace) = workspaces.pop() {
                    workspace.update(ctx, |workspace, ctx| {
                        workspace.handle_action(&WorkspaceAction::OpenCloudAgentSetupGuide, ctx);
                    });
                } else {
                    log::warn!(
                        "no workspace views in window {window_id} for cloud agent setup action"
                    );
                }
            }
            Action::NewCloudAgentConversation => {
                let window_id =
                    primary_window_id.or_else(|| Some(open_new_window_get_handles(None, ctx).0));

                let Some(window_id) = window_id else {
                    log::warn!(
                        "unable to determine window for new cloud agent conversation action"
                    );
                    return;
                };

                let Some(mut workspaces) = ctx.views_of_type::<Workspace>(window_id) else {
                    log::warn!(
                        "no workspace found in window {window_id} for new cloud agent conversation action"
                    );
                    return;
                };

                if let Some(workspace) = workspaces.pop() {
                    workspace.update(ctx, |workspace, ctx| {
                        workspace.handle_action(&WorkspaceAction::AddAmbientAgentTab, ctx);
                    });
                } else {
                    log::warn!(
                        "no workspace views in window {window_id} for new cloud agent conversation action"
                    );
                }
            }
            Action::NewAgentConversation => {
                let window_id =
                    primary_window_id.or_else(|| Some(open_new_window_get_handles(None, ctx).0));

                let Some(window_id) = window_id else {
                    log::warn!("unable to determine window for new agent conversation action");
                    return;
                };

                let Some(workspace) = WorkspaceRegistry::as_ref(ctx).get(window_id, ctx) else {
                    log::warn!(
                        "no workspace found in window {window_id} for new agent conversation action"
                    );
                    return;
                };

                workspace.update(ctx, |workspace, ctx| {
                    workspace.handle_action(&WorkspaceAction::AddAgentTab, ctx);
                });
            }
        }
    }

    /// When handling this URI action, determine which window(s) should be focused.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn window_behavior_hint(&self) -> WindowBehaviorHint {
        use WindowBehaviorHint as W;
        match self {
            Self::OpenRepo
            | Self::CloudAgentSetup
            | Self::NewCloudAgentConversation
            | Self::NewAgentConversation => W::default(),
            Self::NewTab => W::ShowPrimaryWindow(WindowActivationFallbackBehavior::Notify {
                title: "New tab created".to_owned(),
                description: "Go to Mnemos to see your new tab.".to_owned(),
            }),
            Self::NewWindow => W::Nothing,
        }
    }
}

/// Handles all incoming urls. These urls are file urls, auth urls for login,
/// and team urls for opening team settings.
pub fn handle_incoming_uri(url: &Url, ctx: &mut AppContext) {
    // Non-dogfood builds must never log the full URL here: URLs routed to this
    // handler can carry secrets in their query string (for example, the
    // Firebase `refresh_token` on `mnemos://auth/desktop_redirect?...`). Log
    // only the non-sensitive components (scheme, host, path) on release
    // channels; dogfood builds retain the full URL for local debugging.
    safe_info!(
        safe: ("received url {}", safe_url_log_fields(url)),
        full: ("received url {:?}", &url)
    );

    // Pick the window that should be handling the URI.  This has some
    // additional logic to handle the hotkey window and there being no
    // currently-active window.
    let primary_window_id = get_primary_window(ctx.windows().frontmost_window_id(), ctx);

    // If we're running on a platform where we can spawn local TTYs,
    // check if this is a file:// URL and if so, spawn a new session
    // with an initial working directory based on the provided path.
    #[cfg(feature = "local_tty")]
    if url.scheme() == "file" {
        if let Ok(path) = url.to_file_path() {
            open_file(primary_window_id, path, true, ctx);
        }
        return;
    }

    if url.scheme() == local_launch_scheme() {
        handle_local_launch(primary_window_id, url, ctx);
        return;
    }

    match validate_custom_uri(url) {
        Ok(host) => {
            // Mnemos IDE: any web page can open a mnemos:// link, so links
            // that open a shell or run a launch configuration wait for the
            // user to confirm them in the app.
            if let Some(prompt) = confirmation_prompt(&host, url) {
                request_uri_confirmation(primary_window_id, url, prompt, ctx);
                return;
            }
            #[cfg(any(target_os = "linux", windows))]
            let primary_window_id = host.window_behavior_hint().resolve(primary_window_id, ctx);
            host.handle(primary_window_id, url, ctx);
        }
        Err(e) => {
            if let Some(window_id) = primary_window_id {
                ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                    let toast = DismissibleToast::error(format!("Custom URI is invalid: {e:?}"));
                    toast_stack.add_ephemeral_toast(toast, window_id, ctx);
                });
            }
            log::warn!("Custom URI is invalid: {e:?}");
        }
    }
}

/// Mnemos IDE: what to ask before handling `url`, or `None` when it needs no
/// confirmation. Opening a terminal at a path and running a saved launch
/// configuration (which can run commands) both ask.
fn confirmation_prompt(host: &UriHost, url: &Url) -> Option<String> {
    match host {
        UriHost::Launch => Some(format!(
            "A link wants to run the launch configuration \"{}\".",
            get_launch_config_path(url.path()).unwrap_or_default()
        )),
        UriHost::Action => match Action::parse(url) {
            Ok(Action::NewTab | Action::NewWindow) => Some(format!(
                "A link wants to open a terminal at {}.",
                parse_tab_path(url)
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
            )),
            _ => None,
        },
        _ => None,
    }
}

/// Mnemos IDE: open the terminal a [`local_launch_scheme`] link asks for.
/// Only `action/new_tab` and `action/new_window` exist in that scheme.
fn handle_local_launch(primary_window_id: Option<WindowId>, url: &Url, ctx: &mut AppContext) {
    match (url.host_str(), Action::parse(url)) {
        (Some("action"), Ok(action @ (Action::NewTab | Action::NewWindow))) => {
            action.handle(LinkOrigin::LocalLaunch, primary_window_id, url, ctx);
        }
        _ => log::warn!(
            "Ignoring an unknown local launch link: {}",
            safe_url_log_fields(url)
        ),
    }
}

/// Show `prompt` with a Continue link that hands `url` back to
/// [`handle_confirmed_uri`]. With no window open, one is opened to ask in.
fn request_uri_confirmation(
    window_id: Option<WindowId>,
    url: &Url,
    prompt: String,
    ctx: &mut AppContext,
) {
    let window_id = window_id.unwrap_or_else(|| open_new_window_get_handles(None, ctx).0);
    let link = ToastLink::new("Continue".to_owned())
        .with_onclick_action(WorkspaceAction::ConfirmIncomingUri(url.to_string()));
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        let toast = DismissibleToast::default(format!(
            "{prompt} Continue only if you opened this link yourself."
        ))
        .with_link(link);
        toast_stack.add_persistent_toast(toast, window_id, ctx);
    });
}

/// Handle a link the user confirmed from [`request_uri_confirmation`].
pub fn handle_confirmed_uri(url: &str, ctx: &mut AppContext) {
    let Ok(url) = Url::parse(url) else {
        return;
    };
    let primary_window_id = get_primary_window(ctx.windows().frontmost_window_id(), ctx);
    match validate_custom_uri(&url) {
        Ok(host) => {
            #[cfg(any(target_os = "linux", windows))]
            let primary_window_id = host.window_behavior_hint().resolve(primary_window_id, ctx);
            host.handle(primary_window_id, &url, ctx);
        }
        Err(e) => log::warn!("Custom URI is invalid: {e:?}"),
    }
}

/// Mnemos IDE: a path from a mnemos:// link must be an absolute path inside
/// the home directory (after resolving symlinks and `..`), never a UNC or
/// device path, and must exist. Returns the resolved path.
fn validate_link_path(path: &Path, home: &Path) -> Result<PathBuf> {
    let raw = path.as_os_str().to_string_lossy();
    ensure!(
        !raw.starts_with("\\\\") && !raw.starts_with("//"),
        "UNC and device paths are not allowed"
    );
    ensure!(path.is_absolute(), "the path must be absolute");
    let resolved = dunce::canonicalize(path)?;
    let home = dunce::canonicalize(home)?;
    ensure!(
        resolved.starts_with(&home),
        "the path is outside the home directory"
    );
    Ok(resolved)
}

/// Mnemos IDE: a path from a [`local_launch_scheme`] link (the user opened
/// Mnemos on that folder) must be absolute and exist; anywhere is fine,
/// network shares included. Returns the resolved path.
fn validate_local_launch_path(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "the path must be absolute");
    Ok(dunce::canonicalize(path)?)
}

/// Gets the primary window ID, and returns `None` if it does not exist.
/// A primary window is the foregrounded window, or one of the inactive non-quake windows.
/// A closed quake window is not counted.
fn get_primary_window(
    active_window_id: Option<WindowId>,
    ctx: &mut AppContext,
) -> Option<WindowId> {
    // Return quake mode window if it's open
    if let Some(window_id) = quake_mode_window_id()
        .filter(|window_id| quake_mode_window_is_open() && ctx.is_window_open(*window_id))
    {
        return Some(window_id);
    }

    // Otherwise, return active window
    if let Some(window_id) = active_window_id {
        return Some(window_id);
    }

    let mut non_quake_mode_windows = ctx
        .window_ids()
        .filter(|window_id| Some(*window_id) != quake_mode_window_id());

    // There's no active window, return first non-quake mode window or None if none exist.
    non_quake_mode_windows.next()
}

/// Handle an incoming `file://` URL.
/// * Markdown files are opened as notebook panes.
/// * For directories, open a new session at the directory path.
/// * For other files, open a new session at the parent directory path, then possibly execute the
///   file.
///
/// Mnemos IDE: `run_files` is false for paths from a mnemos:// link, which
/// open a terminal next to a file but never run it.
fn open_file(window_id: Option<WindowId>, path: PathBuf, run_files: bool, ctx: &mut AppContext) {
    let primary_window_and_view = window_id.and_then(|window_id| {
        ctx.root_view_id(window_id)
            .map(|view_id| (window_id, view_id))
    });

    if is_markdown_file(&path) {
        if let Some((primary_window_id, root_view_id)) = primary_window_and_view {
            ctx.dispatch_action(
                primary_window_id,
                &[root_view_id],
                "root_view:add_file_pane",
                &path,
                log::Level::Info,
            );
        } else {
            ctx.dispatch_global_action("root_view:open_new_with_file_notebook", &path);
        }
    } else if path.is_file() && is_file_openable_in_mnemos(&path).is_some() {
        #[cfg(feature = "local_fs")]
        {
            use crate::code::editor_management::CodeSource;
            use crate::root_view::{open_new_with_workspace_source, NewWorkspaceSource};
            use crate::util::{
                file::external_editor::EditorSettings,
                openable_file_type::resolve_file_target_to_open_in_mnemos,
            };

            // Open text/code files in Mnemos's code editor, respecting the user's layout preference.
            let editor_settings = EditorSettings::as_ref(ctx);
            let target = resolve_file_target_to_open_in_mnemos(&path, editor_settings, None);

            let window_id = if let Some((wid, _)) = primary_window_and_view {
                wid
            } else {
                open_new_with_workspace_source(
                    NewWorkspaceSource::Session {
                        options: Box::default(),
                    },
                    ctx,
                )
                .0
            };

            ctx.windows().show_window_and_focus_app(window_id);

            if let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id) {
                if let Some(workspace) = workspaces.into_iter().next() {
                    workspace.update(ctx, |workspace, ctx| {
                        let source = CodeSource::Finder { path: path.clone() };
                        workspace.open_file_with_target(path, target, None, source, ctx);
                    });
                }
            }
        }
    } else {
        let directory_path = if path.is_file() {
            match path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => PathBuf::new(),
            }
        } else {
            path.clone()
        };

        if let Some((primary_window_id, root_view_id)) = primary_window_and_view {
            ctx.dispatch_action(
                primary_window_id,
                &[root_view_id],
                "root_view:add_session_at_path",
                &directory_path,
                log::Level::Info,
            );

            // Run command after session has been added
            if run_files && path.is_file() {
                if let Some(path_str) = path.to_str() {
                    execute_file(primary_window_id, path_str, ctx);
                }
            }
        } else {
            let open_path = OpenPath {
                path: directory_path,
            };
            ctx.dispatch_global_action("root_view:open_new_from_path", &open_path);

            // Run command after window has been added
            if run_files && path.is_file() {
                let active_window_id = ctx.windows().active_window();
                if let Some(primary_window_id) = get_primary_window(active_window_id, ctx) {
                    if let Some(path_str) = path.to_str() {
                        execute_file(primary_window_id, path_str, ctx);
                    }
                }
            }
        }

        send_telemetry_from_app_ctx!(TelemetryEvent::OpenNewSessionFromFilePath, ctx);
    }
}

fn execute_file(window_id: WindowId, path_str: &str, ctx: &mut AppContext) {
    active_terminal_in_window(window_id, ctx, |term, t_ctx| {
        let path_str = term.shell_family(t_ctx).shell_escape(path_str);
        term.input().update(t_ctx, |input, i_ctx| {
            input.set_pending_command(&path_str, i_ctx);
        })
    });

    send_telemetry_from_app_ctx!(TelemetryEvent::CommandFileRun, ctx);
}

fn open_window_with_action(active_window_id: Option<WindowId>, action: &str, ctx: &mut AppContext) {
    if let Some(primary_window_id) = active_window_id {
        // Dispatch action to primary window
        if let Some(root_view_id) = ctx.root_view_id(primary_window_id) {
            ctx.dispatch_action(
                primary_window_id,
                &[root_view_id],
                action,
                &(),
                log::Level::Info,
            );
        }
    } else {
        log::warn!("no primary window id to dispatch action to");

        // Open a new window and dispatch action there
        ctx.dispatch_global_action("root_view:open_new", &());
        // TODO: Note we cannot just dispatch here as it will be a no-op.
        // Need to send a callback once window is fully open.
    }
}

/// Helper function to dispatch an action to an existing window
/// or create new window if none exist.
fn dispatch_action_in_new_or_existing_window<T: 'static>(
    primary_window_id: Option<WindowId>,
    existing_window_action: &str,
    new_window_action: &str,
    args: &T,
    ctx: &mut AppContext,
) {
    let primary_window_and_view = primary_window_id.and_then(|window_id| {
        ctx.root_view_id(window_id)
            .map(|view_id| (window_id, view_id))
    });

    if let Some((primary_window_id, root_view_id)) = primary_window_and_view {
        ctx.dispatch_action(
            primary_window_id,
            &[root_view_id],
            existing_window_action,
            args,
            log::Level::Info,
        );
    } else {
        ctx.dispatch_global_action(new_window_action, args);
    }
}

/// Validates an incoming custom URI for security and returns the host.
fn validate_custom_uri(url: &Url) -> Result<UriHost> {
    // For now the only scheme we support is `[scheme_name]://[host_str]/...
    // Ignore all other urls that don't match this scheme for security purposes.
    if url.scheme() != ChannelState::url_scheme() {
        return Err(anyhow!(
            "Received url with unexpected scheme: {} ",
            url.scheme()
        ));
    }

    let host_str = url
        .host_str()
        .ok_or_else(|| anyhow!("Received url with no host str"))?;

    let host = UriHost::from_str(host_str)?;

    // Check if this host is allowed to have arbitrary paths.
    let host_allows_arbitrary_path = match host {
        UriHost::Action
        | UriHost::Launch
        | UriHost::SharedSession
        | UriHost::Conversation
        | UriHost::Drive
        | UriHost::Team
        | UriHost::Settings
        | UriHost::Mcp
        | UriHost::Agents => true,
        // Auth and Home only allow the desktop redirect path
        UriHost::Auth | UriHost::Home => false,
    };

    ensure!(
        host_allows_arbitrary_path || url.path() == DESKTOP_REDIRECT_URI_PATH,
        "Received url with unexpected path: {} ",
        url.path()
    );

    Ok(host)
}

/// Formats the non-sensitive components of an incoming URL for logging on
/// release channels.
///
/// The returned string contains only the URL's scheme, host, and path — never
/// its query string, fragment, or userinfo component. URLs that reach
/// [`handle_incoming_uri`] can carry secrets in their query (for example, the
/// Firebase refresh token in `mnemos://auth/desktop_redirect?refresh_token=...`),
/// so this helper exists to give [`safe_info!`] a redacted representation that
/// still preserves enough signal for triage.
///
/// `url.host_str()` can return `None` for schemes that don't require a host
/// (e.g. some `file://` URLs on certain platforms); the literal `-` is used
/// as a placeholder in that case so the formatter never panics.
fn safe_url_log_fields(url: &Url) -> String {
    format!(
        "scheme={} host={} path={}",
        url.scheme(),
        url.host_str().unwrap_or("-"),
        url.path(),
    )
}

#[cfg(test)]
#[path = "uri_test.rs"]
mod tests;

/// `mnemos://agents/focus?id=<session id>&project=<name>`: find the agent by
/// its Claude session id, else the most recent one in that project, and focus
/// its tab. An unknown agent is a no-op and a log line.
fn focus_agent_from_url(url: &Url, ctx: &mut AppContext) {
    use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
    use crate::workspace::WorkspaceRegistry;

    if url.path().trim_matches('/') != "focus" {
        log::warn!("mnemos://agents: unknown path {}", url.path());
        return;
    }
    let query: HashMap<_, _> = url.query_pairs().collect();
    let id = query.get("id").map(|s| s.to_string()).filter(|s| !s.is_empty());
    let project = query
        .get("project")
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let target = {
        let model = CLIAgentSessionsModel::as_ref(ctx);
        let by_id = id.as_ref().and_then(|id| {
            model
                .iter()
                .find(|(_, s)| s.session_context.session_id.as_deref() == Some(id.as_str()))
                .map(|(tid, _)| tid)
        });
        by_id.or_else(|| {
            let project = project.as_ref()?;
            model
                .iter()
                .filter(|(_, s)| {
                    let c = &s.session_context;
                    c.project.as_deref() == Some(project.as_str())
                        || c.cwd.as_deref().and_then(|cwd| {
                            cwd.rsplit(['/', '\\']).find(|p| !p.is_empty())
                        }) == Some(project.as_str())
                })
                .max_by_key(|(_, s)| s.session_context.last_event_at)
                .map(|(tid, _)| tid)
        })
    };
    let Some(target) = target else {
        log::info!("mnemos://agents/focus: no running agent for id={id:?} project={project:?}");
        return;
    };
    let bits = target.as_usize() as u64;
    let workspaces = WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx);
    for (window_id, workspace) in workspaces {
        if workspace.update(ctx, |ws, wctx| ws.focus_cli_agent(bits, wctx)) {
            ctx.windows().show_window_and_focus_app(window_id);
            return;
        }
    }
    log::info!("mnemos://agents/focus: no window hosts agent terminal {target}");
}
