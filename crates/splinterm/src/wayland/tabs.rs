use super::{
    App, CachedFrameTitle, ChromeSource, ChromeText, ChromeTextStyle, Context, DojoId, HashMap,
    HashSet, LayoutNode, PaneView, Receiver, Rect, RenderContext, ResolvedTheme, Result, Sender,
    SplintId, Waker, WindowDojoIdentity, WindowPaneOptions, WindowTabSet, WindowTopologyCommand,
    WindowTopologyUpdate, WindowUpdate, apply_theme, drain_receiver, fill_rect,
    logical_extent_to_buffer, pane_stream_has_terminal_notice, premultiplied_theme_rgba,
    rect_contains, sanitized_tab_label,
};
use crate::theme::{ChromeTint, ChromeTints};

/// Install new streams for durable IDs that were retired and added in one update.
/// Shared by visible and hidden tabs; preserve pane positions and logical focus.
/// Remaining prepared panes are genuinely new layout members.
pub(super) fn replace_retired_panes<T>(
    active: &mut T,
    inactive: &mut [T],
    prepared: &mut Vec<T>,
    removed: &HashSet<SplintId>,
    identity: impl Fn(&T) -> Option<SplintId>,
) -> bool {
    let mut active_replaced = false;
    for (position, old) in std::iter::once(active)
        .chain(inactive.iter_mut())
        .enumerate()
    {
        let Some(id) = identity(old).filter(|id| removed.contains(id)) else {
            continue;
        };
        if let Some(index) = prepared.iter().position(|pane| identity(pane) == Some(id)) {
            *old = prepared.swap_remove(index);
            active_replaced |= position == 0;
        }
    }
    active_replaced
}

/// The tab strip height before any `shell.toml` is read, in logical px: the
/// stock Omarchy 4 top bar's.
#[cfg(test)]
pub(super) const DEFAULT_TAB_STRIP_LOGICAL_HEIGHT: u32 = 26;
const TAB_PREFERRED_LOGICAL_WIDTH: u32 = 180;
const TAB_MIN_LOGICAL_WIDTH: u32 = 96;

/// The tab strip's height in logical px at every output scale: the Omarchy
/// top bar's `[bar] size-horizontal`, 26 px unless `shell.toml` changes it.
pub(super) fn tab_strip_logical_height(theme: ResolvedTheme) -> u32 {
    u32::from(theme.shell.bar_size_px)
}

/// The active-tab indicator's thickness for a strip `strip_height` logical px
/// tall: 2 px for the 20-29 px strips Omarchy bars use, 3 px at the former
/// 34 px strip and 4 px at the 48 px maximum.
pub(super) const fn tab_indicator_logical_height(strip_height: u32) -> u32 {
    let thickness = strip_height.saturating_add(6) / 12;
    if thickness == 0 { 1 } else { thickness }
}

/// The tab strip's bottom border thickness in buffer px: the light `─`
/// stroke a `line` Splint divider draws for the terminal `cell` (buffer px)
/// at `scale_120`, kept at least 1 buffer px thinner than the active-tab
/// indicator so the indicator painted over it always reads as the heavier
/// line.
pub(super) fn tab_strip_border_thickness(
    strip_height: u32,
    scale_120: u32,
    cell: (u32, u32),
) -> Result<u32> {
    let indicator =
        logical_extent_to_buffer(tab_indicator_logical_height(strip_height), scale_120)?.max(1);
    let stroke = crate::box_drawing::default_thickness(
        cell.0,
        cell.1,
        u16::try_from(scale_120).unwrap_or(u16::MAX),
    );
    Ok(stroke.min(indicator.saturating_sub(1)).max(1))
}

/// Whether the tab strip bottom border is drawn: the option is on and the tab
/// strip itself is shown. A remote Window's host header, which stays when the
/// strip is hidden, draws no border on its own.
pub(super) const fn tab_strip_shows_border(
    enabled: bool,
    managed_tabs: bool,
    strip_visible: bool,
) -> bool {
    enabled && managed_tabs && strip_visible
}

/// The horizontal label inset for a strip `strip_height` logical px tall,
/// 10 px at the former 34 px strip and scaled with the strip from there.
const fn tab_label_logical_inset(strip_height: u32) -> u32 {
    strip_height.saturating_mul(10).saturating_add(17) / 34
}

/// The tallest text cell a strip `strip_height` logical px tall draws at
/// `scale_120`, in buffer px: the strip less the active-tab indicator's
/// thickness above and below, so vertically centred text never reaches the
/// indicator, and never above the chrome line cap. Tab labels, agent status
/// badges, the close and new-tab glyphs and the remote tag all use it.
pub(super) fn tab_text_maximum_cell_height(strip_height: u32, scale_120: u32) -> Result<u32> {
    let strip = logical_extent_to_buffer(strip_height, scale_120)?;
    let indicator =
        logical_extent_to_buffer(tab_indicator_logical_height(strip_height), scale_120)?.max(1);
    Ok(strip
        .saturating_sub(indicator.saturating_mul(2))
        .min(crate::renderer::chrome_maximum_cell_height(scale_120)))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TabHitTarget {
    Activate(DojoId),
    Close(DojoId),
    New,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VisibleTabLayout {
    pub(super) dojo_id: DojoId,
    pub(super) rect: Rect,
    pub(super) label_rect: Rect,
    pub(super) close_rect: Rect,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TabStripLayout {
    pub(super) rect: Rect,
    pub(super) tabs: Vec<VisibleTabLayout>,
    pub(super) new_rect: Rect,
    pub(super) host_rect: Option<Rect>,
}

/// Lays out a strip `height` logical px tall: each tab's close control and the
/// trailing new-tab control are square at the strip height, as the 34 px
/// strip's were, and every hit target spans the strip's full height.
pub(super) fn tab_strip_layout(
    width: u32,
    height: u32,
    dojo_ids: &[DojoId],
    active: usize,
) -> Option<TabStripLayout> {
    if width == 0 || dojo_ids.is_empty() || active >= dojo_ids.len() {
        return None;
    }
    let action_width = height;
    let new_width = action_width.min((width / 4).max(u32::from(width > 0)));
    let tab_space = width.saturating_sub(new_width);
    let capacity = usize::try_from((tab_space / TAB_MIN_LOGICAL_WIDTH).max(1)).unwrap_or(1);
    let visible_count = dojo_ids.len().min(capacity);
    let mut start = active.saturating_sub(visible_count / 2);
    start = start.min(dojo_ids.len().saturating_sub(visible_count));
    let tab_width = if visible_count == 0 {
        0
    } else {
        (tab_space / u32::try_from(visible_count).unwrap_or(1)).min(TAB_PREFERRED_LOGICAL_WIDTH)
    };
    let mut tabs = Vec::with_capacity(visible_count);
    let mut x = 0;
    for dojo_id in dojo_ids.iter().skip(start).take(visible_count).copied() {
        let remaining = tab_space.saturating_sub(x);
        let width = tab_width.min(remaining);
        let close_width = action_width.min((width / 3).max(u32::from(width > 1)));
        let label_space = width.saturating_sub(close_width);
        let label_inset = tab_label_logical_inset(height).min(label_space / 4);
        tabs.push(VisibleTabLayout {
            dojo_id,
            rect: Rect {
                x,
                y: 0,
                width,
                height,
            },
            label_rect: Rect {
                x: x.saturating_add(label_inset),
                y: 0,
                width: label_space.saturating_sub(label_inset.saturating_mul(2)),
                height,
            },
            close_rect: Rect {
                x: x.saturating_add(width.saturating_sub(close_width)),
                y: 0,
                width: close_width,
                height,
            },
        });
        x = x.saturating_add(width);
    }
    Some(TabStripLayout {
        rect: Rect {
            x: 0,
            y: 0,
            width,
            height,
        },
        tabs,
        host_rect: None,
        new_rect: Rect {
            x,
            y: 0,
            width: new_width.min(width.saturating_sub(x)),
            height,
        },
    })
}

/// Moves a tab strip laid out from the Window's left edge right by `left`
/// logical px, so it begins beside a full-height Explorer. Every hit target
/// moves with it.
pub(super) fn offset_tab_strip_layout(mut layout: TabStripLayout, left: u32) -> TabStripLayout {
    if left == 0 {
        return layout;
    }
    let shift = |rect: &mut Rect| rect.x = rect.x.saturating_add(left);
    shift(&mut layout.rect);
    shift(&mut layout.new_rect);
    if let Some(host) = layout.host_rect.as_mut() {
        shift(host);
    }
    for tab in &mut layout.tabs {
        shift(&mut tab.rect);
        shift(&mut tab.label_rect);
        shift(&mut tab.close_rect);
    }
    layout
}

/// The remote tag owns its entire region, including in hidden-tab/observer windows.
/// Reducing the tab layout's input width keeps every existing action out of the tag.
fn remote_header_layout(
    width: u32,
    height: u32,
    dojo_ids: &[DojoId],
    active: usize,
    host_width: u32,
) -> Option<TabStripLayout> {
    if width == 0 {
        return None;
    }
    let host_width = host_width.min(width);
    let tab_width = width.saturating_sub(host_width);
    let empty = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };
    let mut layout =
        tab_strip_layout(tab_width, height, dojo_ids, active).unwrap_or(TabStripLayout {
            rect: empty,
            tabs: Vec::new(),
            new_rect: empty,
            host_rect: None,
        });
    layout.rect = Rect {
        x: 0,
        y: 0,
        width,
        height,
    };
    layout.host_rect = Some(Rect {
        x: tab_width,
        y: 0,
        width: host_width,
        height,
    });
    Some(layout)
}

fn paint_remote_host(
    canvas: &mut [u8],
    size: (u32, u32),
    host_rect: Option<Rect>,
    scale_120: u32,
    foreground: u32,
    host_text: Option<&ChromeText>,
) -> Result<()> {
    let (width, height) = size;
    if let (Some(rect), Some(text)) = (host_rect, host_text) {
        let rect = App::buffer_rect(host_label_rect(rect), scale_120)?;
        text.paint(
            canvas,
            width,
            height,
            (
                rect.x
                    .saturating_add(rect.width.saturating_sub(text.pixel_width())),
                rect.y
                    .saturating_add(rect.height.saturating_sub(text.pixel_height()) / 2),
            ),
            rect,
            foreground,
        );
    }
    Ok(())
}

/// How many chrome cells of `cell_width` fit a tab label `buffer_width`
/// pixels wide; at least one, so a label always keeps its ellipsis.
fn tab_label_maximum_cells(buffer_width: u32, cell_width: u32) -> u32 {
    (buffer_width / cell_width.max(1)).max(1)
}

fn host_label_rect(rect: Rect) -> Rect {
    let inset = 8.min(rect.width / 8);
    Rect {
        x: rect.x.saturating_add(inset),
        width: rect.width.saturating_sub(inset * 2),
        ..rect
    }
}

fn clipped_remote_label(source: &str, maximum_cells: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if source.width() <= maximum_cells {
        return source.to_owned();
    }
    if maximum_cells <= 6 {
        return "Remote".chars().take(maximum_cells).collect();
    }
    let mut label = String::new();
    let mut cells = 0;
    for character in source.chars() {
        let width = character.width().unwrap_or(0);
        if cells + width >= maximum_cells {
            break;
        }
        label.push(character);
        cells += width;
    }
    label.push('…');
    label
}

pub(super) const fn tab_context_target(target: TabHitTarget) -> Option<DojoId> {
    match target {
        TabHitTarget::Activate(dojo_id) | TabHitTarget::Close(dojo_id) => Some(dojo_id),
        TabHitTarget::New => None,
    }
}

const fn tab_foreground(theme: ResolvedTheme, active: bool) -> u32 {
    if active {
        theme.active_tab_foreground
    } else {
        theme.roles.text_muted
    }
}

const fn opaque_rgba(color: u32) -> [u8; 4] {
    [
        ((color >> 16) & 0xff) as u8,
        ((color >> 8) & 0xff) as u8,
        (color & 0xff) as u8,
        u8::MAX,
    ]
}

fn tab_strip_background_rgba(theme: ResolvedTheme) -> [u8; 4] {
    premultiplied_theme_rgba(theme.roles.surface, theme.tab_strip_alpha())
}

/// The selected Dojo tab's body: opaque by default, so it stands apart from
/// a translucent strip, or at `[colors] active-tab-alpha`.
fn active_tab_background_rgba(theme: ResolvedTheme) -> [u8; 4] {
    premultiplied_theme_rgba(theme.active_tab_background, theme.chrome.active_tab_alpha)
}

fn paint_inactive_tab_divider(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    rect: Rect,
    scale_120: u32,
    theme: ResolvedTheme,
) -> Result<()> {
    let divider = logical_extent_to_buffer(1, scale_120)?.max(1);
    fill_rect(
        canvas,
        width,
        height,
        (
            i32::try_from(rect.x.saturating_add(rect.width).saturating_sub(divider))
                .unwrap_or(i32::MAX),
            i32::try_from(rect.y).unwrap_or(i32::MAX),
            divider.min(rect.width),
            rect.height,
        ),
        opaque_rgba(theme.roles.divider),
    );
    Ok(())
}

// Launchers name client-bound Lairs `terminal-<stamp>-<pid>`. Share the
// picker's rule so a tab never shows a generated name the picker hides.
fn is_generated_lair_name(value: &str) -> bool {
    crate::navigation_projection::is_generated_terminal_name(value)
}

fn uses_legacy_generated_dojo_label(identity: &WindowDojoIdentity) -> bool {
    identity.dojo_name == "terminal" && is_generated_lair_name(&identity.lair_name)
}

fn tab_dojo_label(identity: &WindowDojoIdentity) -> String {
    if uses_legacy_generated_dojo_label(identity) {
        "Dojo 1".to_owned()
    } else {
        sanitized_tab_label(&identity.dojo_name, 128, 48)
    }
}

fn ambiguous_tab_label(
    identity: &WindowDojoIdentity,
    dojo_label: &str,
    matching_ordinal: usize,
) -> String {
    if is_generated_lair_name(&identity.lair_name) {
        sanitized_tab_label(
            &format!("{dojo_label} ({})", matching_ordinal.saturating_add(1)),
            128,
            48,
        )
    } else {
        sanitized_tab_label(
            &format!("{} / {}", identity.lair_name, identity.dojo_name),
            128,
            48,
        )
    }
}

pub(super) fn tab_strip_hit_test(
    layout: &TabStripLayout,
    position: (f64, f64),
) -> Option<TabHitTarget> {
    if !rect_contains(layout.rect, position) {
        return None;
    }
    if rect_contains(layout.new_rect, position) {
        return Some(TabHitTarget::New);
    }
    layout.tabs.iter().find_map(|tab| {
        if rect_contains(tab.close_rect, position) {
            Some(TabHitTarget::Close(tab.dojo_id))
        } else if rect_contains(tab.rect, position) {
            Some(TabHitTarget::Activate(tab.dojo_id))
        } else {
            None
        }
    })
}

pub(super) struct DojoTabView {
    pub(super) identity: WindowDojoIdentity,
    pub(super) pane: PaneView,
    pub(super) inactive_panes: Vec<PaneView>,
    pub(super) layout: Option<LayoutNode>,
    pub(super) pending_exited_splints: HashSet<SplintId>,
    pub(super) pending_remote_splits: HashMap<SplintId, SplintId>,
    pub(super) frame_titles: HashMap<SplintId, CachedFrameTitle>,
    pub(super) dirty_inactive_panes: HashSet<SplintId>,
}

impl DojoTabView {
    pub(super) fn focused_splint(&self) -> Option<SplintId> {
        self.pane
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.splint_id)
    }

    pub(super) fn focus_splint(&mut self, splint_id: SplintId) -> bool {
        if self.focused_splint() == Some(splint_id) {
            return true;
        }
        let Some(index) = self.inactive_panes.iter().position(|pane| {
            pane.snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.splint_id == splint_id)
        }) else {
            return false;
        };
        std::mem::swap(&mut self.pane, &mut self.inactive_panes[index]);
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_topology(
        &mut self,
        layout: LayoutNode,
        added: Vec<WindowPaneOptions>,
        removed: Vec<SplintId>,
        focused: Option<SplintId>,
        theme: ResolvedTheme,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<()> {
        let removed = removed.into_iter().collect::<HashSet<_>>();
        let mut prepared = Vec::with_capacity(added.len());
        for mut pane in added {
            apply_theme(&mut pane.snapshot, theme);
            prepared.push(PaneView::from_inactive_options_with_context(
                pane, scale_120, context,
            )?);
        }
        let prepared_ids = prepared
            .iter()
            .filter_map(|pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id));
        let mut identities = std::iter::once(&self.pane)
            .chain(self.inactive_panes.iter())
            .filter_map(|pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id))
            .filter(|splint_id| !removed.contains(splint_id))
            .chain(prepared_ids)
            .collect::<HashSet<_>>();
        anyhow::ensure!(
            identities.len() == layout.splint_count()
                && identities
                    .iter()
                    .all(|splint_id| layout.find_splint(*splint_id).is_some()),
            "topology update pane identities do not match its layout"
        );
        let next_focus = focused.or_else(|| {
            self.focused_splint()
                .filter(|splint_id| identities.contains(splint_id))
        });
        let next_focus = next_focus.unwrap_or_else(|| layout.first_splint_id());
        anyhow::ensure!(
            identities.remove(&next_focus),
            "topology update focus is absent"
        );

        self.pending_exited_splints
            .retain(|splint_id| !removed.contains(splint_id));
        self.pending_remote_splits
            .retain(|_, pending| layout.find_splint(*pending).is_some());
        replace_retired_panes(
            &mut self.pane,
            &mut self.inactive_panes,
            &mut prepared,
            &removed,
            |pane| pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id),
        );
        self.inactive_panes.extend(prepared);
        let focused = self.focus_splint(next_focus);
        debug_assert!(focused);
        self.inactive_panes.retain(|pane| {
            pane.snapshot
                .as_ref()
                .is_none_or(|snapshot| layout.find_splint(snapshot.splint_id).is_some())
        });
        self.layout = Some(layout);
        Ok(())
    }

    /// Drains the hidden tab's pane updates. A stream lost unexpectedly ends
    /// the Window unless `hold_on_disconnect`, the daemon handoff case, where
    /// the pane is only marked disconnected (Plan 0061 M3.12a). Returns
    /// whether it held such a loss.
    pub(super) fn drain_hidden_updates(
        &mut self,
        waker: &Waker,
        theme: ResolvedTheme,
        hold_on_disconnect: bool,
    ) -> Result<bool> {
        let mut held = false;
        for pane in std::iter::once(&mut self.pane).chain(self.inactive_panes.iter_mut()) {
            let mut pending = Vec::new();
            let mut disconnected = false;
            if let Some(updates) = &mut pane.updates {
                let drained = drain_receiver(updates, waker);
                pending = drained.items;
                disconnected = drained.disconnected;
            }
            let terminal_notice = pane_stream_has_terminal_notice(&pending);
            if disconnected && !terminal_notice {
                anyhow::ensure!(
                    hold_on_disconnect,
                    "hidden pane update stream disconnected unexpectedly"
                );
                pane.controller_active = false;
                pane.commands = None;
                pane.updates = None;
                held = true;
            }
            for update in pending {
                if let WindowUpdate::Exited { splint_id } = update {
                    self.pending_exited_splints.insert(splint_id);
                    let impact = pane.apply_background_update(
                        WindowUpdate::Exited { splint_id },
                        theme,
                        "hidden",
                    )?;
                    if impact.frame_dirty {
                        self.dirty_inactive_panes.insert(splint_id);
                    }
                    continue;
                }
                if matches!(update, WindowUpdate::Theme(_) | WindowUpdate::Font(_)) {
                    continue;
                }
                let splint_id = pane.snapshot.as_ref().map(|snapshot| snapshot.splint_id);
                let impact = pane.apply_background_update(update, theme, "hidden")?;
                if impact.frame_dirty
                    && let Some(splint_id) = splint_id
                {
                    self.dirty_inactive_panes.insert(splint_id);
                }
            }
        }
        Ok(held)
    }

    pub(super) fn from_open(
        identity: WindowDojoIdentity,
        layout: LayoutNode,
        mut panes: Vec<WindowPaneOptions>,
        focused: SplintId,
        theme: ResolvedTheme,
        scale_120: u32,
        context: &RenderContext,
    ) -> Result<Self> {
        anyhow::ensure!(
            layout.find_splint(focused).is_some() && layout.splint_count() == panes.len(),
            "opened tab panes do not match its layout"
        );
        for pane in &mut panes {
            apply_theme(&mut pane.snapshot, theme);
        }
        let active_index = panes
            .iter()
            .position(|pane| pane.snapshot.splint_id == focused)
            .context("opened tab focus is absent from its panes")?;
        let active = panes.remove(active_index);
        let pane = PaneView::from_options_with_context(active, scale_120, context)?;
        let inactive_panes = panes
            .into_iter()
            .map(|pane| PaneView::from_inactive_options_with_context(pane, scale_120, context))
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            identity,
            pane,
            inactive_panes,
            layout: Some(layout),
            pending_exited_splints: HashSet::new(),
            pending_remote_splits: HashMap::new(),
            frame_titles: HashMap::new(),
            dirty_inactive_panes: HashSet::new(),
        })
    }
}

pub(super) struct TabsState {
    pub(super) tabs: WindowTabSet<Option<DojoTabView>>,
    pub(super) active_identity: WindowDojoIdentity,
    pub(super) managed_tabs: bool,
    pub(super) tab_strip_visible: bool,
    pub(super) remote_display_identity: Option<crate::endpoint::RemoteDisplayIdentity>,
    pub(super) remote_host_text: Option<(u32, ChromeText)>,
    pub(super) remote_host_clipped: Option<CachedFrameTitle>,
    pub(super) tab_strip_layout: Option<TabStripLayout>,
    pub(super) tab_strip_pressed: Option<(u32, TabHitTarget)>,
    pub(super) tab_label_cache: HashMap<DojoId, CachedFrameTitle>,
    pub(super) tab_close_text: Option<(u32, ChromeText)>,
    pub(super) tab_new_text: Option<(u32, ChromeText)>,
    pub(super) topology_updates: Option<Receiver<WindowTopologyUpdate>>,
    pub(super) topology_commands: Option<Sender<WindowTopologyCommand>>,
    /// Settings Reload configuration publishes to the topology manager.
    pub(super) topology_settings:
        Option<tokio::sync::watch::Sender<crate::config::TopologyReloadSettings>>,
    pub(super) session_switch_pending: bool,
    pub(super) deferred_topology_updates: Vec<WindowTopologyUpdate>,
    pub(super) agent: super::agent_status::TabAgentStatus,
    /// Where this Window publishes its tabs and focus for its handoff
    /// anchor (Plan 0061 M3.14); `None` unless it is a local managed Window.
    pub(super) layout_ledger: Option<crate::window_continuation::WindowLayoutLedger>,
}

impl TabsState {
    pub(super) const fn active_dojo_id(&self) -> DojoId {
        self.active_identity.dojo_id
    }

    /// The tabs in order with each one's focus, and the active tab, as this
    /// Window shows them now (Plan 0061 M3.14): the active tab's focus is
    /// `active_focus`, each other tab's is its saved view's. `None` for a
    /// Window without managed tabs, or before its active tab is listed.
    pub(super) fn window_layout(
        &self,
        active_focus: Option<SplintId>,
    ) -> Option<crate::window_continuation::WindowLayout> {
        if !self.managed_tabs {
            return None;
        }
        let active_dojo = self.active_dojo_id();
        let mut active = None;
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let focus = if tab.dojo_id == active_dojo {
                    active = Some(index);
                    active_focus
                } else {
                    tab.value.as_ref().and_then(DojoTabView::focused_splint)
                };
                splinterm_protocol::window_resume::WindowResumeTab {
                    lair_id: tab.lair_id,
                    dojo_id: tab.dojo_id,
                    focus,
                }
            })
            .collect();
        Some(crate::window_continuation::WindowLayout {
            tabs,
            active: active?,
        })
    }

    pub(super) fn tab_identity(&self, dojo_id: DojoId) -> Option<&WindowDojoIdentity> {
        if dojo_id == self.active_dojo_id() {
            return Some(&self.active_identity);
        }
        self.tabs
            .get(dojo_id)
            .and_then(|tab| tab.value.as_ref())
            .map(|view| &view.identity)
    }

    pub(super) fn tab_label(&self, dojo_id: DojoId) -> String {
        let Some(identity) = self.tab_identity(dojo_id) else {
            return "Untitled Dojo".to_owned();
        };
        label_among_tabs(
            identity,
            self.tabs
                .iter()
                .filter_map(|tab| self.tab_identity(tab.dojo_id)),
        )
    }
}

/// Labels one tab given every tab in strip order, the tab itself included.
fn label_among_tabs<'a>(
    identity: &WindowDojoIdentity,
    tabs: impl Iterator<Item = &'a WindowDojoIdentity>,
) -> String {
    let dojo_label = tab_dojo_label(identity);
    let mut matching_count = 0;
    // Only generated-Lair tabs show ordinals; named Lairs show their name.
    let mut generated_count = 0;
    let mut matching_ordinal = 0;
    for candidate in tabs.filter(|candidate| tab_dojo_label(candidate) == dojo_label) {
        if candidate.dojo_id == identity.dojo_id {
            matching_ordinal = generated_count;
        }
        matching_count += 1;
        if is_generated_lair_name(&candidate.lair_name) {
            generated_count += 1;
        }
    }
    if matching_count > 1 {
        ambiguous_tab_label(identity, &dojo_label, matching_ordinal)
    } else {
        dojo_label
    }
}

impl App {
    /// The strip height this window draws, from its theme's shell settings.
    pub(super) fn tab_strip_logical_height(&self) -> u32 {
        tab_strip_logical_height(self.presentation.theme)
    }

    /// Strip text sized for the current strip height and scale.
    fn load_tab_strip_text(
        &self,
        source: &ChromeSource,
        style: ChromeTextStyle,
    ) -> Result<ChromeText> {
        ChromeText::load_source_styled_within(
            source,
            self.surface.scale_120,
            style,
            &self.presentation.render_context,
            tab_text_maximum_cell_height(self.tab_strip_logical_height(), self.surface.scale_120)?,
        )
    }

    /// Drops every cached strip text and layout, for a strip height change.
    /// Each cache otherwise keys on the scale alone.
    pub(super) fn invalidate_tab_strip_text(&mut self) {
        self.tab_state.tab_strip_layout = None;
        self.tab_state.tab_label_cache.clear();
        self.tab_state.tab_close_text = None;
        self.tab_state.tab_new_text = None;
        self.tab_state.remote_host_text = None;
        self.tab_state.remote_host_clipped = None;
    }

    pub(super) fn prepare_remote_host_text(&mut self) -> Result<()> {
        let Some(identity) = &self.tab_state.remote_display_identity else {
            return Ok(());
        };
        if self
            .tab_state
            .remote_host_text
            .as_ref()
            .is_none_or(|(scale, _)| *scale != self.surface.scale_120)
        {
            let source = ChromeSource::from(identity.label());
            self.tab_state.remote_host_text = Some((
                self.surface.scale_120,
                self.load_tab_strip_text(&source, ChromeTextStyle::Regular)?,
            ));
            self.tab_state.remote_host_clipped = None;
        }
        Ok(())
    }

    pub(super) fn current_tab_strip_layout(&self) -> Option<TabStripLayout> {
        let show_tabs = self.tab_state.managed_tabs && self.tab_state.tab_strip_visible;
        let ids = if show_tabs {
            self.tab_state
                .tabs
                .iter()
                .map(|tab| tab.dojo_id)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let active = if ids.is_empty() {
            0
        } else {
            ids.iter()
                .position(|dojo_id| *dojo_id == self.tab_state.active_dojo_id())?
        };
        let left = self.tab_strip_left().min(self.surface.logical_width);
        let width = self.surface.logical_width.saturating_sub(left);
        let height = self.tab_strip_logical_height();
        let layout = self.tab_strip_layout_at_origin(width, height, &ids, active)?;
        Some(offset_tab_strip_layout(layout, left))
    }

    fn tab_strip_layout_at_origin(
        &self,
        width: u32,
        height: u32,
        ids: &[DojoId],
        active: usize,
    ) -> Option<TabStripLayout> {
        if let Some((_, text)) = &self.tab_state.remote_host_text {
            let cell = text.pixel_width() / text.cells().max(1);
            let logical = |pixels| {
                super::buffer_to_logical_ceil(pixels, self.surface.scale_120).unwrap_or(u32::MAX)
            };
            let minimum = logical(cell.saturating_mul(6)).saturating_add(16);
            let desired = logical(text.pixel_width())
                .saturating_add(16)
                .min(280)
                .max(minimum);
            return remote_header_layout(
                width,
                height,
                ids,
                active,
                desired.min((width / 3).max(minimum)),
            );
        }
        tab_strip_layout(width, height, ids, active)
    }

    fn prepare_clipped_remote_host_text(&mut self, layout: &TabStripLayout) -> Result<()> {
        if let (Some(rect), Some(identity), Some((_, full))) = (
            layout.host_rect,
            &self.tab_state.remote_display_identity,
            &self.tab_state.remote_host_text,
        ) {
            let rect = Self::buffer_rect(host_label_rect(rect), self.surface.scale_120)?;
            let cell = full.pixel_width() / full.cells().max(1);
            let maximum_cells = rect.width / cell.max(1);
            let source = ChromeSource::from(identity.label());
            if self
                .tab_state
                .remote_host_clipped
                .as_ref()
                .is_none_or(|cached| {
                    cached.maximum_cells != maximum_cells
                        || cached.source != source
                        || cached.scale_120 != self.surface.scale_120
                })
            {
                let clipped =
                    ChromeSource::from(clipped_remote_label(source.text(), maximum_cells as usize));
                self.tab_state.remote_host_clipped = Some(CachedFrameTitle {
                    text: self.load_tab_strip_text(&clipped, ChromeTextStyle::Regular)?,
                    source,
                    maximum_cells,
                    scale_120: self.surface.scale_120,
                    bold: false,
                });
            }
        }
        Ok(())
    }

    pub(super) fn prepare_tab_strip_text(&mut self, layout: &TabStripLayout) -> Result<()> {
        self.prepare_clipped_remote_host_text(layout)?;
        let visible = layout
            .tabs
            .iter()
            .map(|tab| tab.dojo_id)
            .collect::<HashSet<_>>();
        self.tab_state
            .tab_label_cache
            .retain(|dojo_id, _| visible.contains(dojo_id));
        self.prepare_tab_action_text()?;
        // Labels are bounded by the chrome cell they are drawn with, which
        // follows the chrome UI text size up to the strip's text cap.
        let cell_width = self
            .tab_state
            .tab_close_text
            .as_ref()
            .map_or(8, |(_, text)| text.pixel_width() / text.cells().max(1));
        for tab in &layout.tabs {
            let maximum_cells = tab_label_maximum_cells(
                Self::buffer_rect(tab.label_rect, self.surface.scale_120)?.width,
                cell_width,
            );
            // The badge takes width from the label, never from the close control.
            let source = crate::frontend::badged_tab_label(
                self.tab_state.agent.badge(tab.dojo_id).as_ref(),
                &self.tab_state.tab_label(tab.dojo_id),
                usize::try_from(maximum_cells).unwrap_or(usize::MAX),
            );
            let active = tab.dojo_id == self.tab_state.active_dojo_id();
            let current = self
                .tab_state
                .tab_label_cache
                .get(&tab.dojo_id)
                .is_some_and(|cached| {
                    cached.source == source
                        && cached.maximum_cells == maximum_cells
                        && cached.scale_120 == self.surface.scale_120
                        && cached.bold == active
                });
            if !current {
                let style = if active {
                    ChromeTextStyle::Bold
                } else {
                    ChromeTextStyle::Regular
                };
                let text = self.load_tab_strip_text(&source, style)?;
                self.tab_state.tab_label_cache.insert(
                    tab.dojo_id,
                    CachedFrameTitle {
                        text,
                        source,
                        maximum_cells,
                        scale_120: self.surface.scale_120,
                        bold: active,
                    },
                );
            }
        }
        Ok(())
    }

    fn prepare_tab_action_text(&mut self) -> Result<()> {
        if self
            .tab_state
            .tab_close_text
            .as_ref()
            .is_none_or(|(scale, _)| *scale != self.surface.scale_120)
        {
            self.tab_state.tab_close_text = Some((
                self.surface.scale_120,
                self.load_tab_strip_text(&ChromeSource::from("×"), ChromeTextStyle::Regular)?,
            ));
            self.tab_state.tab_new_text = Some((
                self.surface.scale_120,
                self.load_tab_strip_text(&ChromeSource::from("+"), ChromeTextStyle::Regular)?,
            ));
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one bounded pass paints tab actions and the noninteractive remote tag"
    )]
    pub(super) fn paint_tab_strip(
        canvas: &mut [u8],
        width: u32,
        height: u32,
        layout: &TabStripLayout,
        scale_120: u32,
        theme: ResolvedTheme,
        status_colors: bool,
        active_dojo: DojoId,
        labels: &HashMap<DojoId, CachedFrameTitle>,
        close_text: Option<&ChromeText>,
        new_text: Option<&ChromeText>,
        host_text: Option<&ChromeText>,
        border_cell: Option<(u32, u32)>,
    ) -> Result<()> {
        let position = |value| i32::try_from(value).unwrap_or(i32::MAX);
        // Status tints for each tab body a badge can sit on, kept at their
        // floors against it; none for `[explorer] status-colors=no`. The
        // remote tag is not a status and keeps its remote-marker color.
        let strip_tints = ChromeTints::on(&theme.roles, theme.roles.surface);
        let badge_tints = status_colors.then_some(strip_tints);
        let active_tints =
            status_colors.then(|| ChromeTints::on(&theme.roles, theme.active_tab_background));
        let strip = Self::buffer_rect(layout.rect, scale_120)?;
        fill_rect(
            canvas,
            width,
            height,
            (
                position(strip.x),
                position(strip.y),
                strip.width,
                strip.height,
            ),
            tab_strip_background_rgba(theme),
        );
        for tab in &layout.tabs {
            let rect = Self::buffer_rect(tab.rect, scale_120)?;
            if tab.dojo_id == active_dojo {
                fill_rect(
                    canvas,
                    width,
                    height,
                    (position(rect.x), position(rect.y), rect.width, rect.height),
                    active_tab_background_rgba(theme),
                );
            } else {
                paint_inactive_tab_divider(canvas, width, height, rect, scale_120, theme)?;
            }
        }
        // `[multiplexer] tab-strip-border`: the light `line` divider stroke
        // along the strip's bottom edge, across its full width and under the
        // active tab's thicker underline.
        if let Some(cell) = border_cell {
            let border =
                tab_strip_border_thickness(layout.rect.height, scale_120, cell)?.min(strip.height);
            fill_rect(
                canvas,
                width,
                height,
                (
                    position(strip.x),
                    position(strip.y.saturating_add(strip.height.saturating_sub(border))),
                    strip.width,
                    border,
                ),
                opaque_rgba(theme.roles.divider),
            );
        }
        for tab in &layout.tabs {
            let rect = Self::buffer_rect(tab.rect, scale_120)?;
            let active = tab.dojo_id == active_dojo;
            let foreground = tab_foreground(theme, active);
            if active {
                let underline = logical_extent_to_buffer(
                    tab_indicator_logical_height(layout.rect.height),
                    scale_120,
                )?
                .max(1);
                fill_rect(
                    canvas,
                    width,
                    height,
                    (
                        position(rect.x),
                        position(rect.y.saturating_add(rect.height.saturating_sub(underline))),
                        rect.width,
                        underline,
                    ),
                    opaque_rgba(theme.roles.accent),
                );
            }
            let label_rect = Self::buffer_rect(tab.label_rect, scale_120)?;
            if let Some(label) = labels.get(&tab.dojo_id) {
                let y = label_rect.y.saturating_add(
                    label_rect.height.saturating_sub(label.text.pixel_height()) / 2,
                );
                let tints = if active { active_tints } else { badge_tints };
                label.text.paint_colored(
                    canvas,
                    width,
                    height,
                    (label_rect.x, y),
                    label_rect,
                    |column| {
                        tints
                            .zip(label.source.tint_at(column))
                            .map_or(foreground, |(tints, tint)| tints.color(tint))
                    },
                );
            }
            let close = Self::buffer_rect(tab.close_rect, scale_120)?;
            if let Some(text) = close_text {
                text.paint(
                    canvas,
                    width,
                    height,
                    (
                        close
                            .x
                            .saturating_add(close.width.saturating_sub(text.pixel_width()) / 2),
                        close
                            .y
                            .saturating_add(close.height.saturating_sub(text.pixel_height()) / 2),
                    ),
                    close,
                    foreground,
                );
            }
        }
        let new_rect = Self::buffer_rect(layout.new_rect, scale_120)?;
        if let Some(text) = new_text {
            text.paint(
                canvas,
                width,
                height,
                (
                    new_rect
                        .x
                        .saturating_add(new_rect.width.saturating_sub(text.pixel_width()) / 2),
                    new_rect
                        .y
                        .saturating_add(new_rect.height.saturating_sub(text.pixel_height()) / 2),
                ),
                new_rect,
                theme.roles.accent,
            );
        }
        paint_remote_host(
            canvas,
            (width, height),
            layout.host_rect,
            scale_120,
            strip_tints.color(ChromeTint::Remote),
            host_text,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn restored_streams_replace_active_and_hidden_panes_without_duplicate_ids() {
        let active_id = super::SplintId::new();
        let hidden_id = super::SplintId::new();
        let added_id = super::SplintId::new();
        let unchanged_id = super::SplintId::new();
        let mut active = (active_id, 7);
        let mut inactive = vec![(hidden_id, 3), (unchanged_id, 1)];
        let mut prepared = vec![(hidden_id, 4), (added_id, 1), (active_id, 8)];
        let removed = super::HashSet::from([active_id, hidden_id]);
        assert!(super::replace_retired_panes(
            &mut active,
            &mut inactive,
            &mut prepared,
            &removed,
            |pane| Some(pane.0)
        ));
        assert_eq!(active, (active_id, 8));
        assert_eq!(inactive, vec![(hidden_id, 4), (unchanged_id, 1)]);
        assert_eq!(prepared, vec![(added_id, 1)]);
        inactive.extend(prepared);
        assert_eq!(
            inactive.iter().filter(|pane| pane.0 == hidden_id).count(),
            1
        );
        assert!(!inactive.iter().any(|pane| pane.0 == active_id));
    }

    #[test]
    fn restoring_hidden_stream_does_not_change_active_pane() {
        let active_id = super::SplintId::new();
        let hidden_id = super::SplintId::new();
        let mut active = (active_id, 7);
        let mut inactive = vec![(hidden_id, 3)];
        let mut prepared = vec![(hidden_id, 4)];
        assert!(!super::replace_retired_panes(
            &mut active,
            &mut inactive,
            &mut prepared,
            &super::HashSet::from([hidden_id]),
            |pane| Some(pane.0)
        ));
        assert_eq!(active, (active_id, 7));
        assert_eq!(inactive, vec![(hidden_id, 4)]);
        assert!(prepared.is_empty());
        // Ordinary removal leaves the old pane for the caller's focus-and-prune step.
        assert!(!super::replace_retired_panes(
            &mut active,
            &mut inactive,
            &mut prepared,
            &super::HashSet::from([active_id]),
            |pane| Some(pane.0)
        ));
        assert_eq!(active, (active_id, 7));
    }

    use std::collections::HashMap;

    use splinterm_core::{LairId, TopologyRevision};

    use super::{
        App, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, DojoId, ResolvedTheme, TabHitTarget,
        WindowDojoIdentity, ambiguous_tab_label, is_generated_lair_name, label_among_tabs,
        opaque_rgba, tab_context_target, tab_dojo_label, tab_foreground, tab_strip_hit_test,
        tab_strip_layout, tab_strip_shows_border,
    };

    #[test]
    fn remote_header_reserves_tag_space_without_tab_hit_targets_even_when_hidden() {
        let ids = (0..12).map(|_| DojoId::new()).collect::<Vec<_>>();
        for width in [0, 1, 3, 35, 80, 120, 420, 1200] {
            for visible in [true, false] {
                let ids = if visible { &ids[..] } else { &[] };
                let Some(layout) = super::remote_header_layout(
                    width,
                    DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
                    ids,
                    10,
                    100,
                ) else {
                    assert_eq!(width, 0);
                    continue;
                };
                let host = layout.host_rect.unwrap();
                assert_eq!(host.x + host.width, width);
                assert!(layout.new_rect.x + layout.new_rect.width <= host.x);
                for tab in &layout.tabs {
                    assert!(tab.rect.x + tab.rect.width <= host.x);
                    assert!(tab.label_rect.x + tab.label_rect.width <= tab.close_rect.x);
                    assert!(tab.close_rect.x + tab.close_rect.width <= host.x);
                }
                for x in host.x..width {
                    assert_eq!(tab_strip_hit_test(&layout, (f64::from(x), 1.0)), None);
                }
                if visible && width > 100 {
                    assert!(layout.tabs.iter().any(|tab| tab.dojo_id == ids[10]));
                } else {
                    assert!(layout.tabs.is_empty());
                    assert_eq!(layout.new_rect.width, 0);
                }
                assert_eq!(layout.rect.height, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT);
            }
        }
        assert!(tab_strip_layout(420, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &[], 0).is_none());
        assert!(
            tab_strip_layout(420, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 0)
                .unwrap()
                .host_rect
                .is_none()
        );
    }

    #[test]
    fn remote_label_truncates_by_unicode_cells_and_keeps_remote_prefix() {
        use unicode_width::UnicodeWidthStr;
        let source = "Remote: 界界.example";
        for cells in 0..30 {
            let label = super::clipped_remote_label(source, cells);
            assert!(label.width() <= cells);
            if cells >= 6 {
                assert!(label.starts_with("Remote"));
            }
            if cells >= source.width() {
                assert_eq!(label, source);
            }
        }
        assert_eq!(super::clipped_remote_label(source, 10), "Remote: …");
    }

    #[test]
    fn remote_tag_paints_only_reserved_header_at_fractional_scales_and_theme_colors() {
        use super::{ChromeText, Rect};
        for scale in [120, 150, 180, 240] {
            let logical_width = 420;
            let logical_height = 64;
            let width = super::logical_extent_to_buffer(logical_width, scale).unwrap();
            let height = super::logical_extent_to_buffer(logical_height, scale).unwrap();
            let layout = super::remote_header_layout(
                logical_width,
                DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
                &[],
                0,
                180,
            )
            .unwrap();
            let host = App::buffer_rect(layout.host_rect.unwrap(), scale).unwrap();
            let label = ChromeText::load("Remote: node", scale).unwrap();
            for foreground in [0xff_00_00, 0x00_ff_00] {
                let theme = ResolvedTheme {
                    background_alpha: u16::MAX,
                    roles: crate::theme::ChromeRoles {
                        surface: 0,
                        text: foreground,
                        // Readable on the surface, so painted as given.
                        remote_marker: foreground,
                        ..crate::theme::ChromeRoles::default()
                    },
                    ..ResolvedTheme::default()
                };
                let mut without = vec![0x41; (width * height * 4) as usize];
                App::paint_tab_strip(
                    &mut without,
                    width,
                    height,
                    &layout,
                    scale,
                    theme,
                    false,
                    DojoId::new(),
                    &HashMap::new(),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                let mut tagged = vec![0x41; (width * height * 4) as usize];
                App::paint_tab_strip(
                    &mut tagged,
                    width,
                    height,
                    &layout,
                    scale,
                    theme,
                    false,
                    DojoId::new(),
                    &HashMap::new(),
                    None,
                    None,
                    Some(&label),
                    None,
                )
                .unwrap();
                let mut changed = 0;
                for (index, (a, b)) in tagged
                    .chunks_exact(4)
                    .zip(without.chunks_exact(4))
                    .enumerate()
                {
                    let x = u32::try_from(index).unwrap() % width;
                    let y = u32::try_from(index).unwrap() / width;
                    if a != b {
                        changed += 1;
                        assert!(super::rect_contains(host, (f64::from(x), f64::from(y))));
                        assert_eq!(a[0], 0);
                        assert_eq!(a[if foreground == 0xff_00_00 { 1 } else { 2 }], 0);
                    }
                    let below = Rect {
                        x: 0,
                        y: super::logical_extent_to_buffer(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, scale)
                            .unwrap(),
                        width,
                        height,
                    };
                    if super::rect_contains(below, (f64::from(x), f64::from(y))) {
                        assert_eq!(a, &[0x41; 4]);
                    }
                }
                assert!(changed > 0);
            }
        }
    }

    #[test]
    fn tab_strip_follows_background_alpha_and_the_active_tab_stays_opaque() {
        use crate::config::ChromeColorOverrides;

        let theme = ResolvedTheme {
            background_alpha: u16::MAX / 2,
            selection_foreground: 0x11_22_33,
            active_tab_background: 0x44_55_66,
            active_tab_foreground: 0xdd_ee_ff,
            roles: crate::theme::ChromeRoles {
                surface: 0x80_40_20,
                text: 0xaa_bb_cc,
                text_muted: 0x99_88_77,
                accent: 0x77_88_99,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        assert_eq!(tab_foreground(theme, false), 0x99_88_77);
        assert_eq!(tab_foreground(theme, true), 0xdd_ee_ff);
        assert_eq!(theme.selection_foreground, 0x11_22_33);
        assert_eq!(opaque_rgba(theme.roles.accent), [0x77, 0x88, 0x99, 0xff]);

        let active_dojo = DojoId::new();
        let layout =
            tab_strip_layout(420, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &[active_dojo], 0).unwrap();
        let mut canvas =
            vec![0; 420 * usize::try_from(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT).unwrap() * 4];
        App::paint_tab_strip(
            &mut canvas,
            420,
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            &layout,
            120,
            theme,
            true,
            active_dojo,
            &HashMap::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let active_x = usize::try_from(layout.tabs[0].rect.x + 1).unwrap();
        let sample_y = usize::try_from(layout.tabs[0].rect.y + 1).unwrap();
        let active_offset = (sample_y * 420 + active_x) * 4;
        // Under glass the selected tab stays opaque, apart from the strip.
        assert_eq!(
            &canvas[active_offset..active_offset + 4],
            &[0x66, 0x55, 0x44, 0xff]
        );

        let underline_y = usize::try_from(
            layout.tabs[0]
                .rect
                .y
                .saturating_add(layout.tabs[0].rect.height.saturating_sub(1)),
        )
        .unwrap();
        let underline_offset = (underline_y * 420 + active_x) * 4;
        assert_eq!(
            &canvas[underline_offset..underline_offset + 4],
            &[0x99, 0x88, 0x77, 0xff]
        );

        let strip_x = usize::try_from(layout.new_rect.x + 1).unwrap();
        let strip_offset = (sample_y * 420 + strip_x) * 4;
        assert_eq!(
            &canvas[strip_offset..strip_offset + 4],
            &[0x10, 0x20, 0x40, 0x7f]
        );

        // `[colors] active-tab-alpha`, `tab-strip-alpha` and
        // `active-tab-background` replace those defaults.
        let overridden = theme.with_chrome_overrides(ChromeColorOverrides {
            active_tab_background: Some(0x20_40_60),
            active_tab_alpha: Some(u16::MAX / 2),
            tab_strip_alpha: Some(u16::MAX),
            ..ChromeColorOverrides::NONE
        });
        canvas.fill(0);
        App::paint_tab_strip(
            &mut canvas,
            420,
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            &layout,
            120,
            overridden,
            true,
            active_dojo,
            &HashMap::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            &canvas[active_offset..active_offset + 4],
            &[0x30, 0x20, 0x10, 0x7f]
        );
        assert_eq!(
            &canvas[strip_offset..strip_offset + 4],
            &[0x20, 0x40, 0x80, 0xff]
        );
    }

    /// Beside a full-height Explorer the strip is laid out at its own width
    /// and moved right; every hit target moves with it, and nothing is left
    /// over the Explorer.
    #[test]
    fn offset_tab_strip_moves_every_hit_target() {
        use super::{TabHitTarget, offset_tab_strip_layout, tab_strip_hit_test};
        let ids = [DojoId::new(), DojoId::new()];
        let layout = tab_strip_layout(400, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 0).unwrap();
        assert_eq!(offset_tab_strip_layout(layout.clone(), 0), layout);
        let left = 280;
        let moved = offset_tab_strip_layout(layout.clone(), left);
        assert_eq!(moved.rect.x, left);
        assert_eq!(moved.rect.width, layout.rect.width);
        assert_eq!(moved.new_rect.x, layout.new_rect.x + left);
        for (moved_tab, tab) in moved.tabs.iter().zip(&layout.tabs) {
            assert_eq!(moved_tab.rect.x, tab.rect.x + left);
            assert_eq!(moved_tab.label_rect.x, tab.label_rect.x + left);
            assert_eq!(moved_tab.close_rect.x, tab.close_rect.x + left);
            assert_eq!(moved_tab.rect.width, tab.rect.width);
        }
        let first = moved.tabs[0].label_rect;
        let inside = (f64::from(first.x + 1), f64::from(first.y + 1));
        assert_eq!(
            tab_strip_hit_test(&moved, inside),
            Some(TabHitTarget::Activate(ids[0]))
        );
        // Where the first tab used to be is now the Explorer's.
        assert_eq!(tab_strip_hit_test(&moved, (1.0, 1.0)), None);
    }

    #[test]
    fn right_click_target_covers_the_tab_body_and_close_affordance_only() {
        let dojo_id = DojoId::new();
        assert_eq!(
            tab_context_target(TabHitTarget::Activate(dojo_id)),
            Some(dojo_id)
        );
        assert_eq!(
            tab_context_target(TabHitTarget::Close(dojo_id)),
            Some(dojo_id)
        );
        assert_eq!(tab_context_target(TabHitTarget::New), None);
    }

    #[test]
    fn tab_strip_layout_keeps_active_visible_with_bounded_non_overlapping_targets() {
        let ids = (0..12).map(|_| DojoId::new()).collect::<Vec<_>>();
        let layout = tab_strip_layout(420, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 10).unwrap();
        assert!(layout.tabs.iter().any(|tab| tab.dojo_id == ids[10]));
        assert!(layout.tabs.len() < ids.len());
        assert!(layout.tabs.iter().all(|tab| {
            tab.rect.x.saturating_add(tab.rect.width) <= layout.new_rect.x
                && tab.label_rect.x.saturating_add(tab.label_rect.width) <= tab.close_rect.x
        }));
        for pair in layout.tabs.windows(2) {
            assert!(pair[0].rect.x.saturating_add(pair[0].rect.width) <= pair[1].rect.x);
        }
        assert_eq!(
            layout.new_rect.x,
            layout
                .tabs
                .last()
                .unwrap()
                .rect
                .x
                .saturating_add(layout.tabs.last().unwrap().rect.width)
        );
        let close = layout.tabs[0].close_rect;
        assert_eq!(
            tab_strip_hit_test(&layout, (f64::from(close.x), f64::from(close.y)),),
            Some(TabHitTarget::Close(layout.tabs[0].dojo_id))
        );
    }

    #[test]
    fn inactive_tabs_have_exact_semantic_dividers() {
        let inactive_dojo = DojoId::new();
        let active_dojo = DojoId::new();
        let layout = tab_strip_layout(
            420,
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            &[inactive_dojo, active_dojo],
            1,
        )
        .unwrap();
        let theme = ResolvedTheme {
            roles: crate::theme::ChromeRoles {
                surface: 0x10_20_30,
                divider: 0x44_55_66,
                ..crate::theme::ChromeRoles::default()
            },
            ..ResolvedTheme::default()
        };
        let mut canvas =
            vec![0; 420 * usize::try_from(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT).unwrap() * 4];
        App::paint_tab_strip(
            &mut canvas,
            420,
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            &layout,
            120,
            theme,
            true,
            active_dojo,
            &HashMap::new(),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let inactive = &layout.tabs[0].rect;
        let divider_x =
            usize::try_from(inactive.x.saturating_add(inactive.width).saturating_sub(1)).unwrap();
        let divider_y = usize::try_from(inactive.height / 2).unwrap();
        let divider_offset = (divider_y * 420 + divider_x) * 4;
        assert_eq!(
            &canvas[divider_offset..divider_offset + 4],
            &[0x66, 0x55, 0x44, 0xff]
        );
    }

    /// The rows and pixel a `line` Splint divider's light `─` paints in one
    /// terminal `cell` at `scale`, drawn by the divider's own painter.
    fn light_divider_stroke(cell: (u32, u32), scale: u32, color: u32) -> (u32, [u8; 4]) {
        let mut canvas = vec![0; usize::try_from(cell.0 * cell.1 * 4).unwrap()];
        let rect = super::Rect {
            x: 0,
            y: 0,
            width: cell.0,
            height: cell.1,
        };
        crate::renderer::paint_box_drawing_cell(
            &mut canvas,
            cell.0,
            cell.1,
            '─',
            rect,
            rect,
            color,
            u16::try_from(scale).unwrap(),
        );
        let painted = (0..cell.1)
            .map(|y| usize::try_from(y * cell.0 * 4).unwrap())
            .filter(|offset| canvas[offset + 3] != 0)
            .collect::<Vec<_>>();
        let pixel = canvas[painted[0]..painted[0] + 4].try_into().unwrap();
        (u32::try_from(painted.len()).unwrap(), pixel)
    }

    #[test]
    fn tab_strip_border_needs_a_shown_tab_strip() {
        assert!(tab_strip_shows_border(true, true, true));
        // Off, a hidden strip (a remote host header alone), or an unmanaged
        // Window: no border.
        assert!(!tab_strip_shows_border(false, true, true));
        assert!(!tab_strip_shows_border(true, true, false));
        assert!(!tab_strip_shows_border(true, false, true));
        assert!(!tab_strip_shows_border(true, false, false));
    }

    #[test]
    fn tab_strip_border_is_the_light_divider_stroke_under_the_active_underline() {
        use super::{tab_indicator_logical_height, tab_strip_border_thickness};

        let ids = [DojoId::new(), DojoId::new(), DojoId::new()];
        let accent = [0x99, 0x88, 0x77, 0xff];
        let mut clamped = 0;
        for strip_height in [20, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 34, 48] {
            let layout = tab_strip_layout(420, strip_height, &ids, 1).unwrap();
            // A hidden strip has no layout, so nothing paints a border.
            assert!(tab_strip_layout(420, strip_height, &[], 0).is_none());
            for scale in [120, 150, 180, 240] {
                let buffer = |logical| super::logical_extent_to_buffer(logical, scale).unwrap();
                let width = buffer(420);
                let strip = buffer(strip_height);
                let height = strip + 10;
                let indicator = buffer(tab_indicator_logical_height(strip_height));
                let active = App::buffer_rect(layout.tabs[1].rect, scale).unwrap();
                let new_rect = App::buffer_rect(layout.new_rect, scale).unwrap();
                // Common terminal cells, and one huge enough to need the clamp.
                for (cell_width, cell_height) in [(7, 17), (9, 21), (14, 34), (48, 112)] {
                    let cell = (buffer(cell_width), buffer(cell_height));
                    let case = format!("strip {strip_height}, scale {scale}, cell {cell:?}");
                    let border = tab_strip_border_thickness(strip_height, scale, cell).unwrap();
                    let (stroke, stroke_pixel) = light_divider_stroke(cell, scale, 0x44_55_66);
                    assert_eq!(stroke_pixel, [0x66, 0x55, 0x44, 0xff], "{case}");
                    if stroke < indicator {
                        assert_eq!(border, stroke, "{case}");
                    } else {
                        clamped += 1;
                        assert_eq!(border, indicator - 1, "{case}");
                    }
                    assert!(border >= 1 && border < indicator, "{case}");
                    for alpha in [u16::MAX, u16::MAX / 2] {
                        let theme = ResolvedTheme {
                            background_alpha: alpha,
                            active_tab_background: 0x22_22_22,
                            roles: crate::theme::ChromeRoles {
                                surface: 0x10_20_30,
                                divider: 0x44_55_66,
                                accent: 0x77_88_99,
                                ..crate::theme::ChromeRoles::default()
                            },
                            ..ResolvedTheme::default()
                        };
                        let paint = |border_cell| {
                            let mut canvas =
                                vec![0x41; usize::try_from(width * height * 4).unwrap()];
                            App::paint_tab_strip(
                                &mut canvas,
                                width,
                                height,
                                &layout,
                                scale,
                                theme,
                                true,
                                ids[1],
                                &HashMap::new(),
                                None,
                                None,
                                None,
                                border_cell,
                            )
                            .unwrap();
                            canvas
                        };
                        let bordered_canvas = paint(Some(cell));
                        let plain_canvas = paint(None);
                        let pixel = |canvas: &[u8], x: u32, y: u32| -> [u8; 4] {
                            let offset = usize::try_from((y * width + x) * 4).unwrap();
                            canvas[offset..offset + 4].try_into().unwrap()
                        };
                        for y in 0..height {
                            for x in 0..width {
                                let bordered = pixel(&bordered_canvas, x, y);
                                let plain = pixel(&plain_canvas, x, y);
                                let in_active = x >= active.x && x < active.x + active.width;
                                if y >= strip {
                                    assert_eq!(bordered, [0x41; 4], "{case}: below at {x},{y}");
                                } else if in_active && y >= strip - indicator {
                                    // The thicker accent underline sits on top.
                                    assert_eq!(bordered, accent, "{case}: underline {x},{y}");
                                    assert_eq!(plain, accent, "{case}: underline {x},{y}");
                                } else if y >= strip - border {
                                    // Full Window width, the divider's opaque colour.
                                    assert_eq!(bordered, stroke_pixel, "{case}: border {x},{y}");
                                } else {
                                    assert_eq!(bordered, plain, "{case}: above border {x},{y}");
                                }
                            }
                        }
                        // Off, the strip's bottom row is plain background.
                        let background = super::tab_strip_background_rgba(theme);
                        assert_eq!(
                            pixel(&plain_canvas, new_rect.x + 1, strip - 1),
                            [background[2], background[1], background[0], background[3]],
                            "{case}"
                        );
                    }
                }
            }
        }
        assert!(clamped > 0, "the huge cell exercises the clamp");
    }

    #[test]
    fn legacy_generated_initial_dojo_names_have_a_stable_friendly_tab_label() {
        for generated in [
            "terminal-1787899189-4132",
            "terminal-1791871642236894856-88001",
            "terminal-17878991890-4132",
            "terminal-1787899189",
            "terminal",
        ] {
            assert!(is_generated_lair_name(generated));
        }
        for explicit in [
            "terminal-123",
            "terminal-work",
            "terminal-123-work",
            "terminal-178789918-4132",
            "terminal-1787899189-12345678901",
            "terminal-1787899189-4132-extra",
        ] {
            assert!(!is_generated_lair_name(explicit));
        }

        let identity = WindowDojoIdentity {
            topology_revision: TopologyRevision::new(1),
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            lair_name: "terminal-1787899189-4132".to_owned(),
            lair_retention: splinterm_core::LairRetention::Disposable,
            dojo_name: "terminal".to_owned(),
        };
        assert_eq!(tab_dojo_label(&identity), "Dojo 1");

        assert_eq!(ambiguous_tab_label(&identity, "Dojo 1", 0), "Dojo 1 (1)");
        assert_eq!(ambiguous_tab_label(&identity, "Dojo 1", 1), "Dojo 1 (2)");
        assert!(!ambiguous_tab_label(&identity, "Dojo 1", 1).contains("terminal-"));

        let explicitly_generated_looking = WindowDojoIdentity {
            dojo_name: "terminal-1787899189-4132".to_owned(),
            ..identity.clone()
        };
        assert_eq!(
            tab_dojo_label(&explicitly_generated_looking),
            "terminal-1787899189-4132"
        );

        let explicitly_named = WindowDojoIdentity {
            lair_name: "terminal-123".to_owned(),
            dojo_name: "terminal-123".to_owned(),
            ..identity
        };
        assert_eq!(tab_dojo_label(&explicitly_named), "terminal-123");
    }

    #[test]
    fn duplicate_dojo_names_in_generated_lairs_use_ordinals_not_lair_names() {
        let generated = WindowDojoIdentity {
            topology_revision: TopologyRevision::new(1),
            lair_id: LairId::new(),
            dojo_id: DojoId::new(),
            lair_name: "terminal-1791871642236894856-88001".to_owned(),
            lair_retention: splinterm_core::LairRetention::Disposable,
            dojo_name: "Dojo 1".to_owned(),
        };
        assert_eq!(tab_dojo_label(&generated), "Dojo 1");
        assert_eq!(ambiguous_tab_label(&generated, "Dojo 1", 0), "Dojo 1 (1)");
        assert_eq!(ambiguous_tab_label(&generated, "Dojo 1", 1), "Dojo 1 (2)");

        let named = WindowDojoIdentity {
            lair_name: "work".to_owned(),
            ..generated.clone()
        };
        assert_eq!(ambiguous_tab_label(&named, "Dojo 1", 1), "work / Dojo 1");

        let second = WindowDojoIdentity {
            dojo_id: DojoId::new(),
            lair_name: "terminal-1791871642236894857-88002".to_owned(),
            ..generated.clone()
        };
        let strip = [named.clone(), generated.clone(), second.clone()];
        let labels: Vec<_> = strip
            .iter()
            .map(|identity| label_among_tabs(identity, strip.iter()))
            .collect();
        assert_eq!(labels, ["work / Dojo 1", "Dojo 1 (1)", "Dojo 1 (2)"]);
        assert_eq!(
            label_among_tabs(&second, [named, second.clone()].iter()),
            "Dojo 1 (1)"
        );
        assert_eq!(label_among_tabs(&second, [second.clone()].iter()), "Dojo 1");
    }

    #[test]
    fn tab_strip_compact_layout_retains_active_close_and_new_targets() {
        let ids = [DojoId::new(), DojoId::new(), DojoId::new()];
        let layout = tab_strip_layout(80, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 1).unwrap();
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.tabs[0].dojo_id, ids[1]);
        assert!(layout.tabs[0].label_rect.width > 0);
        assert!(layout.tabs[0].close_rect.width > 0);
        assert!(layout.new_rect.width > 0);
        for width in [3, 35] {
            let compact =
                tab_strip_layout(width, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 1).unwrap();
            assert!(compact.tabs[0].label_rect.width > 0);
            assert!(compact.tabs[0].close_rect.width > 0);
            assert!(compact.new_rect.width > 0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded badge, scale, and width paint matrix"
    )]
    fn tab_badge_paints_first_and_never_reaches_the_close_control() {
        use super::{CachedFrameTitle, ChromeSource, ChromeText, ChromeTextStyle, Rect};
        use crate::frontend::badged_tab_label;
        use crate::renderer::ChromeIcon;

        // A label starting with a digit must not run into a counted badge.
        let labels = ["a long Dojo label", "2 long Dojo label"];
        let mut counted = ChromeSource::default();
        counted.push_icon(ChromeIcon::MessageQuestion).push_str("2");
        let mut stale = ChromeSource::default();
        stale.push_str("⠋").push_icon(ChromeIcon::Clock);
        let ascii = ChromeSource::from("?2");
        let cases = [&counted, &stale, &ascii]
            .into_iter()
            .flat_map(|badge| labels.map(|label| (badge, label)));
        for (badge, label) in cases {
            for scale in [120, 150, 180, 240] {
                for logical_width in [420, 140, 60] {
                    let dojo = DojoId::new();
                    let layout = tab_strip_layout(
                        logical_width,
                        DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
                        &[dojo],
                        0,
                    )
                    .unwrap();
                    let tab = &layout.tabs[0];
                    let maximum_cells = usize::try_from(super::tab_label_maximum_cells(
                        App::buffer_rect(tab.label_rect, scale).unwrap().width,
                        ChromeText::load("×", scale).unwrap().pixel_width(),
                    ))
                    .unwrap();
                    let width = super::logical_extent_to_buffer(logical_width, scale).unwrap();
                    let height =
                        super::logical_extent_to_buffer(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, scale)
                            .unwrap();
                    let paint = |source: &ChromeSource| {
                        let text =
                            ChromeText::load_source_styled(source, scale, ChromeTextStyle::Bold)
                                .unwrap();
                        let labels = HashMap::from([(
                            dojo,
                            CachedFrameTitle {
                                source: source.clone(),
                                maximum_cells: 0,
                                scale_120: scale,
                                bold: true,
                                text,
                            },
                        )]);
                        let close = ChromeText::load("×", scale).unwrap();
                        let mut canvas = vec![0; (width * height * 4) as usize];
                        App::paint_tab_strip(
                            &mut canvas,
                            width,
                            height,
                            &layout,
                            scale,
                            ResolvedTheme::default(),
                            true,
                            dojo,
                            &labels,
                            Some(&close),
                            None,
                            None,
                            None,
                        )
                        .unwrap();
                        canvas
                    };
                    let badged = badged_tab_label(Some(badge), label, maximum_cells);
                    assert!(badged.text().starts_with(badge.text()), "{badged:?}");
                    assert_eq!(badged.icons(), badge.icons());
                    let plain = paint(&badged_tab_label(None, label, maximum_cells));
                    let with_badge = paint(&badged);
                    let badge_only = paint(badge);
                    let label_rect = App::buffer_rect(tab.label_rect, scale).unwrap();
                    let close_rect = App::buffer_rect(tab.close_rect, scale).unwrap();
                    let cell = ChromeText::load("?", scale).unwrap().pixel_width();
                    let badge_width = u32::try_from(badge.cells()).unwrap() * cell;
                    let badge_cells = Rect {
                        width: badge_width.min(label_rect.width),
                        ..label_rect
                    };
                    // When the label fits, two blank cells separate it from
                    // the badge; on a narrow tab the gap goes with the label.
                    let after_badge = &badged.text()[badge.text().len()..];
                    let gap_cells = if after_badge.is_empty() {
                        0
                    } else {
                        assert!(after_badge.starts_with("  "), "{badged:?}");
                        assert!(!after_badge[2..].starts_with(' '), "{badged:?}");
                        2
                    };
                    let gap = Rect {
                        x: label_rect.x + badge_cells.width,
                        width: (gap_cells * cell)
                            .min(label_rect.width.saturating_sub(badge_cells.width)),
                        ..label_rect
                    };
                    let mut badge_pixels = 0;
                    for (index, ((badged_pixel, plain_pixel), badge_pixel)) in with_badge
                        .chunks_exact(4)
                        .zip(plain.chunks_exact(4))
                        .zip(badge_only.chunks_exact(4))
                        .enumerate()
                    {
                        let x = u32::try_from(index).unwrap() % width;
                        let y = u32::try_from(index).unwrap() / width;
                        let position = (f64::from(x), f64::from(y));
                        if badged_pixel != plain_pixel || badge_pixel != plain_pixel {
                            // Badge icons take width from the label, never the close control.
                            assert!(
                                super::rect_contains(label_rect, position)
                                    && !super::rect_contains(close_rect, position),
                                "badge {badge:?} changed {x},{y} outside the label at scale {scale}, width {logical_width}"
                            );
                        }
                        if super::rect_contains(badge_cells, position) {
                            assert_eq!(badged_pixel, badge_pixel, "badge cells at {x},{y}");
                            badge_pixels += usize::from(badge_pixel != plain_pixel);
                        }
                        if super::rect_contains(gap, position) {
                            // The label paints nothing into the gap.
                            assert_eq!(badged_pixel, badge_pixel, "badge gap at {x},{y}");
                        }
                    }
                    if label_rect.width >= badge_width {
                        assert!(
                            badge_pixels > 0,
                            "badge {badge:?} is visible at width {logical_width}"
                        );
                    }
                }
            }
        }
    }

    /// VM case E6: at maximum zoom tab labels were clipped mid-glyph with
    /// no ellipsis. They now fit the strip and end in `…` when too long, and
    /// centred text stays clear of the active-tab indicator at every strip
    /// height `[bar] size-horizontal` allows. Chrome no longer follows zoom,
    /// so the largest UI text size now stands in for it.
    #[test]
    fn maximum_zoom_tab_labels_fit_the_strip_and_end_in_an_ellipsis() {
        use super::{
            ChromeSource, ChromeText, ChromeTextStyle, tab_indicator_logical_height,
            tab_label_maximum_cells, tab_text_maximum_cell_height,
        };
        use crate::frontend::badged_tab_label;
        use crate::renderer::RenderContext;

        let ids = [DojoId::new(), DojoId::new(), DojoId::new()];
        for strip_height in [DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 20, 34, 48] {
            for scale in [120, 150, 180, 240] {
                let context = RenderContext::new(u16::MAX)
                    .at_maximum_zoom(scale)
                    .at_maximum_chrome_font_size();
                let cap = tab_text_maximum_cell_height(strip_height, scale).unwrap();
                let load = |source: &ChromeSource, style| {
                    ChromeText::load_source_styled_within(source, scale, style, &context, cap)
                        .unwrap()
                };
                let close = load(&ChromeSource::from("×"), ChromeTextStyle::Regular);
                let cell = close.pixel_width() / close.cells().max(1);
                let layout = tab_strip_layout(640, strip_height, &ids, 0).unwrap();
                let strip = App::buffer_rect(layout.rect, scale).unwrap();
                let indicator = super::logical_extent_to_buffer(
                    tab_indicator_logical_height(strip_height),
                    scale,
                )
                .unwrap();
                let case = format!("strip {strip_height}, scale {scale}");
                assert!(close.pixel_height() <= cap, "{case}");
                for tab in &layout.tabs {
                    let label = App::buffer_rect(tab.label_rect, scale).unwrap();
                    let maximum = tab_label_maximum_cells(label.width, cell);
                    let source = badged_tab_label(
                        None,
                        "a long Dojo label name that keeps going past the tab",
                        usize::try_from(maximum).unwrap(),
                    );
                    assert!(source.text().ends_with('…'), "{case}");
                    let text = load(&source, ChromeTextStyle::Bold);
                    assert!(text.pixel_width() <= label.width, "{case}");
                    assert!(text.pixel_height() <= cap, "{case}");
                    // paint_tab_strip centres the cell: the indicator never
                    // reaches it from below, nor a matching gap from above.
                    let top = (label.height - text.pixel_height()) / 2;
                    let bottom = strip.height - top - text.pixel_height();
                    assert!(top >= indicator && bottom >= indicator, "{case}");
                    // The bottom border, at its thickest (clamped below the
                    // indicator for any terminal cell), stays clear too.
                    let border =
                        super::tab_strip_border_thickness(strip_height, scale, (1_000, 2_000))
                            .unwrap();
                    assert!(border < indicator && bottom > border, "{case}");
                }
            }
        }
    }

    /// Every `omarchy-display-text-size` size (9–20 px) fits the strip at 1x
    /// and 2x: the stock bar that grows with the text size, a bar with
    /// `scale-with-font` off, and the compact 20 px strip. Text is drawn at
    /// the UI size while it fits and stops at the strip's cap otherwise.
    #[test]
    fn omarchy_text_sizes_fit_the_tab_strip() {
        use super::{
            ChromeSource, ChromeText, ChromeTextStyle, tab_indicator_logical_height,
            tab_text_maximum_cell_height,
        };
        use crate::{geometry::FontSize, renderer::RenderContext};

        let ids = [DojoId::new(), DojoId::new()];
        for px in 9_u16..=20 {
            // The stock bar's round(26 × base-size / 12), within 20–48 px.
            let scaled_bar = ((26 * u32::from(px) * 2 + 12) / 24).clamp(20, 48);
            for strip_height in [scaled_bar, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 20] {
                for scale in [120, 240] {
                    let context = RenderContext::new(u16::MAX)
                        .at_maximum_zoom(scale)
                        .with_chrome_font_size(FontSize::Pixels(f32::from(px)));
                    let cap = tab_text_maximum_cell_height(strip_height, scale).unwrap();
                    let layout = tab_strip_layout(640, strip_height, &ids, 0).unwrap();
                    let strip = App::buffer_rect(layout.rect, scale).unwrap();
                    let indicator = super::logical_extent_to_buffer(
                        tab_indicator_logical_height(strip_height),
                        scale,
                    )
                    .unwrap();
                    let case = format!("{px} px, strip {strip_height}, scale {scale}");
                    for (source, style) in [
                        (ChromeSource::from("Lair 1"), ChromeTextStyle::Bold),
                        (ChromeSource::from("×"), ChromeTextStyle::Regular),
                        (ChromeSource::from("+"), ChromeTextStyle::Regular),
                    ] {
                        let text = ChromeText::load_source_styled_within(
                            &source, scale, style, &context, cap,
                        )
                        .unwrap();
                        assert!(text.pixel_height() <= cap, "{case}");
                        let label = App::buffer_rect(layout.tabs[0].label_rect, scale).unwrap();
                        let top = (label.height - text.pixel_height()) / 2;
                        let bottom = strip.height - top - text.pixel_height();
                        assert!(top >= indicator && bottom >= indicator, "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn strip_derived_dimensions_follow_the_strip_height() {
        use super::{
            tab_indicator_logical_height, tab_label_logical_inset, tab_strip_logical_height,
            tab_text_maximum_cell_height,
        };
        use crate::renderer::chrome_maximum_cell_height;

        assert_eq!(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 26);
        assert_eq!(
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            u32::from(crate::config::OMARCHY_DEFAULT_BAR_SIZE_PX)
        );
        assert_eq!(
            tab_strip_logical_height(ResolvedTheme::default()),
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT
        );
        let mut theme = ResolvedTheme::default();
        theme.shell.bar_size_px = 40;
        assert_eq!(tab_strip_logical_height(theme), 40);
        // The former 34 px strip keeps its 3 px indicator and 10 px inset.
        for (strip, indicator, inset) in [(20, 2, 6), (26, 2, 8), (34, 3, 10), (48, 4, 14)] {
            assert_eq!(tab_indicator_logical_height(strip), indicator, "{strip}");
            assert_eq!(tab_label_logical_inset(strip), inset, "{strip}");
        }
        // Text cap in buffer px: the strip less the indicator above and below,
        // never above the 24 px chrome line cap.
        assert_eq!(tab_text_maximum_cell_height(26, 120).unwrap(), 22);
        assert_eq!(tab_text_maximum_cell_height(26, 150).unwrap(), 33 - 2 * 3);
        assert_eq!(tab_text_maximum_cell_height(20, 120).unwrap(), 16);
        for scale in [120, 150, 180, 240] {
            assert_eq!(
                tab_text_maximum_cell_height(34, scale).unwrap(),
                chrome_maximum_cell_height(scale)
            );
            assert_eq!(
                tab_text_maximum_cell_height(48, scale).unwrap(),
                chrome_maximum_cell_height(scale)
            );
        }
    }

    #[test]
    fn strip_layout_at_scale_1_and_1_25_keeps_square_actions_and_targets_inside_the_strip() {
        let ids = [DojoId::new(), DojoId::new(), DojoId::new()];
        for strip_height in [DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, 20, 48] {
            let layout = tab_strip_layout(640, strip_height, &ids, 1).unwrap();
            assert_eq!(layout.rect.height, strip_height);
            assert_eq!(layout.new_rect.width, strip_height);
            assert_eq!(layout.new_rect.height, strip_height);
            for tab in &layout.tabs {
                for rect in [tab.rect, tab.label_rect, tab.close_rect] {
                    assert_eq!((rect.y, rect.height), (0, strip_height));
                }
                assert_eq!(tab.close_rect.width, strip_height);
                let x = f64::from(tab.rect.x + 1);
                let last_row = f64::from(strip_height) - 0.5;
                assert_eq!(
                    tab_strip_hit_test(&layout, (x, last_row)),
                    Some(TabHitTarget::Activate(tab.dojo_id))
                );
                assert_eq!(
                    tab_strip_hit_test(&layout, (x, f64::from(strip_height))),
                    None
                );
                let close = tab.close_rect;
                assert_eq!(
                    tab_strip_hit_test(&layout, (f64::from(close.x), last_row)),
                    Some(TabHitTarget::Close(tab.dojo_id))
                );
            }
            let new_x = f64::from(layout.new_rect.x);
            assert_eq!(
                tab_strip_hit_test(&layout, (new_x, 0.0)),
                Some(TabHitTarget::New)
            );
            assert_eq!(
                tab_strip_hit_test(&layout, (new_x, f64::from(strip_height))),
                None
            );
            // The strip spans its whole logical height at scale 1 and 1.25.
            for (scale, buffer_height) in
                [(120, strip_height), (150, (strip_height * 5).div_ceil(4))]
            {
                let strip = App::buffer_rect(layout.rect, scale).unwrap();
                assert_eq!(
                    strip.height, buffer_height,
                    "strip {strip_height}, scale {scale}"
                );
                for tab in &layout.tabs {
                    let close = App::buffer_rect(tab.close_rect, scale).unwrap();
                    assert!(close.y + close.height <= strip.y + strip.height);
                }
            }
        }
        // At the default 26 px, scale 1.25 draws a 33 px buffer strip.
        let layout = tab_strip_layout(640, DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, &ids, 1).unwrap();
        assert_eq!(App::buffer_rect(layout.rect, 150).unwrap().height, 33);
    }

    #[test]
    fn tab_badges_and_the_remote_tag_take_status_colors_on_their_tab_bodies() {
        use super::{CachedFrameTitle, ChromeSource, ChromeText, ChromeTextStyle};
        use crate::color::{NON_TEXT_MIN_CONTRAST, TEXT_MIN_CONTRAST, contrast_ratio};
        use crate::frontend::{AgentAggregate, AgentGlyphs, AgentSpinner, badged_tab_label};
        use crate::navigation_projection::NavigationAgentStatus;
        use crate::theme::{ChromeTint, ChromeTints};
        use splinterm_protocol::AgentState;

        let paints = |canvas: &[u8], color: u32| {
            let [red, green, blue, _] = opaque_rgba(color);
            canvas
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [blue, green, red])
        };
        let theme = ResolvedTheme::default();
        let scale = 120;
        let (active, other) = (DojoId::new(), DojoId::new());
        let logical_width = 420;
        let layout = super::remote_header_layout(
            logical_width,
            DEFAULT_TAB_STRIP_LOGICAL_HEIGHT,
            &[active, other],
            0,
            180,
        )
        .unwrap();
        let width = super::logical_extent_to_buffer(logical_width, scale).unwrap();
        let height =
            super::logical_extent_to_buffer(DEFAULT_TAB_STRIP_LOGICAL_HEIGHT, scale).unwrap();
        let host = ChromeText::load("Remote: node", scale).unwrap();
        let paint = |badge: &ChromeSource, status_colors: bool| {
            let source = badged_tab_label(Some(badge), "work", 20);
            let labels = [active, other].map(|dojo| {
                let text = ChromeText::load_source_styled(&source, scale, ChromeTextStyle::Regular)
                    .unwrap();
                (
                    dojo,
                    CachedFrameTitle {
                        source: source.clone(),
                        maximum_cells: 0,
                        scale_120: scale,
                        bold: false,
                        text,
                    },
                )
            });
            let mut canvas = vec![0; (width * height * 4) as usize];
            App::paint_tab_strip(
                &mut canvas,
                width,
                height,
                &layout,
                scale,
                theme,
                status_colors,
                active,
                &HashMap::from(labels),
                None,
                None,
                Some(&host),
                None,
            )
            .unwrap();
            canvas
        };
        let strip = ChromeTints::on(&theme.roles, theme.roles.surface);
        let body = ChromeTints::on(&theme.roles, theme.active_tab_background);
        let remote = strip.color(ChromeTint::Remote);
        assert!(contrast_ratio(remote, theme.roles.surface) >= TEXT_MIN_CONTRAST);
        for state in [
            AgentState::NeedsInput,
            AgentState::Error,
            AgentState::Working,
            AgentState::Done,
            AgentState::Idle,
        ] {
            let status = NavigationAgentStatus {
                state,
                detail: None,
                stale: false,
                stale_minutes: None,
            };
            let badge = AgentAggregate::from_statuses(&[status])
                .tab_badge(AgentGlyphs::Icons, AgentSpinner::STATIC)
                .unwrap();
            let tint = crate::frontend::state_tint(state, false);
            // Each glyph keeps 3:1 on the body it is painted on.
            let body_contrast = contrast_ratio(body.color(tint), theme.active_tab_background);
            assert!(body_contrast >= NON_TEXT_MIN_CONTRAST);
            let tinted = paint(&badge, true);
            assert!(paints(&tinted, strip.color(tint)), "{state:?} on the strip");
            assert!(
                paints(&tinted, body.color(tint)),
                "{state:?} on the active tab"
            );
            assert!(paints(&tinted, remote));
            // status-colors=no paints exactly an untinted badge; the remote
            // tag is not a status and keeps remote-marker.
            let off = paint(&badge, false);
            assert!(off == paint(&badge.untinted(), true), "{state:?}");
            assert!(paints(&off, remote));
        }
    }
}
