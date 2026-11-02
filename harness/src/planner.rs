use crate::identity::DevLoop;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const MAX_NODES: usize = 32;
pub const MAX_SUBTASK_CHARS: usize = 4_000;
pub const MAX_ID_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spawn {
    pub id: String,
    pub identity: String,
    pub subtask: String,
    #[serde(rename = "loop")]
    pub dev_loop: DevLoop,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRejection {
    EmptyPlan,
    TooManyNodes {
        count: usize,
        max: usize,
    },
    InvalidId {
        index: usize,
    },
    DuplicateId {
        id: String,
    },
    SubtaskEmpty {
        id: String,
    },
    SubtaskTooLong {
        id: String,
        chars: usize,
        max: usize,
    },
    SelfDependency {
        id: String,
    },
    DanglingDependency {
        id: String,
        missing: String,
    },
    DuplicateDependency {
        id: String,
        dependency: String,
    },
    Cycle {
        unresolved: Vec<String>,
    },
}

impl fmt::Display for PlanRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPlan => write!(f, "the plan is empty"),
            Self::TooManyNodes { count, max } => {
                write!(f, "the plan has {count} nodes, above the cap of {max}")
            }
            Self::InvalidId { index } => {
                write!(f, "node {index} has an empty, oversized or non-plain id")
            }
            Self::DuplicateId { id } => write!(f, "node id `{id}` appears more than once"),
            Self::SubtaskEmpty { id } => write!(f, "node `{id}` has an empty subtask"),
            Self::SubtaskTooLong { id, chars, max } => write!(
                f,
                "node `{id}` has a subtask of {chars} characters, above the cap of {max}"
            ),
            Self::SelfDependency { id } => write!(f, "node `{id}` depends on itself"),
            Self::DanglingDependency { id, missing } => write!(
                f,
                "node `{id}` depends on `{missing}`, which is not in the plan"
            ),
            Self::DuplicateDependency { id, dependency } => write!(
                f,
                "node `{id}` lists dependency `{dependency}` more than once"
            ),
            Self::Cycle { unresolved } => write!(
                f,
                "the plan has a dependency cycle among: {}",
                unresolved.join(", ")
            ),
        }
    }
}

impl std::error::Error for PlanRejection {}

fn plain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn plain_id(id: &str) -> bool {
    (1..=MAX_ID_CHARS).contains(&id.chars().count()) && id.chars().all(plain_char)
}

fn printable(name: &str) -> String {
    name.chars()
        .take(MAX_ID_CHARS)
        .map(|c| if plain_char(c) { c } else { '?' })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    nodes: Vec<Spawn>,
    order: Vec<usize>,
}

impl Plan {
    /// ```
    /// use harness::identity::DevLoop;
    /// use harness::planner::{Plan, PlanRejection, Spawn};
    ///
    /// let spawn = |id: &str, deps: &[&str]| Spawn {
    ///     id: id.to_string(),
    ///     identity: "rust-implementer".to_string(),
    ///     subtask: format!("work on {id}"),
    ///     dev_loop: DevLoop::Inner,
    ///     depends_on: deps.iter().map(|d| d.to_string()).collect(),
    /// };
    /// let plan = Plan::new(vec![spawn("ship", &["fix"]), spawn("fix", &[])]).unwrap();
    /// let order: Vec<&str> = plan.topological().map(|n| n.id.as_str()).collect();
    /// assert_eq!(order, ["fix", "ship"]);
    ///
    /// let cycle = Plan::new(vec![spawn("a", &["b"]), spawn("b", &["a"])]);
    /// assert!(matches!(cycle, Err(PlanRejection::Cycle { .. })));
    /// assert_eq!(Plan::new(vec![]), Err(PlanRejection::EmptyPlan));
    /// ```
    pub fn new(nodes: Vec<Spawn>) -> Result<Self, PlanRejection> {
        if nodes.is_empty() {
            return Err(PlanRejection::EmptyPlan);
        }
        if nodes.len() > MAX_NODES {
            return Err(PlanRejection::TooManyNodes {
                count: nodes.len(),
                max: MAX_NODES,
            });
        }
        let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, node) in nodes.iter().enumerate() {
            if !plain_id(&node.id) {
                return Err(PlanRejection::InvalidId { index });
            }
            if index_of.insert(&node.id, index).is_some() {
                return Err(PlanRejection::DuplicateId {
                    id: node.id.clone(),
                });
            }
            let chars = node.subtask.chars().count();
            if node.subtask.trim().is_empty() {
                return Err(PlanRejection::SubtaskEmpty {
                    id: node.id.clone(),
                });
            }
            if chars > MAX_SUBTASK_CHARS {
                return Err(PlanRejection::SubtaskTooLong {
                    id: node.id.clone(),
                    chars,
                    max: MAX_SUBTASK_CHARS,
                });
            }
        }
        for node in &nodes {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for dependency in &node.depends_on {
                if *dependency == node.id {
                    return Err(PlanRejection::SelfDependency {
                        id: node.id.clone(),
                    });
                }
                if !index_of.contains_key(dependency.as_str()) {
                    return Err(PlanRejection::DanglingDependency {
                        id: node.id.clone(),
                        missing: printable(dependency),
                    });
                }
                if !seen.insert(dependency) {
                    return Err(PlanRejection::DuplicateDependency {
                        id: node.id.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        match topological_order(&nodes, &index_of) {
            Ok(order) => Ok(Self { nodes, order }),
            Err(unresolved) => Err(PlanRejection::Cycle { unresolved }),
        }
    }

    pub fn nodes(&self) -> &[Spawn] {
        &self.nodes
    }

    pub fn topological(&self) -> impl Iterator<Item = &Spawn> {
        self.order.iter().map(|index| &self.nodes[*index])
    }
}

fn topological_order(
    nodes: &[Spawn],
    index_of: &BTreeMap<&str, usize>,
) -> Result<Vec<usize>, Vec<String>> {
    let mut waiting: Vec<usize> = nodes.iter().map(|node| node.depends_on.len()).collect();
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for dependency in &node.depends_on {
            dependents[index_of[dependency.as_str()]].push(index);
        }
    }
    let mut ready: BTreeSet<usize> = (0..nodes.len())
        .filter(|index| waiting[*index] == 0)
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(next) = ready.pop_first() {
        order.push(next);
        for dependent in &dependents[next] {
            waiting[*dependent] -= 1;
            if waiting[*dependent] == 0 {
                ready.insert(*dependent);
            }
        }
    }
    if order.len() == nodes.len() {
        return Ok(order);
    }
    let done: BTreeSet<usize> = order.into_iter().collect();
    Err((0..nodes.len())
        .filter(|index| !done.contains(index))
        .map(|index| nodes[index].id.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn node(id: &str, deps: &[&str]) -> Spawn {
        Spawn {
            id: id.to_string(),
            identity: "rust-implementer".to_string(),
            subtask: format!("do {id}"),
            dev_loop: DevLoop::Inner,
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn ids(plan: &Plan) -> Vec<&str> {
        plan.topological().map(|n| n.id.as_str()).collect()
    }

    #[test]
    fn a_linear_plan_is_ordered_by_its_dependencies() {
        let plan = Plan::new(vec![node("c", &["b"]), node("a", &[]), node("b", &["a"])]).unwrap();
        assert_eq!(ids(&plan), ["a", "b", "c"]);
        assert_eq!(plan.nodes().len(), 3);
        assert_eq!(plan.nodes()[0].id, "c");
    }

    #[test]
    fn independent_nodes_keep_their_given_order() {
        let plan = Plan::new(vec![
            node("x", &[]),
            node("y", &[]),
            node("join", &["y", "x"]),
        ])
        .unwrap();
        assert_eq!(ids(&plan), ["x", "y", "join"]);
    }

    #[test]
    fn an_empty_plan_is_rejected() {
        assert_eq!(Plan::new(vec![]), Err(PlanRejection::EmptyPlan));
    }

    fn chain(count: usize) -> Vec<Spawn> {
        (0..count)
            .map(|i| match i {
                0 => node("n0", &[]),
                _ => node(&format!("n{i}"), &[&format!("n{}", i - 1)]),
            })
            .collect()
    }

    #[test]
    fn the_node_cap_is_inclusive() {
        assert!(Plan::new(chain(MAX_NODES)).is_ok());
        assert_eq!(
            Plan::new(chain(MAX_NODES + 1)),
            Err(PlanRejection::TooManyNodes {
                count: MAX_NODES + 1,
                max: MAX_NODES
            })
        );
    }

    #[test]
    fn the_subtask_cap_is_inclusive_and_counts_characters() {
        let mut big = node("a", &[]);
        big.subtask = "é".repeat(MAX_SUBTASK_CHARS);
        assert!(Plan::new(vec![big.clone()]).is_ok());
        big.subtask.push('x');
        assert_eq!(
            Plan::new(vec![big]),
            Err(PlanRejection::SubtaskTooLong {
                id: "a".to_string(),
                chars: MAX_SUBTASK_CHARS + 1,
                max: MAX_SUBTASK_CHARS
            })
        );
    }

    #[test]
    fn a_blank_subtask_is_rejected() {
        let mut blank = node("a", &[]);
        blank.subtask = " \n\t".to_string();
        assert_eq!(
            Plan::new(vec![blank]),
            Err(PlanRejection::SubtaskEmpty {
                id: "a".to_string()
            })
        );
    }

    #[test]
    fn ids_must_be_short_and_plain() {
        for bad in ["", "has space", "semi;colon", "ünï"] {
            assert_eq!(
                Plan::new(vec![node("ok", &[]), node(bad, &[])]),
                Err(PlanRejection::InvalidId { index: 1 }),
                "{bad:?}"
            );
        }
        let longest = "a".repeat(MAX_ID_CHARS);
        assert!(Plan::new(vec![node(&longest, &[])]).is_ok());
        let too_long = "a".repeat(MAX_ID_CHARS + 1);
        assert_eq!(
            Plan::new(vec![node(&too_long, &[])]),
            Err(PlanRejection::InvalidId { index: 0 })
        );
        assert!(Plan::new(vec![node("A_b-1.2", &[])]).is_ok());
    }

    #[test]
    fn a_duplicate_id_is_rejected() {
        assert_eq!(
            Plan::new(vec![node("a", &[]), node("a", &[])]),
            Err(PlanRejection::DuplicateId {
                id: "a".to_string()
            })
        );
    }

    #[test]
    fn a_self_dependency_is_rejected() {
        assert_eq!(
            Plan::new(vec![node("a", &["a"])]),
            Err(PlanRejection::SelfDependency {
                id: "a".to_string()
            })
        );
    }

    #[test]
    fn a_dangling_dependency_is_rejected_and_its_name_is_sanitised() {
        assert_eq!(
            Plan::new(vec![node("a", &["ghost"])]),
            Err(PlanRejection::DanglingDependency {
                id: "a".to_string(),
                missing: "ghost".to_string()
            })
        );
        let hostile = format!("sk-secret token\n{}", "z".repeat(200));
        let Err(PlanRejection::DanglingDependency { missing, .. }) =
            Plan::new(vec![node("a", &[&hostile])])
        else {
            panic!("expected a dangling dependency");
        };
        assert!(!missing.contains(' ') && !missing.contains('\n'));
        assert_eq!(missing.chars().count(), MAX_ID_CHARS);
    }

    #[test]
    fn a_repeated_dependency_is_rejected() {
        assert_eq!(
            Plan::new(vec![node("a", &[]), node("b", &["a", "a"])]),
            Err(PlanRejection::DuplicateDependency {
                id: "b".to_string(),
                dependency: "a".to_string()
            })
        );
    }

    #[test]
    fn cycles_are_rejected_and_name_the_unresolved_nodes() {
        assert_eq!(
            Plan::new(vec![node("a", &["b"]), node("b", &["a"])]),
            Err(PlanRejection::Cycle {
                unresolved: vec!["a".to_string(), "b".to_string()]
            })
        );
        assert_eq!(
            Plan::new(vec![
                node("ok", &[]),
                node("a", &["ok", "c"]),
                node("b", &["a"]),
                node("c", &["b"]),
            ]),
            Err(PlanRejection::Cycle {
                unresolved: vec!["a".to_string(), "b".to_string(), "c".to_string()]
            })
        );
    }

    #[test]
    fn every_rejection_renders_distinctly() {
        let rejections = [
            PlanRejection::EmptyPlan,
            PlanRejection::TooManyNodes { count: 40, max: 32 },
            PlanRejection::InvalidId { index: 3 },
            PlanRejection::DuplicateId { id: "a".into() },
            PlanRejection::SubtaskEmpty { id: "a".into() },
            PlanRejection::SubtaskTooLong {
                id: "a".into(),
                chars: 9,
                max: 4,
            },
            PlanRejection::SelfDependency { id: "a".into() },
            PlanRejection::DanglingDependency {
                id: "a".into(),
                missing: "m".into(),
            },
            PlanRejection::DuplicateDependency {
                id: "a".into(),
                dependency: "d".into(),
            },
            PlanRejection::Cycle {
                unresolved: vec!["a".into(), "b".into()],
            },
        ];
        let rendered: Vec<String> = rejections.iter().map(ToString::to_string).collect();
        let distinct: std::collections::BTreeSet<_> = rendered.iter().collect();
        assert_eq!(distinct.len(), rejections.len());
        assert!(rendered[0].contains("empty"));
        assert!(rendered[1].contains("40") && rendered[1].contains("32"));
        assert!(rendered[2].contains('3'));
        assert!(rendered[9].contains("a, b"));
        assert!(rendered.iter().all(|r| !r.contains("Dangling{")));
        let _: &dyn std::error::Error = &rejections[0];
    }

    proptest! {
        #[test]
        fn any_forward_only_graph_orders_every_dependency_first(
            edges in prop::collection::vec(prop::collection::vec(any::<prop::sample::Index>(), 0..3), 1..12)
        ) {
            let mut nodes = Vec::new();
            for (i, picks) in edges.iter().enumerate() {
                let mut deps: Vec<String> = Vec::new();
                if i > 0 {
                    for pick in picks {
                        let dep = format!("n{}", pick.index(i));
                        if !deps.contains(&dep) {
                            deps.push(dep);
                        }
                    }
                }
                nodes.push(Spawn { depends_on: deps, ..node(&format!("n{i}"), &[]) });
            }
            let plan = Plan::new(nodes).unwrap();
            let position: std::collections::BTreeMap<&str, usize> =
                plan.topological().enumerate().map(|(i, n)| (n.id.as_str(), i)).collect();
            prop_assert_eq!(position.len(), plan.nodes().len());
            for n in plan.nodes() {
                for dep in &n.depends_on {
                    prop_assert!(position[dep.as_str()] < position[n.id.as_str()]);
                }
            }
        }

        #[test]
        fn a_back_edge_always_makes_a_cycle(len in 2usize..10) {
            let mut nodes = chain(len);
            nodes[0].depends_on.push(format!("n{}", len - 1));
            let is_cycle = matches!(Plan::new(nodes), Err(PlanRejection::Cycle { .. }));
            prop_assert!(is_cycle);
        }
    }
}
