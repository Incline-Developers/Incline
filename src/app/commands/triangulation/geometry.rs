/// Point-in-polygon test for a 2D polygon (XY plane), boundary-inclusive.
/// Thin adapter over the robust kernel test.
pub(super) fn point_in_polygon_xy(px: f64, py: f64, poly: &[(f64, f64)]) -> bool {
    !matches!(
        crate::model::kernel::point_in_polygon(glam::DVec2::new(px, py), poly.iter().map(|&(x, y)| glam::DVec2::new(x, y)),),
        crate::model::kernel::PolyContainment::Outside
    )
}
