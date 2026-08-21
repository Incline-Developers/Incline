/// Point-in-polyline test for a closed 2D ring (XY plane), boundary-inclusive.
/// Thin adapter over the robust kernel test.
pub(super) fn point_in_polyline_xy(px: f64, py: f64, poly: &[(f64, f64)]) -> bool {
    !matches!(
        crate::model::kernel::point_in_polyline(glam::DVec2::new(px, py), poly.iter().map(|&(x, y)| glam::DVec2::new(x, y)),),
        crate::model::kernel::PolyContainment::Outside
    )
}
