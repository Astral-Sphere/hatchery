//! The workspace layering contract: `docs/architecture.md` §3 made machine-checkable.
//!
//! Dependencies must point strictly downwards (`L0` → `L1` → `L2` → `L3` → frontends), never
//! sideways between crates of the same layer and never upwards, and the graph must be acyclic.
//! Dev-only crates sit above everything and may depend on any product crate, but no product crate
//! may take a dev crate as a normal dependency — that would ship test code.
//!
//! Runs as `cargo xtask layering` and as an integration test on every `cargo nextest run`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

/// Layer assignment, mirroring `docs/architecture.md` §3. The two must stay in sync: [`check`]
/// fails when a workspace member is missing from this table or a listed crate is not a member.
pub const LAYERS: &[(&str, Layer)] = &[
    ("hatchery-protocol", Layer::L0),
    ("hatchery-kernel", Layer::L0),
    ("hatchery-llm", Layer::L1),
    ("hatchery-store", Layer::L1),
    ("hatchery-capabilities", Layer::L1),
    ("hatchery-tools", Layer::L2),
    ("hatchery-acp", Layer::L2),
    ("hatchery-daemon", Layer::L3),
    ("hatchery-cli", Layer::Frontend),
    ("hatchery-gui", Layer::Frontend),
    ("hatchery-testkit", Layer::Dev),
    ("xtask", Layer::Dev),
];

/// Position in the dependency order. Higher may depend on lower, never the reverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// Wire types and the neutral agent loop.
    L0,
    /// Provider adapters, storage, capability seam.
    L1,
    /// Tools and ACP.
    L2,
    /// Runtime host.
    L3,
    /// CLI and GTK frontends.
    Frontend,
    /// Dev-only crates (`hatchery-testkit`, `xtask`): exempt from the downward rule.
    Dev,
}

impl Layer {
    /// Layer of a workspace member, if it is listed in [`LAYERS`].
    pub fn of(crate_name: &str) -> Option<Self> {
        LAYERS
            .iter()
            .find(|(name, _)| *name == crate_name)
            .map(|(_, layer)| *layer)
    }

    fn is_dev(self) -> bool {
        self == Self::Dev
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::L0 => "L0",
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
            Self::Frontend => "frontend",
            Self::Dev => "dev",
        };
        f.write_str(name)
    }
}

/// Outcome of a successful check.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    /// Number of workspace members.
    pub members: usize,
    /// Number of normal (non-dev, non-build) dependency edges between members.
    pub edges: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "layering ok: {} members, {} dependency edges, strictly downward, no cycles",
            self.members, self.edges
        )
    }
}

type Graph = BTreeMap<String, BTreeSet<String>>;

/// Verifies the whole workspace and reports what was checked.
pub fn check() -> Result<Report> {
    let root = workspace_root()?;
    let packages = cargo_metadata(&root)?;
    let members = members(&packages)?;
    let edges = normal_edges(&packages, &members);
    reject_dev_crates_as_product_deps(&edges, &members)?;
    reject_layer_violations(&edges)?;
    reject_cycles(&edges)?;
    Ok(Report {
        members: members.len(),
        edges: edges.values().map(BTreeSet::len).sum(),
    })
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("xtask must live one level below the workspace root"))
}

fn cargo_metadata(root: &Path) -> Result<Vec<Package>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("failed to run `cargo metadata --no-deps`")?;
    if !output.status.success() {
        return Err(anyhow!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Metadata =
        serde_json::from_slice(&output.stdout).context("unparsable `cargo metadata` output")?;
    Ok(metadata.packages)
}

fn members(packages: &[Package]) -> Result<BTreeSet<String>> {
    let found: BTreeSet<String> = packages.iter().map(|p| p.name.clone()).collect();
    let listed: BTreeSet<String> = LAYERS.iter().map(|(name, _)| (*name).to_owned()).collect();

    let unlisted: Vec<&String> = found.difference(&listed).collect();
    if !unlisted.is_empty() {
        return Err(anyhow!(
            "workspace members missing from the layer table: {unlisted:?} — add them to \
             xtask::layering::LAYERS and to docs/architecture.md §3"
        ));
    }
    let absent: Vec<&String> = listed.difference(&found).collect();
    if !absent.is_empty() {
        return Err(anyhow!(
            "listed in the layer table but not a workspace member: {absent:?} — remove them from \
             xtask::layering::LAYERS and from docs/architecture.md §3"
        ));
    }
    Ok(found)
}

fn normal_edges(packages: &[Package], members: &BTreeSet<String>) -> Graph {
    let mut edges: Graph = members
        .iter()
        .map(|m| (m.clone(), BTreeSet::new()))
        .collect();
    for package in packages {
        let Some(deps) = edges.get_mut(&package.name) else {
            continue;
        };
        for dep in &package.dependencies {
            let is_normal = dep.kind.is_none();
            if is_normal && members.contains(&dep.name) {
                deps.insert(dep.name.clone());
            }
        }
    }
    edges
}

fn reject_dev_crates_as_product_deps(edges: &Graph, members: &BTreeSet<String>) -> Result<()> {
    for product in members {
        if Layer::of(product).is_some_and(Layer::is_dev) {
            continue;
        }
        for dep in &edges[product] {
            if Layer::of(dep).is_some_and(Layer::is_dev) {
                return Err(anyhow!(
                    "{product} takes dev-only crate {dep} as a normal dependency; use \
                     [dev-dependencies] so test code is never shipped"
                ));
            }
        }
    }
    Ok(())
}

fn reject_layer_violations(edges: &Graph) -> Result<()> {
    for (from, deps) in edges {
        let from_layer = layer_or_err(from)?;
        for to in deps {
            let to_layer = layer_or_err(to)?;
            if from_layer <= to_layer {
                return Err(anyhow!(
                    "{from} ({from_layer}) depends on {to} ({to_layer}): dependencies must point \
                     strictly downwards (docs/architecture.md §3)"
                ));
            }
        }
    }
    Ok(())
}

fn layer_or_err(crate_name: &str) -> Result<Layer> {
    Layer::of(crate_name).ok_or_else(|| {
        anyhow!("{crate_name} is not in the layer table; update docs/architecture.md §3")
    })
}

fn reject_cycles(edges: &Graph) -> Result<()> {
    let mut settled: BTreeSet<String> = BTreeSet::new();
    for node in edges.keys().cloned().collect::<Vec<_>>() {
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        visit(&node, edges, &mut settled, &mut seen, &mut path)?;
    }
    Ok(())
}

fn visit(
    node: &str,
    edges: &Graph,
    settled: &mut BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    path: &mut Vec<String>,
) -> Result<()> {
    if settled.contains(node) {
        return Ok(());
    }
    if path.iter().any(|visited| visited == node) {
        let cycle = path
            .iter()
            .skip_while(|visited| *visited != node)
            .cloned()
            .chain([node.to_owned()])
            .collect::<Vec<_>>()
            .join(" -> ");
        return Err(anyhow!("dependency cycle: {cycle}"));
    }
    if !seen.insert(node.to_owned()) {
        return Ok(());
    }

    path.push(node.to_owned());
    for dep in edges.get(node).into_iter().flatten() {
        visit(dep, edges, settled, seen, path)?;
    }
    path.pop();

    settled.insert(node.to_owned());
    Ok(())
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    /// `null` for normal dependencies, `"dev"` or `"build"` otherwise.
    kind: Option<String>,
}
