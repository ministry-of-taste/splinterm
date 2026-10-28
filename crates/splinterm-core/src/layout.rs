use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identity for a terminal surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SplintId(Uuid);

impl SplintId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SplintId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SplintId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for SplintId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitSide {
    First,
    Second,
}

/// Fixed share assigned to the first child of a branch, in thousandths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct SplitRatio(u16);

impl SplitRatio {
    pub const MIN: u16 = 1;
    pub const MAX: u16 = 999;

    /// Creates a ratio in the inclusive range 1..=999.
    ///
    /// # Errors
    ///
    /// Returns the supplied value when it cannot leave space for both children.
    pub const fn new(value: u16) -> Result<Self, u16> {
        if value >= Self::MIN && value <= Self::MAX {
            Ok(Self(value))
        } else {
            Err(value)
        }
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for SplitRatio {
    type Error = u16;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SplitRatio> for u16 {
    fn from(value: SplitRatio) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplintState {
    Starting,
    Running,
    Exited(i32),
}

/// Durable launch policy and last known terminal geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SplintLaunchMetadata {
    pub shell: Option<String>,
    pub login_shell: bool,
    pub scrollback_lines: usize,
    pub columns: u16,
    pub rows: u16,
    pub relaunch_on_restore: bool,
}

impl Default for SplintLaunchMetadata {
    fn default() -> Self {
        Self {
            shell: None,
            login_shell: false,
            scrollback_lines: 10_000,
            columns: 80,
            rows: 24,
            relaunch_on_restore: false,
        }
    }
}

/// The title every new Splint starts with. While a Splint keeps it unset
/// ([`Splint::title_set`] is false), windows may show its foreground app's
/// name instead.
pub const DEFAULT_SPLINT_TITLE: &str = "shell";

/// One terminal surface, eventually backed by a daemon-owned PTY.
///
/// `title_set` records whether a person, an agent, or a preset named the
/// Splint. It is serialized only when it cannot be inferred: a Splint named
/// the literal default title on purpose. Without the field a Splint is named
/// exactly when its title differs from [`DEFAULT_SPLINT_TITLE`], which is also
/// how Splints saved before the field existed load.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "SplintRecord")]
pub struct Splint {
    pub id: SplintId,
    pub title: String,
    /// Whether the title was set by a rename or a preset rather than left as
    /// the default. Always true when `title` is not the default.
    pub title_set: bool,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub launch: Box<SplintLaunchMetadata>,
    pub last_incarnation: Option<u64>,
    pub state: SplintState,
}

impl Splint {
    #[must_use]
    pub fn shell(cwd: PathBuf) -> Self {
        Self {
            id: SplintId::new(),
            title: DEFAULT_SPLINT_TITLE.into(),
            title_set: false,
            cwd,
            command: Vec::new(),
            launch: Box::new(SplintLaunchMetadata::default()),
            last_incarnation: None,
            state: SplintState::Starting,
        }
    }

    /// Whether the title is the default and nobody set it, so a window may
    /// show the foreground app's name in its place.
    #[must_use]
    pub fn uses_default_title(&self) -> bool {
        !self.title_set && self.title == DEFAULT_SPLINT_TITLE
    }
}

/// The serialized form of [`Splint`].
#[derive(Deserialize)]
struct SplintRecord {
    id: SplintId,
    title: String,
    cwd: PathBuf,
    command: Vec<String>,
    #[serde(default)]
    launch: Box<SplintLaunchMetadata>,
    #[serde(default)]
    last_incarnation: Option<u64>,
    state: SplintState,
    #[serde(default)]
    title_set: Option<bool>,
}

impl From<SplintRecord> for Splint {
    fn from(record: SplintRecord) -> Self {
        let title_set = record.title_set.unwrap_or(false) || record.title != DEFAULT_SPLINT_TITLE;
        Self {
            id: record.id,
            title: record.title,
            title_set,
            cwd: record.cwd,
            command: record.command,
            launch: record.launch,
            last_incarnation: record.last_incarnation,
            state: record.state,
        }
    }
}

impl Serialize for Splint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct SplintRecordRef<'a> {
            id: SplintId,
            title: &'a str,
            cwd: &'a PathBuf,
            command: &'a [String],
            launch: &'a SplintLaunchMetadata,
            last_incarnation: Option<u64>,
            state: SplintState,
            #[serde(skip_serializing_if = "Option::is_none")]
            title_set: Option<bool>,
        }
        SplintRecordRef {
            id: self.id,
            title: &self.title,
            cwd: &self.cwd,
            command: &self.command,
            launch: &self.launch,
            last_incarnation: self.last_incarnation,
            state: self.state,
            // Present only when the title alone cannot say it was set.
            title_set: (self.title_set && self.title == DEFAULT_SPLINT_TITLE).then_some(true),
        }
        .serialize(serializer)
    }
}

/// Binary layout tree used to arrange Splints inside a Dojo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutNode {
    Leaf(Splint),
    Branch {
        axis: Axis,
        ratio: SplitRatio,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl LayoutNode {
    #[must_use]
    pub const fn splint_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Branch { first, second, .. } => first.splint_count() + second.splint_count(),
        }
    }

    #[must_use]
    pub fn find_splint(&self, id: SplintId) -> Option<&Splint> {
        match self {
            Self::Leaf(splint) => (splint.id == id).then_some(splint),
            Self::Branch { first, second, .. } => {
                first.find_splint(id).or_else(|| second.find_splint(id))
            }
        }
    }

    pub fn find_splint_mut(&mut self, id: SplintId) -> Option<&mut Splint> {
        match self {
            Self::Leaf(splint) => (splint.id == id).then_some(splint),
            Self::Branch { first, second, .. } => first
                .find_splint_mut(id)
                .or_else(|| second.find_splint_mut(id)),
        }
    }

    #[must_use]
    pub const fn first_splint_id(&self) -> SplintId {
        match self {
            Self::Leaf(splint) => splint.id,
            Self::Branch { first, .. } => first.first_splint_id(),
        }
    }

    pub(crate) fn split(
        &mut self,
        target: SplintId,
        new_splint: Splint,
        axis: Axis,
        side: SplitSide,
        ratio: SplitRatio,
    ) -> bool {
        match self {
            Self::Leaf(splint) if splint.id == target => {
                let target = Self::Leaf(splint.clone());
                let inserted = Self::Leaf(new_splint);
                let (first, second) = match side {
                    SplitSide::First => (inserted, target),
                    SplitSide::Second => (target, inserted),
                };
                *self = Self::Branch {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Branch { first, second, .. } => {
                if first.find_splint(target).is_some() {
                    first.split(target, new_splint, axis, side, ratio)
                } else {
                    second.split(target, new_splint, axis, side, ratio)
                }
            }
        }
    }

    pub(crate) fn set_ancestor_ratio(
        &mut self,
        target: SplintId,
        ancestor: u16,
        ratio: SplitRatio,
    ) -> bool {
        fn path_to_target(node: &LayoutNode, target: SplintId, path: &mut Vec<bool>) -> bool {
            match node {
                LayoutNode::Leaf(splint) => splint.id == target,
                LayoutNode::Branch { first, second, .. } => {
                    path.push(false);
                    if path_to_target(first, target, path) {
                        return true;
                    }
                    path.pop();
                    path.push(true);
                    if path_to_target(second, target, path) {
                        return true;
                    }
                    path.pop();
                    false
                }
            }
        }

        let mut path = Vec::new();
        if !path_to_target(self, target, &mut path) || usize::from(ancestor) >= path.len() {
            return false;
        }
        let branch_depth = path.len() - 1 - usize::from(ancestor);
        let mut node = self;
        for second in path.into_iter().take(branch_depth) {
            let LayoutNode::Branch {
                first,
                second: second_node,
                ..
            } = node
            else {
                return false;
            };
            node = if second { second_node } else { first };
        }
        let LayoutNode::Branch { ratio: current, .. } = node else {
            return false;
        };
        *current = ratio;
        true
    }

    #[must_use]
    pub(crate) fn all_exited(&self) -> bool {
        match self {
            Self::Leaf(splint) => matches!(splint.state, SplintState::Exited(_)),
            Self::Branch { first, second, .. } => first.all_exited() && second.all_exited(),
        }
    }

    #[allow(
        clippy::result_large_err,
        reason = "a miss hands the subtree back by value; boxing it would allocate at every level"
    )]
    pub(crate) fn remove(self, target: SplintId) -> Result<Option<Self>, Self> {
        match self {
            Self::Leaf(splint) if splint.id == target => Ok(None),
            leaf @ Self::Leaf(_) => Err(leaf),
            Self::Branch {
                axis,
                ratio,
                first,
                second,
            } => match first.remove(target) {
                Ok(None) => Ok(Some(*second)),
                Ok(Some(first)) => Ok(Some(Self::Branch {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second,
                })),
                Err(first) => match second.remove(target) {
                    Ok(None) => Ok(Some(first)),
                    Ok(Some(second)) => Ok(Some(Self::Branch {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    })),
                    Err(second) => Err(Self::Branch {
                        axis,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                },
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_nested_splints() {
        let leaf = || LayoutNode::Leaf(Splint::shell(PathBuf::from("/tmp")));
        let layout = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(leaf()),
            second: Box::new(LayoutNode::Branch {
                axis: Axis::Vertical,
                ratio: SplitRatio::new(500).unwrap(),
                first: Box::new(leaf()),
                second: Box::new(leaf()),
            }),
        };

        assert_eq!(layout.splint_count(), 3);
    }

    #[test]
    fn ancestor_ratio_selects_the_requested_nested_branch() {
        let first = Splint::shell(PathBuf::from("/tmp"));
        let target = first.id;
        let mut layout = LayoutNode::Branch {
            axis: Axis::Horizontal,
            ratio: SplitRatio::new(500).unwrap(),
            first: Box::new(LayoutNode::Branch {
                axis: Axis::Vertical,
                ratio: SplitRatio::new(400).unwrap(),
                first: Box::new(LayoutNode::Leaf(first)),
                second: Box::new(LayoutNode::Leaf(Splint::shell(PathBuf::from("/tmp")))),
            }),
            second: Box::new(LayoutNode::Leaf(Splint::shell(PathBuf::from("/tmp")))),
        };

        assert!(layout.set_ancestor_ratio(target, 1, SplitRatio::new(700).unwrap()));
        let LayoutNode::Branch { ratio, first, .. } = layout else {
            panic!("expected outer branch");
        };
        assert_eq!(ratio.get(), 700);
        let LayoutNode::Branch { ratio, .. } = *first else {
            panic!("expected inner branch");
        };
        assert_eq!(ratio.get(), 400);
    }

    #[test]
    fn split_ratios_reject_empty_children() {
        assert_eq!(SplitRatio::new(0), Err(0));
        assert_eq!(SplitRatio::new(1).unwrap().get(), 1);
        assert_eq!(SplitRatio::new(999).unwrap().get(), 999);
        assert_eq!(SplitRatio::new(1000), Err(1000));
    }

    #[test]
    fn title_set_is_inferred_when_absent_and_written_only_when_needed() {
        let mut splint = Splint::shell(PathBuf::from("/tmp"));
        let mut value = serde_json::to_value(&splint).unwrap();
        // A default Splint serializes exactly as before the field existed.
        assert!(value.get("title_set").is_none());
        assert!(
            serde_json::from_value::<Splint>(value.clone())
                .unwrap()
                .uses_default_title()
        );

        // A Splint saved with a custom title before the field existed loads
        // as named.
        value["title"] = serde_json::json!("logs");
        let legacy: Splint = serde_json::from_value(value.clone()).unwrap();
        assert!(legacy.title_set);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("title_set")
                .is_none()
        );

        // A custom title is always set, whatever the field says.
        value["title_set"] = serde_json::json!(false);
        assert!(serde_json::from_value::<Splint>(value).unwrap().title_set);

        // Only a Splint pinned to the default text needs the field.
        splint.title_set = true;
        let pinned = serde_json::to_value(&splint).unwrap();
        assert_eq!(pinned["title_set"], serde_json::json!(true));
        let decoded: Splint = serde_json::from_value(pinned).unwrap();
        assert_eq!(decoded, splint);
        assert!(!decoded.uses_default_title());
    }

    #[test]
    fn splint_ids_use_canonical_uuid_text() {
        let id = SplintId::new();
        assert_eq!(id.to_string().parse(), Ok(id));
        assert!("not-an-id".parse::<SplintId>().is_err());
    }
}
