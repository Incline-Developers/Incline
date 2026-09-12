//! Extensibility for irregular block models and values produced on demand.

/// Immutable precedence provider. Each edge means that mining `block` requires
/// mining its antecedent. Both directions must agree, and indices must be valid.
/// Implementations append neighbours to `out`; callers clear it before each call.
/// Providers must remain deterministic for the lifetime of a solver.
pub trait PrecedenceSource: Send + Sync + 'static {
    /// Set true when successor queries are implemented. Solving only needs antecedents.
    const PROVIDES_SUCCESSORS: bool = false;
    fn num_blocks(&self) -> i64;
    fn antecedents(&self, block: i64, out: &mut Vec<i64>);
    fn successors(&self, _block: i64, _out: &mut Vec<i64>) {}
}

/// Values are read once when constructing or updating a solver.
pub trait BlockValueSource {
    fn num_blocks(&self) -> i64;
    fn value(&self, block: i64) -> i64;
}
