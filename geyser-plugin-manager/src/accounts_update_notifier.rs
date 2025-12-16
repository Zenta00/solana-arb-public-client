/// Module responsible for notifying plugins of account updates
use {
    crate::geyser_plugin_manager::GeyserPluginManager, agave_geyser_plugin_interface::geyser_plugin_interface::{
        ReplicaAccountInfoV3, ReplicaAccountInfoVersions,
    }, arbitrage::simulation::arbitrage_simulator::PoolNotification, crossbeam_channel::Sender, log::*, solana_accounts_db::{
    },
    borsh::de::BorshDeserialize,
    log::*,
    solana_account::{AccountSharedData, ReadableAccount},
    solana_accounts_db::accounts_update_notifier_interface::{
        AccountForGeyser, AccountsUpdateNotifierInterface,
    },
    solana_clock::Slot,
    solana_measure::measure::Measure,
    solana_metrics::*,
    solana_pubkey::Pubkey,
    solana_transaction::sanitized::SanitizedTransaction,
    std::sync::{Arc, RwLock},
    std::collections::HashSet,
};

const PUMPFUN_LAUNCHPAD_ID: Pubkey = Pubkey::from_str_const("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const PUMPFUN_ID: Pubkey = Pubkey::from_str_const("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
const RAYDIUM_LAUNCHPAD_ID: Pubkey = Pubkey::from_str_const("LanMV9sAd7wArD4vJFi2qDdfnVhFxYSUg6eADduJ3uj");
const RAYDIUM_CPMM_ID: Pubkey = Pubkey::from_str_const("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
const RAYDIUM_V4_ID: Pubkey = Pubkey::from_str_const("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");

const LAUNCHPAD_IDS: [Pubkey; 3] = [
    PUMPFUN_ID,
    PUMPFUN_LAUNCHPAD_ID,
    RAYDIUM_LAUNCHPAD_ID,
];
const INIT_TOTAL_SUPPLY: u64 = 1_000_000_000_000_000;

fn is_main_pool(
    account: &AccountSharedData,
    tracked_pubkeys: &Arc<RwLock<HashSet<String>>>,
) -> bool {
    let len = account.data().len();
    let owner = account.owner();

    
    if owner == &PUMPFUN_ID {
        if len == 211 || len == 300 {
            let quote_token_reserve = Pubkey::try_from_slice(&account.data()[171..203]).unwrap();
            tracked_pubkeys.write().unwrap().insert(quote_token_reserve.to_string());
            return true;
        }
    } else if owner == &PUMPFUN_LAUNCHPAD_ID {
        if len >= 48 {
            let total_supply = u64::from_le_bytes(account.data()[40..48].try_into().unwrap());
            return total_supply == INIT_TOTAL_SUPPLY;
        } else {
            return false;
        }
    } else if owner == &RAYDIUM_LAUNCHPAD_ID {
        if len >= 428 {
            let total_supply = u64::from_le_bytes(account.data()[21..29].try_into().unwrap());
            return total_supply == INIT_TOTAL_SUPPLY;
        } else {
            return false;
        }
    } else if owner == &RAYDIUM_CPMM_ID {
        if len != 637 {
            return false;
        }
        return true;
    } else if owner == &RAYDIUM_V4_ID {
        if len != 752 {
            return false;
        }
        return true;
    }
    false
}

#[derive(Debug)]
pub struct AccountsUpdateNotifierImpl {
    tracked_pubkeys: Arc<RwLock<HashSet<String>>>,
    accounts_update_sender: Sender<PoolNotification>,
    snapshot_notifications_enabled: bool,
}

impl AccountsUpdateNotifierInterface for AccountsUpdateNotifierImpl {
    fn snapshot_notifications_enabled(&self) -> bool {
        self.snapshot_notifications_enabled
    }

    fn notify_account_update(
        &self,
        slot: Slot,
        account: &AccountSharedData,
        txn: &Option<&SanitizedTransaction>,
        pubkey: &Pubkey,
        write_version: u64,
    ) {

        if is_main_pool(account, &self.tracked_pubkeys) || self.tracked_pubkeys.read().unwrap().contains(&pubkey.to_string()) {
            if txn.is_none() {
                return;
            }

            let txn_sig = txn.as_ref().unwrap().signature().clone();

            let _ = self.accounts_update_sender.send(PoolNotification {
                pubkey: pubkey.to_string(),
                txn_sig,
                slot,
                data: account.data().to_vec(),
                write_version,
            });
        }
        else {
            return;
        }
    }

    fn notify_account_restore_from_snapshot(
        &self,
        slot: Slot,
        write_version: u64,
        account: &AccountForGeyser<'_>,
    ) {
        let mut measure_all = Measure::start("geyser-plugin-notify-account-restore-all");
        let mut measure_copy = Measure::start("geyser-plugin-copy-stored-account-info");

        let mut account = self.accountinfo_from_account_for_geyser(account);
        account.write_version = write_version;
        measure_copy.stop();

        inc_new_counter_debug!(
            "geyser-plugin-copy-stored-account-info-us",
            measure_copy.as_us() as usize,
            100000,
            100000
        );

        self.notify_plugins_of_account_update(account, slot, true);

        measure_all.stop();

        inc_new_counter_debug!(
            "geyser-plugin-notify-account-restore-all-us",
            measure_all.as_us() as usize,
            100000,
            100000
        );
         
    }

    fn notify_end_of_restore_from_snapshot(&self) {
        /* 
        let plugin_manager = self.plugin_manager.read().unwrap();
        if plugin_manager.plugins.is_empty() {
            return;
        }

        for plugin in plugin_manager.plugins.iter() {
            let mut measure = Measure::start("geyser-plugin-end-of-restore-from-snapshot");
            match plugin.notify_end_of_startup() {
                Err(err) => {
                    error!(
                        "Failed to notify the end of restore from snapshot, error: {} to plugin {}",
                        err,
                        plugin.name()
                    )
                }
                Ok(_) => {
                    trace!(
                        "Successfully notified the end of restore from snapshot to plugin {}",
                        plugin.name()
                    );
                }
            }
            measure.stop();
            inc_new_counter_debug!(
                "geyser-plugin-end-of-restore-from-snapshot",
                measure.as_us() as usize
            );
        }
        */
    }
}

impl AccountsUpdateNotifierImpl {
    pub fn new(
        tracked_pubkeys: Arc<RwLock<HashSet<String>>>, accounts_update_sender: Sender<PoolNotification>,
        snapshot_notifications_enabled: bool,
    ) -> Self {
        AccountsUpdateNotifierImpl {
            
            tracked_pubkeys,
            accounts_update_sender
       ,
            snapshot_notifications_enabled,
        }
    }

    fn accountinfo_from_shared_account_data<'a>(
        &self,
        account: &'a AccountSharedData,
        txn: &'a Option<&'a SanitizedTransaction>,
        pubkey: &'a Pubkey,
        write_version: u64,
    ) -> ReplicaAccountInfoV3<'a> {
        ReplicaAccountInfoV3 {
            pubkey: pubkey.as_ref(),
            lamports: account.lamports(),
            owner: account.owner().as_ref(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data: account.data(),
            write_version,
            txn: *txn,
        }
    }

    fn accountinfo_from_account_for_geyser<'a>(
        &self,
        account: &'a AccountForGeyser<'_>,
    ) -> ReplicaAccountInfoV3<'a> {
        ReplicaAccountInfoV3 {
            pubkey: account.pubkey.as_ref(),
            lamports: account.lamports(),
            owner: account.owner().as_ref(),
            executable: account.executable(),
            rent_epoch: account.rent_epoch(),
            data: account.data(),
            write_version: 0, // can/will be populated afterwards
            txn: None,
        }
    }

    fn notify_plugins_of_account_update(
        &self,
        account: ReplicaAccountInfoV3,
        slot: Slot,
        is_startup: bool,
    ) {
        /*
        let mut measure2 = Measure::start("geyser-plugin-notify_plugins_of_account_update");
        let plugin_manager = self.plugin_manager.read().unwrap();

        if plugin_manager.plugins.is_empty() {
            return;
        }
        for plugin in plugin_manager.plugins.iter() {
            let mut measure = Measure::start("geyser-plugin-update-account");
            match plugin.update_account(
                ReplicaAccountInfoVersions::V0_0_3(&account),
                slot,
                is_startup,
            ) {
                Err(err) => {
                    error!(
                        "Failed to update account {} at slot {}, error: {} to plugin {}",
                        bs58::encode(account.pubkey).into_string(),
                        slot,
                        err,
                        plugin.name()
                    )
                }
                Ok(_) => {
                    trace!(
                        "Successfully updated account {} at slot {} to plugin {}",
                        bs58::encode(account.pubkey).into_string(),
                        slot,
                        plugin.name()
                    );
                }
            }
            measure.stop();
            inc_new_counter_debug!(
                "geyser-plugin-update-account-us",
                measure.as_us() as usize,
                100000,
                100000
            );
        }
        measure2.stop();
        inc_new_counter_debug!(
            "geyser-plugin-notify_plugins_of_account_update-us",
            measure2.as_us() as usize,
            100000,
            100000
        );
         */
    }
}
