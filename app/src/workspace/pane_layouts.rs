use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use pathfinder_geometry::rect::RectF;
use pathfinder_geometry::vector::{Vector2F, vec2f};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use warpui::elements::flex_sizes_with_minimums;

const FORMAT_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_LAYOUT_DEPTH: usize = 32;
const MAX_NAME_BYTES: usize = 256;
const MAX_TOPOLOGY_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LayoutKind {
    Template,
    Snapshot,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LayoutAxis {
    Horizontal,
    Vertical,
}

impl LayoutAxis {
    fn along(self, size: Vector2F) -> f32 {
        match self {
            Self::Horizontal => size.x(),
            Self::Vertical => size.y(),
        }
    }

    fn across(self, size: Vector2F) -> f32 {
        match self {
            Self::Horizontal => size.y(),
            Self::Vertical => size.x(),
        }
    }

    fn vector(self, along: f32, across: f32) -> Vector2F {
        match self {
            Self::Horizontal => vec2f(along, across),
            Self::Vertical => vec2f(across, along),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LayoutNode<T> {
    Pane {
        session: T,
    },
    Stack {
        sessions: Vec<T>,
    },
    Split {
        axis: LayoutAxis,
        children: Vec<LayoutChild<T>>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutChild<T> {
    pub(crate) weight: f32,
    pub(crate) node: LayoutNode<T>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutTab<T = Option<Uuid>> {
    pub(crate) name: Option<String>,
    pub(crate) root: LayoutNode<T>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutDocument {
    version: u32,
    pub(crate) name: String,
    pub(crate) kind: LayoutKind,
    pub(crate) tabs: Vec<LayoutTab>,
}

impl LayoutDocument {
    pub(crate) fn new(name: String, kind: LayoutKind, tabs: Vec<LayoutTab>) -> Result<Self> {
        let document = Self {
            version: FORMAT_VERSION,
            name: name.trim().to_owned(),
            kind,
            tabs,
        };
        document.validate()?;
        Ok(document)
    }

    pub(crate) fn from_json(json: &str) -> Result<Self> {
        ensure!(
            json.len() <= MAX_DOCUMENT_BYTES,
            "The layout document is too large."
        );
        let document: Self = serde_json::from_str(json).context("Invalid pane layout JSON")?;
        document.validate()?;
        Ok(document)
    }

    pub(crate) fn portable_json(&self) -> Result<String> {
        let mut document = self.clone();
        document.kind = LayoutKind::Template;
        for tab in &mut document.tabs {
            tab.root.clear_session_ids();
        }
        let json = serde_json::to_string_pretty(&document)?;
        ensure!(
            json.len() <= MAX_DOCUMENT_BYTES,
            "The layout document is too large."
        );
        Ok(json)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == FORMAT_VERSION,
            "Unsupported pane layout version."
        );
        ensure!(valid_name(&self.name), "Enter a valid pane layout name.");
        ensure!(
            !self.tabs.is_empty(),
            "A layout must contain at least one tab."
        );

        let mut bound_sessions = HashSet::new();
        for tab in &self.tabs {
            if let Some(name) = &tab.name {
                ensure!(valid_name(name), "The layout contains an invalid tab name.");
            }
            tab.root.validate(self.kind, 0, &mut bound_sessions)?;
        }
        Ok(())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.trim() == name
        && name.len() <= MAX_NAME_BYTES
        && !name.chars().any(char::is_control)
}

impl LayoutNode<Option<Uuid>> {
    fn clear_session_ids(&mut self) {
        match self {
            Self::Pane { session } => *session = None,
            Self::Stack { sessions } => sessions.fill(None),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.clear_session_ids();
                }
            }
        }
    }

    fn validate(
        &self,
        kind: LayoutKind,
        depth: usize,
        bound_sessions: &mut HashSet<Uuid>,
    ) -> Result<()> {
        ensure!(
            depth <= MAX_LAYOUT_DEPTH,
            "The layout is nested too deeply."
        );
        let mut validate_slot = |session: Option<Uuid>| {
            if let Some(session) = session {
                ensure!(
                    kind == LayoutKind::Snapshot,
                    "Templates cannot contain session IDs."
                );
                ensure!(
                    bound_sessions.insert(session),
                    "A session appears more than once."
                );
            }
            Ok(())
        };

        match self {
            Self::Pane { session } => validate_slot(*session),
            Self::Stack { sessions } => {
                ensure!(
                    sessions.len() >= 2,
                    "A stack must contain at least two panes."
                );
                sessions.iter().copied().try_for_each(validate_slot)
            }
            Self::Split { children, .. } => {
                ensure!(
                    children.len() >= 2,
                    "A split must contain at least two children."
                );
                for child in children {
                    ensure!(
                        (0.000001..=1_000_000.).contains(&child.weight),
                        "A split has an invalid proportion."
                    );
                    child.node.validate(kind, depth + 1, bound_sessions)?;
                }
                Ok(())
            }
        }
    }

    fn collect_bound_sessions(&self, bound: &mut HashSet<Uuid>) {
        match self {
            Self::Pane { session } => bound.extend(session.iter().copied()),
            Self::Stack { sessions } => bound.extend(sessions.iter().flatten().copied()),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.collect_bound_sessions(bound);
                }
            }
        }
    }

    fn resolve(&self, assignment: &mut SessionAssignment) -> Option<LayoutNode<Uuid>> {
        match self {
            Self::Pane { session } => assignment
                .resolve(*session)
                .map(|session| LayoutNode::Pane { session }),
            Self::Stack { sessions } => LayoutNode::from_sessions(
                sessions
                    .iter()
                    .filter_map(|session| assignment.resolve(*session))
                    .collect(),
            ),
            Self::Split { axis, children } => {
                let mut children: Vec<_> = children
                    .iter()
                    .filter_map(|child| {
                        Some(LayoutChild {
                            weight: child.weight,
                            node: child.node.resolve(assignment)?,
                        })
                    })
                    .collect();
                match children.len() {
                    0 => None,
                    1 => children.pop().map(|child| child.node),
                    _ => Some(LayoutNode::Split {
                        axis: *axis,
                        children,
                    }),
                }
            }
        }
    }
}

struct SessionAssignment {
    remaining: HashSet<Uuid>,
    unbound: VecDeque<Uuid>,
    missing_slots: usize,
}

impl SessionAssignment {
    fn resolve(&mut self, session: Option<Uuid>) -> Option<Uuid> {
        let resolved = session
            .or_else(|| self.unbound.pop_front())
            .filter(|session| self.remaining.remove(session));
        if resolved.is_none() {
            self.missing_slots += 1;
        }
        resolved
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayoutSpace {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) minimum: f32,
    pub(crate) divider: f32,
}

impl LayoutSpace {
    fn size(self) -> Vector2F {
        vec2f(self.width, self.height)
    }

    fn validate(self) -> Result<()> {
        ensure!(
            [self.width, self.height, self.minimum, self.divider]
                .iter()
                .all(|value| value.is_finite() && (0.0..=1_000_000.0).contains(value))
                && self.minimum > 0.,
            "The window has invalid layout dimensions."
        );
        ensure!(
            self.width >= self.minimum && self.height >= self.minimum,
            "Enlarge the window so it can fit a terminal."
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayoutStrategy {
    Preserve,
    Balance,
    Stack,
    Tabs,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LayoutPlan {
    pub(crate) tabs: Vec<LayoutTab<Uuid>>,
    pub(crate) missing_slots: usize,
    pub(crate) overflow_sessions: usize,
    pub(crate) strategy: LayoutStrategy,
}

#[derive(Debug)]
pub(crate) struct LayoutPreviewPane {
    pub(crate) session: Uuid,
    pub(crate) bounds: RectF,
    pub(crate) stack_size: usize,
}

impl LayoutPlan {
    pub(crate) fn preview_panes(
        &self,
        tab_index: usize,
        space: LayoutSpace,
    ) -> Vec<LayoutPreviewPane> {
        let mut panes = Vec::new();
        if let Some(tab) = self.tabs.get(tab_index) {
            tab.root.collect_preview(
                RectF::new(Vector2F::zero(), space.size()),
                space,
                &mut panes,
            );
        }
        panes
    }
}

pub(crate) fn plan_layout(
    document: &LayoutDocument,
    sessions: &[Uuid],
    space: LayoutSpace,
    strategy: LayoutStrategy,
) -> Result<LayoutPlan> {
    document.validate()?;
    space.validate()?;

    let remaining: HashSet<_> = sessions.iter().copied().collect();
    ensure!(
        remaining.len() == sessions.len(),
        "Live pane identities are ambiguous."
    );
    ensure!(
        !sessions.is_empty(),
        "Open a terminal before applying a pane layout."
    );

    let mut bound = HashSet::new();
    for tab in &document.tabs {
        tab.root.collect_bound_sessions(&mut bound);
    }
    let mut assignment = SessionAssignment {
        remaining,
        unbound: sessions
            .iter()
            .filter(|session| !bound.contains(*session))
            .copied()
            .collect(),
        missing_slots: 0,
    };
    let mut tabs = Vec::new();
    for tab in &document.tabs {
        let Some(mut root) = tab.root.resolve(&mut assignment) else {
            continue;
        };
        match strategy {
            LayoutStrategy::Balance => {
                root.balance();
                ensure!(
                    size_fits(space.size(), root.minimum_size(space)),
                    "This layout needs more room even after balancing. Try stacks or separate tabs."
                );
            }
            LayoutStrategy::Preserve | LayoutStrategy::Stack | LayoutStrategy::Tabs
                if root.preserves_proportions(space.size(), space) => {}
            LayoutStrategy::Preserve => anyhow::bail!(
                "This layout would make panes too small. Try balancing, stacks, or separate tabs."
            ),
            LayoutStrategy::Stack => {
                root = LayoutNode::from_sessions(root.into_sessions())
                    .expect("a resolved tab has at least one pane");
            }
            LayoutStrategy::Tabs => {
                tabs.extend(root.into_sessions().into_iter().enumerate().map(
                    |(index, session)| LayoutTab {
                        name: if index == 0 { tab.name.clone() } else { None },
                        root: LayoutNode::Pane { session },
                    },
                ));
                continue;
            }
        }
        tabs.push(LayoutTab {
            name: tab.name.clone(),
            root,
        });
    }

    ensure!(
        !tabs.is_empty(),
        "None of this layout’s saved sessions are still open."
    );
    let overflow_sessions = assignment.remaining.len();
    tabs.extend(
        sessions
            .iter()
            .filter(|session| assignment.remaining.contains(*session))
            .map(|&session| LayoutTab {
                name: None,
                root: LayoutNode::Pane { session },
            }),
    );
    Ok(LayoutPlan {
        tabs,
        missing_slots: assignment.missing_slots,
        overflow_sessions,
        strategy,
    })
}

fn size_fits(size: Vector2F, minimum: Vector2F) -> bool {
    const PIXEL_TOLERANCE: f32 = 0.01;
    size.x() + PIXEL_TOLERANCE >= minimum.x() && size.y() + PIXEL_TOLERANCE >= minimum.y()
}

impl LayoutNode<Uuid> {
    fn from_sessions(mut sessions: Vec<Uuid>) -> Option<Self> {
        match sessions.len() {
            0 => None,
            1 => sessions.pop().map(|session| Self::Pane { session }),
            _ => Some(Self::Stack { sessions }),
        }
    }

    fn into_sessions(self) -> Vec<Uuid> {
        match self {
            Self::Pane { session } => vec![session],
            Self::Stack { sessions } => sessions,
            Self::Split { children, .. } => children
                .into_iter()
                .flat_map(|child| child.node.into_sessions())
                .collect(),
        }
    }

    fn balance(&mut self) {
        if let Self::Split { children, .. } = self {
            for child in children {
                child.weight = 1.;
                child.node.balance();
            }
        }
    }

    fn minimum_size(&self, space: LayoutSpace) -> Vector2F {
        match self {
            Self::Pane { .. } | Self::Stack { .. } => vec2f(space.minimum, space.minimum),
            Self::Split { axis, children } => {
                let mut along = (children.len() - 1) as f32 * space.divider;
                let mut across = 0_f32;
                for child in children {
                    let size = child.node.minimum_size(space);
                    along += axis.along(size);
                    across = across.max(axis.across(size));
                }
                axis.vector(along, across)
            }
        }
    }

    fn preserves_proportions(&self, size: Vector2F, space: LayoutSpace) -> bool {
        match self {
            Self::Pane { .. } | Self::Stack { .. } => size_fits(size, self.minimum_size(space)),
            Self::Split { axis, children } => {
                let available = axis.along(size) - (children.len() - 1) as f32 * space.divider;
                let total: f32 = children.iter().map(|child| child.weight).sum();
                children.iter().all(|child| {
                    let along = available * (child.weight / total);
                    child
                        .node
                        .preserves_proportions(axis.vector(along, axis.across(size)), space)
                })
            }
        }
    }

    fn collect_preview(
        &self,
        bounds: RectF,
        space: LayoutSpace,
        output: &mut Vec<LayoutPreviewPane>,
    ) {
        match self {
            Self::Pane { session } => output.push(LayoutPreviewPane {
                session: *session,
                bounds,
                stack_size: 1,
            }),
            Self::Stack { sessions } => output.push(LayoutPreviewPane {
                session: sessions[0],
                bounds,
                stack_size: sessions.len(),
            }),
            Self::Split { axis, children } => {
                let size = bounds.size();
                let available =
                    (axis.along(size) - (children.len() - 1) as f32 * space.divider).max(0.);
                let weights: Vec<_> = children
                    .iter()
                    .map(|child| (child.weight, axis.along(child.node.minimum_size(space))))
                    .collect();
                let mut origin = bounds.origin();
                for (child, along) in children
                    .iter()
                    .zip(flex_sizes_with_minimums(available, &weights))
                {
                    child.node.collect_preview(
                        RectF::new(origin, axis.vector(along, axis.across(size))),
                        space,
                        output,
                    );
                    origin += axis.vector(along + space.divider, 0.);
                }
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayoutCatalog {
    version: u32,
    documents: Vec<LayoutDocument>,
    #[serde(default)]
    display_profiles: BTreeMap<String, String>,
}

impl Default for LayoutCatalog {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            documents: Vec::new(),
            display_profiles: BTreeMap::new(),
        }
    }
}

impl LayoutCatalog {
    pub(crate) fn load() -> Result<Self> {
        Self::load_from_path(&catalog_path()?)
    }

    pub(crate) fn documents(&self) -> &[LayoutDocument] {
        &self.documents
    }

    pub(crate) fn get(&self, name: &str) -> Option<&LayoutDocument> {
        self.documents
            .iter()
            .find(|document| document.name.eq_ignore_ascii_case(name))
    }

    pub(crate) fn save_document(document: LayoutDocument) -> Result<()> {
        Self::update(&catalog_path()?, |catalog| catalog.add_document(document))
    }

    pub(crate) fn set_display_profile(topology: String, layout_name: &str) -> Result<()> {
        Self::update(&catalog_path()?, |catalog| {
            catalog.bind_display(topology, layout_name)
        })
    }

    pub(crate) fn display_profile(&self, topology: &str) -> Option<&LayoutDocument> {
        self.display_profiles
            .get(topology)
            .and_then(|name| self.get(name))
    }

    fn add_document(&mut self, document: LayoutDocument) -> Result<()> {
        document.validate()?;
        ensure!(
            self.get(&document.name).is_none(),
            "That pane layout name is already in use."
        );
        self.documents.push(document);
        self.documents
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(())
    }

    fn bind_display(&mut self, topology: String, layout_name: &str) -> Result<()> {
        ensure!(
            valid_topology(&topology),
            "There is no usable display topology."
        );
        let name = self
            .get(layout_name)
            .context("The saved pane layout no longer exists.")?
            .name
            .clone();
        self.display_profiles.insert(topology, name);
        Ok(())
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error).context("Could not open the pane layout catalog"),
        };
        let mut bytes = Vec::new();
        file.take((MAX_CATALOG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= MAX_CATALOG_BYTES,
            "The pane layout catalog is too large."
        );
        let catalog: Self =
            serde_json::from_slice(&bytes).context("Invalid pane layout catalog")?;
        catalog.validate()?;
        Ok(catalog)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == FORMAT_VERSION,
            "Unsupported pane layout catalog version."
        );
        let mut names = HashSet::new();
        for document in &self.documents {
            document.validate()?;
            ensure!(
                names.insert(document.name.to_ascii_lowercase()),
                "The catalog contains duplicate layout names."
            );
        }
        for (topology, name) in &self.display_profiles {
            ensure!(
                valid_topology(topology) && names.contains(&name.to_ascii_lowercase()),
                "The catalog contains an invalid display profile."
            );
        }
        Ok(())
    }

    fn update(path: &Path, edit: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        let directory = path
            .parent()
            .context("The pane layout catalog has no directory.")?;
        fs::create_dir_all(directory)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        lock.try_lock()
            .context("Could not lock the pane layout catalog")?;

        let mut catalog = Self::load_from_path(path)?;
        edit(&mut catalog)?;
        let bytes = serde_json::to_vec(&catalog)?;
        ensure!(
            bytes.len() <= MAX_CATALOG_BYTES,
            "The pane layout catalog is full."
        );

        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(path)
            .context("Could not save the pane layout catalog")?;
        Ok(())
    }
}

fn valid_topology(topology: &str) -> bool {
    !topology.is_empty()
        && topology.len() <= MAX_TOPOLOGY_BYTES
        && !topology.chars().any(char::is_control)
}

fn catalog_path() -> Result<PathBuf> {
    let directory = warp_core::paths::state_dir();
    ensure!(
        directory.is_absolute(),
        "Could not find the Warp profile directory."
    );
    Ok(directory.join("pane-layouts.json"))
}

#[cfg(test)]
#[path = "pane_layouts_tests.rs"]
mod tests;
