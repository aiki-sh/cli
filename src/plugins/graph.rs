//! Plugin dependency graph for querying relationships between installed plugins.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use super::list_installed_plugins;
use super::manifest::resolve_display_name;
use super::scanner::derive_plugin_refs;
use super::PluginRef;

/// Pre-built dependency graph over all installed plugins.
///
/// Built once via [`PluginGraph::build`], then queried cheaply.
pub struct PluginGraph {
    /// Forward edges: plugin → set of plugins it depends on.
    deps: HashMap<PluginRef, HashSet<PluginRef>>,
    /// Reverse edges: plugin → set of plugins that depend on it.
    reverse_deps: HashMap<PluginRef, HashSet<PluginRef>>,
    /// Plugins referenced as dependencies but not installed.
    missing: HashSet<PluginRef>,
    /// Cached display names (only populated for plugins with a distinct display name).
    display_names: HashMap<PluginRef, String>,
}

impl PluginGraph {
    /// Scan all installed plugins under `plugins_base` and build the full graph.
    pub fn build(plugins_base: &Path) -> Self {
        let installed = list_installed_plugins(plugins_base);
        let installed_set: HashSet<PluginRef> = installed.iter().cloned().collect();

        let mut deps: HashMap<PluginRef, HashSet<PluginRef>> = HashMap::new();
        let mut reverse_deps: HashMap<PluginRef, HashSet<PluginRef>> = HashMap::new();
        let mut missing = HashSet::new();

        // Ensure every installed plugin has an entry even if it has no deps.
        for plugin in &installed {
            deps.entry(plugin.clone()).or_default();
            reverse_deps.entry(plugin.clone()).or_default();
        }

        let mut display_names = HashMap::new();

        for plugin in &installed {
            let dir = plugin.install_dir(plugins_base);
            let refs = derive_plugin_refs(&dir, Some(plugin));

            // Cache display name
            let plugin_path = plugin.to_string();
            let display = resolve_display_name(&dir, &plugin_path);
            display_names.insert(plugin.clone(), display);

            for dep in refs {
                deps.entry(plugin.clone()).or_default().insert(dep.clone());
                reverse_deps
                    .entry(dep.clone())
                    .or_default()
                    .insert(plugin.clone());

                if !installed_set.contains(&dep) {
                    missing.insert(dep);
                }
            }
        }

        Self {
            deps,
            reverse_deps,
            missing,
            display_names,
        }
    }

    /// All installed plugins in the graph.
    pub fn all_plugins(&self) -> Vec<PluginRef> {
        // Only return plugins that were actually installed (present in deps keys
        // and not in missing).
        self.deps
            .keys()
            .filter(|p| !self.missing.contains(p))
            .cloned()
            .collect()
    }

    /// Cached display name for a plugin (falls back to `namespace/name` path).
    pub fn display_name<'a>(&'a self, plugin: &'a PluginRef) -> Cow<'a, str> {
        self.display_names
            .get(plugin)
            .map(|s| Cow::Borrowed(s.as_str()))
            .unwrap_or_else(|| Cow::Owned(plugin.to_string()))
    }

    /// Direct dependents of a plugin (plugins that depend on it).
    pub fn dependents(&self, plugin: &PluginRef) -> Vec<PluginRef> {
        self.reverse_deps
            .get(plugin)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Transitive dependents via BFS on reverse edges.
    pub fn transitive_dependents(&self, plugin: &PluginRef) -> Vec<PluginRef> {
        bfs(&self.reverse_deps, plugin)
    }

    /// Direct dependencies of a plugin.
    pub fn dependencies(&self, plugin: &PluginRef) -> Vec<PluginRef> {
        self.deps
            .get(plugin)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Transitive dependencies via BFS on forward edges.
    pub fn transitive_dependencies(&self, plugin: &PluginRef) -> Vec<PluginRef> {
        bfs(&self.deps, plugin)
    }

    /// Detect all strongly connected components of size > 1 (cycles).
    ///
    /// Uses Tarjan's algorithm.
    pub fn cycles(&self) -> Vec<Vec<PluginRef>> {
        let nodes: Vec<&PluginRef> = self.deps.keys().collect();
        let mut index_counter: u32 = 0;
        let mut stack: Vec<PluginRef> = Vec::new();
        let mut on_stack: HashSet<PluginRef> = HashSet::new();
        let mut indices: HashMap<PluginRef, u32> = HashMap::new();
        let mut lowlinks: HashMap<PluginRef, u32> = HashMap::new();
        let mut result: Vec<Vec<PluginRef>> = Vec::new();

        for node in &nodes {
            if !indices.contains_key(*node) {
                tarjan_strongconnect(
                    (*node).clone(),
                    &self.deps,
                    &mut index_counter,
                    &mut stack,
                    &mut on_stack,
                    &mut indices,
                    &mut lowlinks,
                    &mut result,
                );
            }
        }

        // Only return SCCs with more than one node (actual cycles).
        result.retain(|scc| scc.len() > 1);
        result
    }

    /// Plugins referenced as dependencies but not installed.
    pub fn missing_dependencies(&self) -> &HashSet<PluginRef> {
        &self.missing
    }

    /// Installed plugins that have no dependents and are not in the provided hooks.yml refs.
    pub fn orphaned(&self, hooks_yml_refs: &HashSet<PluginRef>) -> Vec<PluginRef> {
        self.all_plugins()
            .into_iter()
            .filter(|p| {
                let has_dependents = self
                    .reverse_deps
                    .get(p)
                    .map_or(false, |s| !s.is_empty());
                !has_dependents && !hooks_yml_refs.contains(p)
            })
            .collect()
    }
}

/// BFS traversal from `start` over the given adjacency map, returning all
/// reachable nodes (excluding `start` itself).
fn bfs(
    adj: &HashMap<PluginRef, HashSet<PluginRef>>,
    start: &PluginRef,
) -> Vec<PluginRef> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(start.clone());

    if let Some(neighbors) = adj.get(start) {
        for n in neighbors {
            if visited.insert(n.clone()) {
                queue.push_back(n.clone());
            }
        }
    }

    while let Some(current) = queue.pop_front() {
        if let Some(neighbors) = adj.get(&current) {
            for n in neighbors {
                if visited.insert(n.clone()) {
                    queue.push_back(n.clone());
                }
            }
        }
    }

    visited.remove(start);
    visited.into_iter().collect()
}

/// Tarjan's SCC algorithm — recursive strongconnect.
fn tarjan_strongconnect(
    v: PluginRef,
    adj: &HashMap<PluginRef, HashSet<PluginRef>>,
    index_counter: &mut u32,
    stack: &mut Vec<PluginRef>,
    on_stack: &mut HashSet<PluginRef>,
    indices: &mut HashMap<PluginRef, u32>,
    lowlinks: &mut HashMap<PluginRef, u32>,
    result: &mut Vec<Vec<PluginRef>>,
) {
    indices.insert(v.clone(), *index_counter);
    lowlinks.insert(v.clone(), *index_counter);
    *index_counter += 1;
    stack.push(v.clone());
    on_stack.insert(v.clone());

    if let Some(neighbors) = adj.get(&v) {
        for w in neighbors {
            if !indices.contains_key(w) {
                tarjan_strongconnect(
                    w.clone(),
                    adj,
                    index_counter,
                    stack,
                    on_stack,
                    indices,
                    lowlinks,
                    result,
                );
                let wl = lowlinks[w];
                let vl = lowlinks.get_mut(&v).unwrap();
                if wl < *vl {
                    *vl = wl;
                }
            } else if on_stack.contains(w) {
                let wi = indices[w];
                let vl = lowlinks.get_mut(&v).unwrap();
                if wi < *vl {
                    *vl = wi;
                }
            }
        }
    }

    if lowlinks[&v] == indices[&v] {
        let mut scc = Vec::new();
        loop {
            let w = stack.pop().unwrap();
            on_stack.remove(&w);
            scc.push(w.clone());
            if w == v {
                break;
            }
        }
        result.push(scc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use tempfile::TempDir;

    /// Create a fake installed plugin with optional deps in hooks.yaml.
    fn create_fake_plugin(base: &std::path::Path, ns: &str, name: &str, deps: &[&str]) {
        let dir = base.join(ns).join(name);
        fs::create_dir_all(dir.join(".git")).unwrap();
        fs::write(dir.join("plugin.yaml"), format!("name: {}/{}\n", ns, name)).unwrap();

        if !deps.is_empty() {
            let yaml: Vec<String> = deps
                .iter()
                .enumerate()
                .map(|(i, d)| format!("hook{}:\n  template: {}", i, d))
                .collect();
            fs::write(dir.join("hooks.yaml"), yaml.join("\n")).unwrap();
        }
    }

    fn ref_(s: &str) -> crate::plugins::PluginRef {
        s.parse().unwrap()
    }

    fn names(refs: &[crate::plugins::PluginRef]) -> HashSet<String> {
        refs.iter().map(|r| r.to_string()).collect()
    }

    // --- Test 7: Empty graph ---

    #[test]
    fn empty_graph_returns_empty_queries() {
        let tmp = TempDir::new().unwrap();
        // No plugins installed at all
        let graph = PluginGraph::build(tmp.path());

        assert!(graph.all_plugins().is_empty());
        assert!(graph.cycles().is_empty());
        assert!(graph.missing_dependencies().is_empty());
        assert!(graph.orphaned(&HashSet::new()).is_empty());
    }

    // --- Test 8: Single plugin, no deps ---

    #[test]
    fn single_plugin_no_deps() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "solo", &[]);

        let graph = PluginGraph::build(tmp.path());
        let solo = ref_("aiki/solo");

        assert_eq!(graph.all_plugins().len(), 1);
        assert!(graph.dependents(&solo).is_empty());
        assert!(graph.dependencies(&solo).is_empty());
    }

    // --- Test 9: Linear chain A→B→C ---

    #[test]
    fn linear_chain_dependents() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/c/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "c", &[]);

        let graph = PluginGraph::build(tmp.path());
        let c = ref_("aiki/c");

        // Direct dependents of C = [B]
        let direct = names(&graph.dependents(&c));
        assert_eq!(direct, HashSet::from(["aiki/b".to_string()]));

        // Transitive dependents of C = [B, A]
        let transitive = names(&graph.transitive_dependents(&c));
        assert_eq!(
            transitive,
            HashSet::from(["aiki/a".to_string(), "aiki/b".to_string()])
        );
    }

    // --- Test 10: Diamond A→B, A→C, B→D, C→D ---

    #[test]
    fn diamond_dependents() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl", "aiki/c/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/d/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "c", &["aiki/d/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "d", &[]);

        let graph = PluginGraph::build(tmp.path());
        let d = ref_("aiki/d");

        // Direct dependents of D = [B, C]
        let direct = names(&graph.dependents(&d));
        assert_eq!(
            direct,
            HashSet::from(["aiki/b".to_string(), "aiki/c".to_string()])
        );

        // Transitive dependents of D = [A, B, C]
        let transitive = names(&graph.transitive_dependents(&d));
        assert_eq!(
            transitive,
            HashSet::from([
                "aiki/a".to_string(),
                "aiki/b".to_string(),
                "aiki/c".to_string(),
            ])
        );
    }

    // --- Test 11: Cycle detection A→B→A ---

    #[test]
    fn cycle_detection() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &["aiki/a/tmpl"]);

        let graph = PluginGraph::build(tmp.path());
        let cycles = graph.cycles();

        assert!(!cycles.is_empty(), "Should detect the A↔B cycle");
        // The cycle should contain both A and B
        let all_in_cycles: HashSet<String> = cycles
            .iter()
            .flat_map(|cycle| cycle.iter().map(|r| r.to_string()))
            .collect();
        assert!(all_in_cycles.contains("aiki/a"));
        assert!(all_in_cycles.contains("aiki/b"));
    }

    // --- Test 12: Missing dependency ---

    #[test]
    fn missing_dependency() {
        let tmp = TempDir::new().unwrap();
        // A references B, but B is not installed
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);

        let graph = PluginGraph::build(tmp.path());
        let missing = graph.missing_dependencies();

        let missing_names: HashSet<String> = missing.iter().map(|r| r.to_string()).collect();
        assert!(missing_names.contains("aiki/b"));
    }

    // --- Test 13: Orphan detection ---

    #[test]
    fn orphan_detection() {
        let tmp = TempDir::new().unwrap();
        // A→B installed, D installed but nothing depends on it
        create_fake_plugin(tmp.path(), "aiki", "a", &["aiki/b/tmpl"]);
        create_fake_plugin(tmp.path(), "aiki", "b", &[]);
        create_fake_plugin(tmp.path(), "aiki", "d", &[]);

        let graph = PluginGraph::build(tmp.path());
        let hooks_yml_refs: HashSet<crate::plugins::PluginRef> = HashSet::new();

        let orphans = names(&graph.orphaned(&hooks_yml_refs));
        assert!(orphans.contains("aiki/d"), "D should be orphaned");
        // A is a root (nothing depends on it either, but it depends on B — roots aren't orphans
        // if they have dependents or are referenced). However, A has no dependents AND is not
        // in hooks_yml_refs, so it would also be orphaned. The key distinction is:
        // orphaned = no dependents AND not in hooks_yml_refs.
        // Both A and D have no dependents and are not in hooks_yml_refs, so both are orphans.
        assert!(orphans.contains("aiki/a"), "A should also be orphaned (no dependents, not in hooks.yml)");
        // B is NOT orphaned because A depends on it
        assert!(!orphans.contains("aiki/b"), "B should not be orphaned (A depends on it)");
    }

    // --- Test 14: Not orphaned if in hooks.yml ---

    #[test]
    fn not_orphaned_if_in_hooks_yml() {
        let tmp = TempDir::new().unwrap();
        create_fake_plugin(tmp.path(), "aiki", "d", &[]);

        let graph = PluginGraph::build(tmp.path());
        let mut hooks_yml_refs: HashSet<crate::plugins::PluginRef> = HashSet::new();
        hooks_yml_refs.insert(ref_("aiki/d"));

        let orphans = graph.orphaned(&hooks_yml_refs);
        assert!(
            orphans.is_empty(),
            "D should NOT be orphaned when referenced in hooks.yml"
        );
    }
}
