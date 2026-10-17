//! Citation graphs around a set of seed references, with the papers outside the library that
//! connect to most seeds. Data comes from a citation index (OpenAlex) through the client.

use crate::library::Ref;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The reference field holding its OpenAlex work id.
pub const OPENALEX: &str = "openalex";

/// One paper in the citation index.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Work {
    /// The index's id, such as OpenAlex's `W2919115771`.
    pub id: String,
    /// Lowercase, without the `https://doi.org/` prefix.
    pub doi: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    pub authors: Vec<String>,
    pub venue: Option<String>,
    /// Ids of the works this one cites.
    pub references: Vec<String>,
    pub cited_by_count: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// How many papers outside the library a graph shows.
    pub outside: usize,
    /// How many citing works to fetch per seed, the most cited first.
    pub citing: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { outside: 20, citing: 100 }
    }
}

pub fn work_id(r: &Ref) -> Option<&str> {
    r.extra.get(OPENALEX).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

pub fn set_work_id(r: &mut Ref, id: &str) {
    r.extra.insert(OPENALEX.into(), toml::Value::String(id.into()));
}

/// The DOI the index knows a reference by: its own, else arXiv's for an `eprint`.
pub fn lookup_doi(r: &Ref) -> Option<String> {
    let eprint = || r.extra.get("eprint").and_then(|v| v.as_str()).map(|a| format!("10.48550/arxiv.{a}"));
    r.doi.as_ref().map(|d| d.to_lowercase()).or_else(eprint)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: String,
    /// The library reference's key, if the work is in the library.
    pub key: Option<String>,
    pub seed: bool,
    pub title: String,
    pub year: Option<i32>,
    pub first_author: String,
    pub doi: Option<String>,
    /// How many seeds it cites or is cited by.
    pub links: usize,
    /// Position in the layout: `x` in 0..=1, `row` indexes `Graph::years`, newest first.
    pub x: f64,
    pub row: usize,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    /// `(citing, cited)` node indices.
    pub edges: Vec<(usize, usize)>,
    /// One per layout row, newest first; `None` is the row for works without a year.
    pub years: Vec<Option<i32>>,
}

impl Graph {
    pub fn find(&self, id: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.id == id)
    }

    pub fn find_key(&self, key: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.key.as_deref() == Some(key))
    }

    /// The nodes citing `i`, then those `i` cites.
    pub fn neighbours(&self, i: usize) -> (Vec<usize>, Vec<usize>) {
        let citing = self.edges.iter().filter(|e| e.1 == i).map(|e| e.0).collect();
        let cited = self.edges.iter().filter(|e| e.0 == i).map(|e| e.1).collect();
        (citing, cited)
    }
}

/// Everything known about the works around the seeds, as fetched by the client.
pub struct Known<'a> {
    pub works: &'a HashMap<String, Work>,
    /// Seed id → ids of works citing it, as far as fetched.
    pub citing: &'a HashMap<String, Vec<String>>,
}

/// Works outside the library linked to a seed, with how many seeds each links to, most first.
fn outside_links(refs: &[Ref], seeds: &[String], known: &Known) -> Vec<(String, usize)> {
    let library: HashSet<&str> = refs.iter().filter_map(work_id).collect();
    let mut links: HashMap<&str, usize> = HashMap::new();
    for s in seeds {
        let cited = known.works.get(s).map(|w| w.references.iter()).into_iter().flatten();
        let citing = known.citing.get(s).into_iter().flatten();
        let linked: HashSet<&str> = cited.chain(citing).map(String::as_str).filter(|id| !library.contains(id)).collect();
        for id in linked {
            *links.entry(id).or_default() += 1;
        }
    }
    let mut ranked: Vec<(String, usize)> = links.into_iter().map(|(id, n)| (id.to_string(), n)).collect();
    let cited_by = |id: &str| known.works.get(id).map_or(0, |w| w.cited_by_count);
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(cited_by(&b.0).cmp(&cited_by(&a.0))).then(a.0.cmp(&b.0)));
    ranked
}

/// Outside works whose metadata the ranking needs: every candidate tied with or above the
/// `outside`-th. The client fetches the ones it doesn't have, then calls `build`.
pub fn candidates(refs: &[Ref], seeds: &[String], known: &Known, outside: usize) -> Vec<String> {
    let ranked = outside_links(refs, seeds, known);
    let Some(&(_, cutoff)) = ranked.get(outside.saturating_sub(1)).or(ranked.last()) else { return vec![] };
    // ponytail: capped so one huge tie (a single seed with thousands of references) stays a few requests.
    ranked.into_iter().take_while(|(_, n)| *n >= cutoff).take(500).map(|(id, _)| id).collect()
}

/// The graph of the seeds (work ids), the library works linked to them, and the `outside` best-linked others.
pub fn build(refs: &[Ref], seeds: &[String], known: &Known, outside: usize) -> Graph {
    let by_id: HashMap<&str, &Ref> = refs.iter().filter_map(|r| Some((work_id(r)?, r))).collect();
    let seed_set: HashSet<&str> = seeds.iter().map(String::as_str).collect();
    let mut ids: Vec<String> = seeds.to_vec();
    // Library works linked to a seed, in either direction.
    for id in refs.iter().filter_map(work_id) {
        let w = known.works.get(id);
        let cites_seed = w.is_some_and(|w| w.references.iter().any(|r| seed_set.contains(r.as_str())));
        let cited_by_seed = seeds.iter().any(|s| known.works.get(s).is_some_and(|w| w.references.iter().any(|r| r == id)));
        if !seed_set.contains(id) && (cites_seed || cited_by_seed) {
            ids.push(id.to_string());
        }
    }
    let ranked = outside_links(refs, seeds, known);
    ids.extend(ranked.iter().take(outside).map(|(id, _)| id.clone()));
    let links: HashMap<String, usize> = ranked.into_iter().collect();

    let mut nodes: Vec<Node> = ids
        .iter()
        .map(|id| {
            let r = by_id.get(id.as_str());
            let w = known.works.get(id);
            let author = |a: &String| a.split(',').next().unwrap_or(a).split_whitespace().last().unwrap_or("").to_string();
            Node {
                id: id.clone(),
                key: r.map(|r| r.key.clone()),
                seed: seed_set.contains(id.as_str()),
                title: r.map(|r| r.title.clone()).or(w.map(|w| w.title.clone())).unwrap_or_default(),
                year: r.and_then(|r| r.year).or(w.and_then(|w| w.year)),
                first_author: r
                    .and_then(|r| r.authors.first().map(|a| a.split(',').next().unwrap_or(a).to_string()))
                    .or(w.and_then(|w| w.authors.first()).map(author))
                    .unwrap_or_default(),
                doi: r.and_then(|r| r.doi.clone()).or(w.and_then(|w| w.doi.clone())),
                links: links.get(id).copied().unwrap_or(0),
                x: 0.0,
                row: 0,
            }
        })
        .collect();
    let index: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
    let mut edges = HashSet::new();
    for (i, id) in ids.iter().enumerate() {
        for r in known.works.get(id).map(|w| w.references.as_slice()).unwrap_or_default() {
            if let Some(&j) = index.get(r.as_str()) {
                edges.insert((i, j));
            }
        }
        for c in known.citing.get(id).into_iter().flatten() {
            if let Some(&j) = index.get(c.as_str()) {
                edges.insert((j, i));
            }
        }
    }
    let mut edges: Vec<(usize, usize)> = edges.into_iter().filter(|(a, b)| a != b).collect();
    edges.sort();
    let years = layout(&mut nodes, &edges);
    Graph { nodes, edges, years }
}

/// One row per year, newest at the top; within a row, nodes sit near the mean position of
/// their neighbours, which keeps most edges short.
fn layout(nodes: &mut [Node], edges: &[(usize, usize)]) -> Vec<Option<i32>> {
    let mut years: Vec<Option<i32>> = nodes.iter().map(|n| n.year).collect::<HashSet<_>>().into_iter().collect();
    // Newest first, and the row without a year last.
    years.sort_by_key(|y| (y.is_none(), std::cmp::Reverse(*y)));
    for n in nodes.iter_mut() {
        n.row = years.iter().position(|y| *y == n.year).unwrap_or(0);
    }
    let spread = |nodes: &mut [Node], order: &dyn Fn(&Node, usize) -> f64| {
        for row in 0..years.len() {
            let mut members: Vec<(f64, usize)> =
                nodes.iter().enumerate().filter(|(_, n)| n.row == row).map(|(i, n)| (order(n, i), i)).collect();
            members.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            let count = members.len();
            for (slot, (_, i)) in members.into_iter().enumerate() {
                nodes[i].x = (slot as f64 + 0.5) / count as f64;
            }
        }
    };
    spread(nodes, &|_, i| i as f64);
    // ponytail: three barycenter sweeps; a proper crossing-minimization pass if big graphs get tangled.
    for _ in 0..3 {
        let xs: Vec<f64> = nodes.iter().map(|n| n.x).collect();
        let mean: Vec<f64> = (0..nodes.len())
            .map(|i| {
                let near: Vec<f64> = edges.iter().filter_map(|&(a, b)| if a == i { Some(xs[b]) } else if b == i { Some(xs[a]) } else { None }).collect();
                if near.is_empty() { xs[i] } else { near.iter().sum::<f64>() / near.len() as f64 }
            })
            .collect();
        spread(nodes, &|_, i| mean[i]);
    }
    years
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work(id: &str, year: i32, refs: &[&str], cited_by: u64) -> Work {
        Work { id: id.into(), year: Some(year), references: refs.iter().map(|s| s.to_string()).collect(), cited_by_count: cited_by, ..Default::default() }
    }

    fn lib_ref(key: &str, id: &str) -> Ref {
        let mut r = Ref { key: key.into(), ..Default::default() };
        set_work_id(&mut r, id);
        r
    }

    #[test]
    fn builds_seeds_library_links_and_top_outside() {
        // Seeds A and B; C is in the library and cites A; X is cited by both seeds, Y by A only, Z cites B.
        let refs = vec![lib_ref("a", "A"), lib_ref("b", "B"), lib_ref("c", "C")];
        let works: HashMap<String, Work> = [
            work("A", 2020, &["X", "Y"], 5),
            work("B", 2021, &["X"], 5),
            work("C", 2022, &["A"], 0),
            work("X", 2010, &[], 900),
            work("Y", 2012, &[], 50),
            work("Z", 2023, &["B"], 1),
        ]
        .into_iter()
        .map(|w| (w.id.clone(), w))
        .collect();
        let citing = HashMap::from([("B".to_string(), vec!["Z".to_string()])]);
        let known = Known { works: &works, citing: &citing };
        let seeds = vec!["A".to_string(), "B".to_string()];
        assert_eq!(candidates(&refs, &seeds, &known, 1), ["X"]);

        let g = build(&refs, &seeds, &known, 2);
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["A", "B", "C", "X", "Y"]);
        assert_eq!(g.nodes[3].links, 2);
        let a = g.find("A").unwrap();
        let (citing, cited) = g.neighbours(a);
        assert_eq!(citing, [g.find("C").unwrap()]);
        assert_eq!(cited, [g.find("X").unwrap(), g.find("Y").unwrap()]);
        assert_eq!(g.years[0], Some(2022));
        assert!(g.nodes[g.find("C").unwrap()].row < g.nodes[a].row);
    }
}
