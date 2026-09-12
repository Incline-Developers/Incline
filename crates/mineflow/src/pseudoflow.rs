//! Index-based port of MineFlow's highest-label pseudoflow forest.
//!
//! Tree arcs carry flow; all other precedence arcs are generated lazily. A gap
//! in the distance labels lifts an entire strong branch into the minimum cut.
//!
//! Node links are 32 bit indices with [`NONE`] standing in for a null link, so
//! a node stays inside one cache line. The solver is almost entirely pointer
//! chasing over [`Forest::nodes`], and every byte of that array costs.
use std::collections::VecDeque;

use crate::{Error, Precedence, Result, error::invalid};

/// A node index. The solver addresses at most `NONE - 1` blocks.
type Idx = u32;
const NONE: Idx = Idx::MAX;

/// Out-of-tree lists live beside the nodes rather than inside them: only
/// `find_weak` and the arc split in `push_flow` touch them.
struct Node {
    excess: i128,
    /// The tree arc to this node's parent, or [`NONE`] for a root.
    to_root: Idx,
    /// The other endpoint of `to_root`, cached to avoid chasing the arc.
    parent: Idx,
    first_child: Idx,
    next_child: Idx,
    next_scan: Idx,
    label: Idx,
    next_arc: Idx,
    initialized: bool,
}
impl Node {
    fn new(excess: i128, label: Idx) -> Self {
        Self {
            excess,
            to_root: NONE,
            parent: NONE,
            first_child: NONE,
            next_child: NONE,
            next_scan: NONE,
            label,
            next_arc: 0,
            initialized: false,
        }
    }
}
struct Arc {
    tail: Idx,
    head: Idx,
    flow: i128,
}

pub(crate) struct Forest {
    nodes: Vec<Node>,
    /// Per node, the precedence arcs not currently in the tree.
    out_of_tree: Vec<Vec<Idx>>,
    /// Reused by `find_weak` for one `Precedence::fill` call.
    scratch: Vec<i64>,
    arcs: Vec<Arc>,
    free_arcs: Vec<Idx>,
    labels: Vec<Idx>,
    buckets: Vec<VecDeque<Idx>>,
    pub(crate) used_arcs: usize,
}
impl Forest {
    pub(crate) fn new(values: impl ExactSizeIterator<Item = i128>) -> Result<Self> {
        let n = values.len();
        if n >= NONE as usize {
            return Err(invalid("the solver addresses at most 4294967294 blocks"));
        }
        let mut nodes = Vec::with_capacity(n);
        let mut labels = vec![0; 2];
        let mut buckets = vec![VecDeque::new(); 2];
        for value in values {
            let label = Idx::from(value > 0);
            labels[label as usize] += 1;
            if value > 0 {
                buckets[1].push_back(nodes.len() as Idx);
            }
            nodes.push(Node::new(value, label));
        }
        let mut out_of_tree = Vec::new();
        out_of_tree.try_reserve_exact(n).map_err(|_| invalid("graph exceeds memory"))?;
        out_of_tree.resize_with(n, Vec::new);
        Ok(Self {
            nodes,
            out_of_tree,
            scratch: Vec::new(),
            labels,
            buckets,
            arcs: Vec::new(),
            free_arcs: Vec::new(),
            used_arcs: 0,
        })
    }
    fn node(&self, node: Idx) -> &Node {
        &self.nodes[node as usize]
    }
    fn node_mut(&mut self, node: Idx) -> &mut Node {
        &mut self.nodes[node as usize]
    }
    /// Links `child` under `parent` and records the tree arc joining them.
    fn attach(&mut self, parent: Idx, child: Idx, arc: Idx) {
        debug_assert!({
            let a = &self.arcs[arc as usize];
            (a.tail == child && a.head == parent) || (a.head == child && a.tail == parent)
        });
        self.node_mut(child).to_root = arc;
        self.node_mut(child).parent = parent;
        self.add_child(parent, child);
    }
    fn add_child(&mut self, parent: Idx, child: Idx) {
        debug_assert!(self.node(child).next_child == NONE);
        self.node_mut(child).next_child = self.node(parent).first_child;
        self.node_mut(parent).first_child = child;
    }
    fn remove_child(&mut self, parent: Idx, child: Idx) {
        let next = self.node(child).next_child;
        if self.node(parent).first_child == child {
            self.node_mut(parent).first_child = next;
        } else {
            let mut current = self.node(parent).first_child;
            debug_assert!(current != NONE, "child belongs to parent");
            while self.node(current).next_child != child {
                current = self.node(current).next_child;
                debug_assert!(current != NONE, "child belongs to parent");
            }
            self.node_mut(current).next_child = next;
        }
        self.node_mut(child).next_child = NONE;
    }
    fn increment_label(&mut self, node: Idx) {
        let label = self.node(node).label as usize;
        self.labels[label] -= 1;
        self.node_mut(node).label += 1;
        if self.labels.len() <= label + 1 {
            self.labels.resize(label + 2, 0);
        }
        self.labels[label + 1] += 1;
    }
    fn push_strong(&mut self, node: Idx) {
        let label = self.node(node).label as usize;
        if self.buckets.len() <= label {
            self.buckets.resize_with(label + 1, VecDeque::new);
        }
        self.buckets[label].push_back(node);
    }
    fn next_strong(&mut self, cancelled: &mut impl FnMut() -> bool) -> Result<Option<Idx>> {
        let sink = self.nodes.len() as Idx;
        for label in (1..self.buckets.len()).rev() {
            if self.buckets[label].is_empty() {
                self.buckets.pop();
            } else if self.labels[label - 1] > 0 {
                return Ok(self.buckets[label].pop_front());
            } else {
                // Iterative traversal avoids a stack overflow on deep mine models.
                while let Some(root) = self.buckets[label].pop_front() {
                    let mut stack = vec![root];
                    while let Some(node) = stack.pop() {
                        if cancelled() {
                            return Err(Error::Cancelled);
                        }
                        let label = self.node(node).label as usize;
                        self.labels[label] -= 1;
                        self.node_mut(node).label = sink;
                        let mut child = self.node(node).first_child;
                        while child != NONE {
                            stack.push(child);
                            child = self.node(child).next_child;
                        }
                    }
                }
            }
        }
        if self.buckets[0].is_empty() {
            return Ok(None);
        }
        while let Some(root) = self.buckets[0].pop_front() {
            self.increment_label(root);
            self.push_strong(root);
        }
        Ok(self.buckets[1].pop_front())
    }
    fn find_weak(&mut self, node: Idx, precedence: &Precedence, cancelled: &mut impl FnMut() -> bool) -> Result<Option<Idx>> {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        if !self.node(node).initialized {
            precedence.fill(i64::from(node), false, &mut self.scratch)?;
            let list = &mut self.out_of_tree[node as usize];
            list.clear();
            list.extend(self.scratch.iter().map(|&v| v as Idx));
            self.node_mut(node).initialized = true;
        }
        let Some(target_label) = self.node(node).label.checked_sub(1) else {
            return Ok(None);
        };
        // `out_of_tree` and `nodes` are separate fields, so the scan borrows
        // both at once and costs one random access per candidate.
        let start = self.node(node).next_arc as usize;
        let list = &self.out_of_tree[node as usize];
        let nodes = &self.nodes;
        let found = list[start..].iter().position(|&target| nodes[target as usize].label == target_label);
        let Some(offset) = found else {
            self.node_mut(node).next_arc = list.len() as Idx;
            return Ok(None);
        };
        let i = start + offset;
        self.node_mut(node).next_arc = i as Idx;
        Ok(Some(self.out_of_tree[node as usize].swap_remove(i)))
    }
    fn process_children(&mut self, node: Idx) {
        let label = self.node(node).label;
        let mut child = self.node(node).next_scan;
        while child != NONE {
            debug_assert!(self.node(child).label >= label);
            if self.node(child).label == label {
                self.node_mut(node).next_scan = child;
                return;
            }
            child = self.node(child).next_child;
        }
        self.node_mut(node).next_scan = NONE;
        self.increment_label(node);
        self.node_mut(node).next_arc = 0;
    }
    fn merge(&mut self, strong: Idx, weak: Idx, cancelled: &mut impl FnMut() -> bool) -> Result<()> {
        let arc = Arc {
            tail: strong,
            head: weak,
            flow: 0,
        };
        let mut new_arc = if let Some(i) = self.free_arcs.pop() {
            self.arcs[i as usize] = arc;
            i
        } else {
            self.arcs.push(arc);
            (self.arcs.len() - 1) as Idx
        };
        self.used_arcs += 1;
        let mut current = strong;
        let mut new_parent = weak;
        loop {
            let old_parent = self.node(current).parent;
            if old_parent == NONE {
                break;
            }
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let old_arc = self.node(current).to_root;
            debug_assert!(old_arc != NONE, "non-root has arc");
            self.remove_child(old_parent, current);
            self.attach(new_parent, current, new_arc);
            new_parent = current;
            current = old_parent;
            new_arc = old_arc;
        }
        self.attach(new_parent, current, new_arc);
        self.push_flow(current, cancelled)
    }
    fn push_flow(&mut self, root: Idx, cancelled: &mut impl FnMut() -> bool) -> Result<()> {
        let mut previous_excess = 1;
        let mut current = root;
        while self.node(current).excess > 0 {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let parent = self.node(current).parent;
            if parent == NONE {
                break;
            }
            let arc = self.node(current).to_root as usize;
            debug_assert!(arc != NONE as usize, "non-root has arc");
            previous_excess = self.node(parent).excess;
            let excess = self.node(current).excess;
            if self.arcs[arc].tail == current {
                self.node_mut(parent).excess += excess;
                self.arcs[arc].flow += excess;
                self.node_mut(current).excess = 0;
            } else if self.arcs[arc].flow >= excess {
                self.node_mut(parent).excess += excess;
                self.arcs[arc].flow -= excess;
                self.node_mut(current).excess = 0;
            } else {
                // Exhausted reverse arc: split and restore it to the out-of-tree list.
                let flow = self.arcs[arc].flow;
                self.node_mut(current).excess -= flow;
                self.node_mut(parent).excess += flow;
                self.free_arcs.push(arc as Idx);
                self.out_of_tree[parent as usize].push(current);
                self.remove_child(parent, current);
                self.node_mut(current).to_root = NONE;
                self.node_mut(current).parent = NONE;
                self.push_strong(current);
            }
            current = parent;
        }
        if self.node(current).excess > 0 && previous_excess <= 0 {
            self.push_strong(current);
        }
        Ok(())
    }
    fn process_strong(&mut self, root: Idx, precedence: &Precedence, cancelled: &mut impl FnMut() -> bool) -> Result<()> {
        self.node_mut(root).next_scan = self.node(root).first_child;
        if let Some(weak) = self.find_weak(root, precedence, cancelled)? {
            return self.merge(root, weak, cancelled);
        }
        self.process_children(root);
        let mut strong = root;
        loop {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            let mut node = strong;
            loop {
                let child = self.node(node).next_scan;
                if child == NONE {
                    break;
                }
                self.node_mut(node).next_scan = self.node(child).next_child;
                node = child;
                self.node_mut(node).next_scan = self.node(node).first_child;
                if let Some(weak) = self.find_weak(node, precedence, cancelled)? {
                    return self.merge(node, weak, cancelled);
                }
                self.process_children(node);
            }
            strong = self.node(node).parent;
            if strong == NONE {
                break;
            }
            self.process_children(strong);
        }
        self.push_strong(root);
        Ok(())
    }
    pub(crate) fn solve(&mut self, precedence: &Precedence, cancelled: &mut impl FnMut() -> bool) -> Result<Vec<bool>> {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        while let Some(root) = self.next_strong(cancelled)? {
            self.process_strong(root, precedence, cancelled)?;
        }
        let sink = self.nodes.len() as Idx;
        Ok(self.nodes.iter().map(|node| node.label == sink).collect())
    }

    /// The largest optimal cut excludes precisely the nodes that can reach
    /// a deficit in the residual graph. A positive-flow tree arc is residual
    /// in both directions, so its endpoints always share an answer and are
    /// treated as one contracted branch. Zero-flow arcs must not be contracted:
    /// their endpoints may belong to different optimal cuts, which is where
    /// upstream MineFlow drops break-even blocks from its largest pit.
    pub(crate) fn largest(&self, precedence: &Precedence, cancelled: &mut impl FnMut() -> bool) -> Result<Vec<bool>> {
        let mut excluded = vec![false; self.nodes.len()];
        let mut queue = Vec::new();
        let mut branch = Vec::new();
        for node in 0..self.nodes.len() {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if self.nodes[node].excess < 0 {
                self.exclude_branch(node as Idx, &mut excluded, &mut queue, &mut branch);
            }
        }
        if precedence.reverse_is_exact() {
            self.spread_backwards(precedence, &mut excluded, &mut queue, &mut branch, cancelled)?;
        } else {
            self.spread_through_dependents(precedence, &mut excluded, &mut queue, cancelled)?;
        }
        Ok(excluded.into_iter().map(|out| !out).collect())
    }

    /// Marks every node contracted with `start` and queues the newly marked.
    fn exclude_branch(&self, start: Idx, excluded: &mut [bool], queue: &mut Vec<Idx>, stack: &mut Vec<Idx>) {
        let mark = |node: Idx, excluded: &mut [bool], queue: &mut Vec<Idx>, stack: &mut Vec<Idx>| {
            if excluded[node as usize] {
                return;
            }
            excluded[node as usize] = true;
            queue.push(node);
            stack.push(node);
        };
        mark(start, excluded, queue, stack);
        while let Some(node) = stack.pop() {
            let arc = self.node(node).to_root;
            if arc != NONE && self.arcs[arc as usize].flow > 0 {
                mark(self.node(node).parent, excluded, queue, stack);
            }
            let mut child = self.node(node).first_child;
            while child != NONE {
                let arc = self.node(child).to_root;
                if arc != NONE && self.arcs[arc as usize].flow > 0 {
                    mark(child, excluded, queue, stack);
                }
                child = self.node(child).next_child;
            }
        }
    }

    /// Walks the precedence graph backwards from each excluded node: anything
    /// that requires an excluded block can itself reach a deficit. Each node is
    /// queued once, so this asks the graph for successors at most once per
    /// excluded block and builds nothing.
    fn spread_backwards(&self, precedence: &Precedence, excluded: &mut [bool], queue: &mut Vec<Idx>, branch: &mut Vec<Idx>, cancelled: &mut impl FnMut() -> bool) -> Result<()> {
        let sink = self.nodes.len() as Idx;
        let mut successors = Vec::new();
        while let Some(node) = queue.pop() {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            precedence.fill(i64::from(node), true, &mut successors)?;
            for &successor in &successors {
                let successor = successor as Idx;
                // The smallest optimal cut is already closed and cannot reach
                // a deficit, so it never carries exclusion onwards.
                if !excluded[successor as usize] && self.node(successor).label != sink {
                    self.exclude_branch(successor, excluded, queue, branch);
                }
            }
        }
        Ok(())
    }

    /// The same spread for a source that only answers antecedent queries. The
    /// reversed edges between contracted branches have to be reconstructed, in
    /// compressed row storage: count the crossing edges per branch, prefix-sum,
    /// then fill. A vector per branch would allocate once per node.
    fn spread_through_dependents(&self, precedence: &Precedence, excluded: &mut [bool], queue: &mut Vec<Idx>, cancelled: &mut impl FnMut() -> bool) -> Result<()> {
        let n = self.nodes.len();
        let sink = self.nodes.len() as Idx;
        // One representative per contracted branch, so an edge between two
        // nodes becomes an edge between two branches.
        let mut component: Vec<Idx> = vec![NONE; n];
        let mut stack = Vec::new();
        for node in 0..n {
            if component[node] == NONE {
                self.collect_branch(node as Idx, &mut component, &mut stack);
            }
        }
        let mut offsets = vec![0usize; n + 1];
        let mut neighbours = Vec::new();
        let mut edges_len = 0usize;
        for node in 0..n {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if self.nodes[node].label == sink {
                continue;
            }
            let branch = component[node];
            precedence.fill(node as i64, false, &mut neighbours)?;
            let mut last = NONE;
            for &to in &neighbours {
                let required = component[to as usize];
                if required != branch && required != last {
                    offsets[required as usize + 1] += 1;
                    edges_len += 1;
                    last = required;
                }
            }
        }
        for i in 0..n {
            offsets[i + 1] += offsets[i];
        }
        let mut edges = Vec::new();
        edges.try_reserve_exact(edges_len).map_err(|_| invalid("dependency graph exceeds memory"))?;
        edges.resize(edges_len, 0);
        // The filling pass must visit the same edges in the same order as the
        // counting pass; the bounds check below catches a source that does not.
        let mut cursor = offsets.clone();
        for node in 0..n {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            if self.nodes[node].label == sink {
                continue;
            }
            let branch = component[node];
            precedence.fill(node as i64, false, &mut neighbours)?;
            let mut last = NONE;
            for &to in &neighbours {
                let required = component[to as usize];
                if required != branch && required != last {
                    let slot = &mut cursor[required as usize];
                    if *slot >= offsets[required as usize + 1] {
                        return Err(invalid("precedence source is not deterministic"));
                    }
                    edges[*slot] = branch;
                    *slot += 1;
                    last = required;
                }
            }
        }
        if cursor[..n] != offsets[1..=n] {
            return Err(invalid("precedence source is not deterministic"));
        }
        drop(cursor);

        // The queue holds nodes; exclusion spreads between branch representatives.
        let mut branches: Vec<Idx> = queue.drain(..).map(|node| component[node as usize]).collect();
        branches.sort_unstable();
        branches.dedup();
        while let Some(branch) = branches.pop() {
            if cancelled() {
                return Err(Error::Cancelled);
            }
            for &dependent in &edges[offsets[branch as usize]..offsets[branch as usize + 1]] {
                if !excluded[dependent as usize] {
                    excluded[dependent as usize] = true;
                    branches.push(dependent);
                }
            }
        }
        for node in 0..n {
            excluded[node] = excluded[component[node] as usize];
        }
        Ok(())
    }

    /// Labels every node contracted with `start` by one representative index.
    fn collect_branch(&self, start: Idx, component: &mut [Idx], stack: &mut Vec<Idx>) {
        component[start as usize] = start;
        stack.push(start);
        while let Some(node) = stack.pop() {
            let visit = |next: Idx, component: &mut [Idx], stack: &mut Vec<Idx>| {
                if component[next as usize] == NONE {
                    component[next as usize] = start;
                    stack.push(next);
                }
            };
            let arc = self.node(node).to_root;
            if arc != NONE && self.arcs[arc as usize].flow > 0 {
                visit(self.node(node).parent, component, stack);
            }
            let mut child = self.node(node).first_child;
            while child != NONE {
                let arc = self.node(child).to_root;
                if arc != NONE && self.arcs[arc as usize].flow > 0 {
                    visit(child, component, stack);
                }
                child = self.node(child).next_child;
            }
        }
    }
}
