//! Dependency-graph utilities: topological sort and cycle detection.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::config::ServiceName;
use crate::error::ConfigError;

/// Compute a deterministic topological start order for the given dependency
/// graph.
///
/// `deps` maps each service name to the list of services it depends on
/// (i.e. services that must start *before* it).
///
/// Returns the names in start order (dependencies first), or a
/// [`ConfigError::DependencyCycle`] if the graph contains a cycle.
pub fn topological_sort(
    deps: &BTreeMap<ServiceName, Vec<ServiceName>>,
) -> Result<Vec<ServiceName>, ConfigError> {
    // Kahn's algorithm
    // in_degree[n] = number of services n depends on that haven't been
    // scheduled yet.
    let mut in_degree: BTreeMap<&ServiceName, usize> = BTreeMap::new();
    // adj[dep] = list of services that are waiting on dep.
    let mut adj: BTreeMap<&ServiceName, Vec<&ServiceName>> = BTreeMap::new();

    for name in deps.keys() {
        in_degree.entry(name).or_insert(0);
        adj.entry(name).or_default();
    }

    for (name, service_deps) in deps {
        for dep in service_deps {
            *in_degree.entry(name).or_insert(0) += 1;
            adj.entry(dep).or_default().push(name);
        }
    }

    // Seed queue with nodes that have no unsatisfied dependencies, sorted for
    // determinism.
    let mut queue: VecDeque<&ServiceName> = {
        let mut ready: Vec<&ServiceName> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(name, _)| *name)
            .collect();
        ready.sort();
        ready.into_iter().collect()
    };

    let mut order: Vec<ServiceName> = Vec::with_capacity(deps.len());

    while let Some(name) = queue.pop_front() {
        order.push(name.clone());

        let mut next: Vec<&ServiceName> = adj.get(name).cloned().unwrap_or_default();
        next.sort();

        for successor in next {
            let deg = in_degree.get_mut(successor).expect("all nodes registered");
            *deg -= 1;
            if *deg == 0 {
                // Insert in sorted order to keep the queue deterministic.
                let pos = queue.partition_point(|&q| q < successor);
                queue.insert(pos, successor);
            }
        }
    }

    if order.len() != deps.len() {
        // There is at least one cycle among the unscheduled nodes.
        let remaining: BTreeSet<&ServiceName> = in_degree
            .iter()
            .filter(|(_, &deg)| deg > 0)
            .map(|(name, _)| *name)
            .collect();
        let cycle = find_cycle(deps, &remaining);
        return Err(ConfigError::DependencyCycle { cycle });
    }

    Ok(order)
}

/// DFS through `deps` restricted to `remaining` to find and format one cycle.
fn find_cycle(
    deps: &BTreeMap<ServiceName, Vec<ServiceName>>,
    remaining: &BTreeSet<&ServiceName>,
) -> String {
    let mut path: Vec<&ServiceName> = Vec::new();
    let mut on_path: BTreeSet<&ServiceName> = BTreeSet::new();
    let mut finished: BTreeSet<&ServiceName> = BTreeSet::new();

    for start in remaining {
        if !finished.contains(start) {
            if let Some(cycle) = dfs(
                start,
                deps,
                remaining,
                &mut path,
                &mut on_path,
                &mut finished,
            ) {
                return cycle;
            }
        }
    }

    // Fallback (should not happen in practice).
    remaining
        .iter()
        .map(|n| n.as_str())
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn dfs<'a>(
    node: &'a ServiceName,
    deps: &'a BTreeMap<ServiceName, Vec<ServiceName>>,
    remaining: &'a BTreeSet<&'a ServiceName>,
    path: &mut Vec<&'a ServiceName>,
    on_path: &mut BTreeSet<&'a ServiceName>,
    finished: &mut BTreeSet<&'a ServiceName>,
) -> Option<String> {
    if on_path.contains(node) {
        // We've closed the cycle: extract the loop from `path`.
        let start_idx = path.iter().position(|&n| n == node).expect("node on path");
        let mut parts: Vec<&str> = path[start_idx..].iter().map(|n| n.as_str()).collect();
        parts.push(node.as_str());
        return Some(parts.join(" -> "));
    }

    if finished.contains(node) || !remaining.contains(node) {
        return None;
    }

    on_path.insert(node);
    path.push(node);

    if let Some(service_deps) = deps.get(node) {
        // Visit in deterministic order.
        let mut sorted_deps: Vec<&ServiceName> = service_deps.iter().collect();
        sorted_deps.sort();

        for dep in sorted_deps {
            if let Some(cycle) = dfs(dep, deps, remaining, path, on_path, finished) {
                return Some(cycle);
            }
        }
    }

    path.pop();
    on_path.remove(node);
    finished.insert(node);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<ServiceName> {
        items.iter().map(|s| ServiceName(s.to_string())).collect()
    }

    fn graph(pairs: &[(&str, &[&str])]) -> BTreeMap<ServiceName, Vec<ServiceName>> {
        pairs
            .iter()
            .map(|(name, deps)| (ServiceName(name.to_string()), names(deps)))
            .collect()
    }

    #[test]
    fn single_node_no_deps() {
        let g = graph(&[("a", &[])]);
        let order = topological_sort(&g).unwrap();
        assert_eq!(order, names(&["a"]));
    }

    #[test]
    fn linear_chain() {
        let g = graph(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let order = topological_sort(&g).unwrap();
        // a must come before b, b before c
        let pos_a = order.iter().position(|n| n.as_str() == "a").unwrap();
        let pos_b = order.iter().position(|n| n.as_str() == "b").unwrap();
        let pos_c = order.iter().position(|n| n.as_str() == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn diamond_dependency() {
        //      a
        //     / \
        //    b   c
        //     \ /
        //      d
        let g = graph(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let order = topological_sort(&g).unwrap();
        let pos_a = order.iter().position(|n| n.as_str() == "a").unwrap();
        let pos_b = order.iter().position(|n| n.as_str() == "b").unwrap();
        let pos_c = order.iter().position(|n| n.as_str() == "c").unwrap();
        let pos_d = order.iter().position(|n| n.as_str() == "d").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_a < pos_c);
        assert!(pos_b < pos_d);
        assert!(pos_c < pos_d);
    }

    #[test]
    fn simple_cycle_detected() {
        let g = graph(&[("a", &["b"]), ("b", &["a"])]);
        let err = topological_sort(&g).unwrap_err();
        let cycle = match err {
            ConfigError::DependencyCycle { cycle } => cycle,
            _ => panic!("expected DependencyCycle"),
        };
        // Both 'a' and 'b' must appear in the cycle description.
        assert!(cycle.contains('a'), "cycle={cycle}");
        assert!(cycle.contains('b'), "cycle={cycle}");
    }

    #[test]
    fn three_node_cycle() {
        let g = graph(&[("a", &["c"]), ("b", &["a"]), ("c", &["b"])]);
        let err = topological_sort(&g).unwrap_err();
        let cycle = match err {
            ConfigError::DependencyCycle { cycle } => cycle,
            _ => panic!("expected DependencyCycle"),
        };
        assert!(cycle.contains('a'));
        assert!(cycle.contains('b'));
        assert!(cycle.contains('c'));
    }

    #[test]
    fn deterministic_order() {
        // Services with no mutual dependencies should come out alphabetically.
        let g = graph(&[("z", &[]), ("a", &[]), ("m", &[])]);
        let order = topological_sort(&g).unwrap();
        assert_eq!(order, names(&["a", "m", "z"]));
    }
}
