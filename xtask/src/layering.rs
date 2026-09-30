//! The workspace layering contract: `docs/architecture.md` §3 made machine-checkable.
//!
//! Dependencies must point strictly downwards (`L0` → `L1` → `L2` → `L3` → `L4` → frontends),
//! never sideways between crates of the same layer and never upwards. All three dependency kinds
//! `cargo metadata` reports are checked, because a rule that only sees one kind certifies less
//! than it claims:
//!
//! - **normal** and **build** edges form the build graph: strictly downward, acyclic, and no
//!   product crate may take a dev-only crate (`hatchery-testkit`, `xtask`) — that would ship
//!   test code.
//! - **dev** edges may point at the dev-only crates (the sanctioned way to use the testkit), but
//!   every other dev edge must be strictly downward too. Cargo permits dev-dependency cycles and
//!   upward dev edges, so the direction rule is the only thing standing between the contract and
//!   a test-only coupling that inverts it: `[dev-dependencies] hatchery-kernel` in
//!   `hatchery-protocol` would be an `L0` → `L1` edge wearing a test costume.
//! - Edges whose *target* is a dev-only crate are exempt from the direction rule, and dev edges
//!   do not join the cycle walk: `hatchery-kernel` dev-depends on `hatchery-testkit` while the
//!   testkit normal-depends on the kernel — a cycle in the union graph, but not in the build
//!   graph cargo actually compiles.
//!
//! `hatchery-protocol` sits at the bottom because its vocabulary (ids, `Content`, `ToolOutput`,
//! `ApprovalRequest`, `Usage`) is shared: the kernel needs those types too, and a sideways `L0`
//! edge would be a cycle waiting to happen. The "kernel must not see `capabilities`" rule from
//! ADR-0004 is unaffected — that edge points *upwards* and stays forbidden.
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
    ("hatchery-kernel", Layer::L1),
    ("hatchery-llm", Layer::L2),
    ("hatchery-store", Layer::L2),
    ("hatchery-capabilities", Layer::L2),
    ("hatchery-tools", Layer::L3),
    ("hatchery-acp", Layer::L3),
    ("hatchery-daemon", Layer::L4),
    ("hatchery-cli", Layer::Frontend),
    ("hatchery-gui", Layer::Frontend),
    ("hatchery-testkit", Layer::Dev),
    ("xtask", Layer::Dev),
];

/// Position in the dependency order. Higher may depend on lower, never the reverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    /// The shared vocabulary: wire types, ids, content, tool and approval value types.
    L0,
    /// The neutral agent loop: turn state machine plus its injected traits.
    L1,
    /// Provider adapters, storage, capability seam.
    L2,
    /// Tools and ACP.
    L3,
    /// Runtime host.
    L4,
    /// CLI and GTK frontends.
    Frontend,
    /// Dev-only crates (`hatchery-testkit`, `xtask`): any crate may depend on them from
    /// `[dev-dependencies]`, and they may depend on anything.
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
            Self::L4 => "L4",
            Self::Frontend => "frontend",
            Self::Dev => "dev",
        };
        f.write_str(name)
    }
}

/// How a dependency is declared, as `cargo metadata` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DepKind {
    /// `[dependencies]` — part of the build graph.
    Normal,
    /// `[build-dependencies]` — part of the build graph, and just as shippable: a build script
    /// runs on every user's machine.
    Build,
    /// `[dev-dependencies]` — built only for this crate's own tests and examples.
    Dev,
}

impl DepKind {
    fn from_metadata(kind: Option<&str>) -> Self {
        match kind {
            None => Self::Normal,
            Some("dev") => Self::Dev,
            // Cargo reports no other kinds; an unknown spelling is treated as part of the build
            // graph, which is the strictest reading.
            Some(_) => Self::Build,
        }
    }

    /// True when the edge is part of the graph cargo compiles in dependency order.
    fn in_build_graph(self) -> bool {
        self != Self::Dev
    }
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Normal => "normal",
            Self::Build => "build",
            Self::Dev => "dev",
        };
        f.write_str(name)
    }
}

/// One dependency edge between workspace members.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    /// The crate depended on.
    pub to: String,
    /// How the dependency is declared.
    pub kind: DepKind,
}

/// Outcome of a successful check.
#[derive(Clone, Copy, Debug)]
pub struct Report {
    /// Number of workspace members.
    pub members: usize,
    /// Build-graph edges (normal + build) between members.
    pub edges: usize,
    /// Dev-only edges between members.
    pub dev_edges: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "layering ok: {} members, {} build edges + {} dev edges, strictly downward, no cycles",
            self.members, self.edges, self.dev_edges
        )
    }
}

type Graph = BTreeMap<String, BTreeSet<Edge>>;

/// Verifies the whole workspace and reports what was checked.
pub fn check() -> Result<Report> {
    let root = workspace_root()?;
    let packages = cargo_metadata(&root)?;
    let members = members(&packages)?;
    let edges = member_edges(&packages, &members);
    reject_dev_crates_as_product_deps(&edges, &members)?;
    reject_layer_violations(&edges)?;
    reject_cycles(&edges)?;
    let all: Vec<&Edge> = edges.values().flatten().collect();
    Ok(Report {
        members: members.len(),
        edges: all.iter().filter(|edge| edge.kind.in_build_graph()).count(),
        dev_edges: all.iter().filter(|edge| edge.kind == DepKind::Dev).count(),
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
             xtask::layering::LAYERS and docs/architecture.md §3"
        ));
    }
    Ok(found)
}

/// Every member-to-member dependency edge, of every kind.
fn member_edges(packages: &[Package], members: &BTreeSet<String>) -> Graph {
    let mut edges: Graph = members
        .iter()
        .map(|m| (m.clone(), BTreeSet::new()))
        .collect();
    for package in packages {
        let Some(from) = edges.get_mut(&package.name) else {
            continue;
        };
        for dep in &package.dependencies {
            if members.contains(&dep.name) {
                from.insert(Edge {
                    to: dep.name.clone(),
                    kind: DepKind::from_metadata(dep.kind.as_deref()),
                });
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
        for edge in &edges[product] {
            // A dev-dependency on the testkit is the sanctioned pattern; anything else that pulls
            // a dev crate into a product crate ships test code to users.
            if edge.kind == DepKind::Dev {
                continue;
            }
            if Layer::of(&edge.to).is_some_and(Layer::is_dev) {
                return Err(anyhow!(
                    "{product} takes dev-only crate {} as a {} dependency; only \
                     [dev-dependencies] may point at dev crates, so test code is never shipped",
                    edge.to,
                    edge.kind
                ));
            }
        }
    }
    Ok(())
}

fn reject_layer_violations(edges: &Graph) -> Result<()> {
    for (from, deps) in edges {
        let from_layer = layer_or_err(from)?;
        for edge in deps {
            let to_layer = layer_or_err(&edge.to)?;
            // Dev-only crates sit above everything: any crate may use them from its tests, and
            // they may use each other.
            if to_layer.is_dev() {
                continue;
            }
            if from_layer <= to_layer {
                return Err(anyhow!(
                    "{from} ({from_layer}) depends on {} ({to_layer}) via a {} dependency: \
                     dependencies must point strictly downwards (docs/architecture.md §3)",
                    edge.to,
                    edge.kind
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
    // Dev edges stay out of the walk on purpose — see the module docs. The build graph is what
    // cargo compiles in dependency order, so it is what must be acyclic.
    let build: Graph = edges
        .iter()
        .map(|(from, deps)| {
            (
                from.clone(),
                deps.iter()
                    .filter(|edge| edge.kind.in_build_graph())
                    .cloned()
                    .collect(),
            )
        })
        .collect();
    let mut settled: BTreeSet<String> = BTreeSet::new();
    for node in build.keys().cloned().collect::<Vec<_>>() {
        let mut seen = BTreeSet::new();
        let mut path = Vec::new();
        visit(&node, &build, &mut settled, &mut seen, &mut path)?;
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
    for edge in edges.get(node).into_iter().flatten() {
        visit(&edge.to, edges, settled, seen, path)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn members_all() -> BTreeSet<String> {
        LAYERS.iter().map(|(name, _)| (*name).to_owned()).collect()
    }

    fn edge(to: &str, kind: DepKind) -> Edge {
        Edge {
            to: to.to_owned(),
            kind,
        }
    }

    fn graph(entries: &[(&str, &[Edge])]) -> Graph {
        let mut graph: Graph = members_all()
            .iter()
            .map(|m| (m.clone(), BTreeSet::new()))
            .collect();
        for (from, edges) in entries {
            graph
                .get_mut(*from)
                .expect("a real member")
                .extend(edges.iter().cloned());
        }
        graph
    }

    #[test]
    fn downward_edges_of_every_kind_pass() {
        let edges = graph(&[
            (
                "hatchery-kernel",
                &[edge("hatchery-protocol", DepKind::Normal)],
            ),
            (
                "hatchery-daemon",
                &[edge("hatchery-kernel", DepKind::Build)],
            ),
            ("hatchery-daemon", &[edge("hatchery-store", DepKind::Dev)]),
        ]);
        reject_dev_crates_as_product_deps(&edges, &members_all()).expect("no dev crate shipped");
        reject_layer_violations(&edges).expect("strictly downward");
        reject_cycles(&edges).expect("acyclic");
    }

    #[test]
    fn an_upward_dev_edge_is_rejected() {
        // The prospective hole this rewrite closes: cargo permits it, the contract must not.
        let edges = graph(&[(
            "hatchery-protocol",
            &[edge("hatchery-kernel", DepKind::Dev)],
        )]);
        let error = reject_layer_violations(&edges)
            .expect_err("L0 must not depend on L1, not even from [dev-dependencies]");
        assert!(
            error.to_string().contains("dev dependency"),
            "the message must name the kind that violated: {error}"
        );
    }

    #[test]
    fn a_sideways_dev_edge_is_rejected() {
        let edges = graph(&[("hatchery-store", &[edge("hatchery-llm", DepKind::Dev)])]);
        reject_layer_violations(&edges).expect_err("two L2 crates must not depend on each other");
    }

    #[test]
    fn a_product_crate_may_dev_depend_on_the_testkit() {
        let edges = graph(&[("hatchery-kernel", &[edge("hatchery-testkit", DepKind::Dev)])]);
        reject_dev_crates_as_product_deps(&edges, &members_all()).expect("the sanctioned pattern");
        reject_layer_violations(&edges).expect("dev crates sit above everything");
    }

    #[test]
    fn a_build_dependency_on_a_dev_crate_is_rejected() {
        // A build script runs on every user's machine, so this ships test code just like a
        // normal dependency would.
        let edges = graph(&[(
            "hatchery-daemon",
            &[edge("hatchery-testkit", DepKind::Build)],
        )]);
        let error = reject_dev_crates_as_product_deps(&edges, &members_all())
            .expect_err("test code must never be shipped");
        assert!(error.to_string().contains("build dependency"), "{error}");
    }

    #[test]
    fn dev_crates_may_depend_on_each_other_and_on_products() {
        let edges = graph(&[
            (
                "hatchery-testkit",
                &[edge("hatchery-kernel", DepKind::Normal)],
            ),
            ("hatchery-testkit", &[edge("xtask", DepKind::Normal)]),
            ("xtask", &[edge("hatchery-testkit", DepKind::Normal)]),
        ]);
        reject_layer_violations(&edges).expect("dev sources are above every product layer");
    }

    #[test]
    fn the_kernel_testkit_dev_cycle_is_not_a_build_cycle() {
        // kernel dev-depends on testkit (its tests use the fakes) while testkit normal-depends
        // on kernel (the fakes implement its seams). Cargo compiles this fine: the dev edge is
        // not part of the build graph.
        let edges = graph(&[
            ("hatchery-kernel", &[edge("hatchery-testkit", DepKind::Dev)]),
            (
                "hatchery-testkit",
                &[edge("hatchery-kernel", DepKind::Normal)],
            ),
        ]);
        reject_cycles(&edges).expect("a dev edge does not close a build cycle");
    }

    #[test]
    fn a_cycle_in_the_build_graph_is_rejected() {
        // Both crates are Dev, so the direction rule is exempt and only the cycle walk can see
        // this — which is why dev crates are not exempt from acyclicity either.
        let edges = graph(&[
            ("hatchery-testkit", &[edge("xtask", DepKind::Normal)]),
            ("xtask", &[edge("hatchery-testkit", DepKind::Normal)]),
        ]);
        let error = reject_cycles(&edges).expect_err("the build graph must be acyclic");
        assert!(error.to_string().contains("dependency cycle"), "{error}");
    }

    #[test]
    fn metadata_kinds_map_onto_the_three_declared_tables() {
        assert_eq!(DepKind::from_metadata(None), DepKind::Normal);
        assert_eq!(DepKind::from_metadata(Some("dev")), DepKind::Dev);
        assert_eq!(DepKind::from_metadata(Some("build")), DepKind::Build);
        assert_eq!(
            DepKind::from_metadata(Some("something-new")),
            DepKind::Build,
            "an unknown kind is read as part of the build graph: the strictest interpretation"
        );
    }
}
