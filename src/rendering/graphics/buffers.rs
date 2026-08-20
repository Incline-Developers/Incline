use super::*;

impl<'a> Graphics<'a> {
    pub(super) fn create_stream_buffer(device: &wgpu::Device, label: &'static str, size_bytes: usize, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size_bytes.max(4) as wgpu::BufferAddress,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub(super) fn ensure_stream_capacity(
        device: &wgpu::Device,
        buffer: &mut wgpu::Buffer,
        capacity_items: &mut usize,
        required_items: usize,
        item_size: usize,
        usage: wgpu::BufferUsages,
        label: &'static str,
    ) {
        if required_items <= *capacity_items {
            return;
        }
        // Power-of-two growth must not overshoot the device's per-buffer
        // limit; callers clamp their CPU-side data to the same limit first.
        let max_buffer_size = usize::try_from(device.limits().max_buffer_size).unwrap_or(usize::MAX);
        let max_items = (max_buffer_size / item_size).max(1);
        let new_capacity = checked_growth_capacity(required_items, max_items).expect("scene stream was not clamped to the device buffer limit");
        *buffer = Self::create_stream_buffer(device, label, new_capacity * item_size, usage);
        *capacity_items = new_capacity;
    }

    /// Truncate a CPU-side vertex/index stream pair so both fit the device's
    /// per-buffer limit. Draw calls and picking take their counts from these
    /// vecs, so truncating here keeps every downstream consumer consistent;
    /// the result is missing geometry rather than a fatal validation error.
    pub(super) fn clamp_stream_geometry<V>(device: &wgpu::Device, vertices: &mut Vec<V>, indices: &mut Vec<u32>, label: &'static str) {
        let max_buffer_size = device.limits().max_buffer_size;
        let max_buffer_size_usize = usize::try_from(max_buffer_size).unwrap_or(usize::MAX);
        let max_vertices = max_buffer_size_usize / size_of::<V>();
        let max_indices = max_buffer_size_usize / size_of::<u32>() / 3 * 3;
        if vertices.len() <= max_vertices && indices.len() <= max_indices {
            return;
        }
        crate::userspace_error!(
            "{label}: tessellated geometry ({} vertices, {} indices) exceeds the GPU's {} MiB per-buffer limit; truncating - some geometry will not be displayed",
            vertices.len(),
            indices.len(),
            max_buffer_size / (1024 * 1024)
        );
        vertices.truncate(max_vertices);
        // Retain only the prefix before the *first* invalid triangle. Index
        // maxima are not guaranteed to be monotonic: a later in-range
        // primitive must never hide an earlier out-of-range one.
        let cut = valid_triangle_prefix_len(indices, max_indices, max_vertices);
        indices.truncate(cut);
    }

    pub(super) fn upload_scene_stream_buffers(&mut self) {
        Self::clamp_stream_geometry(&self.device, &mut self.lyon_buffer.vertices, &mut self.lyon_buffer.indices, "Lyon Buffer");
        Self::clamp_stream_geometry(&self.device, &mut self.stroke_vertex_buf, &mut self.stroke_index_buf, "Stroke Buffer");
        Self::clamp_stream_geometry(&self.device, &mut self.text_vertex_buf, &mut self.text_index_buf, "Document Text Buffer");
        if !self.lyon_buffer.vertices.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.lyon_vertex_gpu,
                &mut self.lyon_vertex_capacity,
                self.lyon_buffer.vertices.len(),
                size_of::<Vertex>(),
                wgpu::BufferUsages::VERTEX,
                "Lyon Vertex Buffer",
            );
            self.queue.write_buffer(&self.lyon_vertex_gpu, 0, bytemuck::cast_slice(&self.lyon_buffer.vertices));
        }
        if !self.lyon_buffer.indices.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.lyon_index_gpu,
                &mut self.lyon_index_capacity,
                self.lyon_buffer.indices.len(),
                size_of::<u32>(),
                wgpu::BufferUsages::INDEX,
                "Lyon Index Buffer",
            );
            self.queue.write_buffer(&self.lyon_index_gpu, 0, bytemuck::cast_slice(&self.lyon_buffer.indices));
        }
        if !self.stroke_vertex_buf.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.stroke_vertex_gpu,
                &mut self.stroke_vertex_capacity,
                self.stroke_vertex_buf.len(),
                size_of::<StrokeVertex>(),
                wgpu::BufferUsages::VERTEX,
                "Stroke Vertex Buffer",
            );
            self.queue.write_buffer(&self.stroke_vertex_gpu, 0, bytemuck::cast_slice(&self.stroke_vertex_buf));
        }
        if !self.stroke_index_buf.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.stroke_index_gpu,
                &mut self.stroke_index_capacity,
                self.stroke_index_buf.len(),
                size_of::<u32>(),
                wgpu::BufferUsages::INDEX,
                "Stroke Index Buffer",
            );
            self.queue.write_buffer(&self.stroke_index_gpu, 0, bytemuck::cast_slice(&self.stroke_index_buf));
        }
        if !self.text_vertex_buf.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.text_vertex_gpu,
                &mut self.text_vertex_capacity,
                self.text_vertex_buf.len(),
                size_of::<Vertex>(),
                wgpu::BufferUsages::VERTEX,
                "Document Text Vertex Buffer",
            );
            self.queue.write_buffer(&self.text_vertex_gpu, 0, bytemuck::cast_slice(&self.text_vertex_buf));
        }
        if !self.text_index_buf.is_empty() {
            Self::ensure_stream_capacity(
                &self.device,
                &mut self.text_index_gpu,
                &mut self.text_index_capacity,
                self.text_index_buf.len(),
                size_of::<u32>(),
                wgpu::BufferUsages::INDEX,
                "Document Text Index Buffer",
            );
            self.queue.write_buffer(&self.text_index_gpu, 0, bytemuck::cast_slice(&self.text_index_buf));
        }
    }
}

/// Capacity growth that cannot overflow and never exceeds a hard item limit.
fn checked_growth_capacity(required: usize, maximum: usize) -> Option<usize> {
    if required > maximum || maximum == 0 {
        return None;
    }
    Some(required.checked_next_power_of_two().unwrap_or(maximum).min(maximum).max(1))
}

/// Number of indices in the largest complete-triangle prefix that both fits
/// the index buffer and references only retained vertices.
fn valid_triangle_prefix_len(indices: &[u32], max_indices: usize, max_vertices: usize) -> usize {
    let capped = indices.len().min(max_indices) / 3 * 3;
    indices[..capped]
        .as_chunks::<3>()
        .0
        .iter()
        .position(|triangle| triangle.iter().any(|&index| (index as usize) >= max_vertices))
        .map_or(capped, |triangle| triangle * 3)
}
