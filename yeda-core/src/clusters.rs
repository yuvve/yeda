//! Papers and their labels (tags, or a table column's values) as one graph, laid out so papers
//! sharing labels sit together.

use crate::library::Ref;
use std::collections::BTreeMap;

/// How strongly everything is pulled to the middle; stronger packs groups closer, leaving each more room.
const GRAVITY: f64 = 0.4;

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A reference, by key.
    Paper(String),
    Label,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    /// `Surname 2020` for a paper, the label itself for a label.
    pub name: String,
    /// The paper's title; empty for a label.
    pub title: String,
    /// Indexes of the linked nodes: a paper's labels, a label's papers.
    pub links: Vec<usize>,
    /// In 0..=1, 0 left and bottom.
    pub x: f64,
    pub y: f64,
}

pub struct Clusters {
    pub nodes: Vec<Node>,
}

impl Clusters {
    /// `labels` gives each reference's labels; references without any are left out.
    pub fn build<'a>(refs: impl IntoIterator<Item = &'a Ref>, labels: impl Fn(&Ref) -> Vec<String>) -> Clusters {
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_label: BTreeMap<String, usize> = BTreeMap::new();
        for r in refs {
            let mut own = labels(r);
            own.retain(|l| !l.trim().is_empty());
            own.sort();
            own.dedup();
            if own.is_empty() {
                continue;
            }
            let p = nodes.len();
            let author = r.authors.first().map(|a| crate::graph::surname(a)).unwrap_or_default();
            let name = format!("{author} {}", r.year.map(|y| y.to_string()).unwrap_or_default()).trim().to_string();
            nodes.push(Node { kind: Kind::Paper(r.key.clone()), name, title: r.title.clone(), links: vec![], x: 0.0, y: 0.0 });
            for l in own {
                let at = *by_label.entry(l.clone()).or_insert_with(|| {
                    nodes.push(Node { kind: Kind::Label, name: l, title: String::new(), links: vec![], x: 0.0, y: 0.0 });
                    nodes.len() - 1
                });
                nodes[p].links.push(at);
                nodes[at].links.push(p);
            }
        }
        let mut c = Clusters { nodes };
        c.layout();
        c
    }

    pub fn find_key(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.kind == Kind::Paper(key.to_string()))
    }

    /// Fruchterman–Reingold: linked nodes pull together, all nodes push apart.
    // ponytail: O(n²) per step; fine for a few hundred nodes, a quadtree past that.
    fn layout(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        // A sunflower spiral: spread out and the same every time.
        let mut pos: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let r = ((i as f64 + 0.5) / n as f64).sqrt();
                let a = i as f64 * 2.399_963;
                (r * a.cos(), r * a.sin())
            })
            .collect();
        let k = (4.0 / n as f64).sqrt();
        let steps = 300;
        for step in 0..steps {
            let mut force = vec![(0.0, 0.0); n];
            for i in 0..n {
                for j in i + 1..n {
                    let (dx, dy) = (pos[i].0 - pos[j].0, pos[i].1 - pos[j].1);
                    let d2 = (dx * dx + dy * dy).max(1e-6);
                    let f = k * k / d2;
                    force[i].0 += dx * f;
                    force[i].1 += dy * f;
                    force[j].0 -= dx * f;
                    force[j].1 -= dy * f;
                }
            }
            for (i, node) in self.nodes.iter().enumerate() {
                for &j in node.links.iter().filter(|&&j| j > i) {
                    let (dx, dy) = (pos[i].0 - pos[j].0, pos[i].1 - pos[j].1);
                    let d = (dx * dx + dy * dy).sqrt().max(1e-3);
                    let f = d / k;
                    force[i].0 -= dx * f;
                    force[i].1 -= dy * f;
                    force[j].0 += dx * f;
                    force[j].1 += dy * f;
                }
            }
            // Unlinked groups would drift apart for ever; a weak pull to the middle keeps them in view.
            for (p, f) in pos.iter().zip(force.iter_mut()) {
                f.0 -= p.0 * GRAVITY / k;
                f.1 -= p.1 * GRAVITY / k;
            }
            let temperature = 0.1 * (1.0 - step as f64 / steps as f64) + 0.002;
            for (p, f) in pos.iter_mut().zip(&force) {
                let len = (f.0 * f.0 + f.1 * f.1).sqrt().max(1e-9);
                let step = len.min(temperature);
                p.0 += f.0 / len * step;
                p.1 += f.1 / len * step;
            }
        }
        let (min_x, max_x) = pos.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
        let (min_y, max_y) = pos.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let scale = |v: f64, lo: f64, hi: f64| if hi - lo < 1e-9 { 0.5 } else { (v - lo) / (hi - lo) };
        for (node, p) in self.nodes.iter_mut().zip(pos) {
            node.x = scale(p.0, min_x, max_x);
            node.y = scale(p.1, min_y, max_y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(key: &str, tags: &[&str]) -> Ref {
        Ref { key: key.into(), authors: vec![format!("Smith, {key}")], year: Some(2020), tags: tags.iter().map(|t| t.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn links_papers_to_labels_and_clusters_them() {
        let refs = [paper("a", &["x"]), paper("b", &["x", "x"]), paper("c", &["y"]), paper("d", &["y"]), paper("e", &[])];
        let c = Clusters::build(&refs, |r| r.tags.clone());
        let names: Vec<&str> = c.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["Smith 2020", "x", "Smith 2020", "Smith 2020", "y", "Smith 2020"]);
        assert_eq!(c.nodes[1].links, [0, 2]);
        assert_eq!(c.find_key("c"), Some(3));
        assert!(c.find_key("e").is_none());
        let dist = |i: usize, j: usize| ((c.nodes[i].x - c.nodes[j].x).powi(2) + (c.nodes[i].y - c.nodes[j].y).powi(2)).sqrt();
        // Papers on one label sit closer to each other than to the other label's papers.
        assert!(dist(0, 2) < dist(0, 3) && dist(3, 5) < dist(2, 5));
        assert!(c.nodes.iter().all(|n| (0.0..=1.0).contains(&n.x) && (0.0..=1.0).contains(&n.y)));
    }
}
