use super::*;

#[test]
fn test_package_name() {
    assert_eq!(package_name(Channel::Stable), "mnemos-terminal");
    assert_eq!(package_name(Channel::Dev), "mnemos-terminal-dev");
}
