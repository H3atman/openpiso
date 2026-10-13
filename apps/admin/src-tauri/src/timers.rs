use std::time::Duration;

use sqlx::SqlitePool;
use tracing::error;

use crate::{db, events::EventHub, sessions::SessionStore};

pub async fn run(pool: SqlitePool, events: EventHub, sessions: SessionStore) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        interval.tick().await;
        // Attendance finalization rides the tick so crossed local dates and
        // deadlines advance without a dedicated scheduler (ADR-0084). A no-op
        // tick costs one clock read; failures log, they never stop the timer.
        match db::attendance::finalize_calendar_dates(
            &pool,
            chrono::Utc::now(),
            db::attendance::host_offset(),
        )
        .await
        {
            Ok(_) => {}
            Err(error) => {
                error!(target: "openpiso_admin_lib::timers", "OpenPiso Attendance finalization tick failed: {error}");
            }
        }
        match db::observe_shop_status(&pool).await {
            Ok(status) => {
                if events.shop_status().as_ref() != Some(&status) {
                    events.set_shop_status(status);
                    if let Ok(pcs) = db::pcs(&pool).await {
                        for pc in sessions.enrich_pcs(pcs).await {
                            events.publish_pc_updated(pc);
                        }
                    }
                }
            }
            Err(error) => {
                error!(target: "openpiso_admin_lib::timers", "OpenPiso Shop Hours observer failed: {error}");
            }
        }
        let account_pcs = match sessions.tick_accounts(&pool).await {
            Ok(pc_ids) => pc_ids,
            Err(error) => {
                error!(target: "openpiso_admin_lib::timers", "OpenPiso account timer tick failed: {error}");
                continue;
            }
        };
        let active_pc_ids = sessions.active_pc_ids().await;
        let mut changed_pcs = Vec::new();
        match db::tick_guest_pcs(&pool, &active_pc_ids).await {
            Ok(pcs) => {
                // End active sessions for PCs that timed out and went offline.
                for pc in &pcs {
                    if pc.status == "offline" {
                        sessions.disconnect_pc(&pc.id).await;
                    }
                }
                changed_pcs = pcs;
            }
            Err(error) => {
                error!(target: "openpiso_admin_lib::timers", "OpenPiso guest timer tick failed: {error}")
            }
        }
        let mut changed_ids = account_pcs;
        changed_ids.extend(changed_pcs.into_iter().map(|pc| pc.id));
        changed_ids.sort();
        changed_ids.dedup();
        for pc_id in changed_ids {
            match db::pc(&pool, &pc_id).await {
                Ok(pc) => {
                    events.publish_pc_updated(sessions.enrich_pc(pc).await);
                }
                Err(error) => {
                    error!(target: "openpiso_admin_lib::timers", pc_id = %pc_id, "Could not publish timer state for {pc_id}: {error}")
                }
            }
        }
    }
}
