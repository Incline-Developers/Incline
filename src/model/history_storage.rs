//! Geometry held by undo is backed too: otherwise unloading an edited layer
//! would keep its vertices alive through earlier commands.

use anyhow::Result;

use super::{Command, History, ItemRef, LayerId, asset_storage::Backing};

#[derive(Clone)]
pub(crate) struct ArchiveRequest {
    pub(crate) sequence: u64,
    pub(crate) path: Vec<usize>,
    pub(crate) command: Command,
}

pub(crate) struct ArchivedCommand {
    pub(crate) sequence: u64,
    pub(crate) path: Vec<usize>,
    pub(crate) command: Command,
}

impl ArchiveRequest {
    pub(crate) fn write(self) -> Result<ArchivedCommand> {
        let (layers, items) = self.command.payload_owners();
        let backing = Backing::write(&serde_json::to_vec(&self.command)?)?;
        Ok(ArchivedCommand {
            sequence: self.sequence,
            path: self.path,
            command: Command::Archived { backing, layers, items },
        })
    }
}

impl Command {
    fn payload_owners(&self) -> (Vec<LayerId>, Vec<ItemRef>) {
        match self {
            Self::AddObject(object) | Self::DeleteObject { object, .. } => (vec![object.layer()], Vec::new()),
            Self::Replace { before, after } => (vec![before.layer(), after.layer()], Vec::new()),
            Self::AddLayerSnapshot { layer, .. } | Self::DeleteLayerSnapshot { layer, .. } => (vec![layer.id], Vec::new()),
            Self::MoveCollars { dataset, .. } | Self::RotateCollars { dataset, .. } | Self::SetTieIns { dataset, .. } => (Vec::new(), vec![ItemRef::DrillHole(*dataset)]),
            _ => (Vec::new(), Vec::new()),
        }
    }

    fn collect_archives(&self, sequence: u64, path: &mut Vec<usize>, layers: &[LayerId], items: &[ItemRef], into: &mut Vec<ArchiveRequest>) {
        if let Self::Batch(commands) = self {
            for (index, command) in commands.iter().enumerate() {
                path.push(index);
                command.collect_archives(sequence, path, layers, items, into);
                path.pop();
            }
        } else {
            let (owned_layers, owned_items) = self.payload_owners();
            if owned_layers.iter().any(|layer| layers.contains(layer)) || owned_items.iter().any(|item| items.contains(item)) {
                into.push(ArchiveRequest {
                    sequence,
                    path: path.clone(),
                    command: self.clone(),
                });
            }
        }
    }

    fn collect_restores(&self, sequence: u64, path: &mut Vec<usize>, into: &mut Vec<(u64, Vec<usize>, Backing)>) {
        match self {
            Self::Batch(commands) => {
                for (index, command) in commands.iter().enumerate() {
                    path.push(index);
                    command.collect_restores(sequence, path, into);
                    path.pop();
                }
            }
            Self::Archived { backing, .. } => into.push((sequence, path.clone(), backing.clone())),
            _ => {}
        }
    }

    fn replace_at(&mut self, path: &[usize], replacement: Command) {
        if let Some((&index, rest)) = path.split_first() {
            if let Self::Batch(commands) = self
                && let Some(command) = commands.get_mut(index)
            {
                command.replace_at(rest, replacement);
            }
        } else {
            *self = replacement;
        }
    }
}

impl History {
    pub(crate) fn archive_revision(&self) -> u64 {
        self.archive_revision
    }

    pub(crate) fn archive_requests(&mut self, layers: &[LayerId], items: &[ItemRef]) -> Vec<ArchiveRequest> {
        self.end_interaction();
        let mut requests = Vec::new();
        for entry in self.project.undo.iter().chain(&self.project.redo) {
            entry.command.collect_archives(entry.sequence, &mut Vec::new(), layers, items, &mut requests);
        }
        requests
    }

    pub(crate) fn archived_step(&self, undo: bool) -> Vec<(u64, Vec<usize>, Backing)> {
        let mut requests = Vec::new();
        if let Some(entry) = (if undo { &self.project.undo } else { &self.project.redo }).last() {
            entry.command.collect_restores(entry.sequence, &mut Vec::new(), &mut requests);
        }
        requests
    }

    pub(crate) fn install_archived_commands(&mut self, commands: Vec<ArchivedCommand>) {
        for replacement in commands {
            if let Some(entry) = self
                .project
                .undo
                .iter_mut()
                .chain(&mut self.project.redo)
                .find(|entry| entry.sequence == replacement.sequence)
            {
                entry.command.replace_at(&replacement.path, replacement.command);
                entry.estimated_bytes = entry.command.estimated_bytes();
            }
        }
        self.retained_bytes = self.project.undo.iter().chain(&self.project.redo).map(|entry| entry.estimated_bytes).sum();
    }
}
