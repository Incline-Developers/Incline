//! Format-independent triangulation data structures.

use std::{error::Error, fmt};

#[derive(Debug, PartialEq)]
pub struct Triangulation {
    vertices: Vec<Vertex>,
    faces: Vec<Face>,
    bounds: Bounds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    indices: [u32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Triangle {
    pub face_index: usize,
    pub vertex_indices: [usize; 3],
    pub vertices: [Vertex; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min: Vertex,
    pub max: Vertex,
}

#[derive(Debug)]
pub enum ReadError {
    Overflow(&'static str),
    InvalidRawCounts { vertices: usize, faces: usize },
    InvalidCoordinates,
    InvalidFaceIndices { min: u32, max: u32, vertex_count: usize },
}

impl Triangulation {
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    pub fn vertices(&self) -> &[Vertex] {
        &self.vertices
    }

    pub fn face(&self, index: usize) -> Option<Face> {
        self.faces.get(index).copied()
    }

    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub fn face_vertex_indices(&self, face_index: usize) -> Option<[usize; 3]> {
        self.face(face_index).map(Face::indices_zero_based)
    }

    pub fn triangle(&self, face_index: usize) -> Option<Triangle> {
        let face = self.face(face_index)?;
        let vertex_indices = face.indices_zero_based();
        let vertices = [
            self.vertices.get(vertex_indices[0]).copied()?,
            self.vertices.get(vertex_indices[1]).copied()?,
            self.vertices.get(vertex_indices[2]).copied()?,
        ];

        Some(Triangle {
            face_index,
            vertex_indices,
            vertices,
        })
    }

    pub fn triangles(&self) -> impl Iterator<Item = Triangle> + '_ {
        (0..self.faces.len()).filter_map(|index| self.triangle(index))
    }

    pub fn face_vertex_indices_iter(&self) -> impl Iterator<Item = [usize; 3]> + '_ {
        self.faces.iter().copied().map(Face::indices_zero_based)
    }

    /// Construct a triangulation from a computed vertex list and face index triples.
    /// Face indices must be zero-based and within bounds.
    pub fn from_vertices_and_faces(vertices: Vec<Vertex>, faces: Vec<[u32; 3]>) -> Result<Self, ReadError> {
        let vertex_count = vertices.len();
        let face_count = faces.len();
        if vertex_count == 0 || face_count == 0 {
            return Err(ReadError::InvalidRawCounts {
                vertices: vertex_count,
                faces: face_count,
            });
        }
        let vertex_count_u32 = u32::try_from(vertex_count).map_err(|_| ReadError::Overflow("generated vertex count"))?;
        u32::try_from(face_count).map_err(|_| ReadError::Overflow("generated face count"))?;
        if vertices.iter().any(|vertex| !vertex.x.is_finite() || !vertex.y.is_finite() || !vertex.z.is_finite()) {
            return Err(ReadError::InvalidCoordinates);
        }
        let (min_index, max_index) = faces.iter().flatten().fold((u32::MAX, 0u32), |(min, max), &index| (min.min(index), max.max(index)));
        if max_index >= vertex_count_u32 {
            return Err(ReadError::InvalidFaceIndices {
                min: min_index,
                max: max_index,
                vertex_count,
            });
        }
        let bounds = Bounds::from_vertices(&vertices);
        let faces = faces.into_iter().map(|indices| Face { indices }).collect();
        Ok(Self { vertices, faces, bounds })
    }
}

impl Vertex {
    pub const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }

    pub const fn as_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }
}

impl Face {
    pub fn indices_zero_based(self) -> [usize; 3] {
        [self.indices[0] as usize, self.indices[1] as usize, self.indices[2] as usize]
    }
}

impl Bounds {
    pub fn from_vertices(vertices: &[Vertex]) -> Self {
        let mut min = Vertex::new(f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut max = Vertex::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);

        for vertex in vertices {
            min.x = min.x.min(vertex.x);
            min.y = min.y.min(vertex.y);
            min.z = min.z.min(vertex.z);
            max.x = max.x.max(vertex.x);
            max.y = max.y.max(vertex.y);
            max.z = max.z.max(vertex.z);
        }

        Self { min, max }
    }

    pub fn center(self) -> Vertex {
        Vertex::new((self.min.x + self.max.x) * 0.5, (self.min.y + self.max.y) * 0.5, (self.min.z + self.max.z) * 0.5)
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Overflow(section) => write!(f, "{section} section overflow"),
            ReadError::InvalidRawCounts { vertices, faces } => {
                write!(f, "invalid raw counts: vertices={vertices} faces={faces}")
            }
            ReadError::InvalidCoordinates => {
                write!(f, "vertex section contains non-finite coordinates")
            }
            ReadError::InvalidFaceIndices { min, max, vertex_count } => {
                write!(f, "face indices are outside the vertex range: min={min} max={max} vertices={vertex_count}")
            }
        }
    }
}

impl Error for ReadError {}
