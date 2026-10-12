use mnemos_core::{context_flag::ContextFlag, features::FeatureFlag};
use mnemos_ui::ViewContext;

use super::{
    ContentItem, ContentSectionData, FeatureItem, FeatureSection, FeatureSectionData,
    ResourceCenterMainView, Section, Tip, TipAction, TipHint,
};

pub fn sections(ctx: &mut ViewContext<ResourceCenterMainView>) -> Vec<Section> {
    let mut sections = vec![Section::Changelog()];

    if FeatureFlag::AvatarInTabBar.is_enabled() {
        return sections;
    }

    let get_started = FeatureSectionData {
        section_name: FeatureSection::GettingStarted,
        items: vec![
            FeatureItem::new(
                "Create your first block",
                "Run a command to see your command and output grouped.",
                Tip::Hint(TipHint::CreateBlock),
                ctx,
            ),
            FeatureItem::new(
                "Navigate blocks",
                "Click to select a block and navigate with arrow keys.",
                Tip::Hint(TipHint::BlockSelect),
                ctx,
            ),
            FeatureItem::new(
                "Take an action on block",
                "Right click on a block to copy/paste, share, more.",
                Tip::Hint(TipHint::BlockAction),
                ctx,
            ),
            FeatureItem::new(
                "Open command palette",
                "Access all of Mnemos via the keyboard.",
                Tip::Action(TipAction::CommandPalette),
                ctx,
            ),
            FeatureItem::new(
                "Set your theme",
                "Make Mnemos your own by choosing a theme.",
                Tip::Action(TipAction::ThemePicker),
                ctx,
            ),
        ],
    };
    sections.push(Section::Feature(get_started));

    let maximize_mnemos = FeatureSectionData {
        section_name: FeatureSection::MaximizeMnemos,
        items: maximize_mnemos_items(ctx),
    };
    sections.push(Section::Feature(maximize_mnemos));

    let advanced_setup = ContentSectionData {
        section_name: FeatureSection::AdvancedSetup,
        items: vec![
            ContentItem {
                title: "Use your custom prompt",
                description: "Set up Mnemos to honor your PS1 setting",
                url: crate::util::links::MNEMOS_DOCS_URL,
                button_label: "View documentation",
            },
            ContentItem {
                title: "Integrate Mnemos with your IDE",
                description: "Configure Mnemos to launch from your most used development tools",
                url: crate::util::links::MNEMOS_DOCS_URL,
                button_label: "View documentation",
            },
        ],
    };
    sections.push(Section::Content(advanced_setup));

    sections
}

fn maximize_mnemos_items(ctx: &mut ViewContext<ResourceCenterMainView>) -> Vec<FeatureItem> {
    let mut maximize_mnemos_items = vec![];

    maximize_mnemos_items.push(FeatureItem::new(
        "Command search",
        "Find and run previously executed commands, workflows, and more.",
        Tip::Action(TipAction::CommandSearch),
        ctx,
    ));

    maximize_mnemos_items.push(FeatureItem::new(
        "AI command search",
        "Generate shell commands with natural language.",
        Tip::Action(TipAction::AiCommandSearch),
        ctx,
    ));

    if ContextFlag::CreateNewSession.is_enabled() {
        maximize_mnemos_items.push(FeatureItem::new(
            "Split panes",
            "Split tabs into multiple panes to make your ideal layout.",
            Tip::Action(TipAction::SplitPane),
            ctx,
        ));
    }

    if ContextFlag::LaunchConfigurations.is_enabled() {
        maximize_mnemos_items.push(FeatureItem::new(
            "Launch configuration",
            "Save your current configuration of windows, tabs, and panes.",
            Tip::Action(TipAction::SaveNewLaunchConfig),
            ctx,
        ));
    }

    maximize_mnemos_items
}
