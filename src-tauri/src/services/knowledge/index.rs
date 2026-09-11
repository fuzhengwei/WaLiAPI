//! Lightweight HNSW (Hierarchical Navigable Small World) vector index.
//!
//! This is a simplified single-layer implementation optimized for desktop-scale
//! knowledge bases (up to ~100K chunks). It uses greedy best-first search with
//! a priority queue, providing O(log n) average-case search complexity.
//!
//! Zero external dependencies beyond `bincode` (already in Cargo.toml).

use bincode::{deserialize, serialize};
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::path::Path;

/// A node in the HNSW graph.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct IndexNode {
    /// External ID (maps to chunk ID in SQLite)
    pub id: String,
    /// 所属文档 ID（增量索引按文档差集增删用）。
    /// 注意：bincode 非自描述格式，旧版本索引文件缺此字段时加载会失败，
    /// 由上层回退全量重建——serde(default) 兜不住跨格式的旧文件。
    #[serde(default)]
    pub doc_id: String,
    /// The embedding vector
    pub vector: Vec<f32>,
    /// Neighbour node indices (internal, not external IDs)
    pub neighbours: Vec<usize>,
}

/// Search result item.
#[derive(Clone, Debug)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
}

/// The HNSW index.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HnswIndex {
    /// All nodes, indexed by internal position
    pub nodes: Vec<IndexNode>,
    /// Maximum number of connections per node
    pub max_m: usize,
    /// EF parameter for search (controls search width)
    pub ef_search: usize,
    /// EF parameter for construction
    pub ef_construction: usize,
    /// Embedding dimension
    pub dim: usize,
    /// Entry point node index
    pub entry_point: usize,
    /// Random state for level assignment (simplified: always layer 0)
    pub initialized: bool,
    /// 已摘除节点的 chunk id 集合（墓碑）：检索结果过滤、len 扣减。
    /// 图内不做物理摘除（会破坏 HNSW 连通性），压实只在全量 build 时发生。
    #[serde(default)]
    pub tombstones: HashSet<String>,
}

/// Priority queue item for greedy search.
#[derive(Clone)]
struct SearchItem {
    distance: f32,
    id: usize,
}

impl PartialEq for SearchItem {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for SearchItem {}

impl PartialOrd for SearchItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: reverse ordering; equal distances use a stable node order.
        other
            .distance
            .total_cmp(&self.distance)
            .then(other.id.cmp(&self.id))
    }
}

impl HnswIndex {
    /// Create a new empty index.
    pub fn new(dim: usize, max_m: usize, ef_construction: usize, ef_search: usize) -> Self {
        Self {
            nodes: Vec::new(),
            max_m,
            ef_search,
            ef_construction,
            dim,
            entry_point: 0,
            initialized: false,
            tombstones: HashSet::new(),
        }
    }

    /// Build the index from a list of (id, doc_id, vector) triples.
    pub fn build(&mut self, items: &[(String, String, Vec<f32>)]) {
        self.build_with_progress(items, |_, _| {});
    }

    /// Build the index with a progress callback.
    /// `callback(current, total)` is called periodically during construction.
    pub fn build_with_progress<F: Fn(usize, usize)>(
        &mut self,
        items: &[(String, String, Vec<f32>)],
        callback: F,
    ) {
        if items.is_empty() {
            self.nodes.clear();
            self.tombstones.clear();
            self.initialized = false;
            self.entry_point = 0;
            return;
        }

        // 全量重建 = 天然压实：输入来自库内现存行，墓碑一并清零
        self.tombstones.clear();

        // Store all nodes
        self.nodes = items
            .iter()
            .map(|(id, doc_id, vec)| IndexNode {
                id: id.clone(),
                doc_id: doc_id.clone(),
                vector: vec.clone(),
                neighbours: Vec::new(),
            })
            .collect();

        self.entry_point = 0;
        self.initialized = true;

        // Build connectivity: for each node, find M nearest neighbours
        // Use brute-force KNN with early-termination optimization.
        let n = self.nodes.len();
        let max_m = self.max_m;
        let progress_step = (n / 100).max(1); // Report ~every 1%

        for i in 0..n {
            // Compute distances to all other nodes
            let query = &self.nodes[i].vector;
            let mut dists: Vec<(f32, usize)> = Vec::with_capacity(n - 1);
            for j in 0..n {
                if j == i {
                    continue;
                }
                dists.push((cosine_distance(query, &self.nodes[j].vector), j));
            }

            // Partial sort: only need top-M, use selection instead of full sort
            // select_nth_unstable_by pivot index must be < len (0..len-1)
            let k = max_m.min(dists.len().saturating_sub(1));
            if !dists.is_empty() {
                dists.select_nth_unstable_by(k, |a, b| {
                    a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal)
                });
            }
            let top_m: Vec<usize> = dists.into_iter().take(max_m).map(|(_, j)| j).collect();

            // Set neighbours for node i
            self.nodes[i].neighbours = top_m.clone();

            // Add reverse edges
            for &neighbour_idx in &top_m {
                if neighbour_idx == i {
                    continue;
                }
                if neighbour_idx < self.nodes.len() {
                    let node = &mut self.nodes[neighbour_idx];
                    if !node.neighbours.contains(&i) && node.neighbours.len() < max_m {
                        node.neighbours.push(i);
                    }
                }
            }

            // Report progress every 1%
            if i > 0 && i % progress_step == 0 {
                let pct = i * 100 / n;
                tracing::info!("HNSW build progress: {}/{} nodes ({}%)", i, n, pct);
                callback(i, n);
            }
        }

        // 近邻图可能分成多个簇；保留相邻节点的双向骨架，度数最多 M + 2。
        self.connect_backbone();

        // Final callback
        callback(n, n);

        tracing::info!(
            "HNSW index built: {} nodes, dim {}, M={}, ef_search={}",
            n,
            self.dim,
            self.max_m,
            self.ef_search
        );
    }

    /// Search the index for the k nearest neighbours.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<SearchResult> {
        if !self.initialized || self.nodes.is_empty() || k == 0 || query.len() != self.dim {
            return Vec::new();
        }

        // ponytail: 千级以内的小库直接精确搜索；大库保留图搜索，避免线性开销。
        // 同时兼容旧图的连通性缺陷，不需要为查询重新调用 embedding。
        let candidates = if self.nodes.len() <= 1024 {
            let mut all: Vec<SearchItem> = self
                .nodes
                .iter()
                .enumerate()
                .map(|(id, node)| SearchItem {
                    id,
                    distance: cosine_distance(query, &node.vector),
                })
                .collect();
            all.sort_by(|a, b| a.distance.total_cmp(&b.distance).then(a.id.cmp(&b.id)));
            all
        } else {
            let ef = self
                .ef_search
                .max(k)
                .saturating_add(self.tombstones.len())
                .min(self.nodes.len());
            self.search_internal(query, ef, usize::MAX)
        };

        // Convert internal indices to external IDs and compute scores.
        // 墓碑节点（已摘除）在结果组装前过滤，保证 take(k) 全部是存活节点。
        candidates
            .into_iter()
            .filter(|r| !self.tombstones.contains(&self.nodes[r.id].id))
            .take(k)
            .map(|r| SearchResult {
                id: self.nodes[r.id].id.clone(),
                score: 1.0 - r.distance, // Convert distance to similarity score
            })
            .collect()
    }

    /// 单点插入（增量索引用）：贪心下沉找最近邻、双向连边，不重排既有节点。
    /// 返回 false 表示未插入（维度不符，或同 id 存活节点已存在——
    /// 增量差集保证新 chunk id 全新，出现重复说明上游数据异常）。
    pub fn insert(&mut self, id: &str, doc_id: &str, vector: &[f32]) -> bool {
        if vector.len() != self.dim || self.contains_live(id) {
            return false;
        }
        let new_idx = self.nodes.len();

        // 空索引/未初始化：直接作为唯一节点（即入口点）
        let neighbours: Vec<usize> = if !self.initialized || self.nodes.is_empty() {
            Vec::new()
        } else {
            let ef = self
                .ef_construction
                .max(self.max_m)
                .saturating_add(self.tombstones.len())
                .min(self.nodes.len());
            self.search_internal(vector, ef, usize::MAX)
                .into_iter()
                .filter(|r| !self.tombstones.contains(&self.nodes[r.id].id))
                .map(|r| r.id)
                .take(self.max_m)
                .collect()
        };

        self.nodes.push(IndexNode {
            id: id.to_string(),
            doc_id: doc_id.to_string(),
            vector: vector.to_vec(),
            neighbours: neighbours.clone(),
        });

        // 反向连边：未满直接加；已满则用新节点替换 host 当前最远邻居。
        // 全量 build 会把多数节点的邻居表占满 max_m，只做「满则跳过」会
        // 让新节点在图中不可达（贪心搜索没有任何边指向它）。
        for &nb in &neighbours {
            if nb >= new_idx {
                continue;
            }
            let host_full = self.nodes[nb].neighbours.len() >= self.max_m;
            if !host_full {
                let host = &mut self.nodes[nb];
                if !host.neighbours.contains(&new_idx) {
                    host.neighbours.push(new_idx);
                }
                continue;
            }
            let host_vec = self.nodes[nb].vector.clone();
            let mut worst: Option<(f32, usize)> = None; // (distance, position)
            for (pos, &cand) in self.nodes[nb].neighbours.iter().enumerate() {
                // 不替换连通骨架边，否则增量插入可能再次割裂旧图。
                if cand.abs_diff(nb) == 1 {
                    continue;
                }
                let d = cosine_distance(&host_vec, &self.nodes[cand].vector);
                if worst.map(|(wd, _)| d > wd).unwrap_or(true) {
                    worst = Some((d, pos));
                }
            }
            if let Some((worst_d, pos)) = worst {
                if cosine_distance(&host_vec, vector) < worst_d
                    && !self.nodes[nb].neighbours.contains(&new_idx)
                {
                    self.nodes[nb].neighbours[pos] = new_idx;
                }
            }
        }

        if new_idx > 0 {
            self.connect_pair(new_idx - 1, new_idx);
        }

        if !self.initialized {
            self.entry_point = new_idx;
            self.initialized = true;
        } else if self.tombstones.contains(&self.nodes[self.entry_point].id) {
            // 入口点已被摘除：刷新为新节点，保证贪心搜索从存活节点出发
            self.entry_point = new_idx;
        }
        true
    }

    fn connect_pair(&mut self, a: usize, b: usize) {
        if !self.nodes[a].neighbours.contains(&b) {
            self.nodes[a].neighbours.push(b);
        }
        if !self.nodes[b].neighbours.contains(&a) {
            self.nodes[b].neighbours.push(a);
        }
    }

    fn connect_backbone(&mut self) {
        for i in 1..self.nodes.len() {
            self.connect_pair(i - 1, i);
        }
    }

    /// 按 chunk id 摘除（墓碑）：检索不再返回、len 扣减。
    /// 图内不做物理摘除，压实由全量 build 完成；节点不存在返回 false。
    pub fn remove(&mut self, id: &str) -> bool {
        if !self.contains_live(id) {
            return false;
        }
        self.tombstones.insert(id.to_string());
        true
    }

    /// 该文档当前存活的 chunk id 列表（增量差集的索引侧输入）
    pub fn doc_node_ids(&self, doc_id: &str) -> Vec<String> {
        self.nodes
            .iter()
            .filter(|n| n.doc_id == doc_id && !self.tombstones.contains(&n.id))
            .map(|n| n.id.clone())
            .collect()
    }

    /// 是否存在该 chunk id 的存活节点
    pub fn contains_live(&self, id: &str) -> bool {
        self.nodes
            .iter()
            .any(|n| n.id == id && !self.tombstones.contains(&n.id))
    }

    /// 旧格式索引（节点无 doc_id）：增量路径应回退全量重建。
    /// schema 中 chunk 的 doc_id 非空，因此「有节点且全空」即旧文件。
    pub fn is_legacy_format(&self) -> bool {
        self.initialized && !self.nodes.is_empty() && self.nodes.iter().all(|n| n.doc_id.is_empty())
    }

    /// Internal greedy search starting from the entry point.
    /// Returns internal node indices sorted by distance (closest first).
    /// `exclude` is the node index to exclude (used during construction).
    fn search_internal(&self, query: &[f32], ef: usize, exclude: usize) -> Vec<SearchItem> {
        if self.nodes.is_empty() || ef == 0 {
            return Vec::new();
        }

        let n = self.nodes.len();
        let start = if exclude == self.entry_point && n > 1 {
            1
        } else {
            self.entry_point
        };

        let mut visited: HashSet<usize> = HashSet::new();
        visited.insert(exclude);

        let mut candidates: BinaryHeap<SearchItem> = BinaryHeap::new();
        // candidates 最近者优先；results 必须最远者优先，容量满时才会淘汰差结果。
        let mut results: BinaryHeap<Reverse<SearchItem>> = BinaryHeap::new();

        // Start from entry point
        let start_dist = cosine_distance(query, &self.nodes[start].vector);
        candidates.push(SearchItem {
            distance: start_dist,
            id: start,
        });
        results.push(Reverse(SearchItem {
            distance: start_dist,
            id: start,
        }));
        visited.insert(start);

        while let Some(SearchItem {
            distance: dist,
            id: curr,
        }) = candidates.pop()
        {
            // Check if we should stop
            let furthest_in_results = results.peek().map(|r| r.0.distance).unwrap_or(f32::MAX);

            if results.len() >= ef && dist > furthest_in_results {
                break;
            }

            // Explore neighbours
            for &neighbour_idx in &self.nodes[curr].neighbours {
                if visited.contains(&neighbour_idx) || neighbour_idx >= self.nodes.len() {
                    continue;
                }
                visited.insert(neighbour_idx);

                let neighbour_dist = cosine_distance(query, &self.nodes[neighbour_idx].vector);

                let furthest = results.peek().map(|r| r.0.distance).unwrap_or(f32::MAX);

                if results.len() < ef || neighbour_dist < furthest {
                    candidates.push(SearchItem {
                        distance: neighbour_dist,
                        id: neighbour_idx,
                    });
                    results.push(Reverse(SearchItem {
                        distance: neighbour_dist,
                        id: neighbour_idx,
                    }));

                    // Keep results bounded to ef — pop the furthest (max distance)
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        // Sort results by distance (ascending)
        let mut sorted: Vec<SearchItem> = results.drain().map(|r| r.0).collect();
        sorted.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(Ordering::Equal)
        });
        sorted
    }

    /// Serialize the index to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serialize(self).unwrap_or_default()
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        let mut index: Self =
            deserialize(data).map_err(|e| format!("Failed to deserialize HNSW index: {}", e))?;
        // 旧文件无需改格式；加载后补齐骨架，后续增量保存会保留修复结果。
        index.connect_backbone();
        Ok(index)
    }

    /// Save to file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let data = self.to_bytes();
        std::fs::write(path, &data).map_err(|e| format!("Failed to write index file: {}", e))
    }

    /// Load from file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let data = std::fs::read(path).map_err(|e| format!("Failed to read index file: {}", e))?;
        Self::from_bytes(&data)
    }

    /// Get number of live nodes (physical nodes minus tombstones).
    pub fn len(&self) -> usize {
        self.nodes.len() - self.tombstones.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Cosine distance (1 - cosine_similarity).
/// Returns 0 for identical vectors, 2 for opposite vectors.
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 1.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom == 0.0 {
        return 1.0;
    }

    let similarity = dot / denom;
    // Clamp to [-1, 1] to handle floating point errors
    let clamped = similarity.clamp(-1.0, 1.0);
    1.0 - clamped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_heap_retains_nearest_when_capacity_is_exceeded() {
        // 完全图排除近似召回因素。旧结果堆在装入第四项时会弹出最近项。
        let mut index = HnswIndex::new(2, 8, 10, 3);
        let items = (0..8)
            .map(|i| (i.to_string(), "doc".to_string(), vec![1.0, i as f32]))
            .collect::<Vec<_>>();
        index.build(&items);
        let found = index.search_internal(&[1.0, 0.0], 3, usize::MAX);
        assert_eq!(
            found.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn large_graph_search_returns_live_neighbors_past_tombstones() {
        let mut index = HnswIndex::new(2, 2, 20, 16);
        index.nodes = (0..1100)
            .map(|i| IndexNode {
                id: i.to_string(),
                doc_id: "doc".to_string(),
                vector: vec![(i as f32 * 0.002).cos(), (i as f32 * 0.002).sin()],
                neighbours: Vec::new(),
            })
            .collect();
        index.initialized = true;
        index.connect_backbone();
        for i in 1090..1100 {
            index.remove(&i.to_string());
        }
        let results = index.search(&index.nodes[1099].vector, 3);
        assert_eq!(
            results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["1089", "1088", "1087"]
        );
    }

    fn reachable(index: &HnswIndex) -> usize {
        let mut seen = HashSet::new();
        let mut pending = vec![index.entry_point];
        while let Some(node) = pending.pop() {
            if seen.insert(node) {
                pending.extend(&index.nodes[node].neighbours);
            }
        }
        seen.len()
    }

    #[test]
    fn clustered_build_load_and_far_insert_remain_connected() {
        let mut index = HnswIndex::new(2, 2, 20, 10);
        let items = (0..12)
            .map(|i| {
                (
                    i.to_string(),
                    "doc".to_string(),
                    if i < 6 {
                        vec![1.0, i as f32 * 0.01]
                    } else {
                        vec![-1.0, i as f32 * 0.01]
                    },
                )
            })
            .collect::<Vec<_>>();
        index.build(&items);
        assert_eq!(reachable(&index), items.len());
        assert!(index
            .nodes
            .iter()
            .all(|n| n.neighbours.len() <= index.max_m + 2));
        assert!(index.insert("far", "doc-new", &[0.0, -1.0]));
        assert_eq!(reachable(&index), items.len() + 1);

        // 模拟相同序列化格式的旧断连图，加载时自动补齐骨架。
        for node in &mut index.nodes {
            node.neighbours.clear();
        }
        let restored = HnswIndex::from_bytes(&index.to_bytes()).unwrap();
        assert_eq!(reachable(&restored), restored.nodes.len());
        assert_eq!(restored.search(&[0.0, -1.0], 1)[0].id, "far");
    }

    #[test]
    fn small_index_exact_results_survive_tombstones_and_empty_rebuild() {
        let mut index = HnswIndex::new(2, 2, 10, 1);
        let items = (0..20)
            .map(|i| (i.to_string(), "doc".to_string(), vec![1.0, i as f32]))
            .collect::<Vec<_>>();
        index.build(&items);
        for i in 0..18 {
            index.remove(&i.to_string());
        }
        assert_eq!(
            index
                .search(&[1.0, 0.0], 2)
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["18", "19"]
        );
        assert!(index.search(&[1.0, 0.0], 0).is_empty());
        assert!(index.search(&[1.0], 1).is_empty());
        index.build(&[]);
        assert!(index.search(&[1.0, 0.0], 2).is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_cosine_distance() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_distance(&a, &b) - 0.0).abs() < 1e-6);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_distance(&a, &c) - 1.0).abs() < 1e-6);

        let d = vec![-1.0, 0.0, 0.0];
        assert!((cosine_distance(&a, &d) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn test_build_and_search() {
        let mut index = HnswIndex::new(3, 8, 50, 20);

        // Create 100 random-ish vectors
        let items: Vec<(String, String, Vec<f32>)> = (0..100)
            .map(|i| {
                let v = vec![
                    ((i as f32) * 0.1).sin(),
                    ((i as f32) * 0.2).cos(),
                    (i as f32) * 0.01,
                ];
                (format!("chunk-{}", i), format!("doc-{}", i % 5), v)
            })
            .collect();

        index.build(&items);

        // Search for a vector similar to item 5
        let query = items[5].2.clone();
        let results = index.search(&query, 5);

        assert!(!results.is_empty());
        // The most similar should be item 5 itself (or very close)
        assert!(results[0].score > 0.99);
        assert_eq!(results[0].id, "chunk-5");
    }

    #[test]
    fn test_empty_index() {
        let index = HnswIndex::new(3, 8, 50, 20);
        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn test_serialization() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..10)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    format!("doc-{}", i % 3),
                    vec![i as f32, (i as f32) * 2.0, (i as f32) * 3.0],
                )
            })
            .collect();
        index.build(&items);

        let bytes = index.to_bytes();
        let restored = HnswIndex::from_bytes(&bytes).unwrap();

        assert_eq!(restored.len(), 10);
        assert_eq!(restored.dim, 3);

        let query = vec![1.0, 2.0, 3.0];
        let r1 = index.search(&query, 3);
        let r2 = restored.search(&query, 3);
        assert_eq!(r1.len(), r2.len());
        for i in 0..r1.len() {
            assert_eq!(r1[i].id, r2[i].id);
            assert!((r1[i].score - r2[i].score).abs() < 1e-5);
        }
    }

    #[test]
    fn test_insert_then_search_finds_new_node() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..20)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    "doc-base".to_string(),
                    vec![
                        ((i as f32) * 0.1).sin(),
                        ((i as f32) * 0.2).cos(),
                        (i as f32) * 0.01,
                    ],
                )
            })
            .collect();
        index.build(&items);

        // 新增节点紧贴 item 3 的向量
        let near3 = items[3].2.clone();
        assert!(index.insert("chunk-new", "doc-new", &near3));
        assert_eq!(index.len(), 21);

        let results = index.search(&near3, 3);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"chunk-new"), "results: {:?}", ids);
    }

    #[test]
    fn test_remove_tombstones_and_len() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..10)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    "doc-base".to_string(),
                    vec![i as f32, (i as f32) * 2.0, (i as f32) * 3.0],
                )
            })
            .collect();
        index.build(&items);

        assert!(index.remove("chunk-5"));
        assert!(!index.remove("chunk-5"), "double remove is a no-op");
        assert!(!index.remove("chunk-missing"));
        assert_eq!(index.len(), 9);

        let query = items[5].2.clone();
        let results = index.search(&query, 10);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(!ids.contains(&"chunk-5"), "tombstoned must not surface");
    }

    #[test]
    fn test_remove_does_not_harm_other_recall() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..100)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    format!("doc-{}", i % 7),
                    vec![
                        ((i as f32) * 0.1).sin(),
                        ((i as f32) * 0.2).cos(),
                        (i as f32) * 0.01,
                    ],
                )
            })
            .collect();
        index.build(&items);

        // 摘掉若干无关节点后，查询点的 Top1 仍是自身
        for i in [90usize, 91, 92, 93, 94, 95] {
            assert!(index.remove(&format!("chunk-{}", i)));
        }
        let query = items[7].2.clone();
        let results = index.search(&query, 5);
        assert_eq!(results[0].id, "chunk-7");
    }

    #[test]
    fn test_build_rebuild_compacts_tombstones() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..10)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    "doc-base".to_string(),
                    vec![i as f32, (i as f32) * 2.0, (i as f32) * 3.0],
                )
            })
            .collect();
        index.build(&items);
        index.remove("chunk-3");
        index.remove("chunk-8");
        assert_eq!(index.len(), 8);

        // 全量重建压实：墓碑清零、节点全部复活（数据仍来自库内）
        index.build(&items);
        assert_eq!(index.len(), 10);
        assert!(index.contains_live("chunk-3"));
        assert!(index.contains_live("chunk-8"));
    }

    #[test]
    fn test_doc_node_ids_and_legacy_detection() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..9)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    format!("doc-{}", i / 3),
                    vec![i as f32, (i as f32) * 2.0, (i as f32) * 3.0],
                )
            })
            .collect();
        index.build(&items);

        let mut d0 = index.doc_node_ids("doc-0");
        d0.sort();
        assert_eq!(d0, vec!["chunk-0", "chunk-1", "chunk-2"]);
        assert!(!index.is_legacy_format());

        // 摘除后 doc_node_ids 同步收缩
        index.remove("chunk-1");
        let mut d0 = index.doc_node_ids("doc-0");
        d0.sort();
        assert_eq!(d0, vec!["chunk-0", "chunk-2"]);

        // 旧格式：节点存在但 doc_id 全空
        let legacy_items: Vec<(String, String, Vec<f32>)> = (0..5)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    String::new(),
                    vec![i as f32, 1.0, 2.0],
                )
            })
            .collect();
        let mut legacy = HnswIndex::new(3, 8, 50, 20);
        legacy.build(&legacy_items);
        assert!(legacy.is_legacy_format());
    }

    #[test]
    fn test_insert_rejects_duplicate_and_dim_mismatch() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..5)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    "doc-base".to_string(),
                    vec![i as f32, 1.0, 2.0],
                )
            })
            .collect();
        index.build(&items);

        assert!(!index.insert("chunk-2", "doc-x", &[9.0, 9.0, 9.0]));
        assert!(!index.insert("chunk-new", "doc-x", &[1.0, 2.0]));
        assert_eq!(index.len(), 5);
    }

    #[test]
    fn test_insert_into_uninitialized_index() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        assert!(index.insert("first", "doc-a", &[1.0, 0.0, 0.0]));
        assert!(index.initialized);
        assert_eq!(index.len(), 1);
        let results = index.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results[0].id, "first");

        // 摘除唯一节点后再插入：物理节点仍在（墓碑），新节点可正常服务检索
        assert!(index.remove("first"));
        assert!(index.insert("second", "doc-b", &[0.0, 1.0, 0.0]));
        let results = index.search(&[0.0, 1.0, 0.0], 2);
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["second"]);
    }

    #[test]
    fn test_serde_roundtrip_preserves_tombstones_and_doc_ids() {
        let mut index = HnswIndex::new(3, 8, 50, 20);
        let items: Vec<(String, String, Vec<f32>)> = (0..10)
            .map(|i| {
                (
                    format!("chunk-{}", i),
                    format!("doc-{}", i % 2),
                    vec![i as f32, (i as f32) * 2.0, (i as f32) * 3.0],
                )
            })
            .collect();
        index.build(&items);
        index.remove("chunk-4");
        index.insert("chunk-new", "doc-9", &[4.0, 8.0, 12.0]);

        let restored = HnswIndex::from_bytes(&index.to_bytes()).unwrap();
        assert_eq!(restored.len(), index.len());
        assert!(restored.tombstones.contains("chunk-4"));
        assert!(restored.contains_live("chunk-new"));
        assert_eq!(restored.doc_node_ids("doc-9"), vec!["chunk-new"]);
    }
}
