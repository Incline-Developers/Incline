//! Cached document text layout and vector glyph meshes.

use std::{
    collections::{HashMap, HashSet, hash_map},
    hash::{BuildHasher, Hash, Hasher},
};

use cosmic_text::{Attrs, AttrsList, BufferLine, Color, FamilyOwned, FontSystem, Style, Weight};
use fontmesh::{FontMeshError, FontRef, GlyphId, Mesh2D, glyph_to_mesh_2d};
use glam::{Mat4, Vec3};

use crate::{
    fonts::cosmic_text_font_system,
    rendering::{Vertex, color::linear_to_srgb_byte},
};

/// Curves are flattened once per distinct glyph. Twelve subdivisions per
/// Bezier is smooth at normal CAD label sizes without producing excessive
/// document geometry when the same cached glyph is emitted many times.
const GLYPH_CURVE_SUBDIVISIONS: u8 = 12;

#[derive(Debug, Clone, Copy, Hash)]
struct Font<'a> {
    family: cosmic_text::Family<'a>,
    weight: Weight,
    style: Style,
}

#[derive(Clone, Copy, Hash)]
pub(crate) struct SectionKey<'a> {
    content: &'a str,
    font: Font<'a>,
    color: Color,
    index: usize,
}

#[derive(Clone)]
pub(crate) struct Key<'a> {
    lines: Vec<Vec<SectionKey<'a>>>,
    size: f32,
    line_height: f32,
    bounds: (f32, f32),
}

type KeyHash = u64;
type HashBuilder = twox_hash::xxhash64::RandomState;

#[derive(Default)]
pub(crate) struct TextCache {
    entries: HashMap<KeyHash, cosmic_text::Buffer>,
    recently_used: HashSet<KeyHash>,
    hasher: HashBuilder,
}

impl TextCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn allocate(&mut self, font_system: &mut FontSystem, key: Key<'_>) -> KeyHash {
        let hash = {
            let mut hasher = self.hasher.build_hasher();

            key.lines.hash(&mut hasher);
            key.size.to_bits().hash(&mut hasher);
            key.line_height.to_bits().hash(&mut hasher);
            key.bounds.0.to_bits().hash(&mut hasher);
            key.bounds.1.to_bits().hash(&mut hasher);

            hasher.finish()
        };

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(hash) {
            let metrics = cosmic_text::Metrics::new(key.size, key.line_height);
            let mut buffer = cosmic_text::Buffer::new(font_system, metrics);

            buffer.set_size(Some(key.bounds.0), Some(key.bounds.1.max(key.line_height)));
            buffer.lines.clear();

            for line in key.lines {
                let mut line_str = String::new();
                let mut attrs_list = AttrsList::new(&Attrs::new());
                for section in line {
                    let start = line_str.len();
                    line_str.push_str(section.content);
                    let end = line_str.len();
                    attrs_list.add_span(
                        start..end,
                        &Attrs::new()
                            .family(section.font.family)
                            .weight(section.font.weight)
                            .style(section.font.style)
                            .color(section.color)
                            .metadata(section.index),
                    );
                }
                buffer
                    .lines
                    .push(BufferLine::new(line_str, cosmic_text::LineEnding::CrLf, attrs_list, cosmic_text::Shaping::Advanced));
            }

            buffer.shape_until_scroll(font_system, true);
            entry.insert(buffer);
        }

        let _ = self.recently_used.insert(hash);
        hash
    }

    pub(crate) fn trim(&mut self) {
        self.entries.retain(|key, _| self.recently_used.contains(key));
        self.recently_used.clear();
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct GlyphMeshKey {
    font_id: cosmic_text::fontdb::ID,
    glyph_id: u16,
}

#[derive(Default)]
struct GlyphMeshCache {
    /// `None` is cached as well so spaces, bitmap-only glyphs, and malformed
    /// outlines are not reparsed on every document rebuild.
    entries: HashMap<GlyphMeshKey, Option<Mesh2D>>,
}

impl GlyphMeshCache {
    fn get_or_create(&mut self, database: &cosmic_text::fontdb::Database, key: GlyphMeshKey) -> Option<&Mesh2D> {
        self.entries
            .entry(key)
            .or_insert_with(|| {
                let generated = database
                    .with_face_data(key.font_id, |data, face_index| {
                        let font = FontRef::from_index(data, face_index).map_err(|error| format!("failed to parse font face {face_index}: {error:?}"))?;
                        match glyph_to_mesh_2d(&font, GlyphId::new(u32::from(key.glyph_id)), GLYPH_CURVE_SUBDIVISIONS) {
                            Ok(mesh) => Ok(Some(mesh)),
                            Err(FontMeshError::NoOutline) => Ok(None),
                            Err(error) => Err(error.to_string()),
                        }
                    })
                    .unwrap_or_else(|| Err("font face is no longer available".to_owned()));

                match generated {
                    Ok(mesh) => mesh,
                    Err(error) => {
                        log::warn!(
                            "{}",
                            crate::i18n::tr_format!(
                                literal = "Could not build vector mesh for font %font%, glyph %glyph%: %error%",
                                font = format!("{:?}", key.font_id),
                                glyph = key.glyph_id,
                                error = error
                            )
                        );
                        None
                    }
                }
            })
            .as_ref()
    }
}

pub(crate) struct TextSystem {
    pub(crate) font_system: FontSystem,
    pub(crate) text_cache: TextCache,
    glyph_mesh_cache: GlyphMeshCache,
}

impl TextSystem {
    pub(crate) fn new() -> Self {
        Self {
            font_system: cosmic_text_font_system(),
            text_cache: TextCache::new(),
            glyph_mesh_cache: GlyphMeshCache::default(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Text {
    pub(crate) text: String,
    pub(crate) color: Option<[f32; 4]>,
    pub(crate) is_bold: bool,
    pub(crate) is_italic: bool,
    pub(crate) font_family: FamilyOwned,
    pub(crate) default_color: [f32; 4],
}

impl Text {
    pub(crate) fn new(text: String, default_text_color: [f32; 4]) -> Self {
        Self {
            text,
            default_color: default_text_color,
            color: None,
            is_bold: false,
            is_italic: false,
            font_family: FamilyOwned::Monospace,
        }
    }

    fn color(&self) -> [f32; 4] {
        self.color.unwrap_or(self.default_color)
    }

    fn style(&self) -> Style {
        if self.is_italic { Style::Italic } else { Style::Normal }
    }

    fn weight(&self) -> Weight {
        if self.is_bold { Weight::BOLD } else { Weight::NORMAL }
    }

    pub(crate) fn section_keys(&self, index: usize) -> Vec<SectionKey<'_>> {
        let color = self.color();
        let color = Color::rgba(
            linear_to_srgb_byte(color[0]),
            linear_to_srgb_byte(color[1]),
            linear_to_srgb_byte(color[2]),
            (color[3].clamp(0.0, 1.0) * 255.) as u8,
        );
        let font = Font {
            family: self.font_family.as_family(),
            weight: self.weight(),
            style: self.style(),
        };
        self.text
            .lines()
            .map(|line| SectionKey {
                content: line,
                font,
                color,
                index,
            })
            .collect()
    }
}

#[derive(Clone)]
pub(crate) struct TextBox {
    pub(crate) font_size: f32,
    pub(crate) line_height_factor: f32,
    pub(crate) texts: Vec<Text>,
    pub(crate) hidpi_scale: f32,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height_factor: 1.1,
            texts: Vec::new(),
            hidpi_scale: 1.0,
        }
    }
}

impl TextBox {
    pub(crate) fn new(texts: Vec<Text>, hidpi_scale: f32) -> TextBox {
        TextBox {
            texts,
            hidpi_scale,
            ..Default::default()
        }
    }

    pub(crate) fn line_height(&self, zoom: f32) -> f32 {
        self.font_size * self.line_height_factor * self.hidpi_scale * zoom
    }

    pub(crate) fn key(&self, bounds: (f32, f32)) -> Key<'_> {
        let mut lines = Vec::new();
        let mut sections = Vec::new();
        for (i, text) in self.texts.iter().enumerate() {
            let text_lines = text.section_keys(i);
            for (line_index, line_section) in text_lines.into_iter().enumerate() {
                if line_index > 0 {
                    lines.push(std::mem::take(&mut sections));
                }
                sections.push(line_section);
            }
            if text.text.ends_with('\n') {
                lines.push(std::mem::take(&mut sections));
            }
        }
        if !sections.is_empty() {
            lines.push(sections);
        }

        Key {
            lines,
            size: self.font_size * self.hidpi_scale,
            line_height: self.line_height(1.),
            bounds,
        }
    }

    /// Maximum shaped line advance in layout units. Unlike a character-count
    /// estimate, this accounts for the selected font, fallback faces,
    /// ligatures, and per-glyph advances.
    pub(crate) fn layout_width(&self, text_system: &mut TextSystem, bounds: (f32, f32)) -> f32 {
        let TextSystem { font_system, text_cache, .. } = text_system;
        let key = text_cache.allocate(font_system, self.key(bounds));
        text_cache
            .entries
            .get(&key)
            .map(|buffer| buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max))
            .unwrap_or(0.0)
    }

    /// Shape this text and append its filled glyph outlines as ordinary
    /// scene-origin-relative document triangles.
    pub(crate) fn append_meshes(&self, text_system: &mut TextSystem, bounds: (f32, f32), transform: Mat4, vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>) {
        let TextSystem {
            font_system,
            text_cache,
            glyph_mesh_cache,
        } = text_system;
        let key = text_cache.allocate(font_system, self.key(bounds));
        let Some(buffer) = text_cache.entries.get(&key) else {
            return;
        };
        let database = font_system.db();

        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let mesh_key = GlyphMeshKey {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                };
                let Some(mesh) = glyph_mesh_cache.get_or_create(database, mesh_key) else {
                    continue;
                };
                let color = self.texts.get(glyph.metadata).map(Text::color).unwrap_or([1.0; 4]);
                let glyph_x = glyph.x + glyph.font_size * glyph.x_offset;
                let baseline_y = run.line_y + glyph.y - glyph.font_size * glyph.y_offset;

                if !append_glyph_mesh(mesh, glyph_x, baseline_y, glyph.font_size, transform, color, vertices, indices) {
                    log::warn!("{}", crate::i18n::tr!(literal = "Document text mesh exceeded its u32 index range"));
                    return;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn append_glyph_mesh(mesh: &Mesh2D, glyph_x: f32, baseline_y: f32, font_size: f32, transform: Mat4, color: [f32; 4], vertices: &mut Vec<Vertex>, indices: &mut Vec<u32>) -> bool {
    let Ok(base_vertex) = u32::try_from(vertices.len()) else {
        return false;
    };
    let Some(vertex_end) = vertices.len().checked_add(mesh.vertices.len()) else {
        return false;
    };
    if vertex_end > u32::MAX as usize || mesh.indices.iter().any(|&index| index as usize >= mesh.vertices.len()) {
        return false;
    }

    vertices.extend(mesh.vertices.iter().map(|point| {
        // Font outlines are Y-up around the baseline, while the cosmic-text
        // layout is Y-down. Convert to layout-local coordinates before
        // applying the same Y-flipping world transform formerly used for
        // raster glyph quads.
        let local = Vec3::new(glyph_x + point.x * font_size, baseline_y - point.y * font_size, 0.0);
        Vertex {
            pos: transform.transform_point3(local).to_array(),
            color,
        }
    }));
    indices.extend(mesh.indices.iter().map(|index| base_vertex + index));
    true
}
