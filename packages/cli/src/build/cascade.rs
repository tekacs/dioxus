use crate::Result;
use anyhow::ensure;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Collect changed crates and their dependents inside the tip's dependency closure.
/// An excluded crate cannot reach an included one, so it is safe to prune its walk.
pub(super) fn cascade(
    changed: impl IntoIterator<Item = String>,
    allowed: &HashSet<String>,
    dependents: impl Fn(&str) -> Vec<String>,
) -> HashSet<String> {
    let mut pending: Vec<_> = changed.into_iter().collect();
    let mut selected = HashSet::new();
    while let Some(name) = pending.pop() {
        if allowed.contains(&name) && selected.insert(name.clone()) {
            pending.extend(dependents(&name));
        }
    }
    selected
}

/// Order selected libraries before their dependents; the tip binary compiles separately.
/// A package with both lib and bin targets must replay its library before that final step.
/// Library presence comes from Cargo target metadata, never from capture availability.
/// Missing captures remain selected so replay reports the missing input.
pub(super) fn replay(
    modified: &HashSet<String>,
    tip: &str,
    library: bool,
    dependents: impl Fn(&str) -> Vec<String>,
) -> Result<Vec<String>> {
    let crates: HashSet<&String> = modified
        .iter()
        .filter(|name| name.as_str() != tip || library)
        .collect();
    let mut indegree: HashMap<&String, usize> = crates.iter().map(|name| (*name, 0)).collect();
    let mut edges: HashMap<&String, Vec<&String>> = HashMap::new();

    for crate_name in &crates {
        for dependent in dependents(crate_name) {
            if let Some(dep) = crates.get(&dependent) {
                *indegree.entry(dep).or_default() += 1;
                edges.entry(crate_name).or_default().push(dep);
            }
        }
    }

    let mut ready: BTreeSet<&String> = indegree
        .iter()
        .filter(|&(_, &degree)| degree == 0)
        .map(|(name, _)| *name)
        .collect();
    let mut ordered = Vec::with_capacity(crates.len());
    while let Some(name) = ready.pop_first() {
        ordered.push(name.clone());
        for dep in edges.get(name).into_iter().flatten() {
            let degree = indegree.get_mut(dep).unwrap();
            *degree -= 1;
            if *degree == 0 {
                ready.insert(dep);
            }
        }
    }

    ensure!(
        ordered.len() == crates.len(),
        "Cycle in workspace dependency graph — cannot determine replay order"
    );
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn dependents(name: &str) -> Vec<String> {
        let values: &[&str] = match name {
            "leaf" => &["shared", "unused"],
            "shared" => &["app", "sibling"],
            "app" => &["sibling"],
            _ => panic!("must not traverse excluded crate {name}"),
        };
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn tip_library_replays_before_binary() {
        assert_eq!(
            replay(&names(&["app"]), "app", true, dependents).unwrap(),
            vec!["app"]
        );
    }

    #[test]
    fn binary_only_tip_has_no_library_replay() {
        assert!(
            replay(&names(&["app"]), "app", false, dependents)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn tip_library_follows_its_dependencies() {
        assert_eq!(
            replay(
                &names(&["app", "leaf", "shared"]),
                "app",
                true,
                dependents
            )
            .unwrap(),
            vec!["leaf", "shared", "app"]
        );
    }

    #[test]
    fn binary_only_tip_still_replays_modified_dependencies() {
        assert_eq!(
            replay(
                &names(&["app", "shared"]),
                "app",
                false,
                dependents
            )
            .unwrap(),
            vec!["shared"]
        );
    }

    #[test]
    fn replay_ties_are_lexicographic() {
        assert_eq!(
            replay(&names(&["z", "a"]), "app", false, |_| Vec::new()).unwrap(),
            vec!["a", "z"]
        );
    }

    #[test]
    fn replay_rejects_cycles() {
        assert!(
            replay(&names(&["a", "b"]), "app", false, |name| {
                vec![if name == "a" { "b" } else { "a" }.to_string()]
            })
            .is_err()
        );
    }

    #[test]
    fn tip_edit_excludes_sibling() {
        let allowed = names(&["leaf", "shared", "app"]);
        assert_eq!(
            cascade(names(&["app"]), &allowed, dependents),
            names(&["app"])
        );
    }

    #[test]
    fn leaf_edit_keeps_cascade_without_siblings() {
        let allowed = names(&["leaf", "shared", "app"]);
        assert_eq!(cascade(names(&["leaf"]), &allowed, dependents), allowed);
    }

    #[test]
    fn direct_sibling_edit_is_excluded() {
        let allowed = names(&["leaf", "shared", "app"]);
        assert_eq!(
            cascade(names(&["app", "sibling"]), &allowed, dependents),
            names(&["app"])
        );
    }

    #[test]
    fn repeated_paths_visit_each_crate_once() {
        let allowed = names(&["leaf", "shared", "app"]);
        let visited = std::cell::RefCell::new(HashSet::new());
        let selected = cascade(
            names(&["leaf", "shared", "app"]),
            &allowed,
            |name| {
                assert!(visited.borrow_mut().insert(name.to_string()));
                dependents(name)
            },
        );
        assert_eq!(selected, allowed);
        assert_eq!(visited.into_inner(), allowed);
    }
}
