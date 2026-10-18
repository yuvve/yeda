use yeda_core::clusters::{Clusters, Kind};

/// `:graph tags` and `:graph col <column>`: the papers and their labels, in the right column.
pub struct ClusterView {
    pub clusters: Clusters,
    /// What the labels are, for titles: `tags`, or the column's name.
    pub what: String,
    pub sel: usize,
    /// The highlighted entry of `entries()`.
    pub nbr: usize,
    /// List every node rather than the selected one's links.
    pub all: bool,
    /// Nodes navigated away from, for Backspace.
    pub back: Vec<usize>,
}

impl ClusterView {
    /// Starts at the label with the most papers.
    pub fn new(clusters: Clusters, what: String) -> Self {
        let sel = (0..clusters.nodes.len()).filter(|&i| clusters.nodes[i].kind == Kind::Label).max_by_key(|&i| clusters.nodes[i].links.len()).unwrap_or(0);
        ClusterView { clusters, what, sel, nbr: 0, all: false, back: Vec::new() }
    }

    /// The selected node's links, papers by name; or every node, the largest labels first, then papers.
    pub fn entries(&self) -> Vec<usize> {
        let nodes = &self.clusters.nodes;
        let mut out: Vec<usize> = match (self.all, nodes.get(self.sel)) {
            (true, _) => (0..nodes.len()).collect(),
            (false, Some(n)) => n.links.clone(),
            (false, None) => Vec::new(),
        };
        out.sort_by_key(|&i| (nodes[i].kind != Kind::Label, std::cmp::Reverse(nodes[i].links.len()), nodes[i].name.to_lowercase()));
        out
    }

    pub fn go(&mut self, i: usize) {
        if i != self.sel {
            self.back.push(self.sel);
            self.sel = i;
        }
        self.nbr = 0;
        self.all = false;
    }
}
