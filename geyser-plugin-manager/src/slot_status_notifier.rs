use {
    crate::geyser_plugin_manager::GeyserPluginManager,
    agave_geyser_plugin_interface::geyser_plugin_interface::SlotStatus,
    log::*,
    solana_clock::Slot,
    solana_measure::measure::Measure,
    solana_metrics::*,
    solana_rpc::slot_status_notifier::SlotStatusNotifierInterface,
    std::sync::{Arc, RwLock},
    crossbeam_channel::Sender,
};

pub struct SlotStatusNotifierImpl {
    slot_status_sender: Sender<Slot>,
}

impl SlotStatusNotifierInterface for SlotStatusNotifierImpl {
    fn notify_slot_confirmed(&self, slot: Slot, parent: Option<Slot>) {
        self.notify_slot_status(slot, parent, SlotStatus::Confirmed);
    }

    fn notify_slot_processed(&self, slot: Slot, parent: Option<Slot>) {
        self.notify_slot_status(slot, parent, SlotStatus::Processed);
    }

    fn notify_slot_rooted(&self, slot: Slot, parent: Option<Slot>) {
        self.notify_slot_status(slot, parent, SlotStatus::Rooted);
    }

    fn notify_first_shred_received(&self, slot: Slot) {
        self.notify_slot_status(slot, None, SlotStatus::FirstShredReceived);
    }

    fn notify_completed(&self, slot: Slot) {
        self.notify_slot_status(slot, None, SlotStatus::Completed);
    }

    fn notify_created_bank(&self, slot: Slot, parent: Slot) {
        self.notify_slot_status(slot, Some(parent), SlotStatus::CreatedBank);
    }

    fn notify_slot_dead(&self, slot: Slot, parent: Slot, error: String) {
        self.notify_slot_status(slot, Some(parent), SlotStatus::Dead(error));
    }
}

impl SlotStatusNotifierImpl {
    pub fn new(
        slot_status_sender: Sender<Slot>,
    ) -> Self {
        Self {
            slot_status_sender,
        }
    }

    pub fn notify_slot_status(&self, slot: Slot, parent: Option<Slot>, slot_status: SlotStatus) {
        if slot_status == SlotStatus::FirstShredReceived {
            self.slot_status_sender.send(slot).unwrap();
        }
    }
}
