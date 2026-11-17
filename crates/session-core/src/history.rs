//! Undo/Redo snapshot history stack for session edits.
//! Deduplicates `state_base64` plugin chunks across snapshots via shared `Arc<str>` handles.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};

use uuid::Uuid;

use crate::SessionConfig;

#[derive(Debug, Clone)]
struct HistorySnapshot {
    session: SessionConfig,
    /// Shared plugin state chunks keyed by slot UUID so unchanged blobs are not duplicated.
    slot_blobs: HashMap<Uuid, Arc<str>>,
}

/// Bounded Undo/Redo history manager storing session snapshots before mutations.
#[derive(Debug, Clone)]
pub struct SessionHistory {
    undo_stack: VecDeque<HistorySnapshot>,
    redo_stack: Vec<HistorySnapshot>,
    blob_pool: HashMap<Uuid, Weak<str>>,
    max_entries: usize,
}

impl Default for SessionHistory {
    fn default() -> Self {
        Self::new(32)
    }
}

impl SessionHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo_stack: VecDeque::new(),
            redo_stack: Vec::new(),
            blob_pool: HashMap::new(),
            max_entries: max_entries.max(1),
        }
    }

    fn pack_snapshot(&mut self, mut state: SessionConfig) -> HistorySnapshot {
        let mut slot_blobs = HashMap::new();
        for rack in &mut state.racks {
            for slot in &mut rack.slots {
                if let Some(raw) = slot.state_base64.take() {
                    let shared = if let Some(existing) =
                        self.blob_pool.get(&slot.id).and_then(Weak::upgrade)
                    {
                        if existing.as_ref() == raw.as_str() {
                            existing
                        } else {
                            let fresh: Arc<str> = Arc::from(raw);
                            self.blob_pool.insert(slot.id, Arc::downgrade(&fresh));
                            fresh
                        }
                    } else {
                        let fresh: Arc<str> = Arc::from(raw);
                        self.blob_pool.insert(slot.id, Arc::downgrade(&fresh));
                        fresh
                    };
                    slot_blobs.insert(slot.id, shared);
                }
            }
        }
        self.blob_pool.retain(|_, weak| weak.strong_count() > 0);
        HistorySnapshot {
            session: state,
            slot_blobs,
        }
    }

    fn unpack_snapshot(mut snap: HistorySnapshot) -> SessionConfig {
        for rack in &mut snap.session.racks {
            for slot in &mut rack.slots {
                if let Some(blob) = snap.slot_blobs.get(&slot.id) {
                    slot.state_base64 = Some(blob.to_string());
                }
            }
        }
        snap.session
    }

    fn snapshots_equal(a: &HistorySnapshot, b: &HistorySnapshot) -> bool {
        if a.session != b.session || a.slot_blobs.len() != b.slot_blobs.len() {
            return false;
        }
        a.slot_blobs
            .iter()
            .all(|(id, blob_a)| b.slot_blobs.get(id).is_some_and(|blob_b| blob_a == blob_b))
    }

    /// Pushes `previous_state` onto the undo stack and clears any pending redo branch.
    pub fn push(&mut self, previous_state: SessionConfig) {
        let packed = self.pack_snapshot(previous_state);
        if self
            .undo_stack
            .back()
            .is_some_and(|back| Self::snapshots_equal(back, &packed))
        {
            return;
        }
        if self.undo_stack.len() >= self.max_entries {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(packed);
        self.redo_stack.clear();
    }

    /// Pops the previous state if available, saving `current_state` onto the redo stack.
    pub fn undo(&mut self, current_state: SessionConfig) -> Option<SessionConfig> {
        let prev = self.undo_stack.pop_back()?;
        let cur = self.pack_snapshot(current_state);
        self.redo_stack.push(cur);
        Some(Self::unpack_snapshot(prev))
    }

    /// Reapplies an undone state if available, saving `current_state` onto the undo stack.
    pub fn redo(&mut self, current_state: SessionConfig) -> Option<SessionConfig> {
        let next = self.redo_stack.pop()?;
        let cur = self.pack_snapshot(current_state);
        if self.undo_stack.len() >= self.max_entries {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(cur);
        Some(Self::unpack_snapshot(next))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.blob_pool.clear();
        self.shrink_to_fit();
    }

    /// Prunes expired weak blob handles and releases excess container capacity when idle.
    pub fn shrink_to_fit(&mut self) {
        self.blob_pool.retain(|_, weak| weak.strong_count() > 0);
        self.undo_stack.shrink_to_fit();
        self.redo_stack.shrink_to_fit();
        self.blob_pool.shrink_to_fit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltInEffectKind, PluginKind, PluginSlotConfig};

    #[test]
    fn undo_and_redo_restore_session_snapshots() {
        let mut history = SessionHistory::new(10);
        let s0 = SessionConfig {
            name: "Verse".to_string(),
            ..Default::default()
        };
        history.push(s0.clone());

        let mut s1 = s0.clone();
        s1.name = "Chorus".to_string();

        let undone = history.undo(s1.clone()).expect("undo available");
        assert_eq!(undone.name, "Verse");
        assert!(!history.can_undo());
        assert!(history.can_redo());

        let redone = history.redo(undone).expect("redo available");
        assert_eq!(redone.name, "Chorus");
        assert!(history.can_undo());
        assert!(!history.can_redo());
    }

    #[test]
    fn deduplicates_identical_plugin_state_blobs_across_snapshots() {
        let mut history = SessionHistory::new(10);
        let mut s0 = SessionConfig::default();
        let mut slot = PluginSlotConfig::new(
            "TestFX",
            PluginKind::BuiltInEffect(BuiltInEffectKind::GainLimiter),
        );
        slot.state_base64 = Some("AAAA_LARGE_BLOB_PAYLOAD".to_string());
        let slot_id = slot.id;
        s0.racks[0].slots.push(slot);

        history.push(s0.clone());
        let mut s1 = s0.clone();
        s1.name = "Modified".to_string();
        history.push(s1.clone());

        let arc0 = &history.undo_stack[0].slot_blobs[&slot_id];
        let arc1 = &history.undo_stack[1].slot_blobs[&slot_id];
        assert!(Arc::ptr_eq(arc0, arc1));

        let restored = history.undo(s1).expect("undo");
        assert_eq!(
            restored.racks[0].slots.last().and_then(|s| s.state_base64.as_deref()),
            Some("AAAA_LARGE_BLOB_PAYLOAD")
        );
    }
}
