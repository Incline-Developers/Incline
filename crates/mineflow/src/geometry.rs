//! Angles and the regular block model definition.

use std::fmt;

use crate::error::{Result, invalid};

/// An angle, stored in radians.
///
/// Slopes and azimuths are radians throughout the library and degrees
/// everywhere a human is involved, so this type carries the unit rather than
/// leaving it to a naming convention.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Angle {
    radians: f64,
}

impl Angle {
    pub const fn from_radians(radians: f64) -> Self {
        Angle { radians }
    }

    pub fn from_degrees(degrees: f64) -> Self {
        Angle { radians: degrees.to_radians() }
    }

    pub const fn as_radians(self) -> f64 {
        self.radians
    }

    pub fn as_degrees(self) -> f64 {
        self.radians.to_degrees()
    }
}

impl fmt::Display for Angle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4}deg", self.as_degrees())
    }
}

/// Shorthand for [`Angle::from_degrees`].
pub fn degrees(value: f64) -> Angle {
    Angle::from_degrees(value)
}

/// Shorthand for [`Angle::from_radians`].
pub const fn radians(value: f64) -> Angle {
    Angle::from_radians(value)
}

/// A regular 3D block model.
///
/// Block index 0 is the lowest x, lowest y, lowest z block. The 1D index
/// increases fastest in x, then y, then z.
///
/// Builders store dimensions without validation. Call [`Self::checked_num_blocks`]
/// before using indexing helpers with external input. Precedence and pattern
/// constructors validate the model automatically; indexing helpers require valid
/// dimensions and in-range coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockModel {
    pub num_x: i64,
    pub num_y: i64,
    pub num_z: i64,
    /// The lowest corner of the model, in x, y and z.
    pub origin: [f64; 3],
    /// The size of a single block, in x, y and z.
    pub block_size: [f64; 3],
}

impl BlockModel {
    /// A model of unit sized blocks at the origin.
    pub fn new(num_x: i64, num_y: i64, num_z: i64) -> Self {
        BlockModel {
            num_x,
            num_y,
            num_z,
            origin: [0.0; 3],
            block_size: [1.0; 3],
        }
    }

    pub fn with_block_size(mut self, size_x: f64, size_y: f64, size_z: f64) -> Self {
        self.block_size = [size_x, size_y, size_z];
        self
    }

    pub fn with_origin(mut self, min_x: f64, min_y: f64, min_z: f64) -> Self {
        self.origin = [min_x, min_y, min_z];
        self
    }

    pub fn num_blocks(&self) -> i64 {
        self.num_x * self.num_y * self.num_z
    }

    /// The 1D index of the block at the given 3D indices.
    pub fn grid_index(&self, x: i64, y: i64, z: i64) -> i64 {
        x + y * self.num_x + z * self.num_x * self.num_y
    }

    /// The 3D indices of the block at the given 1D index.
    pub fn xyz(&self, index: i64) -> (i64, i64, i64) {
        let layer = self.num_x * self.num_y;
        (index % self.num_x, (index % layer) / self.num_x, index / layer)
    }

    /// The centroid of the block at the given 1D index.
    pub fn centroid(&self, index: i64) -> [f64; 3] {
        let (x, y, z) = self.xyz(index);
        [
            self.origin[0] + (x as f64 + 0.5) * self.block_size[0],
            self.origin[1] + (y as f64 + 0.5) * self.block_size[1],
            self.origin[2] + (z as f64 + 0.5) * self.block_size[2],
        ]
    }

    pub(crate) fn extent(&self) -> [i64; 3] {
        [self.num_x, self.num_y, self.num_z]
    }

    pub fn contains(&self, x: i64, y: i64, z: i64) -> bool {
        x >= 0 && x < self.num_x && y >= 0 && y < self.num_y && z >= 0 && z < self.num_z
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.num_x <= 0 || self.num_y <= 0 || self.num_z <= 0 {
            return Err(invalid("block counts must be positive"));
        }
        if self.block_size.iter().any(|size| !size.is_finite() || *size <= 0.0) {
            return Err(invalid("block sizes must be finite and positive"));
        }
        if self.origin.iter().any(|value| !value.is_finite()) {
            return Err(invalid("the origin must be finite"));
        }
        self.num_x
            .checked_mul(self.num_y)
            .and_then(|layer| layer.checked_mul(self.num_z))
            .ok_or_else(|| invalid("the block count overflows a 64 bit integer"))?;
        Ok(())
    }

    /// Validates the grid and returns its addressable block count.
    pub fn checked_num_blocks(&self) -> Result<usize> {
        self.validate()?;
        usize::try_from(self.num_blocks()).map_err(|_| invalid("block count exceeds address space"))
    }

    pub(crate) fn offset(&self, index: i64, offset: (i64, i64, i64)) -> Option<i64> {
        let (x, y, z) = self.xyz(index);
        let (x, y, z) = (x.checked_add(offset.0)?, y.checked_add(offset.1)?, z.checked_add(offset.2)?);
        self.contains(x, y, z).then(|| self.grid_index(x, y, z))
    }
}
