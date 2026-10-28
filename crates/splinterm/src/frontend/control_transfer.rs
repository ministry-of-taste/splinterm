//! What a pane says while an agent asks to control it (Plan 0067 M4).
//!
//! The request reaches the Window only as a transfer ID, with no name for
//! who asks, so the notice says "an agent". Its keys are the ones the
//! keymap binds to `control.accept-transfer` and `control.deny-transfer`,
//! so a remapped keymap shows its own keys and an unbound action says so.

use crate::keymap::{ActionId, ResolvedKeymap};

use super::shortcut_chips;

/// The notice a pane shows while an agent asks to control it.
pub(crate) const CONTROL_TRANSFER_NOTICE: &str = "An agent asks to control this pane";

/// The keys that answer a pending control transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ControlTransferKeys {
    /// `(key, verb)` pairs for the notice's bottom edge, as the popup
    /// legends write keys (`ctrl shift y`). An unbound action reads
    /// `accept unbound`; with neither bound, the command palette, which
    /// lists both while a transfer is pending, follows when it is bound.
    pub(crate) legend: Vec<(String, &'static str)>,
    /// The same keys for the Window title, as the keymap writes them:
    /// `Ctrl+Shift+Y accept / Ctrl+Shift+N deny`.
    pub(crate) title: String,
}

impl ControlTransferKeys {
    /// The legend as the box painter takes it.
    #[must_use]
    pub(crate) fn legend_pairs(&self) -> Vec<(&str, &str)> {
        self.legend
            .iter()
            .map(|(key, verb)| (key.as_str(), *verb))
            .collect()
    }
}

fn legend_key(display: &str) -> String {
    shortcut_chips(display).join(" ")
}

/// The keys `keymap` answers a pending control transfer with.
#[must_use]
pub(crate) fn control_transfer_keys(keymap: &ResolvedKeymap) -> ControlTransferKeys {
    let mut legend = Vec::new();
    let mut title = Vec::new();
    let mut bound = false;
    for (action, verb) in [
        (ActionId::AcceptControlTransfer, "accept"),
        (ActionId::DenyControlTransfer, "deny"),
    ] {
        let display = keymap.primary_shortcut(action);
        if display.is_empty() {
            legend.push((verb.to_owned(), "unbound"));
            title.push(format!("{verb} unbound"));
        } else {
            bound = true;
            legend.push((legend_key(display), verb));
            title.push(format!("{display} {verb}"));
        }
    }
    let palette = keymap.primary_shortcut(ActionId::CommandPalette);
    if !bound && !palette.is_empty() {
        legend.push((legend_key(palette), "commands"));
        title.push(format!("{palette} commands"));
    }
    ControlTransferKeys {
        legend,
        title: title.join(" / "),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::keymap::{KeymapProfile, built_in_keymap, resolve_keymap_text};

    fn keymap(overlay: &str) -> ResolvedKeymap {
        resolve_keymap_text(
            KeymapProfile::Splinterm,
            &format!("version = 1\n{overlay}"),
            Path::new("keybindings.toml"),
        )
        .unwrap()
        .keymap
    }

    #[test]
    fn the_notice_names_the_default_keys() {
        let keys = control_transfer_keys(&built_in_keymap(KeymapProfile::Splinterm));
        assert_eq!(
            keys.legend_pairs(),
            [("ctrl shift y", "accept"), ("ctrl shift n", "deny")]
        );
        assert_eq!(keys.title, "Ctrl+Shift+Y accept / Ctrl+Shift+N deny");
        assert_eq!(
            control_transfer_keys(&built_in_keymap(KeymapProfile::OmarchyTmux)),
            keys
        );
    }

    #[test]
    fn the_notice_names_remapped_keys() {
        let keys = control_transfer_keys(&keymap(
            "[[unbind]]\nsequence = [\"Ctrl+Shift+Y\"]\n\
             [[binding]]\nsequence = [\"Ctrl+Alt+A\"]\naction = \"control.accept-transfer\"\n",
        ));
        assert_eq!(
            keys.legend_pairs(),
            [("ctrl alt a", "accept"), ("ctrl shift n", "deny")]
        );
        assert_eq!(keys.title, "Ctrl+Alt+A accept / Ctrl+Shift+N deny");
    }

    #[test]
    fn an_unbound_answer_says_so() {
        let deny_only =
            control_transfer_keys(&keymap("[[unbind]]\nsequence = [\"Ctrl+Shift+Y\"]\n"));
        assert_eq!(
            deny_only.legend_pairs(),
            [("accept", "unbound"), ("ctrl shift n", "deny")]
        );
        assert_eq!(deny_only.title, "accept unbound / Ctrl+Shift+N deny");

        // With neither bound, the command palette lists both answers.
        let neither = control_transfer_keys(&keymap(
            "[[unbind]]\nsequence = [\"Ctrl+Shift+Y\"]\n\
             [[unbind]]\nsequence = [\"Ctrl+Shift+N\"]\n",
        ));
        let palette = built_in_keymap(KeymapProfile::Splinterm)
            .primary_shortcut(ActionId::CommandPalette)
            .to_owned();
        assert!(!palette.is_empty());
        assert_eq!(
            neither.legend_pairs(),
            [
                ("accept", "unbound"),
                ("deny", "unbound"),
                (legend_key(&palette).as_str(), "commands"),
            ]
        );
        assert_eq!(
            neither.title,
            format!("accept unbound / deny unbound / {palette} commands")
        );
    }
}
