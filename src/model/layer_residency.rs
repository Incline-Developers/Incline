//! Backing for unloaded design layers, including their per-object hide flags.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{Document, LayerId, Object, ObjectId, asset_storage::Backing};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct LayerPayload {
    pub(crate) objects: Vec<(Object, bool)>,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredLayer {
    pub(crate) backing: Backing,
    pub(crate) payload_hash: u64,
    pub(crate) object_count: usize,
    pub(crate) max_object_id: u64,
    /// Foreign merges allocate fresh IDs without keeping an object-ID map in RAM.
    pub(crate) remap_start: Option<u64>,
}

impl LayerPayload {
    pub(crate) fn store(&self) -> Result<DeferredLayer> {
        let mut hasher = DefaultHasher::new();
        for (object, hidden) in &self.objects {
            object.geometry_hash().hash(&mut hasher);
            hidden.hash(&mut hasher);
        }
        Ok(DeferredLayer {
            backing: Backing::write(&serde_json::to_vec(self)?)?,
            payload_hash: hasher.finish(),
            object_count: self.objects.len(),
            max_object_id: self.objects.iter().map(|(object, _)| object.id().0 & u64::from(u32::MAX)).max().unwrap_or(0),
            remap_start: None,
        })
    }
}

impl DeferredLayer {
    pub(crate) fn read(&self, layer: LayerId) -> Result<LayerPayload> {
        let mut payload: LayerPayload = serde_json::from_slice(&self.backing.read()?)?;
        let namespace = layer.0 & !u64::from(u32::MAX);
        for (index, (object, _)) in payload.objects.iter_mut().enumerate() {
            let local = self.remap_start.map_or(object.id().0 & u64::from(u32::MAX), |start| start + index as u64);
            *object = object.with_id_and_layer(ObjectId(namespace | local), layer);
        }
        Ok(payload)
    }
}

impl Document {
    pub(crate) fn layer_payload(&self, id: LayerId) -> LayerPayload {
        LayerPayload {
            objects: self
                .objects
                .iter()
                .filter(|object| object.layer() == id)
                .map(|object| (object.clone(), self.is_object_hidden(object.id())))
                .collect(),
        }
    }

    pub(crate) fn install_deferred_layer(&mut self, id: LayerId, deferred: DeferredLayer) {
        self.objects.retain(|object| object.layer() != id);
        self.objects.shrink_to_fit();
        self.deferred_layers.insert(id, deferred);
        self.rebuild_object_index();
        self.hidden_objects.retain(|id| self.object_index.contains_key(id));
        self.object_revisions.retain(|id, _| self.object_index.contains_key(id));
        self.object_index.shrink_to_fit();
        self.object_revisions.shrink_to_fit();
        self.hidden_objects.shrink_to_fit();
    }

    pub(crate) fn restore_layer_payload(&mut self, id: LayerId, payload: LayerPayload) {
        self.deferred_layers.remove(&id);
        for (object, hidden) in payload.objects {
            let object_id = object.id();
            self.bump_next_object_id(object_id);
            self.object_revisions.insert(object_id, self.revision);
            self.objects.push(object);
            if hidden {
                self.hidden_objects.insert(object_id);
            }
        }
        self.rebuild_object_index();
    }

    pub(crate) fn copy_deferred_layers(&mut self, imported: &Document, layer_map: &HashMap<LayerId, LayerId>, preserve_ids: bool) {
        for (&old, deferred) in &imported.deferred_layers {
            let Some(&id) = layer_map.get(&old) else {
                continue;
            };
            let mut deferred = deferred.clone();
            if !preserve_ids || old != id {
                let start = self.next_object_id;
                self.next_object_id += deferred.object_count as u64;
                deferred.remap_start = Some(start & u64::from(u32::MAX));
                deferred.max_object_id = self.next_object_id.saturating_sub(1) & u64::from(u32::MAX);
            } else {
                self.next_object_id = self.next_object_id.max(deferred.max_object_id + 1);
            }
            self.deferred_layers.insert(id, deferred);
        }
    }

    pub(crate) fn payload_hashes(&self, cache: &mut HashMap<ObjectId, (u64, u64)>) -> HashMap<LayerId, u64> {
        let mut hashers: HashMap<LayerId, DefaultHasher> = self.layers.iter().map(|layer| (layer.id, DefaultHasher::new())).collect();
        for object in &self.objects {
            let id = object.id();
            let revision = self.object_revision(id);
            let hash = match cache.get(&id) {
                Some(&(cached_revision, hash)) if cached_revision == revision => hash,
                _ => {
                    let hash = object.geometry_hash();
                    cache.insert(id, (revision, hash));
                    hash
                }
            };
            if let Some(hasher) = hashers.get_mut(&object.layer()) {
                hash.hash(hasher);
                self.is_object_hidden(id).hash(hasher);
            }
        }
        cache.retain(|id, _| self.object_index.contains_key(id));
        if cache.capacity() > cache.len().saturating_mul(2).max(64) {
            cache.shrink_to_fit();
        }
        hashers
            .into_iter()
            .map(|(id, hasher)| (id, self.deferred_layers.get(&id).map_or_else(|| hasher.finish(), |stored| stored.payload_hash)))
            .collect()
    }
}
