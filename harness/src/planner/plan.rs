//! The plan DAG: which identities run which subtasks, and in what order.

use crate::identity::{DevLoop, IdentityCatalog};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use thiserror::Error;

/// Identifier for one node in a [`Plan`], unique within that plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PlanNodeId(pub String);

impl PlanNodeId {
    /// A node id from anything string-like.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for PlanNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for PlanNodeId {
    fn from(id: &str) -> Self {
        Self(id.to_string())
    }
}

impl From<String> for PlanNodeId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

/// One agent spawn the plan wants to make: which identity runs which
/// subtask, in which development loop, after which other nodes finish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnNode {
    /// This node's id, unique within its [`Plan`].
    pub id: PlanNodeId,
    /// Name of the identity card the planner wants to play.
    pub identity: String,
    /// The subtask text the spawned agent would receive.
    pub subtask: String,
    /// The development loop the planner placed the subtask in.
    pub dev_loop: DevLoop,
    /// Ids of the nodes that must finish before this one may start.
    #[serde(default)]
    pub depends_on: Vec<PlanNodeId>,
}

/// Errors constructing, ordering or validating a [`Plan`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanError {
    /// A plan with no nodes was rejected.
    #[error("plan has no nodes")]
    Empty,
    /// Two nodes in the same plan declared the same id.
    #[error("plan node id `{0}` is used by more than one node")]
    DuplicateNode(String),
    /// A node's `depends_on` names an id no node in the plan declares.
    #[error("plan node `{node}` depends on unknown node `{depends_on}`")]
    UnknownDependency {
        /// The node whose dependency is dangling.
        node: String,
        /// The missing id it named.
        depends_on: String,
    },
    /// A node named an identity absent from the catalog it was planned
    /// against. The whole plan is rejected; no node is silently dropped.
    #[error("plan references identity `{0}`, which is not in the catalog")]
    UnknownIdentity(String),
    /// The plan's dependency graph has a cycle; the listed ids cannot be
    /// ordered.
    #[error("plan has a dependency cycle among: {}", .0.join(", "))]
    Cycle(Vec<String>),
    /// The planner model's reply could not be parsed into a plan.
    #[error("planner model output could not be parsed: {0}")]
    ModelOutput(String),
    /// The planner model call itself failed.
    #[error("planner model call failed: {0}")]
    Model(String),
}

/// A DAG of [`SpawnNode`]s: the planner's answer to "who runs what, in what
/// order".
///
/// [`Plan::new`] rejects an empty node list, a duplicate id, and a
/// `depends_on` that names an id absent from the plan. It does not detect
/// cycles; call [`Plan::topological_order`] for that.
///
/// ```
/// use harness::identity::DevLoop;
/// use harness::planner::{Plan, PlanError, PlanNodeId, SpawnNode};
///
/// let node = |id: &str, depends_on: &[&str]| SpawnNode {
///     id: PlanNodeId::new(id),
///     identity: "rust-implementer".to_string(),
///     subtask: "do work".to_string(),
///     dev_loop: DevLoop::Inner,
///     depends_on: depends_on.iter().map(|d| PlanNodeId::new(*d)).collect(),
/// };
///
/// let plan = Plan::new(vec![node("a", &[]), node("b", &["a"])]).unwrap();
/// let order = plan.topological_order().unwrap();
/// assert_eq!(order, vec![PlanNodeId::new("a"), PlanNodeId::new("b")]);
///
/// let cyclic = Plan::new(vec![node("a", &["b"]), node("b", &["a"])]).unwrap();
/// assert_eq!(
///     cyclic.topological_order().unwrap_err(),
///     PlanError::Cycle(vec!["a".to_string(), "b".to_string()])
/// );
///
/// assert_eq!(Plan::new(vec![]).unwrap_err(), PlanError::Empty);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    nodes: Vec<SpawnNode>,
}

impl Plan {
    /// Build a plan from `nodes`, rejecting an empty list, a duplicate id or
    /// a dangling `depends_on` reference.
    pub fn new(nodes: Vec<SpawnNode>) -> Result<Self, PlanError> {
        if nodes.is_empty() {
            return Err(PlanError::Empty);
        }
        let mut seen: HashSet<&PlanNodeId> = HashSet::new();
        for node in &nodes {
            if !seen.insert(&node.id) {
                return Err(PlanError::DuplicateNode(node.id.0.clone()));
            }
        }
        for node in &nodes {
            for dep in &node.depends_on {
                if !seen.contains(dep) {
                    return Err(PlanError::UnknownDependency {
                        node: node.id.0.clone(),
                        depends_on: dep.0.clone(),
                    });
                }
            }
        }
        Ok(Self { nodes })
    }

    /// Every node in the plan, in declaration order.
    pub fn nodes(&self) -> &[SpawnNode] {
        &self.nodes
    }

    /// The node with `id`, if the plan has one.
    pub fn node(&self, id: &PlanNodeId) -> Option<&SpawnNode> {
        self.nodes.iter().find(|node| &node.id == id)
    }

    /// Number of nodes in the plan.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the plan has no nodes. Always `false` for a plan built by
    /// [`Plan::new`], which rejects an empty node list; kept alongside
    /// [`Plan::len`] as the idiomatic pair.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Every identity the plan names that is absent from `catalog`.
    ///
    /// The whole plan is invalid if this is non-empty: a hallucinated
    /// identity name is a hard rejection, not a dropped node.
    pub fn check_identities(&self, catalog: &IdentityCatalog) -> Result<(), PlanError> {
        match self
            .nodes
            .iter()
            .find(|node| catalog.get(&node.identity).is_none())
        {
            Some(node) => Err(PlanError::UnknownIdentity(node.identity.clone())),
            None => Ok(()),
        }
    }

    /// A dependency order in which every node appears after everything it
    /// `depends_on`, via Kahn's algorithm. Nodes with no unresolved
    /// dependency are scheduled in declaration order, so the result is
    /// deterministic for a given plan.
    ///
    /// Returns [`PlanError::Cycle`] naming every node that could not be
    /// scheduled when the dependency graph has a cycle.
    pub fn topological_order(&self) -> Result<Vec<PlanNodeId>, PlanError> {
        let mut in_degree: HashMap<&PlanNodeId, usize> =
            self.nodes.iter().map(|node| (&node.id, 0)).collect();
        let mut dependents: HashMap<&PlanNodeId, Vec<&PlanNodeId>> = HashMap::new();
        for node in &self.nodes {
            for dep in &node.depends_on {
                *in_degree.get_mut(&node.id).expect("node id present") += 1;
                dependents.entry(dep).or_default().push(&node.id);
            }
        }

        let mut queue: VecDeque<&PlanNodeId> = self
            .nodes
            .iter()
            .map(|node| &node.id)
            .filter(|id| in_degree[id] == 0)
            .collect();
        let mut order: Vec<PlanNodeId> = Vec::with_capacity(self.nodes.len());
        while let Some(id) = queue.pop_front() {
            order.push(id.clone());
            let freed = dependents.get(id).into_iter().flatten();
            for dependent in freed {
                let degree = in_degree.get_mut(dependent).expect("dependent id present");
                *degree -= 1;
                if *degree == 0 {
                    queue.push_back(dependent);
                }
            }
        }

        if order.len() == self.nodes.len() {
            return Ok(order);
        }
        let scheduled: HashSet<&PlanNodeId> = order.iter().collect();
        let remaining: Vec<String> = self
            .nodes
            .iter()
            .map(|node| &node.id)
            .filter(|id| !scheduled.contains(id))
            .map(|id| id.0.clone())
            .collect();
        Err(PlanError::Cycle(remaining))
    }

    /// Whether any node in the plan sits outside the inner loop and would
    /// therefore need to be scheduled through a human-availability window
    /// (issue #654) rather than run immediately.
    ///
    /// ```
    /// use harness::identity::DevLoop;
    /// use harness::planner::{Plan, PlanNodeId, SpawnNode};
    ///
    /// let inner_only = Plan::new(vec![SpawnNode {
    ///     id: PlanNodeId::new("a"),
    ///     identity: "rust-implementer".to_string(),
    ///     subtask: "do work".to_string(),
    ///     dev_loop: DevLoop::Inner,
    ///     depends_on: vec![],
    /// }])
    /// .unwrap();
    /// assert!(!inner_only.needs_availability_window());
    /// ```
    pub fn needs_availability_window(&self) -> bool {
        self.nodes
            .iter()
            .any(|node| node.dev_loop != DevLoop::Inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, dev_loop: DevLoop, depends_on: &[&str]) -> SpawnNode {
        SpawnNode {
            id: PlanNodeId::new(id),
            identity: "rust-implementer".to_string(),
            subtask: "do work".to_string(),
            dev_loop,
            depends_on: depends_on.iter().map(|d| PlanNodeId::new(*d)).collect(),
        }
    }

    fn inner(id: &str, depends_on: &[&str]) -> SpawnNode {
        node(id, DevLoop::Inner, depends_on)
    }

    #[test]
    fn plan_node_id_displays_and_converts_from_strings() {
        let from_str: PlanNodeId = "a".into();
        let from_string: PlanNodeId = "a".to_string().into();
        assert_eq!(from_str, PlanNodeId::new("a"));
        assert_eq!(from_string, PlanNodeId::new("a"));
        assert_eq!(from_str.to_string(), "a");
    }

    #[test]
    fn new_rejects_an_empty_plan() {
        assert_eq!(Plan::new(vec![]).unwrap_err(), PlanError::Empty);
    }

    #[test]
    fn new_rejects_a_duplicate_node_id() {
        let err = Plan::new(vec![inner("a", &[]), inner("a", &[])]).unwrap_err();
        assert_eq!(err, PlanError::DuplicateNode("a".to_string()));
    }

    #[test]
    fn new_rejects_a_dangling_dependency() {
        let err = Plan::new(vec![inner("a", &["ghost"])]).unwrap_err();
        assert_eq!(
            err,
            PlanError::UnknownDependency {
                node: "a".to_string(),
                depends_on: "ghost".to_string(),
            }
        );
    }

    #[test]
    fn accessors_expose_nodes_by_id_and_count() {
        let plan = Plan::new(vec![inner("a", &[]), inner("b", &["a"])]).unwrap();
        assert_eq!(plan.len(), 2);
        assert!(!plan.is_empty());
        assert_eq!(plan.nodes().len(), 2);
        assert_eq!(
            plan.node(&PlanNodeId::new("a")).unwrap().id,
            PlanNodeId::new("a")
        );
        assert!(plan.node(&PlanNodeId::new("ghost")).is_none());
    }

    #[test]
    fn topological_order_respects_a_diamond_dependency() {
        let plan = Plan::new(vec![
            inner("a", &[]),
            inner("b", &["a"]),
            inner("c", &["a"]),
            inner("d", &["b", "c"]),
        ])
        .unwrap();
        let order = plan.topological_order().unwrap();
        assert_eq!(order.len(), 4);
        let position = |id: &str| order.iter().position(|n| n.0 == id).unwrap();
        assert!(position("a") < position("b"));
        assert!(position("a") < position("c"));
        assert!(position("b") < position("d"));
        assert!(position("c") < position("d"));
    }

    #[test]
    fn topological_order_is_deterministic_for_independent_nodes() {
        let plan = Plan::new(vec![inner("a", &[]), inner("b", &[]), inner("c", &[])]).unwrap();
        assert_eq!(
            plan.topological_order().unwrap(),
            vec![
                PlanNodeId::new("a"),
                PlanNodeId::new("b"),
                PlanNodeId::new("c")
            ]
        );
    }

    #[test]
    fn topological_order_detects_a_two_node_cycle() {
        let plan = Plan::new(vec![inner("a", &["b"]), inner("b", &["a"])]).unwrap();
        let err = plan.topological_order().unwrap_err();
        match err {
            PlanError::Cycle(mut nodes) => {
                nodes.sort();
                assert_eq!(nodes, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn topological_order_detects_a_cycle_that_spares_an_unrelated_node() {
        let plan = Plan::new(vec![
            inner("a", &["b"]),
            inner("b", &["a"]),
            inner("c", &[]),
        ])
        .unwrap();
        let err = plan.topological_order().unwrap_err();
        match err {
            PlanError::Cycle(mut nodes) => {
                nodes.sort();
                assert_eq!(nodes, vec!["a".to_string(), "b".to_string()]);
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn check_identities_passes_when_every_identity_is_known() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rust-implementer.toml"),
            r#"
[identity]
name = "rust-implementer"
description = "Implements a scoped issue."
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "Implement the issue." }

[scope]
repos = []
paths = ["**"]
max_effect = "repository"
tools = ["read_file", "write_file"]

[limits]
max_iterations = 10
max_wall_clock_secs = 60
max_concurrent = 1
"#,
        )
        .unwrap();
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        let plan = Plan::new(vec![inner("a", &[])]).unwrap();
        assert!(plan.check_identities(&catalog).is_ok());
    }

    #[test]
    fn check_identities_rejects_the_whole_plan_for_one_hallucinated_name() {
        let plan = Plan::new(vec![inner("a", &[])]).unwrap();
        let err = plan
            .check_identities(&IdentityCatalog::default())
            .unwrap_err();
        assert_eq!(
            err,
            PlanError::UnknownIdentity("rust-implementer".to_string())
        );
    }

    #[test]
    fn needs_availability_window_is_true_for_any_non_inner_node() {
        let inner_only = Plan::new(vec![inner("a", &[])]).unwrap();
        assert!(!inner_only.needs_availability_window());
        let mixed = Plan::new(vec![inner("a", &[]), node("b", DevLoop::Outer, &["a"])]).unwrap();
        assert!(mixed.needs_availability_window());
    }

    #[test]
    fn plan_error_display_messages() {
        assert_eq!(PlanError::Empty.to_string(), "plan has no nodes");
        assert_eq!(
            PlanError::DuplicateNode("a".to_string()).to_string(),
            "plan node id `a` is used by more than one node"
        );
        assert_eq!(
            PlanError::UnknownDependency {
                node: "a".to_string(),
                depends_on: "b".to_string()
            }
            .to_string(),
            "plan node `a` depends on unknown node `b`"
        );
        assert_eq!(
            PlanError::UnknownIdentity("ghost".to_string()).to_string(),
            "plan references identity `ghost`, which is not in the catalog"
        );
        assert_eq!(
            PlanError::Cycle(vec!["a".to_string(), "b".to_string()]).to_string(),
            "plan has a dependency cycle among: a, b"
        );
        assert_eq!(
            PlanError::ModelOutput("bad".to_string()).to_string(),
            "planner model output could not be parsed: bad"
        );
        assert_eq!(
            PlanError::Model("down".to_string()).to_string(),
            "planner model call failed: down"
        );
    }
}
