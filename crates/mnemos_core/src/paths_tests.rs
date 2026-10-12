use dirs::home_dir;

use super::*;

/// Mnemos IDE: when a workspace test run builds this crate with `test-util`, the
/// `Local` default points the config and data dirs at a per-process temp home
/// (see [`ChannelState::unit_test_home_dir`]), and the real-home expectations
/// below do not apply.
fn unit_test_home() -> Option<PathBuf> {
    #[cfg(feature = "test-util")]
    if let Some(home) = ChannelState::unit_test_home_dir() {
        return Some(home);
    }
    None
}

#[test]
fn test_unit_test_home_is_outside_the_real_home() {
    let Some(test_home) = unit_test_home() else {
        return;
    };
    assert!(test_home.starts_with(std::env::temp_dir()));
    assert!(config_local_dir().starts_with(&test_home));
    assert!(data_dir().starts_with(&test_home));
}

#[test]
fn test_data_dir_path() {
    if unit_test_home().is_some() {
        return;
    }
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Local.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(data_dir(), home_dir.join(".mnemos-local"));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(data_dir(), home_dir.join(".local/share/mnemos-local"));
        } else if #[cfg(windows)] {
            assert_eq!(data_dir(), home_dir.join("AppData\\Roaming\\mnemos\\MnemosLocal\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_config_local_dir_path() {
    if unit_test_home().is_some() {
        return;
    }
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Local.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(config_local_dir(), home_dir.join(".mnemos-local"));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(config_local_dir(), home_dir.join(".config/mnemos-local"));
        } else if #[cfg(windows)] {
            assert_eq!(config_local_dir(), home_dir.join("AppData\\Local\\mnemos\\MnemosLocal\\config"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_mnemos_home_config_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    let expected_dir_name = match ChannelState::data_profile() {
        Some(data_profile) => format!(".mnemos-local-{data_profile}"),
        None => ".mnemos-local".to_string(),
    };

    assert_eq!(
        mnemos_home_config_dir(),
        Some(home_dir.join(expected_dir_name))
    );
}

#[test]
fn test_mnemos_home_skills_and_mcp_paths() {
    let Some(config_dir) = mnemos_home_config_dir() else {
        panic!("Should be able to compute Mnemos home config directory");
    };

    assert_eq!(mnemos_home_skills_dir(), Some(config_dir.join("skills")));
    assert_eq!(
        mnemos_home_mcp_config_file_path(),
        Some(config_dir.join(".mcp.json"))
    );
}
#[test]
fn test_cache_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    // ChannelState, by default, is configured for Channel::Local.
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(cache_dir(), home_dir.join("Library/Application Support/dev.mnemos.MnemosLocal"));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(cache_dir(), home_dir.join(".cache/mnemos-local"));
        } else if #[cfg(windows)] {
            assert_eq!(cache_dir(), home_dir.join("AppData\\Local\\mnemos\\MnemosLocal\\cache"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_state_dir_path() {
    let home_dir = home_dir().expect("Should be able to compute home directory");
    cfg_if::cfg_if! {
        // ChannelState, by default, is configured for Channel::Local.
        if #[cfg(target_os = "macos")] {
            assert_eq!(state_dir(), home_dir.join("Library/Application Support/dev.mnemos.MnemosLocal"));
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(state_dir(), home_dir.join(".local/state/mnemos-local"));
        } else if #[cfg(windows)] {
            assert_eq!(state_dir(), home_dir.join("AppData\\Local\\mnemos\\MnemosLocal\\data"));
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_mnemos_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "mnemos", "Mnemos"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "dev.mnemos.Mnemos");
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(project_dirs.project_path(), "mnemos");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "mnemos\\Mnemos");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_mnemos_dev_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "mnemos", "MnemosDev"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "dev.mnemos.MnemosDev");
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(project_dirs.project_path(), "mnemos-dev");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "mnemos\\MnemosDev");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}

#[test]
fn test_project_path_for_oss_app_id() {
    let project_dirs = project_dirs_for_app_id(AppId::new("dev", "mnemos", "MnemosLocal"), None)
        .expect("should be able to compute project dirs");
    cfg_if::cfg_if! {
        if #[cfg(target_os = "macos")] {
            assert_eq!(project_dirs.project_path(), "dev.mnemos.MnemosLocal");
        } else if #[cfg(target_os = "linux")] {
            assert_eq!(project_dirs.project_path(), "mnemos-local");
        } else if #[cfg(windows)] {
            assert_eq!(project_dirs.project_path(), "mnemos\\MnemosLocal");
        } else {
            unimplemented!("Need to update tests for current platform!");
        }
    }
}
