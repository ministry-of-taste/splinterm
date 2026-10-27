//! Pure presentation of explicitly reported agent status.
//!
//! Status is display data from the daemon, never authority and never inferred
//! from terminal content. Every state is distinguishable by its glyph's shape
//! and by text alone. Only a fresh working status moves: its Braille spinner
//! steps at most four times a second, in a focused Window, without reduced
//! motion, and otherwise holds its first frame. The free-text detail is
//! Explorer-only and never reaches Dojo tabs.

use std::fmt::Write as _;
use std::time::Duration;

use splinterm_protocol::AgentState;
use unicode_width::UnicodeWidthChar;

use crate::config::StatusGlyphs;
use crate::navigation_projection::NavigationAgentStatus;
use crate::renderer::{ChromeIcon, ChromeSource};
use crate::theme::ChromeTint;

/// Display priority, highest first. Stale statuses rank below every fresh one.
const PRIORITY: [AgentState; 5] = [
    AgentState::NeedsInput,
    AgentState::Error,
    AgentState::Working,
    AgentState::Done,
    AgentState::Idle,
];

/// Marker after an ASCII glyph for a stale status.
pub(crate) const ASCII_STALE_MARKER: &str = "~";

/// Explorer and accessibility text when endpoint data is stale or disconnected.
pub(crate) const UNAVAILABLE_TEXT: &str = "Agent status unavailable";

/// The working state's Braille spinner frames, in order. The renderer draws
/// Braille cells itself, so the spinner needs neither the embedded icon face
/// nor a font and is the same in both glyph styles.
pub(crate) const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// One frame of the working spinner. [`Self::STATIC`], the first frame, is
/// what a stale, unfocused, or reduced-motion working status shows.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct AgentSpinner(usize);

impl AgentSpinner {
    pub(crate) const STATIC: Self = Self(0);

    /// The shortest time between two frames: at most four frames a second.
    pub(crate) const STEP: Duration = Duration::from_millis(250);

    pub(crate) const fn glyph(self) -> char {
        SPINNER_FRAMES[self.0 % SPINNER_FRAMES.len()]
    }

    #[must_use]
    pub(crate) const fn next(self) -> Self {
        Self((self.0 + 1) % SPINNER_FRAMES.len())
    }

    pub(crate) const fn is_static(self) -> bool {
        self.0 == 0
    }

    /// Whether the spinner moves: only in a focused Window, without reduced
    /// motion, while a fresh working status is shown.
    pub(crate) const fn animates(reduced_motion: bool, focused: bool, working_shown: bool) -> bool {
        !reduced_motion && focused && working_shown
    }

    /// The frame to show after a timer tick, or `None` when it stays. A
    /// moving spinner steps one frame once [`Self::STEP`] has passed since the
    /// last step; a stopped one returns to its first frame.
    #[must_use]
    pub(crate) fn tick(self, animates: bool, since_last_step: Duration) -> Option<Self> {
        if animates {
            (since_last_step >= Self::STEP).then(|| self.next())
        } else {
            (!self.is_static()).then_some(Self::STATIC)
        }
    }

    /// How long until the next frame is due while the spinner moves.
    pub(crate) fn remaining(animates: bool, since_last_step: Duration) -> Option<Duration> {
        animates.then(|| Self::STEP.saturating_sub(since_last_step))
    }
}

/// Whether any of `statuses` is a fresh working status, the only one that
/// shows a moving spinner.
pub(crate) fn shows_working<'a>(
    statuses: impl IntoIterator<Item = &'a NavigationAgentStatus>,
) -> bool {
    statuses
        .into_iter()
        .any(|status| status.state == AgentState::Working && !status.stale)
}

/// How status is drawn. Every style is visual only: accessible text and
/// search always use state words. Each glyph is tinted by its status role
/// (see [`state_tint`]); the painter decides whether tints show.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AgentGlyphs {
    /// Embedded Tabler icons, two cells each; a clock icon marks stale. The
    /// working spinner takes the first cell of its two-cell slot.
    #[default]
    Icons,
    /// One character per state, ASCII except the Braille working spinner;
    /// `~` marks stale. Used when the embedded icon face cannot be loaded,
    /// never a replacement glyph.
    Ascii,
    /// `[explorer] status-glyphs=dots`: one small geometric shape per state
    /// from the chrome font, each a different shape (see [`Self::dot`]), the
    /// Braille working spinner, and `~` for stale.
    Dots,
}

impl AgentGlyphs {
    /// Dots when asked for; otherwise icons, or ASCII when the embedded icon
    /// face cannot be loaded.
    pub(crate) const fn select(icons_available: bool, style: StatusGlyphs) -> Self {
        match (style, icons_available) {
            (StatusGlyphs::Dots, _) => Self::Dots,
            (StatusGlyphs::Icons, true) => Self::Icons,
            (StatusGlyphs::Icons, false) => Self::Ascii,
        }
    }

    /// The dot of every state but working, which is the spinner: a diamond
    /// asks for input, a triangle warns of an error, a filled square is done
    /// and a hollow square is idle. Circles stay the Explorer's live dot.
    pub(crate) const fn dot(state: AgentState) -> Option<&'static str> {
        match state {
            AgentState::Working => None,
            AgentState::NeedsInput => Some("◆"),
            AgentState::Error => Some("▲"),
            AgentState::Done => Some("■"),
            AgentState::Idle => Some("□"),
        }
    }

    /// The icon of every state but working, which is the spinner.
    pub(crate) const fn icon(state: AgentState) -> Option<ChromeIcon> {
        match state {
            AgentState::Working => None,
            AgentState::NeedsInput => Some(ChromeIcon::MessageQuestion),
            AgentState::Done => Some(ChromeIcon::CircleCheck),
            AgentState::Error => Some(ChromeIcon::AlertCircle),
            AgentState::Idle => Some(ChromeIcon::Point),
        }
    }

    /// The ASCII glyph of every state but working, which is the spinner.
    pub(crate) const fn ascii(state: AgentState) -> Option<&'static str> {
        match state {
            AgentState::Working => None,
            AgentState::NeedsInput => Some("?"),
            AgentState::Done => Some("+"),
            AgentState::Error => Some("!"),
            AgentState::Idle => Some("-"),
        }
    }

    /// Cells of the glyph slot.
    const fn glyph_cells(self) -> usize {
        match self {
            Self::Icons => ChromeIcon::CELLS as usize,
            Self::Ascii | Self::Dots => 1,
        }
    }

    /// Cells of the stale marker slot.
    const fn stale_cells(self) -> usize {
        match self {
            Self::Icons => ChromeIcon::CELLS as usize,
            Self::Ascii | Self::Dots => 1,
        }
    }

    /// Cells of the fixed Explorer column: the glyph and the stale slot.
    pub(crate) const fn column_cells(self) -> usize {
        self.glyph_cells() + self.stale_cells()
    }

    /// The glyph alone, without padding, tinted by its status role: a
    /// working spinner is one cell.
    fn push_glyph(
        self,
        source: &mut ChromeSource,
        state: AgentState,
        is_stale: bool,
        spinner: AgentSpinner,
    ) {
        source.push_tinted(state_tint(state, is_stale), |source| {
            match (
                Self::icon(state),
                Self::ascii(state),
                Self::dot(state),
                self,
            ) {
                (Some(icon), _, _, Self::Icons) => {
                    source.push_icon(icon);
                }
                (_, Some(text), _, Self::Ascii) | (_, _, Some(text), Self::Dots) => {
                    source.push_str(text);
                }
                _ => {
                    source.push_str(spinner.glyph().encode_utf8(&mut [0; 4]));
                }
            }
        });
    }

    fn push_stale(self, source: &mut ChromeSource) {
        source.push_tinted(ChromeTint::StatusStale, |source| {
            match self {
                Self::Icons => source.push_icon(ChromeIcon::Clock),
                Self::Ascii | Self::Dots => source.push_str(ASCII_STALE_MARKER),
            };
        });
    }

    /// The fixed-width column for one state, or blank cells without one. A
    /// stale working status holds the spinner's first frame.
    fn column(self, state: Option<(AgentState, bool)>, spinner: AgentSpinner) -> ChromeSource {
        let mut column = ChromeSource::default();
        match state {
            None => {
                column.push_str(&" ".repeat(self.column_cells()));
            }
            Some((state, is_stale)) => {
                let spinner = if is_stale {
                    AgentSpinner::STATIC
                } else {
                    spinner
                };
                self.push_glyph(&mut column, state, is_stale, spinner);
                let padding = self.glyph_cells().saturating_sub(column.cells());
                column.push_str(&" ".repeat(padding));
                if is_stale {
                    self.push_stale(&mut column);
                } else {
                    column.push_str(&" ".repeat(self.stale_cells()));
                }
            }
        }
        column
    }
}

/// herdr's mapping of a status onto its role: working is a warning, needs
/// input and error are danger (told apart by their glyphs), done is info,
/// idle is success, and any stale status is faint.
pub(crate) const fn state_tint(state: AgentState, is_stale: bool) -> ChromeTint {
    if is_stale {
        return ChromeTint::StatusStale;
    }
    match state {
        AgentState::Working => ChromeTint::StatusWorking,
        AgentState::NeedsInput => ChromeTint::StatusNeedsInput,
        AgentState::Error => ChromeTint::StatusError,
        AgentState::Done => ChromeTint::StatusDone,
        AgentState::Idle => ChromeTint::StatusIdle,
    }
}

pub(crate) const fn state_text(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "Working",
        AgentState::NeedsInput => "Needs input",
        AgentState::Done => "Done",
        AgentState::Error => "Error",
        AgentState::Idle => "Idle",
    }
}

const fn priority(state: AgentState) -> usize {
    match state {
        AgentState::NeedsInput => 0,
        AgentState::Error => 1,
        AgentState::Working => 2,
        AgentState::Done => 3,
        AgentState::Idle => 4,
    }
}

/// Lower is more urgent: every fresh state outranks every stale one.
const fn rank(status: &NavigationAgentStatus) -> usize {
    priority(status.state) + if status.stale { PRIORITY.len() } else { 0 }
}

/// One Splint row's status text, e.g. "Needs input · waiting for approval" or
/// "Working · stale (no report for 2 min)".
pub(crate) fn splint_text(status: &NavigationAgentStatus) -> String {
    let mut text = state_text(status.state).to_owned();
    if status.stale {
        let minutes = status.stale_minutes.unwrap_or(0).max(1);
        let _ = write!(text, " · stale (no report for {minutes} min)");
    }
    if let Some(detail) = &status.detail {
        text.push_str(" · ");
        text.push_str(detail);
    }
    text
}

/// One Splint's status for the Explorer footer's short status line, e.g.
/// "needs input · waiting for approval" or "working · stale 2 min". The
/// detail is the agent's own text and keeps its case.
pub(crate) fn splint_brief(status: &NavigationAgentStatus) -> String {
    let mut text = state_text(status.state).to_lowercase();
    if status.stale {
        let minutes = status.stale_minutes.unwrap_or(0).max(1);
        let _ = write!(text, " · stale {minutes} min");
    }
    if let Some(detail) = &status.detail {
        text.push_str(" · ");
        text.push_str(detail);
    }
    text
}

/// Words Explorer search matches for a Splint, e.g. `needs input needs_input`.
pub(crate) fn search_terms(status: &NavigationAgentStatus) -> String {
    let state = state_text(status.state).to_lowercase();
    let mut terms = format!("{state} {}", status.state.as_str());
    if status.stale {
        terms.push_str(" stale");
    }
    terms
}

/// Fixed-width Explorer glyph column: the glyph and a stale marker slot. A
/// fresh working status shows `spinner`'s frame.
pub(crate) fn glyph_column(
    status: Option<&NavigationAgentStatus>,
    glyphs: AgentGlyphs,
    spinner: AgentSpinner,
) -> ChromeSource {
    glyphs.column(status.map(|status| (status.state, status.stale)), spinner)
}

/// Status counts of a set of Splints, ordered by display priority.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct AgentAggregate {
    /// `(state, stale, count)` groups, most urgent first; empty when none.
    groups: Vec<(AgentState, bool, usize)>,
}

impl AgentAggregate {
    pub(crate) fn from_statuses<'a>(
        statuses: impl IntoIterator<Item = &'a NavigationAgentStatus>,
    ) -> Self {
        let mut counts = [0_usize; 2 * PRIORITY.len()];
        for status in statuses {
            counts[rank(status)] += 1;
        }
        let groups = counts
            .iter()
            .enumerate()
            .filter(|(_, count)| **count > 0)
            .map(|(rank, count)| {
                (
                    PRIORITY[rank % PRIORITY.len()],
                    rank >= PRIORITY.len(),
                    *count,
                )
            })
            .collect();
        Self { groups }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// The most urgent group, which supplies the aggregate glyph.
    pub(crate) fn top(&self) -> Option<(AgentState, bool, usize)> {
        self.groups.first().copied()
    }

    /// Explorer glyph column for a Dojo or Lair row: the most urgent state.
    pub(crate) fn glyph_column(&self, glyphs: AgentGlyphs, spinner: AgentSpinner) -> ChromeSource {
        glyphs.column(
            self.top().map(|(state, is_stale, _)| (state, is_stale)),
            spinner,
        )
    }

    /// Counts in words for accessibility, e.g. "1 needs input, 2 working".
    pub(crate) fn counts_words(&self) -> String {
        self.counts().join(", ")
    }

    /// Counts for the Explorer footer's short status line, most urgent
    /// first, e.g. "1 needs input · 2 working".
    pub(crate) fn counts_brief(&self) -> String {
        self.counts().join(" · ")
    }

    /// One phrase per group, most urgent first; a state no Splint is in is
    /// left out.
    fn counts(&self) -> Vec<String> {
        self.groups
            .iter()
            .map(|(state, is_stale, count)| {
                format!(
                    "{count} {}{}",
                    state_text(*state).to_lowercase(),
                    if *is_stale { " (stale)" } else { "" }
                )
            })
            .collect()
    }

    /// One Dojo tab badge: the aggregate glyph, a count when more than one
    /// Splint shares the top state, and the stale marker, e.g.
    /// `[message-question]2` or `⠋[clock]`; ASCII `?2` or `⠋~`. A fresh
    /// working badge shows `spinner`'s frame, a stale one the first frame.
    pub(crate) fn tab_badge(
        &self,
        glyphs: AgentGlyphs,
        spinner: AgentSpinner,
    ) -> Option<ChromeSource> {
        let (top_state, is_stale, count) = self.top()?;
        let mut badge = ChromeSource::default();
        let spinner = if is_stale {
            AgentSpinner::STATIC
        } else {
            spinner
        };
        glyphs.push_glyph(&mut badge, top_state, is_stale, spinner);
        if count > 1 {
            badge.push_str(&count.min(99).to_string());
        }
        if is_stale {
            glyphs.push_stale(&mut badge);
        }
        Some(badge)
    }
}

fn cells(text: &str) -> usize {
    text.chars()
        .map(|character| character.width().unwrap_or(0).min(2))
        .sum()
}

/// Blank cells between a Dojo tab badge and its label: two, twice a word
/// space, so a count such as the `2` before "Dojo 2" reads as part of the
/// badge rather than as the first word of the name.
const TAB_BADGE_GAP: &str = "  ";

/// Composes a Dojo tab label within `maximum_cells`. The badge comes first and
/// the label text gives up width before it, so on a narrow tab the badge is
/// the last thing dropped. The gap after the badge is dropped together with
/// the label. Without a badge a label too long for the tab ends in an
/// ellipsis.
pub(crate) fn badged_tab_label(
    badge: Option<&ChromeSource>,
    label: &str,
    maximum_cells: usize,
) -> ChromeSource {
    let Some(badge) = badge else {
        return ChromeSource::from(ellipsized(label, maximum_cells.max(1)));
    };
    let mut composed = badge.clone();
    // The separating gap, then at least one label cell and an ellipsis.
    let label_budget =
        maximum_cells.saturating_sub(badge.cells().saturating_add(cells(TAB_BADGE_GAP)));
    if label.is_empty() || label_budget < 2 {
        return composed;
    }
    composed
        .push_str(TAB_BADGE_GAP)
        .push_str(&ellipsized(label, label_budget));
    composed
}

/// `label` within `maximum_cells`, its last kept cell replaced by `…` when it
/// is longer; `maximum_cells` must be at least one.
fn ellipsized(label: &str, maximum_cells: usize) -> String {
    if cells(label) <= maximum_cells {
        return label.to_owned();
    }
    let mut kept = String::new();
    let mut used = 0_usize;
    for character in label.chars() {
        let width = character.width().unwrap_or(0).min(2);
        if used.saturating_add(width) > maximum_cells - 1 {
            break;
        }
        kept.push(character);
        used = used.saturating_add(width);
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: AgentState, is_stale: bool) -> NavigationAgentStatus {
        NavigationAgentStatus {
            state,
            detail: None,
            stale: is_stale,
            stale_minutes: is_stale.then_some(2),
        }
    }

    fn describe(source: &ChromeSource) -> String {
        source.describe()
    }

    #[test]
    fn icon_and_text_tables_cover_every_state_with_ascii_fallbacks() {
        let table = [
            (AgentState::Working, None, None, "Working"),
            (
                AgentState::NeedsInput,
                Some(ChromeIcon::MessageQuestion),
                Some("?"),
                "Needs input",
            ),
            (
                AgentState::Done,
                Some(ChromeIcon::CircleCheck),
                Some("+"),
                "Done",
            ),
            (
                AgentState::Error,
                Some(ChromeIcon::AlertCircle),
                Some("!"),
                "Error",
            ),
            (AgentState::Idle, Some(ChromeIcon::Point), Some("-"), "Idle"),
        ];
        for (state, icon, ascii, text) in table {
            assert_eq!(AgentGlyphs::icon(state), icon);
            assert_eq!(AgentGlyphs::ascii(state), ascii);
            assert!(ascii.is_none_or(|ascii| ascii.is_ascii() && cells(ascii) == 1));
            assert_eq!(state_text(state), text);
        }
        // Shape alone distinguishes every state in every style, and the stale
        // marker is distinct from every state. Working is the spinner in
        // every style, which is neither an icon, ASCII nor a dot.
        for glyphs in [AgentGlyphs::Icons, AgentGlyphs::Ascii, AgentGlyphs::Dots] {
            let mut drawn = PRIORITY
                .map(|state| {
                    let mut source = ChromeSource::default();
                    glyphs.push_glyph(&mut source, state, false, AgentSpinner::STATIC);
                    source.describe()
                })
                .to_vec();
            assert_eq!(drawn[2], "⠋");
            let mut stale = ChromeSource::default();
            glyphs.push_stale(&mut stale);
            drawn.push(stale.describe());
            drawn.sort_unstable();
            drawn.dedup();
            assert_eq!(drawn.len(), PRIORITY.len() + 1, "{glyphs:?}");
        }
    }

    #[test]
    fn spinner_cycles_ten_braille_frames_from_the_first() {
        assert_eq!(
            SPINNER_FRAMES,
            ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
        );
        assert_eq!(AgentSpinner::default(), AgentSpinner::STATIC);
        assert!(AgentSpinner::STATIC.is_static());
        let mut spinner = AgentSpinner::STATIC;
        let mut seen = Vec::new();
        for _ in 0..SPINNER_FRAMES.len() {
            seen.push(spinner.glyph());
            spinner = spinner.next();
            assert_eq!(spinner.is_static(), spinner.glyph() == '⠋');
        }
        assert_eq!(seen, SPINNER_FRAMES);
        assert_eq!(spinner, AgentSpinner::STATIC);
        // Every frame is one Braille cell, which the renderer draws itself.
        assert!(SPINNER_FRAMES.iter().all(|frame| {
            ('\u{2800}'..='\u{28ff}').contains(frame) && frame.width() == Some(1)
        }));
    }

    #[test]
    fn spinner_steps_at_most_four_times_a_second_and_stops_static() {
        let step = AgentSpinner::STEP;
        assert!(step >= Duration::from_millis(250));
        let moving = AgentSpinner::STATIC.next().next();
        // Before a step is due nothing changes, however often the timer wakes.
        for elapsed in [0, 1, 100, 249] {
            assert_eq!(moving.tick(true, Duration::from_millis(elapsed)), None);
        }
        // Once due, exactly one frame per tick, never a catch-up jump.
        assert_eq!(moving.tick(true, step), Some(moving.next()));
        assert_eq!(
            moving.tick(true, Duration::from_secs(10)),
            Some(moving.next())
        );
        // A stopped spinner returns to its first frame once and then rests.
        assert_eq!(
            moving.tick(false, Duration::ZERO),
            Some(AgentSpinner::STATIC)
        );
        assert_eq!(
            AgentSpinner::STATIC.tick(false, Duration::from_secs(10)),
            None
        );
        // Over one simulated second of 1 ms timer wakeups, at most four steps.
        let (mut spinner, mut last, mut steps) = (AgentSpinner::STATIC, 0_u64, 0);
        for now in 0..=1000_u64 {
            if let Some(next) = spinner.tick(true, Duration::from_millis(now - last)) {
                spinner = next;
                last = now;
                steps += 1;
            }
        }
        assert_eq!(steps, 4);
        assert_eq!(
            AgentSpinner::remaining(true, Duration::from_millis(100)),
            Some(Duration::from_millis(150))
        );
        assert_eq!(
            AgentSpinner::remaining(true, step * 3),
            Some(Duration::ZERO)
        );
        assert_eq!(AgentSpinner::remaining(false, Duration::ZERO), None);
    }

    #[test]
    fn spinner_moves_only_focused_without_reduced_motion_while_fresh_work_shows() {
        assert!(AgentSpinner::animates(false, true, true));
        assert!(!AgentSpinner::animates(true, true, true));
        assert!(!AgentSpinner::animates(false, false, true));
        assert!(!AgentSpinner::animates(false, true, false));
        assert!(shows_working(&[status(AgentState::Working, false)]));
        assert!(!shows_working(&[status(AgentState::Working, true)]));
        assert!(!shows_working(&[
            status(AgentState::NeedsInput, false),
            status(AgentState::Done, false)
        ]));
        // A stale working status holds the first frame in columns and badges.
        let frame = AgentSpinner::STATIC.next().next().next();
        for glyphs in [AgentGlyphs::Icons, AgentGlyphs::Ascii, AgentGlyphs::Dots] {
            let fresh = status(AgentState::Working, false);
            let stale = status(AgentState::Working, true);
            assert!(
                glyph_column(Some(&fresh), glyphs, frame)
                    .text()
                    .starts_with('⠸')
            );
            assert!(
                glyph_column(Some(&stale), glyphs, frame)
                    .text()
                    .starts_with('⠋')
            );
            let badge = |statuses: &[NavigationAgentStatus]| {
                AgentAggregate::from_statuses(statuses)
                    .tab_badge(glyphs, frame)
                    .unwrap()
                    .text()
                    .to_owned()
            };
            assert!(badge(&[fresh]).starts_with('⠸'));
            assert!(badge(&[stale]).starts_with('⠋'));
        }
    }

    #[test]
    fn ascii_is_used_only_when_icons_are_unavailable() {
        assert_eq!(AgentGlyphs::default(), AgentGlyphs::Icons);
        assert_eq!(
            AgentGlyphs::select(true, StatusGlyphs::Icons),
            AgentGlyphs::Icons
        );
        assert_eq!(
            AgentGlyphs::select(false, StatusGlyphs::Icons),
            AgentGlyphs::Ascii
        );
        let blocked = [status(AgentState::NeedsInput, true)];
        let aggregate = AgentAggregate::from_statuses(&blocked);
        let ascii = aggregate
            .tab_badge(AgentGlyphs::Ascii, AgentSpinner::STATIC)
            .unwrap();
        assert_eq!(ascii.text(), "?~");
        assert!(ascii.icons().is_empty());
        let column = glyph_column(Some(&blocked[0]), AgentGlyphs::Ascii, AgentSpinner::STATIC);
        assert_eq!(column.text(), "?~");
        assert!(column.icons().is_empty());
    }

    #[test]
    fn aggregation_prefers_needs_input_and_ranks_stale_last() {
        let statuses = [
            status(AgentState::Idle, false),
            status(AgentState::Working, false),
            status(AgentState::NeedsInput, true),
            status(AgentState::Done, false),
            status(AgentState::Working, false),
            status(AgentState::Error, false),
            status(AgentState::NeedsInput, false),
        ];
        let aggregate = AgentAggregate::from_statuses(&statuses);
        assert_eq!(aggregate.top(), Some((AgentState::NeedsInput, false, 1)));
        assert_eq!(
            aggregate.counts_words(),
            "1 needs input, 1 error, 2 working, 1 done, 1 idle, 1 needs input (stale)"
        );
        for (higher, lower) in PRIORITY.iter().zip(PRIORITY.iter().skip(1)) {
            let pair = [status(*lower, false), status(*higher, false)];
            assert_eq!(
                AgentAggregate::from_statuses(&pair).top().unwrap().0,
                *higher
            );
            let stale_higher = [status(*lower, false), status(*higher, true)];
            assert_eq!(
                AgentAggregate::from_statuses(&stale_higher).top().unwrap(),
                (*lower, false, 1)
            );
        }
        assert!(AgentAggregate::from_statuses(&[]).is_empty());
        assert_eq!(
            AgentAggregate::default().tab_badge(AgentGlyphs::Icons, AgentSpinner::STATIC),
            None
        );
    }

    #[test]
    fn tab_badge_counts_only_the_top_state_and_marks_stale() {
        let badge = |statuses: &[NavigationAgentStatus], glyphs| {
            describe(
                &AgentAggregate::from_statuses(statuses)
                    .tab_badge(glyphs, AgentSpinner::STATIC)
                    .unwrap(),
            )
        };
        let two_blocked = [
            status(AgentState::NeedsInput, false),
            status(AgentState::NeedsInput, false),
            status(AgentState::Working, false),
        ];
        assert_eq!(
            badge(&two_blocked, AgentGlyphs::Icons),
            "[message-question]2"
        );
        assert_eq!(badge(&two_blocked, AgentGlyphs::Ascii), "?2");
        let stale = [status(AgentState::Working, true)];
        assert_eq!(badge(&stale, AgentGlyphs::Icons), "⠋[clock]");
        assert_eq!(badge(&stale, AgentGlyphs::Ascii), "⠋~");
        let many = vec![status(AgentState::Done, false); 120];
        assert_eq!(badge(&many, AgentGlyphs::Icons), "[circle-check]99");
        let source = AgentAggregate::from_statuses(&many)
            .tab_badge(AgentGlyphs::Icons, AgentSpinner::STATIC)
            .unwrap();
        assert_eq!(source.cells(), 4);
        assert_eq!(source.icons(), &[(0, ChromeIcon::CircleCheck)]);
    }

    #[test]
    fn splint_text_appends_stale_age_and_detail() {
        let mut working = status(AgentState::Working, true);
        working.detail = Some("running tests".to_owned());
        assert_eq!(
            splint_text(&working),
            "Working · stale (no report for 2 min) · running tests"
        );
        working.stale_minutes = Some(0);
        assert!(splint_text(&working).contains("no report for 1 min"));
        let mut blocked = status(AgentState::NeedsInput, false);
        blocked.detail = Some("waiting for approval".to_owned());
        assert_eq!(splint_text(&blocked), "Needs input · waiting for approval");
        assert_eq!(search_terms(&blocked), "needs input needs_input");
        assert_eq!(search_terms(&working), "working working stale");
    }

    #[test]
    fn glyph_column_has_a_fixed_width_per_style() {
        for glyphs in [AgentGlyphs::Icons, AgentGlyphs::Ascii, AgentGlyphs::Dots] {
            let columns = [
                glyph_column(None, glyphs, AgentSpinner::STATIC),
                glyph_column(
                    Some(&status(AgentState::Working, false)),
                    glyphs,
                    AgentSpinner::STATIC,
                ),
                glyph_column(
                    Some(&status(AgentState::Done, true)),
                    glyphs,
                    AgentSpinner::STATIC,
                ),
                AgentAggregate::default().glyph_column(glyphs, AgentSpinner::STATIC),
                AgentAggregate::from_statuses(&[status(AgentState::Error, false)])
                    .glyph_column(glyphs, AgentSpinner::STATIC),
            ];
            assert!(
                columns
                    .iter()
                    .all(|column| column.cells() == glyphs.column_cells())
            );
        }
        assert_eq!(AgentGlyphs::Icons.column_cells(), 4);
        assert_eq!(
            describe(&glyph_column(
                Some(&status(AgentState::Done, true)),
                AgentGlyphs::Icons,
                AgentSpinner::STATIC,
            )),
            "[circle-check][clock]"
        );
        assert_eq!(
            describe(&glyph_column(
                Some(&status(AgentState::NeedsInput, false)),
                AgentGlyphs::Icons,
                AgentSpinner::STATIC,
            )),
            "[message-question]  "
        );
        // The working spinner takes the first cell of its two-cell slot.
        assert_eq!(
            describe(&glyph_column(
                Some(&status(AgentState::Working, false)),
                AgentGlyphs::Icons,
                AgentSpinner::STATIC.next(),
            )),
            "⠙   "
        );
        assert_eq!(
            glyph_column(
                Some(&status(AgentState::Working, true)),
                AgentGlyphs::Ascii,
                AgentSpinner::STATIC.next(),
            )
            .text(),
            "⠋~"
        );
        assert_eq!(
            glyph_column(
                Some(&status(AgentState::Done, true)),
                AgentGlyphs::Ascii,
                AgentSpinner::STATIC,
            )
            .text(),
            "+~"
        );
    }

    #[test]
    fn tab_label_width_budget_drops_label_text_before_the_badge() {
        let label = |badge: Option<&str>, text: &str, maximum: usize| {
            badged_tab_label(badge.map(ChromeSource::from).as_ref(), text, maximum)
                .text()
                .to_owned()
        };
        assert_eq!(label(None, "editor", 6), "editor");
        assert_eq!(label(None, "editor", 3), "ed…");
        assert_eq!(label(None, "editor", 0), "…");
        assert_eq!(label(Some("?"), "editor", 20), "?  editor");
        assert_eq!(label(Some("?2"), "editor", 10), "?2  editor");
        assert_eq!(label(Some("?2"), "editor", 9), "?2  edit…");
        assert_eq!(label(Some("?2"), "editor", 8), "?2  edi…");
        assert_eq!(label(Some("?2"), "editor", 6), "?2  e…");
        // Too narrow for one label cell and an ellipsis: the gap goes with
        // the label and the badge stays.
        assert_eq!(label(Some("?2"), "editor", 5), "?2");
        assert_eq!(label(Some("?2"), "editor", 1), "?2");
        assert_eq!(label(Some("?2"), "", 20), "?2");
        assert_eq!(label(Some(">~"), "日本語", 7), ">~  日…");
        let mut icon_badge = ChromeSource::default();
        icon_badge.push_icon(ChromeIcon::CircleCheck).push_str("3");
        icon_badge.push_icon(ChromeIcon::Clock);
        for maximum in 0..24 {
            let composed = badged_tab_label(Some(&icon_badge), "a long Dojo name", maximum);
            assert!(composed.describe().starts_with("[circle-check]3[clock]"));
            assert_eq!(composed.icons(), icon_badge.icons());
            assert!(composed.cells() <= maximum.max(5));
        }
        assert_eq!(
            badged_tab_label(Some(&icon_badge), "editor", 13).describe(),
            "[circle-check]3[clock]  editor"
        );
        // A label never contributes icons, even with icon codepoints in it.
        assert!(badged_tab_label(Some(&icon_badge), "\u{ea05}x", 20).icons() == icon_badge.icons());
    }

    /// VM acceptance on 8979fa54: a counted badge before "Dojo 2" read as
    /// "2 Dojo 2", the count one word space from the name. The badge, count
    /// and stale marker included, is now two blank cells from the label in
    /// both glyph styles, whether the label starts with a digit or a letter.
    #[test]
    fn tab_badge_count_is_separated_from_the_label() {
        let counted = [
            status(AgentState::NeedsInput, false),
            status(AgentState::NeedsInput, false),
        ];
        let stale_counted = [
            status(AgentState::Working, true),
            status(AgentState::Working, true),
        ];
        for statuses in [&counted[..], &stale_counted[..]] {
            for glyphs in [AgentGlyphs::Icons, AgentGlyphs::Ascii] {
                let badge = AgentAggregate::from_statuses(statuses)
                    .tab_badge(glyphs, AgentSpinner::STATIC)
                    .unwrap();
                for name in ["Dojo 2", "2 builds", "editor"] {
                    let composed = badged_tab_label(Some(&badge), name, 40);
                    let after_badge = &composed.text()[badge.text().len()..];
                    assert_eq!(
                        after_badge,
                        format!("{TAB_BADGE_GAP}{name}"),
                        "{glyphs:?} {}",
                        composed.describe()
                    );
                    assert_eq!(composed.icons(), badge.icons());
                }
            }
        }
        let ascii = AgentAggregate::from_statuses(&counted)
            .tab_badge(AgentGlyphs::Ascii, AgentSpinner::STATIC)
            .unwrap();
        assert_eq!(
            badged_tab_label(Some(&ascii), "Dojo 2", 40).text(),
            "?2  Dojo 2"
        );
        let icons = AgentAggregate::from_statuses(&stale_counted)
            .tab_badge(AgentGlyphs::Icons, AgentSpinner::STATIC)
            .unwrap();
        assert_eq!(
            badged_tab_label(Some(&icons), "2 builds", 40).describe(),
            "⠋2[clock]  2 builds"
        );
    }

    #[test]
    fn status_glyphs_carry_herdrs_tints_and_stale_is_faint() {
        let table = [
            (AgentState::Working, ChromeTint::StatusWorking),
            (AgentState::NeedsInput, ChromeTint::StatusNeedsInput),
            (AgentState::Error, ChromeTint::StatusError),
            (AgentState::Done, ChromeTint::StatusDone),
            (AgentState::Idle, ChromeTint::StatusIdle),
        ];
        for glyphs in [AgentGlyphs::Icons, AgentGlyphs::Ascii, AgentGlyphs::Dots] {
            let glyph = u32::try_from(glyphs.glyph_cells()).unwrap();
            let marker = u32::try_from(glyphs.stale_cells()).unwrap();
            for (state, tint) in table {
                assert_eq!(state_tint(state, false), tint);
                assert_eq!(state_tint(state, true), ChromeTint::StatusStale);
                // The working spinner is one cell of its slot.
                let drawn = if state == AgentState::Working {
                    1
                } else {
                    glyph
                };
                let fresh = glyph_column(Some(&status(state, false)), glyphs, AgentSpinner::STATIC);
                assert_eq!(fresh.tints(), [(0, drawn, tint)], "{glyphs:?} {state:?}");
                // A stale status is faint, its marker too; padding is untinted.
                let stale = glyph_column(Some(&status(state, true)), glyphs, AgentSpinner::STATIC);
                assert_eq!(
                    stale.tints(),
                    [
                        (0, drawn, ChromeTint::StatusStale),
                        (glyph, glyph + marker, ChromeTint::StatusStale)
                    ],
                    "{glyphs:?} {state:?}"
                );
                if drawn < glyph {
                    assert_eq!(stale.tint_at(drawn), None);
                }
                // A tab badge tints its glyph, never its count.
                let two = [status(state, false), status(state, false)];
                let badge = AgentAggregate::from_statuses(&two)
                    .tab_badge(glyphs, AgentSpinner::STATIC)
                    .unwrap();
                assert_eq!(badge.tints(), [(0, drawn, tint)]);
                assert_eq!(badge.tint_at(drawn), None);
            }
            assert!(
                glyph_column(None, glyphs, AgentSpinner::STATIC)
                    .tints()
                    .is_empty()
            );
        }
    }

    #[test]
    fn dots_give_each_state_its_own_one_cell_shape() {
        // Dots are asked for, so they need no icon face.
        for icons_available in [true, false] {
            assert_eq!(
                AgentGlyphs::select(icons_available, StatusGlyphs::Dots),
                AgentGlyphs::Dots
            );
        }
        assert_eq!(AgentGlyphs::Dots.column_cells(), 2);
        let dots: Vec<_> = PRIORITY
            .iter()
            .filter_map(|state| AgentGlyphs::dot(*state))
            .collect();
        assert_eq!(dots, ["◆", "▲", "■", "□"]);
        for dot in &dots {
            assert_eq!(cells(dot), 1);
            // Never the Explorer's live dots.
            assert!(!["●", "○"].contains(dot));
        }
        assert_eq!(
            AgentAggregate::from_statuses(&[status(AgentState::Error, true)])
                .tab_badge(AgentGlyphs::Dots, AgentSpinner::STATIC)
                .unwrap()
                .describe(),
            "▲~"
        );
        assert_eq!(
            glyph_column(
                Some(&status(AgentState::Done, false)),
                AgentGlyphs::Dots,
                AgentSpinner::STATIC
            )
            .describe(),
            "■ "
        );
    }
}
