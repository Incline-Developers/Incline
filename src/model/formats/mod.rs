//! Translation between the application's triangulations and common triangle-mesh formats.
//!
//! OBJ and PLY polylines are triangulated in their dominant plane. STL normals,
//! OBJ materials/texture coordinates, and PLY properties other than positions
//! and vertex indices are intentionally ignored.

pub(crate) mod block_model_data;
pub(crate) mod csv_block_model;
pub(crate) mod csv_drill_hole;
pub(crate) mod dxf;
pub(crate) mod mesh_data;
pub(crate) mod omf;
pub(crate) mod point_cloud;

use std::{
    collections::HashMap,
    error::Error,
    fmt,
    io::{self, Write},
    path::Path,
};

use mesh_data::{ReadError, Triangulation, Vertex};

use crate::model::progress::Phase;

/// Items parsed between progress reports, so a tight parse loop isn't paced by
/// its own bookkeeping.
const PARSE_PROGRESS_STRIDE: usize = 4096;
/// Share of a load spent pulling the file off disk, as opposed to parsing it.
/// Both halves report exactly; only the split between them is an estimate.
#[cfg(not(target_arch = "wasm32"))]
const FILE_READ_SHARE: f32 = 0.35;
/// Bytes per `read` when reading a file with progress.
#[cfg(not(target_arch = "wasm32"))]
const FILE_READ_CHUNK: usize = 1 << 22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeshFormat {
    Obj,
    Stl,
    Ply,
}

impl MeshFormat {
    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "obj" => Some(Self::Obj),
            "stl" => Some(Self::Stl),
            "ply" => Some(Self::Ply),
            _ => None,
        }
    }

    pub fn from_path(path: impl AsRef<Path>) -> Option<Self> {
        path.as_ref().extension().and_then(|value| value.to_str()).and_then(Self::from_extension)
    }
}

#[derive(Debug)]
pub enum TranslationError {
    Io(io::Error),
    UnsupportedExtension(String),
    UnsupportedFeature(String),
    InvalidData { line: Option<usize>, message: String },
    InvalidMesh(String),
    TooLarge(&'static str),
    InvalidMeshStructure(ReadError),
}

impl fmt::Display for TranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::UnsupportedExtension(ext) => write!(f, "unsupported mesh extension: {ext}"),
            Self::UnsupportedFeature(message) => write!(f, "unsupported mesh feature: {message}"),
            Self::InvalidData { line: Some(line), message } => write!(f, "line {line}: {message}"),
            Self::InvalidData { line: None, message } => write!(f, "{message}"),
            Self::InvalidMesh(message) => write!(f, "invalid mesh: {message}"),
            Self::TooLarge(section) => write!(f, "{section} count exceeds the format limit"),
            Self::InvalidMeshStructure(error) => write!(f, "{error}"),
        }
    }
}

impl Error for TranslationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidMeshStructure(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for TranslationError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<ReadError> for TranslationError {
    fn from(value: ReadError) -> Self {
        Self::InvalidMeshStructure(value)
    }
}

/// Read the mesh at `path`, reporting into `progress`: the file read is exact
/// (the size is known up front) and each parser counts the items it walks, so
/// the percentage is real rather than a guess at how long a load takes.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_mesh_with_progress(path: impl AsRef<Path>, progress: &Phase) -> Result<Triangulation, TranslationError> {
    let path = path.as_ref();
    let format = MeshFormat::from_path(path).ok_or_else(|| unsupported_path(path))?;
    let bytes = read_file_with_progress(path, &progress.phase(0.0, FILE_READ_SHARE))?;
    parse_mesh_bytes(format, &bytes, &progress.phase(FILE_READ_SHARE, 1.0))
}

/// Read a whole file in chunks, reporting bytes read. `fs::read` would be one
/// opaque call; the file size is known here, so the bar can move while a large
/// mesh streams off disk.
#[cfg(not(target_arch = "wasm32"))]
fn read_file_with_progress(path: &Path, progress: &Phase) -> io::Result<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let total = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    let mut chunk = vec![0u8; FILE_READ_CHUNK];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        // A file that grew since the metadata call would report past 100%;
        // `set_items` clamps, and the next phase corrects the bar anyway.
        progress.set_items(bytes.len() as u64, total.max(bytes.len() as u64));
    }
    Ok(bytes)
}

fn parse_mesh_bytes(format: MeshFormat, bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    match format {
        MeshFormat::Obj => read_obj_bytes_with_progress(bytes, progress),
        MeshFormat::Stl => read_stl_bytes_with_progress(bytes, progress),
        MeshFormat::Ply => read_ply_bytes_with_progress(bytes, progress),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn read_mesh_bytes(source_name: &str, bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    let format = MeshFormat::from_path(source_name)
        .ok_or_else(|| TranslationError::UnsupportedExtension(Path::new(source_name).extension().and_then(|value| value.to_str()).unwrap_or("").to_owned()))?;
    parse_mesh_bytes(format, bytes, progress)
}

#[cfg(target_arch = "wasm32")]
pub fn write_mesh_bytes(mesh: &Triangulation, format: MeshFormat) -> Result<Vec<u8>, TranslationError> {
    let mut bytes = Vec::new();
    let mut progress = |_, _| {};
    match format {
        MeshFormat::Obj => write_obj_with_progress(mesh, &mut bytes, &mut progress)?,
        MeshFormat::Stl => write_stl_with_progress(mesh, &mut bytes, &mut progress)?,
        MeshFormat::Ply => write_ply_with_progress(mesh, &mut bytes, &mut progress)?,
    }
    Ok(bytes)
}

/// Write `mesh` to `path` in the format implied by the extension, invoking
/// `progress` with `(items_written, items_total)` as the file is written
/// (throttled to every few thousand items). An item is one vertex or one face,
/// so the counts are directly reportable in the UI. Pass `&mut |_, _| {}` when
/// progress is not needed.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_mesh_with_progress(mesh: &Triangulation, path: impl AsRef<Path>, progress: &mut dyn FnMut(u64, u64)) -> Result<(), TranslationError> {
    let path = path.as_ref();
    let format = MeshFormat::from_path(path).ok_or_else(|| unsupported_path(path))?;
    crate::model::atomic_file::write_atomic(path, |file| {
        let mut writer = io::BufWriter::new(file);
        match format {
            MeshFormat::Obj => write_obj_with_progress(mesh, &mut writer, progress)?,
            MeshFormat::Stl => write_stl_with_progress(mesh, &mut writer, progress)?,
            MeshFormat::Ply => write_ply_with_progress(mesh, &mut writer, progress)?,
        }
        writer.flush()?;
        Ok(())
    })
    .map_err(|error| error.downcast::<TranslationError>().unwrap_or_else(|error| TranslationError::Io(io::Error::other(error))))
}

/// How many vertices/faces to write between progress callbacks.
pub(crate) const WRITE_PROGRESS_STRIDE: usize = 4096;

/// Total item count reported to a write-progress callback. Clamped to at least
/// one so consumers can divide by it for a percentage.
pub(crate) fn write_item_total(items: usize) -> u64 {
    items.max(1) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn unsupported_path(path: &Path) -> TranslationError {
    TranslationError::UnsupportedExtension(path.extension().and_then(|s| s.to_str()).unwrap_or("").to_string())
}

pub fn read_obj_bytes_with_progress(bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    use rayon::prelude::*;

    let text = std::str::from_utf8(bytes).map_err(|_| invalid(None, "OBJ is not valid UTF-8"))?;

    // Pass 1 (sequential): classify lines. For face lines we record the vertex count at that
    // point so that negative OBJ indices (relative to current position) resolve correctly.
    let mut vertex_lines: Vec<(usize, &str)> = Vec::new();
    let mut face_lines: Vec<(usize, &str, usize)> = Vec::new();
    let mut vertex_count = 0usize;

    // The three passes are one scan of the text plus one item per vertex and
    // face, so each reports over its own share of the parse.
    let scan = progress.phase(0.0, 0.4);
    let mut scanned_bytes = 0usize;

    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        scanned_bytes += raw_line.len() + 1;
        if line_index.is_multiple_of(PARSE_PROGRESS_STRIDE) {
            scan.set_items(scanned_bytes as u64, text.len() as u64);
        }
        let line = raw_line.split('#').next().unwrap_or("").trim();
        match line.split_ascii_whitespace().next() {
            Some("v") => {
                vertex_lines.push((line_number, line));
                vertex_count += 1;
            }
            Some("f") => {
                face_lines.push((line_number, line, vertex_count));
            }
            _ => {}
        }
    }

    scan.finish();

    // Pass 2: parse vertices in parallel (order preserved by rayon collect).
    let parse_vertices = progress.phase(0.4, 0.6).counter(vertex_lines.len());
    let vertices = vertex_lines
        .par_iter()
        .map(|(line_number, line)| {
            parse_vertices.advance_by(1);
            let mut fields = line.split_ascii_whitespace();
            fields.next(); // skip "v"
            let x = parse_f64(fields.next(), *line_number, "vertex x")?;
            let y = parse_f64(fields.next(), *line_number, "vertex y")?;
            let z = parse_f64(fields.next(), *line_number, "vertex z")?;
            Ok(Vertex::new(x, y, z))
        })
        .collect::<Result<Vec<Vertex>, TranslationError>>()?;

    // Pass 3: parse and triangulate face lines in parallel.
    let parse_faces = progress.phase(0.6, 1.0).counter(face_lines.len());
    let face_groups = face_lines
        .par_iter()
        .map(|(line_number, line, vcount_at_point)| {
            parse_faces.advance_by(1);
            let mut fields = line.split_ascii_whitespace();
            fields.next(); // skip "f"
            let polyline = fields.map(|field| parse_obj_index(field, *vcount_at_point, *line_number)).collect::<Result<Vec<_>, _>>()?;
            let mut tris = Vec::new();
            triangulate(&polyline, &vertices, &mut tris, *line_number)?;
            Ok(tris)
        })
        .collect::<Result<Vec<Vec<[u32; 3]>>, TranslationError>>()?;

    let triangles: Vec<[u32; 3]> = face_groups.into_iter().flatten().collect();

    build_mesh(vertices, triangles)
}

fn parse_obj_index(field: &str, vertex_count: usize, line: usize) -> Result<u32, TranslationError> {
    let raw = field.split('/').next().unwrap_or("");
    let index = raw.parse::<i64>().map_err(|_| invalid(Some(line), "invalid OBJ vertex index"))?;
    if index == 0 {
        return Err(invalid(Some(line), "OBJ indices cannot be zero"));
    }
    let zero_based = if index > 0 { index - 1 } else { vertex_count as i64 + index };
    if zero_based < 0 || zero_based >= vertex_count as i64 {
        return Err(invalid(Some(line), "OBJ vertex index is out of range"));
    }
    u32::try_from(zero_based).map_err(|_| TranslationError::TooLarge("vertex"))
}

pub fn read_stl_bytes_with_progress(bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    if bytes.len() >= 84 {
        let count = u32::from_le_bytes(bytes[80..84].try_into().unwrap()) as usize;
        if 84usize.checked_add(count.saturating_mul(50)) == Some(bytes.len()) {
            return read_binary_stl(bytes, count, progress);
        }
    }
    read_ascii_stl(bytes, progress)
}

fn read_binary_stl(bytes: &[u8], count: usize, progress: &Phase) -> Result<Triangulation, TranslationError> {
    let mut vertices = Vec::new();
    let mut triangles = Vec::with_capacity(count);
    let mut indices = HashMap::<[u32; 3], u32>::new();
    for face in 0..count {
        if face.is_multiple_of(PARSE_PROGRESS_STRIDE) {
            progress.set_items(face as u64, count as u64);
        }
        let base = 84 + face * 50 + 12;
        let mut triangle = [0; 3];
        for (corner, target) in triangle.iter_mut().enumerate() {
            let offset = base + corner * 12;
            let coords = [
                f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()),
                f32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()),
                f32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().unwrap()),
            ];
            if !coords.iter().all(|v| v.is_finite()) {
                return Err(invalid(None, "STL contains non-finite coordinates"));
            }
            *target = intern_stl_vertex(coords, &mut vertices, &mut indices)?;
        }
        triangles.push(triangle);
    }
    build_mesh(vertices, triangles)
}

fn read_ascii_stl(bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid(None, "invalid binary or ASCII STL"))?;
    // Facet count is unknown until the end, so report by text consumed.
    let mut scanned_bytes = 0usize;
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    let mut indices = HashMap::<[u32; 3], u32>::new();
    let mut in_solid = false;
    let mut in_facet = false;
    let mut in_loop = false;
    let mut loop_closed = false;
    let mut current = [0u32; 3];
    let mut current_len = 0usize;
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        scanned_bytes += line.len() + 1;
        if line_index.is_multiple_of(PARSE_PROGRESS_STRIDE) {
            progress.set_items(scanned_bytes as u64, text.len() as u64);
        }
        let mut fields = line.split_whitespace();
        let Some(keyword) = fields.next() else {
            continue;
        };
        match keyword {
            "solid" if !in_solid && !in_facet => in_solid = true,
            "facet" if in_solid && !in_facet => {
                if fields.next() != Some("normal") {
                    return Err(invalid(Some(line_number), "expected 'facet normal x y z'"));
                }
                for axis in ["normal x", "normal y", "normal z"] {
                    parse_f32(fields.next(), line_number, axis)?;
                }
                if fields.next().is_some() {
                    return Err(invalid(Some(line_number), "extra data after facet normal"));
                }
                in_facet = true;
                loop_closed = false;
                current_len = 0;
            }
            "outer" if in_facet && !in_loop && !loop_closed => {
                if fields.next() != Some("loop") || fields.next().is_some() {
                    return Err(invalid(Some(line_number), "expected 'outer loop'"));
                }
                in_loop = true;
            }
            "vertex" if in_facet && in_loop => {
                if current_len >= 3 {
                    return Err(invalid(Some(line_number), "ASCII STL facet has more than 3 vertices"));
                }
                let xyz = [
                    parse_f32(fields.next(), line_number, "vertex x")?,
                    parse_f32(fields.next(), line_number, "vertex y")?,
                    parse_f32(fields.next(), line_number, "vertex z")?,
                ];
                if fields.next().is_some() {
                    return Err(invalid(Some(line_number), "extra data after STL vertex"));
                }
                current[current_len] = intern_stl_vertex(xyz, &mut vertices, &mut indices)?;
                current_len += 1;
            }
            "endloop" if in_facet && in_loop => {
                if fields.next().is_some() || current_len != 3 {
                    return Err(invalid(Some(line_number), "ASCII STL loop must contain exactly 3 vertices"));
                }
                in_loop = false;
                loop_closed = true;
            }
            "endfacet" if in_facet && !in_loop && loop_closed => {
                if fields.next().is_some() {
                    return Err(invalid(Some(line_number), "extra data after endfacet"));
                }
                triangles.push(current);
                in_facet = false;
                loop_closed = false;
                current_len = 0;
            }
            "endsolid" if in_solid && !in_facet => {
                in_solid = false;
            }
            _ => {
                return Err(invalid(Some(line_number), format!("unexpected ASCII STL record '{keyword}'")));
            }
        }
    }
    if in_solid || in_facet || in_loop {
        return Err(invalid(None, "incomplete ASCII STL structure"));
    }
    build_mesh(vertices, triangles)
}

fn intern_stl_vertex(xyz: [f32; 3], vertices: &mut Vec<Vertex>, indices: &mut HashMap<[u32; 3], u32>) -> Result<u32, TranslationError> {
    let key = xyz.map(f32::to_bits);
    if let Some(index) = indices.get(&key) {
        return Ok(*index);
    }
    let index = u32::try_from(vertices.len()).map_err(|_| TranslationError::TooLarge("vertex"))?;
    vertices.push(Vertex::new(xyz[0] as f64, xyz[1] as f64, xyz[2] as f64));
    indices.insert(key, index);
    Ok(index)
}

pub fn read_ply_bytes_with_progress(bytes: &[u8], progress: &Phase) -> Result<Triangulation, TranslationError> {
    let data_start = ply_data_start(bytes)?;
    let header_text = std::str::from_utf8(&bytes[..data_start]).map_err(|_| invalid(None, "PLY header is not valid UTF-8"))?;
    let header = parse_ply_header(header_text)?;
    match header.format {
        PlyFormat::Ascii => read_ascii_ply(&bytes[data_start..], &header, progress),
        PlyFormat::BinaryLittleEndian => read_binary_ply(&bytes[data_start..], &header, progress),
    }
}

fn ply_data_start(bytes: &[u8]) -> Result<usize, TranslationError> {
    let mut line_start = 0;
    while line_start < bytes.len() {
        let Some(relative_line_end) = bytes[line_start..].iter().position(|byte| *byte == b'\n') else {
            return Err(invalid(None, "missing PLY end_header"));
        };
        let line_end = line_start + relative_line_end;
        let mut line = &bytes[line_start..line_end];
        if line.ends_with(b"\r") {
            line = &line[..line.len() - 1];
        }
        if line == b"end_header" {
            return Ok(line_end + 1);
        }
        line_start = line_end + 1;
    }
    Err(invalid(None, "missing PLY end_header"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlyFormat {
    Ascii,
    BinaryLittleEndian,
}

#[derive(Clone, Copy, Debug)]
enum PlyScalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl PlyScalar {
    const fn byte_len(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }

    const fn is_integer(self) -> bool {
        !matches!(self, Self::F32 | Self::F64)
    }
}

#[derive(Clone, Debug)]
enum PlyProperty {
    Scalar { data_type: PlyScalar, name: String },
    List { count_type: PlyScalar, item_type: PlyScalar, name: String },
}

#[derive(Clone, Debug)]
struct PlyElement {
    name: String,
    count: usize,
    properties: Vec<PlyProperty>,
}

struct PlyHeader {
    format: PlyFormat,
    elements: Vec<PlyElement>,
}

impl PlyHeader {
    fn element(&self, name: &str) -> Result<&PlyElement, TranslationError> {
        let mut matching = self.elements.iter().filter(|element| element.name == name);
        let element = matching.next().ok_or_else(|| invalid(None, format!("missing PLY {name} element")))?;
        if matching.next().is_some() {
            return Err(invalid(None, format!("duplicate PLY {name} element")));
        }
        Ok(element)
    }
}

fn parse_ply_header(text: &str) -> Result<PlyHeader, TranslationError> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|line| line.trim()) != Some("ply") {
        return Err(invalid(Some(1), "missing PLY signature"));
    }
    let mut format = None;
    let mut elements = Vec::<PlyElement>::new();
    let mut current_element = None;
    for (line_index, line) in lines.iter().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            ["format", "ascii", "1.0"] => format = Some(PlyFormat::Ascii),
            ["format", "binary_little_endian", "1.0"] => format = Some(PlyFormat::BinaryLittleEndian),
            ["format", "binary_big_endian", _] => {
                return Err(TranslationError::UnsupportedFeature("big-endian binary PLY is not supported".into()));
            }
            ["element", name, count] => {
                let count = count.parse::<usize>().map_err(|_| invalid(Some(line_index + 1), "invalid PLY element count"))?;
                elements.push(PlyElement {
                    name: (*name).to_string(),
                    count,
                    properties: Vec::new(),
                });
                current_element = Some(elements.len() - 1);
            }
            ["property", "list", count_type, item_type, name] => {
                let element_index = current_element.ok_or_else(|| invalid(Some(line_index + 1), "PLY property appears before an element"))?;
                let count_type = parse_ply_scalar(count_type, line_index + 1)?;
                if !count_type.is_integer() {
                    return Err(invalid(Some(line_index + 1), "PLY list count type must be an integer"));
                }
                elements[element_index].properties.push(PlyProperty::List {
                    count_type,
                    item_type: parse_ply_scalar(item_type, line_index + 1)?,
                    name: (*name).to_string(),
                });
            }
            ["property", data_type, name] => {
                let element_index = current_element.ok_or_else(|| invalid(Some(line_index + 1), "PLY property appears before an element"))?;
                elements[element_index].properties.push(PlyProperty::Scalar {
                    data_type: parse_ply_scalar(data_type, line_index + 1)?,
                    name: (*name).to_string(),
                });
            }
            ["end_header"] => break,
            _ => {}
        }
    }
    let format = format.ok_or_else(|| invalid(None, "missing or unsupported PLY format"))?;
    let header = PlyHeader { format, elements };
    header.element("vertex")?;
    header.element("face")?;
    Ok(header)
}

fn parse_ply_scalar(name: &str, line: usize) -> Result<PlyScalar, TranslationError> {
    match name {
        "char" | "int8" => Ok(PlyScalar::I8),
        "uchar" | "uint8" => Ok(PlyScalar::U8),
        "short" | "int16" => Ok(PlyScalar::I16),
        "ushort" | "uint16" => Ok(PlyScalar::U16),
        "int" | "int32" => Ok(PlyScalar::I32),
        "uint" | "uint32" => Ok(PlyScalar::U32),
        "float" | "float32" => Ok(PlyScalar::F32),
        "double" | "float64" => Ok(PlyScalar::F64),
        _ => Err(invalid(Some(line), format!("unsupported PLY scalar type {name}"))),
    }
}

fn ply_position_properties(element: &PlyElement) -> Result<[usize; 3], TranslationError> {
    let positions = ["x", "y", "z"].map(|name| {
        element
            .properties
            .iter()
            .position(|property| matches!(property, PlyProperty::Scalar { name: property_name, .. } if property_name == name))
    });
    if positions.iter().any(Option::is_none) {
        return Err(invalid(None, "PLY vertices require x, y, and z properties"));
    }
    Ok(positions.map(Option::unwrap))
}

fn read_ascii_ply(bytes: &[u8], header: &PlyHeader, progress: &Phase) -> Result<Triangulation, TranslationError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid(None, "invalid ASCII PLY data"))?;
    let vertex_element = header.element("vertex")?;
    let positions = ply_position_properties(vertex_element)?;
    let mut rows = text.lines().enumerate();
    let mut vertices = Vec::new();
    let mut polylines = Vec::<(Vec<u32>, usize)>::new();
    // The header declares every element count, so the row total is exact.
    let total_rows = ply_row_total(header);
    let mut rows_read = 0usize;

    for element in &header.elements {
        for _ in 0..element.count {
            rows_read += 1;
            if rows_read.is_multiple_of(PARSE_PROGRESS_STRIDE) {
                progress.set_items(rows_read as u64, total_rows);
            }
            let (line_index, line) = rows.next().ok_or_else(|| invalid(None, "PLY data is truncated"))?;
            let line_number = line_index + 1;
            let fields: Vec<&str> = line.split_whitespace().collect();
            let mut field_index = 0usize;

            if element.name == "vertex" {
                let mut xyz = [0.0; 3];
                for (property_index, property) in element.properties.iter().enumerate() {
                    let values = ascii_ply_property_values(&fields, &mut field_index, property, line_number)?;
                    if let Some(axis) = positions.iter().position(|position| *position == property_index) {
                        xyz[axis] = values[0]
                            .parse::<f64>()
                            .ok()
                            .filter(|value| value.is_finite())
                            .ok_or_else(|| invalid(Some(line_number), "invalid PLY vertex coordinate"))?;
                    }
                }
                vertices.push(Vertex::new(xyz[0], xyz[1], xyz[2]));
            } else if element.name == "face" {
                let mut polyline = None;
                for property in &element.properties {
                    let values = ascii_ply_property_values(&fields, &mut field_index, property, line_number)?;
                    if matches!(property, PlyProperty::List { name, .. } if name == "vertex_indices" || name == "vertex_index") {
                        polyline = Some(
                            values
                                .iter()
                                .map(|value| value.parse::<u32>().map_err(|_| invalid(Some(line_number), "invalid PLY face index")))
                                .collect::<Result<Vec<_>, _>>()?,
                        );
                    }
                }
                polylines.push((polyline.ok_or_else(|| invalid(Some(line_number), "PLY face has no vertex_indices property"))?, line_number));
            } else {
                for property in &element.properties {
                    ascii_ply_property_values(&fields, &mut field_index, property, line_number)?;
                }
            }
        }
    }

    let mut triangles = Vec::new();
    for (polyline, line_number) in polylines {
        triangulate(&polyline, &vertices, &mut triangles, line_number)?;
    }
    build_mesh(vertices, triangles)
}

fn ascii_ply_property_values<'a>(fields: &'a [&str], field_index: &mut usize, property: &PlyProperty, line: usize) -> Result<&'a [&'a str], TranslationError> {
    let count = match property {
        PlyProperty::Scalar { .. } => 1,
        PlyProperty::List { .. } => {
            let count = fields
                .get(*field_index)
                .and_then(|value| value.parse::<usize>().ok())
                .ok_or_else(|| invalid(Some(line), "invalid PLY list count"))?;
            *field_index = field_index.checked_add(1).ok_or_else(|| invalid(Some(line), "PLY list count is too large"))?;
            count
        }
    };
    let end = field_index.checked_add(count).ok_or_else(|| invalid(Some(line), "PLY list count is too large"))?;
    let values = fields.get(*field_index..end).ok_or_else(|| invalid(Some(line), "PLY row is truncated"))?;
    *field_index = end;
    Ok(values)
}

/// Total rows/records the header declares across all elements.
fn ply_row_total(header: &PlyHeader) -> u64 {
    header.elements.iter().map(|element| element.count as u64).sum()
}

fn read_binary_ply(bytes: &[u8], header: &PlyHeader, progress: &Phase) -> Result<Triangulation, TranslationError> {
    let vertex_element = header.element("vertex")?;
    let positions = ply_position_properties(vertex_element)?;
    let mut offset = 0;
    let mut vertices = Vec::new();
    let mut polylines = Vec::<(Vec<u32>, usize)>::new();
    let total_rows = ply_row_total(header);
    let mut rows_read = 0usize;

    for element in &header.elements {
        for element_index in 0..element.count {
            rows_read += 1;
            if rows_read.is_multiple_of(PARSE_PROGRESS_STRIDE) {
                progress.set_items(rows_read as u64, total_rows);
            }
            if element.name == "vertex" {
                let mut xyz = [0.0; 3];
                for (property_index, property) in element.properties.iter().enumerate() {
                    match property {
                        PlyProperty::Scalar { data_type, .. } => {
                            let value = read_ply_number(bytes, &mut offset, *data_type)?;
                            if let Some(axis) = positions.iter().position(|position| *position == property_index) {
                                xyz[axis] = value;
                            }
                        }
                        PlyProperty::List { count_type, item_type, .. } => skip_binary_ply_list(bytes, &mut offset, *count_type, *item_type)?,
                    }
                }
                if !xyz.iter().all(|value| value.is_finite()) {
                    return Err(invalid(None, "PLY contains non-finite coordinates"));
                }
                vertices.push(Vertex::new(xyz[0], xyz[1], xyz[2]));
            } else if element.name == "face" {
                let mut polyline = None;
                for property in &element.properties {
                    match property {
                        PlyProperty::Scalar { data_type, .. } => {
                            read_ply_number(bytes, &mut offset, *data_type)?;
                        }
                        PlyProperty::List { count_type, item_type, name } => {
                            let count = read_ply_integer(bytes, &mut offset, *count_type)?;
                            ensure_binary_ply_items(bytes, offset, count, *item_type)?;
                            if name == "vertex_indices" || name == "vertex_index" {
                                if !item_type.is_integer() {
                                    return Err(invalid(None, "PLY face indices must use an integer type"));
                                }
                                let mut values = Vec::new();
                                values.try_reserve_exact(count).map_err(|_| invalid(None, "PLY face list is too large to allocate"))?;
                                for _ in 0..count {
                                    let value = read_ply_integer(bytes, &mut offset, *item_type)?;
                                    values.push(u32::try_from(value).map_err(|_| TranslationError::TooLarge("face index"))?);
                                }
                                polyline = Some(values);
                            } else {
                                offset = offset
                                    .checked_add(count.checked_mul(item_type.byte_len()).ok_or_else(|| invalid(None, "PLY list byte length overflows"))?)
                                    .ok_or_else(|| invalid(None, "PLY list byte length overflows"))?;
                            }
                        }
                    }
                }
                polylines.push((polyline.ok_or_else(|| invalid(None, "PLY face has no vertex_indices property"))?, element_index + 1));
            } else {
                for property in &element.properties {
                    match property {
                        PlyProperty::Scalar { data_type, .. } => {
                            read_ply_number(bytes, &mut offset, *data_type)?;
                        }
                        PlyProperty::List { count_type, item_type, .. } => skip_binary_ply_list(bytes, &mut offset, *count_type, *item_type)?,
                    }
                }
            }
        }
    }

    let mut triangles = Vec::new();
    for (polyline, face_index) in polylines {
        triangulate(&polyline, &vertices, &mut triangles, face_index)?;
    }
    build_mesh(vertices, triangles)
}

fn ensure_binary_ply_items(bytes: &[u8], offset: usize, count: usize, item_type: PlyScalar) -> Result<(), TranslationError> {
    let byte_len = count.checked_mul(item_type.byte_len()).ok_or_else(|| invalid(None, "PLY list byte length overflows"))?;
    let end = offset.checked_add(byte_len).ok_or_else(|| invalid(None, "PLY list byte length overflows"))?;
    if end > bytes.len() {
        return Err(invalid(None, "binary PLY data is truncated"));
    }
    Ok(())
}

fn skip_binary_ply_list(bytes: &[u8], offset: &mut usize, count_type: PlyScalar, item_type: PlyScalar) -> Result<(), TranslationError> {
    let count = read_ply_integer(bytes, offset, count_type)?;
    ensure_binary_ply_items(bytes, *offset, count, item_type)?;
    *offset = offset
        .checked_add(count.checked_mul(item_type.byte_len()).ok_or_else(|| invalid(None, "PLY list byte length overflows"))?)
        .ok_or_else(|| invalid(None, "PLY list byte length overflows"))?;
    Ok(())
}

fn read_ply_number(bytes: &[u8], offset: &mut usize, data_type: PlyScalar) -> Result<f64, TranslationError> {
    let take = |offset: &mut usize, size: usize| -> Result<&[u8], TranslationError> {
        let end = offset.checked_add(size).ok_or_else(|| invalid(None, "binary PLY offset overflows"))?;
        let value = bytes.get(*offset..end).ok_or_else(|| invalid(None, "binary PLY data is truncated"))?;
        *offset = end;
        Ok(value)
    };
    Ok(match data_type {
        PlyScalar::I8 => take(offset, 1)?[0] as i8 as f64,
        PlyScalar::U8 => take(offset, 1)?[0] as f64,
        PlyScalar::I16 => i16::from_le_bytes(take(offset, 2)?.try_into().unwrap()) as f64,
        PlyScalar::U16 => u16::from_le_bytes(take(offset, 2)?.try_into().unwrap()) as f64,
        PlyScalar::I32 => i32::from_le_bytes(take(offset, 4)?.try_into().unwrap()) as f64,
        PlyScalar::U32 => u32::from_le_bytes(take(offset, 4)?.try_into().unwrap()) as f64,
        PlyScalar::F32 => f32::from_le_bytes(take(offset, 4)?.try_into().unwrap()) as f64,
        PlyScalar::F64 => f64::from_le_bytes(take(offset, 8)?.try_into().unwrap()),
    })
}

fn read_ply_integer(bytes: &[u8], offset: &mut usize, data_type: PlyScalar) -> Result<usize, TranslationError> {
    match data_type {
        PlyScalar::F32 | PlyScalar::F64 => Err(invalid(None, "PLY list sizes and indices must use integer types")),
        _ => {
            let value = read_ply_number(bytes, offset, data_type)?;
            if value < 0.0 || value > usize::MAX as f64 {
                return Err(invalid(None, "invalid PLY list integer"));
            }
            Ok(value as usize)
        }
    }
}

pub fn write_obj_with_progress(mesh: &Triangulation, writer: &mut impl Write, progress: &mut dyn FnMut(u64, u64)) -> io::Result<()> {
    let vertex_count = mesh.vertex_count();
    let total = write_item_total(vertex_count + mesh.face_count());
    writeln!(writer, "# exported by Incline Design")?;
    for (i, v) in mesh.vertices().iter().enumerate() {
        writeln!(writer, "v {:.12} {:.12} {:.12}", v.x, v.y, v.z)?;
        if i % WRITE_PROGRESS_STRIDE == 0 {
            progress(i as u64, total);
        }
    }
    for (i, f) in mesh.face_vertex_indices_iter().enumerate() {
        writeln!(writer, "f {} {} {}", f[0] + 1, f[1] + 1, f[2] + 1)?;
        if i % WRITE_PROGRESS_STRIDE == 0 {
            progress((vertex_count + i) as u64, total);
        }
    }
    progress(total, total);
    Ok(())
}

pub fn write_stl_with_progress(mesh: &Triangulation, writer: &mut impl Write, progress: &mut dyn FnMut(u64, u64)) -> io::Result<()> {
    // Validate the full mesh before emitting a header. Binary STL stores
    // coordinates as f32 even though the in-memory mesh uses f64.
    let stl_face_count = u32::try_from(mesh.face_count()).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "STL face count exceeds the format limit"))?;
    for vertex in mesh.vertices() {
        for value in vertex.as_array() {
            finite_stl_f32(value)?;
        }
    }
    for triangle in mesh.triangles() {
        stl_normal(triangle.vertices)?;
    }

    let mut header = [0u8; 80];
    let label = b"binary STL from Incline Design";
    header[..label.len()].copy_from_slice(label);
    writer.write_all(&header)?;
    writer.write_all(&stl_face_count.to_le_bytes())?;
    let total = write_item_total(mesh.face_count());
    for (face_index, triangle) in mesh.triangles().enumerate() {
        if face_index % WRITE_PROGRESS_STRIDE == 0 {
            progress(face_index as u64, total);
        }
        let normal = stl_normal(triangle.vertices)?;
        for value in normal.into_iter().chain(triangle.vertices.into_iter().flat_map(Vertex::as_array)) {
            writer.write_all(&finite_stl_f32(value)?.to_le_bytes())?;
        }
        writer.write_all(&0u16.to_le_bytes())?;
    }
    progress(total, total);
    Ok(())
}

fn finite_stl_f32(value: f64) -> io::Result<f32> {
    let narrowed = value as f32;
    if narrowed.is_finite() {
        Ok(narrowed)
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, "STL coordinate is outside the finite f32 range"))
    }
}

fn stl_normal(vertices: [Vertex; 3]) -> io::Result<[f64; 3]> {
    let [a, b, c] = vertices;
    let u = [b.x - a.x, b.y - a.y, b.z - a.z];
    let v = [c.x - a.x, c.y - a.y, c.z - a.z];
    let mut normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
    let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
    if !length.is_finite() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "STL triangle normal is not finite"));
    }
    if length > 0.0 {
        normal.iter_mut().for_each(|value| *value /= length);
    }
    Ok(normal)
}

pub fn write_ply_with_progress(mesh: &Triangulation, writer: &mut impl Write, progress: &mut dyn FnMut(u64, u64)) -> io::Result<()> {
    writeln!(writer, "ply\nformat ascii 1.0\ncomment exported by Incline Design")?;
    writeln!(writer, "element vertex {}\nproperty double x\nproperty double y\nproperty double z", mesh.vertex_count())?;
    writeln!(writer, "element face {}\nproperty list uchar uint vertex_indices\nend_header", mesh.face_count())?;
    let vertex_count = mesh.vertex_count();
    let total = write_item_total(vertex_count + mesh.face_count());
    for (i, v) in mesh.vertices().iter().enumerate() {
        writeln!(writer, "{:.12} {:.12} {:.12}", v.x, v.y, v.z)?;
        if i % WRITE_PROGRESS_STRIDE == 0 {
            progress(i as u64, total);
        }
    }
    for (i, f) in mesh.face_vertex_indices_iter().enumerate() {
        writeln!(writer, "3 {} {} {}", f[0], f[1], f[2])?;
        if i % WRITE_PROGRESS_STRIDE == 0 {
            progress((vertex_count + i) as u64, total);
        }
    }
    progress(total, total);
    Ok(())
}

fn triangulate(polyline: &[u32], vertices: &[Vertex], output: &mut Vec<[u32; 3]>, line: usize) -> Result<(), TranslationError> {
    if polyline.len() < 3 {
        return Err(invalid(Some(line), "face has fewer than three vertices"));
    }
    if polyline.iter().any(|index| *index as usize >= vertices.len()) {
        return Err(invalid(Some(line), "face index is out of range"));
    }
    if polyline.len() == 3 {
        output.push([polyline[0], polyline[1], polyline[2]]);
        return Ok(());
    }

    // Project onto the polyline's dominant plane so vertical and tilted faces
    // triangulate just as correctly as horizontal ones. Scale first so valid
    // but very large finite coordinates cannot overflow Newell/earcut math.
    let coordinate_scale = polyline.iter().flat_map(|index| vertices[*index as usize].as_array()).map(f64::abs).fold(0.0_f64, f64::max);
    if coordinate_scale == 0.0 || !coordinate_scale.is_finite() {
        return Err(invalid(Some(line), "face is degenerate"));
    }
    let scaled = polyline
        .iter()
        .map(|index| vertices[*index as usize].as_array().map(|value| value / coordinate_scale))
        .collect::<Vec<_>>();
    let mut normal = [0.0_f64; 3];
    for index in 0..polyline.len() {
        let current = scaled[index];
        let next = scaled[(index + 1) % polyline.len()];
        normal[0] += (current[1] - next[1]) * (current[2] + next[2]);
        normal[1] += (current[2] - next[2]) * (current[0] + next[0]);
        normal[2] += (current[0] - next[0]) * (current[1] + next[1]);
    }
    if !normal.iter().all(|value| value.is_finite()) || normal.iter().all(|value| *value == 0.0) {
        return Err(invalid(Some(line), "face is degenerate"));
    }
    let drop_axis = normal
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.abs().total_cmp(&b.abs()))
        .map(|(axis, _)| axis)
        .unwrap_or(2);
    let projected = scaled.into_iter().map(|vertex| match drop_axis {
        0 => [vertex[1], vertex[2]],
        1 => [vertex[0], vertex[2]],
        _ => [vertex[0], vertex[1]],
    });
    let mut local_triangles = Vec::<usize>::new();
    earcut::Earcut::new().earcut(projected, &[], &mut local_triangles);
    if local_triangles.len() < 3 || !local_triangles.len().is_multiple_of(3) {
        return Err(invalid(Some(line), "face could not be triangulated"));
    }
    output.extend(
        local_triangles
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| [polyline[triangle[0]], polyline[triangle[1]], polyline[triangle[2]]]),
    );
    Ok(())
}

fn build_mesh(vertices: Vec<Vertex>, triangles: Vec<[u32; 3]>) -> Result<Triangulation, TranslationError> {
    if vertices.is_empty() {
        return Err(TranslationError::InvalidMesh("mesh has no vertices".into()));
    }
    if triangles.is_empty() {
        return Err(TranslationError::InvalidMesh("mesh has no faces".into()));
    }
    if !vertices.iter().all(|v| v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) {
        return Err(TranslationError::InvalidMesh("mesh contains non-finite coordinates".into()));
    }
    Ok(Triangulation::from_vertices_and_faces(vertices, triangles)?)
}

fn parse_f64(value: Option<&str>, line: usize, name: &str) -> Result<f64, TranslationError> {
    value
        .and_then(|v| v.parse().ok())
        .filter(|v: &f64| v.is_finite())
        .ok_or_else(|| invalid(Some(line), format!("invalid {name}")))
}

fn parse_f32(value: Option<&str>, line: usize, name: &str) -> Result<f32, TranslationError> {
    value
        .and_then(|v| v.parse().ok())
        .filter(|v: &f32| v.is_finite())
        .ok_or_else(|| invalid(Some(line), format!("invalid {name}")))
}

fn invalid(line: Option<usize>, message: impl Into<String>) -> TranslationError {
    TranslationError::InvalidData { line, message: message.into() }
}
