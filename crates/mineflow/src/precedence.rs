//! Explicit and implicit precedence graphs. An edge (a, b) means a requires b.
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{
    Angle, BlockModel, Pattern, PrecedenceSource, Slope,
    error::{Result, invalid},
    pattern::Offset,
};

trait GraphSource: Send + Sync {
    fn fill(&self, block: i64, reverse: bool, out: &mut Vec<i64>) -> Result<()>;
    /// True for the graphs this crate builds itself. Two guarantees follow from
    /// their construction and let the solver skip work: every index they return
    /// addresses the model, and their successors are exactly the reverse of
    /// their antecedents. Neither holds for an arbitrary [`PrecedenceSource`].
    fn is_builtin(&self) -> bool {
        false
    }
}
struct SourceAdapter<S, const BUILTIN: bool>(S);
impl<S: PrecedenceSource, const BUILTIN: bool> GraphSource for SourceAdapter<S, BUILTIN> {
    fn is_builtin(&self) -> bool {
        BUILTIN
    }
    fn fill(&self, block: i64, reverse: bool, out: &mut Vec<i64>) -> Result<()> {
        if reverse {
            if !S::PROVIDES_SUCCESSORS {
                return Err(invalid("source does not provide successors"));
            }
            self.0.successors(block, out);
        } else {
            self.0.antecedents(block, out);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Precedence {
    source: Arc<dyn GraphSource>,
    count: usize,
}
impl std::fmt::Debug for Precedence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Precedence").field("num_blocks", &self.count).finish_non_exhaustive()
    }
}
struct Explicit {
    above: Vec<Vec<i64>>,
    below: Vec<Vec<i64>>,
}
impl PrecedenceSource for Explicit {
    const PROVIDES_SUCCESSORS: bool = true;
    fn num_blocks(&self) -> i64 {
        self.above.len() as i64
    }
    fn antecedents(&self, b: i64, out: &mut Vec<i64>) {
        out.extend(&self.above[b as usize]);
    }
    fn successors(&self, b: i64, out: &mut Vec<i64>) {
        out.extend(&self.below[b as usize]);
    }
}
/// The inclusive coordinate range on each axis within which every offset of a
/// pattern lands inside the model. A block in that box needs no range tests at
/// all, which is the common case away from the model boundary. An empty range
/// simply never matches.
#[derive(Clone, Copy)]
struct Interior {
    lo: [i64; 3],
    hi: [i64; 3],
}
impl Interior {
    fn new(extent: [i64; 3], offsets: &[Offset], reverse: bool) -> Self {
        let mut interior = Self { lo: [0; 3], hi: [0; 3] };
        for (axis, size) in extent.iter().enumerate() {
            let (mut least, mut most) = (0, 0);
            for offset in offsets {
                let d = [offset.0, offset.1, offset.2][axis];
                // Offsets are reversible: `Pattern::from_offsets` rejects i64::MIN.
                let d = if reverse { -d } else { d };
                least = least.min(d);
                most = most.max(d);
            }
            interior.lo[axis] = 0i64.saturating_sub(least).max(0);
            interior.hi[axis] = (size - 1).saturating_sub(most);
        }
        interior
    }
    fn contains(&self, x: i64, y: i64, z: i64) -> bool {
        let p = [x, y, z];
        (0..3).all(|axis| p[axis] >= self.lo[axis] && p[axis] <= self.hi[axis])
    }
}
/// One pattern, resolved against a model.
///
/// `deltas[i]` is the change offset `i` makes to the 1D block index; negated,
/// it is the change its reverse makes. Whenever the offset block is inside the
/// model that delta is exact, so no division is needed.
struct Steps {
    offsets: Vec<Offset>,
    deltas: Vec<i64>,
    forward: Interior,
    reverse: Interior,
}
impl Steps {
    fn new(model: &BlockModel, pattern: &Pattern) -> Self {
        let layer = model.num_x.wrapping_mul(model.num_y);
        let deltas = pattern
            .offsets
            .iter()
            .map(|&(x, y, z)| x.wrapping_add(y.wrapping_mul(model.num_x)).wrapping_add(z.wrapping_mul(layer)))
            .collect();
        let extent = model.extent();
        Self {
            forward: Interior::new(extent, &pattern.offsets, false),
            reverse: Interior::new(extent, &pattern.offsets, true),
            offsets: pattern.offsets.clone(),
            deltas,
        }
    }
}
struct Regular {
    model: BlockModel,
    steps: Vec<Steps>,
    keys: Option<Vec<usize>>,
}
impl Regular {
    fn new(model: BlockModel, patterns: &[Pattern], keys: Option<Vec<usize>>) -> Self {
        Self {
            steps: patterns.iter().map(|p| Steps::new(&model, p)).collect(),
            model,
            keys,
        }
    }
    fn key(&self, b: i64) -> usize {
        self.keys.as_ref().map_or(0, |k| k[b as usize])
    }
}
impl PrecedenceSource for Regular {
    const PROVIDES_SUCCESSORS: bool = true;
    fn num_blocks(&self) -> i64 {
        self.model.num_blocks()
    }
    fn antecedents(&self, b: i64, out: &mut Vec<i64>) {
        let steps = &self.steps[self.key(b)];
        let (x, y, z) = self.model.xyz(b);
        if steps.forward.contains(x, y, z) {
            out.extend(steps.deltas.iter().map(|delta| b + delta));
            return;
        }
        out.reserve(steps.deltas.len());
        for (&(dx, dy, dz), delta) in steps.offsets.iter().zip(&steps.deltas) {
            if let (Some(x), Some(y), Some(z)) = (x.checked_add(dx), y.checked_add(dy), z.checked_add(dz))
                && self.model.contains(x, y, z)
            {
                out.push(b + delta);
            }
        }
    }
    fn successors(&self, b: i64, out: &mut Vec<i64>) {
        // With more than one pattern a candidate only counts when it is the
        // pattern that block actually uses, which needs the candidate's index.
        let [steps] = &self.steps[..] else {
            for (key, steps) in self.steps.iter().enumerate() {
                for &(x, y, z) in &steps.offsets {
                    if let Some(i) = self.model.offset(b, (-x, -y, -z))
                        && self.key(i) == key
                    {
                        out.push(i);
                    }
                }
            }
            return;
        };
        let (x, y, z) = self.model.xyz(b);
        if steps.reverse.contains(x, y, z) {
            out.extend(steps.deltas.iter().map(|delta| b - delta));
            return;
        }
        out.reserve(steps.deltas.len());
        for (&(dx, dy, dz), delta) in steps.offsets.iter().zip(&steps.deltas) {
            if let (Some(x), Some(y), Some(z)) = (x.checked_sub(dx), y.checked_sub(dy), z.checked_sub(dz))
                && self.model.contains(x, y, z)
            {
                out.push(b - delta);
            }
        }
    }
}

/// Reusable visitation storage. A generation counter avoids clearing every node
/// between searches; nodes are visited at most once even for cyclic graphs.
#[derive(Debug)]
pub struct SearchBuffer {
    seen: Vec<u64>,
    generation: u64,
    stack: Vec<i64>,
    neighbours: Vec<i64>,
}
impl SearchBuffer {
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.seen.fill(0);
            self.generation = 1;
        }
        self.stack.clear();
    }
    fn queue(&mut self, b: i64) {
        if self.seen[b as usize] != self.generation {
            self.seen[b as usize] = self.generation;
            self.stack.push(b);
        }
    }
}
impl Precedence {
    pub fn explicit(num_blocks: i64, edges: &[(i64, i64)]) -> Result<Self> {
        let n = usize::try_from(num_blocks).map_err(|_| invalid("block count must be nonnegative and addressable"))?;
        let mut above = Vec::new();
        above.try_reserve_exact(n).map_err(|_| invalid("graph exceeds memory"))?;
        above.resize_with(n, Vec::new);
        let mut below = Vec::new();
        below.try_reserve_exact(n).map_err(|_| invalid("graph exceeds memory"))?;
        below.resize_with(n, Vec::new);
        for &(a, b) in edges {
            if a < 0 || b < 0 || a >= num_blocks || b >= num_blocks {
                return Err(invalid("precedence index outside model"));
            }
            above[a as usize].push(b);
            below[b as usize].push(a);
        }
        for list in above.iter_mut().chain(below.iter_mut()) {
            list.sort_unstable();
            list.dedup();
        }
        Self::adapt::<_, true>(Explicit { above, below })
    }
    pub fn regular_2d_45(num_x: i64, num_z: i64) -> Result<Self> {
        Self::regular_3d(&BlockModel::new(num_x, 1, num_z), &Pattern::from_offsets(&[(-1, 0, 1), (0, 0, 1), (1, 0, 1)])?)
    }
    pub fn regular_3d(model: &BlockModel, pattern: &Pattern) -> Result<Self> {
        model.checked_num_blocks()?;
        Self::adapt::<_, true>(Regular::new(*model, std::slice::from_ref(pattern), None))
    }
    pub fn regular_3d_keyed(model: &BlockModel, patterns: &[Pattern], pattern_indices: &[i64]) -> Result<Self> {
        let n = model.checked_num_blocks()?;
        if patterns.is_empty() || pattern_indices.len() != n || pattern_indices.iter().any(|&k| k < 0 || k as usize >= patterns.len()) {
            return Err(invalid("one valid pattern index is required per block"));
        }
        Self::adapt::<_, true>(Regular::new(*model, patterns, Some(pattern_indices.iter().map(|&k| k as usize).collect())))
    }
    pub fn regular_3d_varying_slopes(model: &BlockModel, slopes: &[Angle], benches: i64, tolerance: Angle) -> Result<Self> {
        if slopes.len() != model.checked_num_blocks()? {
            return Err(invalid("one slope is required per block"));
        }
        let step = tolerance.as_radians();
        if !step.is_finite() || step <= 0.0 {
            return Err(invalid("slope tolerance must be finite and positive"));
        }
        let mut seen = HashMap::new();
        let mut patterns = Vec::new();
        let mut indices = Vec::with_capacity(slopes.len());
        for &slope in slopes {
            Slope::validate_slope(slope)?;
            let key = (slope.as_radians() / step).round();
            if !key.is_finite() || key >= i64::MAX as f64 {
                return Err(invalid("slope tolerance is too small"));
            }
            let key = key as i64;
            let index = if let Some(&i) = seen.get(&key) {
                i
            } else {
                let angle = crate::radians((key as f64 * step).min(std::f64::consts::FRAC_PI_2));
                let p = Pattern::min_search(model, &Slope::constant(angle)?, benches)?;
                let i = patterns.len() as i64;
                patterns.push(p);
                seen.insert(key, i);
                i
            };
            indices.push(index);
        }
        Self::regular_3d_keyed(model, &patterns, &indices)
    }
    pub fn from_source<S: PrecedenceSource>(source: S) -> Result<Self> {
        Self::adapt::<S, false>(source)
    }
    fn adapt<S: PrecedenceSource, const BUILTIN: bool>(source: S) -> Result<Self> {
        let count = usize::try_from(source.num_blocks()).map_err(|_| invalid("block count must be nonnegative and addressable"))?;
        Ok(Self {
            source: Arc::new(SourceAdapter::<S, BUILTIN>(source)),
            count,
        })
    }
    /// True when `fill` with `reverse` set returns exactly the blocks that name
    /// this one as an antecedent, so the solver can walk the graph backwards
    /// instead of reconstructing reversed edges.
    pub(crate) fn reverse_is_exact(&self) -> bool {
        self.source.is_builtin()
    }
    pub fn num_blocks(&self) -> Result<i64> {
        Ok(self.count as i64)
    }
    pub fn try_clone(&self) -> Result<Self> {
        Ok(self.clone())
    }
    pub(crate) fn fill(&self, b: i64, reverse: bool, out: &mut Vec<i64>) -> Result<()> {
        if b < 0 || b as usize >= self.count {
            return Err(invalid("block index outside model"));
        }
        out.clear();
        self.source.fill(b, reverse, out)?;
        if !self.source.is_builtin() && out.iter().any(|&v| v < 0 || v as usize >= self.count) {
            return Err(invalid("source returned a block outside model"));
        }
        Ok(())
    }
    pub fn antecedents(&self, b: i64) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        self.fill(b, false, &mut out)?;
        Ok(out)
    }
    pub fn successors(&self, b: i64) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        self.fill(b, true, &mut out)?;
        Ok(out)
    }
    pub fn num_constraints(&self) -> Result<i64> {
        let mut n = 0i64;
        let mut out = Vec::new();
        for i in 0..self.count {
            self.fill(i as i64, false, &mut out)?;
            n = n.checked_add(out.len() as i64).ok_or_else(|| invalid("constraint count overflow"))?;
        }
        Ok(n)
    }
    /// Returns the exact count, potentially traversing every block.
    pub fn approx_num_constraints(&self) -> Result<i64> {
        self.num_constraints()
    }
    /// Stop enumeration when the visitor returns false.
    pub fn for_each_constraint<F: FnMut(i64, i64) -> bool>(&self, mut visit: F) -> Result<()> {
        let mut out = Vec::new();
        for a in 0..self.count {
            self.fill(a as i64, false, &mut out)?;
            for &b in &out {
                if !visit(a as i64, b) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
    pub fn is_consistent(&self) -> Result<bool> {
        let mut forward = HashSet::new();
        self.for_each_constraint(|a, b| {
            forward.insert((a, b));
            true
        })?;
        let mut reverse = HashSet::new();
        for b in 0..self.count {
            for a in self.successors(b as i64)? {
                reverse.insert((a, b as i64));
            }
        }
        Ok(forward == reverse)
    }
    pub fn search_buffer(&self) -> Result<SearchBuffer> {
        let mut seen = Vec::new();
        seen.try_reserve_exact(self.count).map_err(|_| invalid("search exceeds memory"))?;
        seen.resize(self.count, 0);
        Ok(SearchBuffer {
            seen,
            generation: 0,
            stack: Vec::new(),
            neighbours: Vec::new(),
        })
    }
    fn walk<F: FnMut(i64) -> bool>(&self, b: i64, buffer: &mut SearchBuffer, reverse: bool, mut visit: F) -> Result<()> {
        if buffer.seen.len() != self.count {
            return Err(invalid("search buffer has a different block count"));
        }
        buffer.reset();
        self.fill(b, reverse, &mut buffer.neighbours)?;
        for i in 0..buffer.neighbours.len() {
            buffer.queue(buffer.neighbours[i]);
        }
        while let Some(v) = buffer.stack.pop() {
            if visit(v) {
                self.fill(v, reverse, &mut buffer.neighbours)?;
                for i in 0..buffer.neighbours.len() {
                    buffer.queue(buffer.neighbours[i]);
                }
            }
        }
        Ok(())
    }
    /// A false visitor result prunes that node's descendants, not the entire search.
    pub fn for_each_reachable_antecedent<F: FnMut(i64) -> bool>(&self, b: i64, buffer: &mut SearchBuffer, visit: F) -> Result<()> {
        self.walk(b, buffer, false, visit)
    }
    pub fn for_each_reachable_successor<F: FnMut(i64) -> bool>(&self, b: i64, buffer: &mut SearchBuffer, visit: F) -> Result<()> {
        self.walk(b, buffer, true, visit)
    }
    pub fn reachable_antecedents(&self, b: i64, buffer: &mut SearchBuffer) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        self.walk(b, buffer, false, |v| {
            out.push(v);
            true
        })?;
        Ok(out)
    }
    pub fn reachable_successors(&self, b: i64, buffer: &mut SearchBuffer) -> Result<Vec<i64>> {
        let mut out = Vec::new();
        self.walk(b, buffer, true, |v| {
            out.push(v);
            true
        })?;
        Ok(out)
    }
}
