//! Session lifecycle: starting, stopping and evicting runtimes.
//!
//! These are the only methods that create or destroy a [`SessionRuntime`], so
//! every mutation of the session map lives here.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::Utc;
use futures_util::future::join_all;
use parking_lot::RwLock;
use tracing::{debug, info, warn};

use crate::{
    config::AppConfig,
    error::{AppError, Result},
    session::{SessionEvent, validate_session_metadata},
};

use super::super::{
    SessionMeta, SessionStatus, StartSpec,
    persist::append_event,
    runtime::{SessionRuntime, generate_session_id, spawn_session},
};
use super::{
    PreparedStart, SessionHandle, SessionStore, TERMINATE_POLL_INTERVAL, build_soft_stop_schedule,
    log_soft_stop_send,
};

impl SessionStore {
    /// Persist and evict completed sessions that have aged past the in-memory
    /// retention window, and cap oversized live output logs.
    ///
    /// `max_output_log_bytes` of `0` disables the size cap.
    pub async fn run_maintenance(&self, max_output_log_bytes: u64) {
        if max_output_log_bytes > 0 {
            self.truncate_oversized_logs(max_output_log_bytes);
        }
        self.prune_evicted_sessions().await;
    }

    /// Truncate any session `output.log` that has grown past
    /// `max_output_log_bytes`, resetting the persisted log index and the
    /// runtime byte counters so live attach/tail/pagination stay consistent.
    ///
    /// The in-memory rendered screen (`screen_parser`) is untouched, so an
    /// attached client keeps seeing the correct current screen; only the
    /// on-disk scrollback history is dropped.
    fn truncate_oversized_logs(&self, max_output_log_bytes: u64) {
        use crate::session::persist::{append_event, current_output_offset, truncate_output_log};

        let sessions = self.sessions.load_full();
        for (id, handle) in sessions.iter() {
            let mut rt = handle.write();
            // Only cap sessions that are still producing output; a completed
            // session's log is frozen and about to be evicted anyway.
            if rt.is_completed() {
                continue;
            }
            if current_output_offset(&rt.dir) <= max_output_log_bytes {
                continue;
            }

            if let Err(err) = truncate_output_log(&rt.dir) {
                warn!(session_id = %id, %err, "failed to truncate oversized output.log");
                continue;
            }
            crate::session::logs::discard_persisted_log_index(&rt.dir);
            rt.raw_total_bytes = 0;
            rt.last_total_bytes = 0;
            rt.resize_history.clear();
            let _ = append_event(&rt.dir, "output.log truncated (size cap reached)");
            info!(
                session_id = %id,
                max_output_log_bytes,
                "truncated oversized output.log"
            );
        }
    }

    /// Load session history from the SQLite database on daemon startup.
    ///
    /// Any stale `running` / `stopping` sessions are reconciled to `failed`,
    /// persisted back to SQLite, and returned so callers can emit user-facing
    /// startup notifications.
    pub async fn load_running_stopping_sessions(&self) -> Vec<SessionMeta> {
        let db = self.db.clone();

        let mut startup_failed = Vec::new();

        match db
            .load_sessions_with_status(&[SessionStatus::Running, SessionStatus::Stopping])
            .await
        {
            Ok(rows) => {
                for (_, mut meta) in rows {
                    meta.status = SessionStatus::Failed;
                    meta.exit_code = None;
                    if let Err(err) = db.update_session(&meta).await {
                        tracing::warn!(
                            %err,
                            session_id = %meta.id,
                            "failed to persist startup stale-session reconciliation"
                        );
                    }
                    startup_failed.push(meta);
                }
            }
            Err(err) => {
                tracing::warn!(%err, "failed to load startup stale-status sessions from DB");
            }
        }

        startup_failed
    }

    pub async fn start_session_via_handle(
        store_handle: &Arc<Self>,
        config: &AppConfig,
        spec: StartSpec,
    ) -> Result<String> {
        let prepared = store_handle.prepare_start_session(config, spec).await?;

        let PreparedStart {
            mut meta,
            session_dir,
            rows,
            cols,
            notifications_enabled,
        } = prepared;
        let session_id = meta.id.clone();
        let runtime = match spawn_session(
            &mut meta,
            session_dir,
            rows,
            cols,
            notifications_enabled,
            config.screen_scrollback_rows,
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = store_handle.abort_started_session(&session_id).await;
                return Err(err);
            }
        };
        let cleanup_runtime = Arc::clone(&runtime);

        let result = store_handle.commit_started_session(meta, runtime).await;

        if result.is_err() {
            {
                let mut rt = cleanup_runtime.write();
                let _ = rt.pty.kill();
                rt.mark_completed(SessionStatus::Failed, None);
            }
            let _ = store_handle.abort_started_session(&session_id).await;
        } else if let Some(summary) = store_handle.get_summary(&session_id) {
            let _ = store_handle
                .event_tx
                .send(SessionEvent::SessionCreated(summary));
        }

        result
    }

    pub(super) async fn prepare_start_session(
        &self,
        config: &AppConfig,
        spec: StartSpec,
    ) -> Result<PreparedStart> {
        let sessions = self.sessions.load();
        let running_count = sessions
            .values()
            .filter(|handle| !handle.read().is_completed())
            .count();

        let mut state = self.mutable.lock().await;
        if running_count + state.starting_sessions.len() >= config.max_running_sessions {
            return Err(AppError::MaxSessionsReached(config.max_running_sessions));
        }

        let id = generate_session_id(|candidate| {
            sessions.contains_key(candidate) || state.starting_sessions.contains(candidate)
        });

        let rows = spec.rows.unwrap_or(24).max(1);
        let cols = spec.cols.unwrap_or(80).max(1);
        let created_at = Utc::now();
        let (title, tags) = validate_session_metadata(spec.title, spec.tags)?;

        let meta = SessionMeta {
            id: id.clone(),
            title,
            tags,
            command: spec.cmd,
            args: spec.args,
            cwd: spec.cwd,
            created_at,
            started_at: Some(created_at),
            ended_at: None,
            status: SessionStatus::Running,
            pid: None,
            exit_code: None,
        };

        state.starting_sessions.insert(id.clone());
        drop(state);

        if let Err(err) = self.db.insert_session(&meta).await {
            self.mutable.lock().await.starting_sessions.remove(&id);
            return Err(err);
        }

        Ok(PreparedStart {
            meta,
            session_dir: config.sessions_dir.join(&id),
            rows,
            cols,
            notifications_enabled: spec.notifications_enabled,
        })
    }

    pub(super) async fn commit_started_session(
        &self,
        meta: SessionMeta,
        runtime: Arc<RwLock<SessionRuntime>>,
    ) -> Result<String> {
        let id = meta.id.clone();
        let update_result = self.db.update_session(&meta).await;
        self.mutable.lock().await.starting_sessions.remove(&id);
        update_result?;
        let handle = Arc::new(SessionHandle::new(runtime));
        self.sessions.rcu(|current| {
            let mut next = (**current).clone();
            next.insert(id.clone(), handle.clone());
            next
        });
        Ok(id)
    }

    pub(super) async fn abort_started_session(&self, id: &str) -> Result<()> {
        self.mutable.lock().await.starting_sessions.remove(id);
        self.db.delete_session(id).await
    }

    pub async fn stop_session(&self, id: &str, grace_seconds: u64) -> bool {
        self.terminate_session(id, grace_seconds, SessionStatus::Stopped)
            .await
    }

    pub async fn kill_session(&self, id: &str) -> bool {
        self.terminate_session(id, 0, SessionStatus::Killed).await
    }

    async fn terminate_session(
        &self,
        id: &str,
        grace_seconds: u64,
        requested_final_status: SessionStatus,
    ) -> bool {
        let Ok(runtime) = self.lookup_runtime(id).await else {
            debug!(
                session_id = id,
                requested_final_status = requested_final_status.as_str(),
                "terminate session lookup missed"
            );
            return false;
        };

        let terminated = Self::terminate_runtime(
            id.to_string(),
            runtime,
            grace_seconds,
            requested_final_status,
        )
        .await;

        if terminated {
            if let Some(summary) = self.get_summary(id) {
                let _ = self.event_tx.send(SessionEvent::SessionUpdated(summary));
            }
        }

        terminated
    }

    async fn terminate_runtime(
        session_id: String,
        handle: Arc<SessionHandle>,
        grace_seconds: u64,
        requested_final_status: SessionStatus,
    ) -> bool {
        let grace = Duration::from_secs(grace_seconds);
        let start = Instant::now();
        let deadline = start + grace;
        let soft_stop_schedule = build_soft_stop_schedule(start, grace, requested_final_status);
        let mut next_soft_stop_index = 0usize;
        debug!(
            session_id = %session_id,
            requested_final_status = requested_final_status.as_str(),
            grace_seconds,
            soft_stop_attempts = soft_stop_schedule.len(),
            "session termination requested"
        );

        // Begin a soft-stop sequence and let the child exit on its own before
        // escalating to a forced kill when the grace window expires.
        {
            // Brief write lock: check/update status.
            let mut rt = handle.write();
            if rt.refresh_status() {
                debug!(
                    session_id = %session_id,
                    status = rt.meta.status.as_str(),
                    exit_code = ?rt.meta.exit_code,
                    "session already completed before termination started"
                );
                return true;
            }
            rt.requested_final_status = Some(requested_final_status);
            rt.meta.status = SessionStatus::Stopping;
        }
        // Read lock: send first soft-stop input (channel send is &self).
        if let Some((_, input)) = soft_stop_schedule.first() {
            let rt = handle.read();
            log_soft_stop_send(
                &rt.pty,
                &session_id,
                1,
                soft_stop_schedule.len(),
                input,
                &start,
            );
            next_soft_stop_index = 1;
        }

        while Instant::now() < deadline {
            {
                // Brief write lock: poll child exit status.
                let mut rt = handle.write();
                if rt.refresh_status() {
                    debug!(
                        session_id = %session_id,
                        elapsed_ms = start.elapsed().as_millis(),
                        status = rt.meta.status.as_str(),
                        exit_code = ?rt.meta.exit_code,
                        "session exited during grace window"
                    );
                    return true;
                }
            }
            // Read lock: send any due staged soft-stop inputs.
            {
                let rt = handle.read();
                while let Some((at, input)) = soft_stop_schedule.get(next_soft_stop_index) {
                    if Instant::now() < *at {
                        break;
                    }
                    log_soft_stop_send(
                        &rt.pty,
                        &session_id,
                        next_soft_stop_index + 1,
                        soft_stop_schedule.len(),
                        input,
                        &start,
                    );
                    next_soft_stop_index += 1;
                }
            }
            tokio::time::sleep(TERMINATE_POLL_INTERVAL).await;
        }

        let mut rt = handle.write();
        if rt.refresh_status() {
            info!(
                session_id = %session_id,
                elapsed_ms = start.elapsed().as_millis(),
                status = rt.meta.status.as_str(),
                exit_code = ?rt.meta.exit_code,
                "session exited at grace deadline"
            );
            return true;
        }
        debug!(
            session_id = %session_id,
            requested_final_status = requested_final_status.as_str(),
            grace_seconds,
            "session did not stop within grace window; forcing termination"
        );
        if rt.pty.kill().is_ok() {
            let _ = rt.refresh_status();
            info!(
                session_id = %session_id,
                status = rt.meta.status.as_str(),
                exit_code = ?rt.meta.exit_code,
                "forced termination completed"
            );
            true
        } else {
            warn!(
                session_id = %session_id,
                "failed to force terminate session process"
            );
            false
        }
    }

    pub async fn stop_all_sessions(&self, grace_seconds: u64) -> bool {
        let sessions = self.sessions.load();
        let runtimes: Vec<_> = sessions
            .iter()
            .map(|(id, runtime)| (id.clone(), runtime.clone()))
            .collect();

        info!(
            session_count = runtimes.len(),
            grace_seconds, "stopping all sessions"
        );

        let results = join_all(runtimes.into_iter().map(|(session_id, runtime)| {
            Self::terminate_runtime(session_id, runtime, grace_seconds, SessionStatus::Stopped)
        }))
        .await;

        let stopped_count = results.iter().filter(|stopped| **stopped).count();

        info!(
            stopped_count,
            total_sessions = results.len(),
            grace_seconds,
            "completed stop-all session termination pass"
        );
        results.into_iter().all(|stopped| stopped)
    }

    async fn prune_evicted_sessions(&self) {
        let now = Instant::now();
        let mut to_persist: Vec<SessionMeta> = Vec::new();
        let mut evicted_ids: Vec<String> = Vec::new();
        let sessions = self.sessions.load_full();

        for (id, handle) in sessions.iter() {
            let mut rt = handle.write();
            rt.refresh_status();

            if rt.is_completed() && !rt.persisted {
                to_persist.push(rt.meta.clone());
                rt.persisted = true;
            }

            if rt.is_completed() {
                let Some(completed_at) = rt.completed_at else {
                    rt.completed_at = Some(now);
                    continue;
                };
                if now.duration_since(completed_at) >= self.eviction_ttl() {
                    tracing::info!(
                        session_id = id,
                        age_seconds = now.duration_since(completed_at).as_secs(),
                        "evicting completed session from memory after eviction TTL"
                    );
                    let _ = append_event(&rt.dir, "session evicted from memory");
                    evicted_ids.push(id.clone());
                }
            }
        }

        // Persist completed sessions outside the borrow of `self.sessions`.
        for meta in to_persist {
            debug!(session_id = %meta.id, status = meta.status.as_str(), "persisting completed session metadata");
            if let Err(err) = self.db.update_session(&meta).await {
                tracing::error!(%err, session_id = meta.id, "failed to persist completed session");
            }
        }

        if !evicted_ids.is_empty() {
            let evicted_set: HashSet<_> = evicted_ids.iter().cloned().collect();
            self.sessions.rcu(|current| {
                let mut next = (**current).clone();
                next.retain(|id, _| !evicted_set.contains(id));
                next
            });

            let mut state = self.mutable.lock().await;
            for id in evicted_ids {
                debug!(session_id = %id, "session evicted from in-memory store");
                state.evicted_sessions.insert(id, now);
            }
            Self::evict_old_tombstones(&mut state.evicted_sessions, now, self.eviction_ttl());
            return;
        }

        let mut state = self.mutable.lock().await;
        Self::evict_old_tombstones(&mut state.evicted_sessions, now, self.eviction_ttl());
    }

    fn evict_old_tombstones(
        evicted_sessions: &mut HashMap<String, Instant>,
        now: Instant,
        eviction_ttl: Duration,
    ) {
        evicted_sessions.retain(|_, evicted_at| now.duration_since(*evicted_at) < eviction_ttl);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testsupport::*;
    use super::*;
    use crate::session::SessionStatus;
    use chrono::Utc;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    #[tokio::test]
    async fn test_run_maintenance_evicts_completed_session_after_ttl() {
        let rt = make_runtime("evict001", SessionStatus::Stopped, "", None);
        {
            let mut locked = rt.write();
            locked.meta.exit_code = Some(0);
            locked.meta.ended_at = Some(Utc::now());
            locked.completed_at = Some(Instant::now() - Duration::from_secs(2));
        }

        let db = make_test_db().await;
        let store = SessionStore::new(1, db);
        let handle = Arc::new(SessionHandle::new(rt));
        store.sessions.rcu(|current| {
            let mut next = (**current).clone();
            next.insert("evict001".to_string(), handle.clone());
            next
        });

        store.run_maintenance(0).await;

        let sessions = store.sessions.load();
        assert!(
            !sessions.contains_key("evict001"),
            "completed sessions older than the eviction TTL should be removed from memory"
        );
        assert!(
            store
                .mutable
                .lock()
                .await
                .evicted_sessions
                .contains_key("evict001"),
            "evicted sessions should leave a tombstone for follow-up lookups"
        );
    }

    #[tokio::test]
    async fn test_run_maintenance_truncates_oversized_live_log() {
        use crate::session::persist::{append_output_raw, current_output_offset};

        let rt = make_runtime("big01", SessionStatus::Running, "", None);
        let dir = rt.read().dir.clone();
        // Write well past the cap we will set.
        append_output_raw(&dir, &vec![b'x'; 4096]).expect("seed oversized log");
        assert!(current_output_offset(&dir) >= 4096);

        let store = store_with(vec![rt.clone()], make_test_db().await);
        store.run_maintenance(1024).await;

        assert_eq!(
            current_output_offset(&dir),
            0,
            "oversized live log should be truncated to zero"
        );
        let locked = rt.read();
        assert_eq!(locked.raw_total_bytes, 0, "attach offset should reset");
        assert_eq!(
            locked.last_total_bytes, 0,
            "meaningful counter should reset"
        );
    }

    #[tokio::test]
    async fn test_run_maintenance_leaves_small_log_untouched() {
        use crate::session::persist::{append_output_raw, current_output_offset};

        let rt = make_runtime("small1", SessionStatus::Running, "", None);
        let dir = rt.read().dir.clone();
        append_output_raw(&dir, b"hello").expect("seed small log");

        let store = store_with(vec![rt], make_test_db().await);
        store.run_maintenance(1024).await;

        assert_eq!(
            current_output_offset(&dir),
            5,
            "a log under the cap must not be truncated"
        );
    }

    #[tokio::test]
    async fn test_start_session_enforces_limit() {
        let config = make_test_config(1);
        // Create 1 running session
        let rt = make_runtime("s1", SessionStatus::Running, "", None);
        let store = store_with(vec![rt], make_test_db().await);

        // Try to start a 2nd session
        let spec = StartSpec {
            title: None,
            tags: vec![],
            cmd: "echo".into(),
            args: vec![],
            cwd: None,
            rows: None,
            cols: None,
            notifications_enabled: true,
        };

        let result = store.prepare_start_session(&config, spec).await;

        // Assert it fails with MaxSessionsReached
        assert!(result.is_err());
        match result {
            Err(crate::error::AppError::MaxSessionsReached(limit)) => {
                assert_eq!(limit, 1);
            }
            _ => panic!("Expected MaxSessionsReached error, got {:?}", result),
        }
    }

    #[tokio::test]
    async fn test_prepare_start_session_reserves_capacity_until_abort() {
        let config = make_test_config(1);
        let db = make_test_db().await;
        let store = SessionStore::new(900, db.clone());
        let spec = StartSpec {
            title: None,
            tags: vec![],
            cmd: "echo".into(),
            args: vec![],
            cwd: None,
            rows: None,
            cols: None,
            notifications_enabled: true,
        };

        let prepared = store
            .prepare_start_session(&config, spec)
            .await
            .expect("first reservation should succeed");
        assert!(
            db.session_exists(&prepared.meta.id).await,
            "reservation should persist a placeholder session row"
        );

        let result = store
            .prepare_start_session(
                &config,
                StartSpec {
                    title: None,
                    tags: vec![],
                    cmd: "echo".into(),
                    args: vec![],
                    cwd: None,
                    rows: None,
                    cols: None,
                    notifications_enabled: true,
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(crate::error::AppError::MaxSessionsReached(1))
        ));

        store
            .abort_started_session(&prepared.meta.id)
            .await
            .expect("aborting reservation should succeed");
        assert!(
            !db.session_exists(&prepared.meta.id).await,
            "aborting reservation should clean up the placeholder session row"
        );
    }

    // -----------------------------------------------------------------------
    // attach_input — data forwarding and last_input_at tracking
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_stop_session_preserves_completed_failure() {
        let (rt, mut writer_rx) = make_runtime_writable("stp0001", SessionStatus::Failed);
        let rt_clone = rt.clone();
        {
            let mut locked = rt.write();
            locked.meta.exit_code = Some(42);
            locked.meta.ended_at = Some(Utc::now());
            locked.completed_at = Some(Instant::now());
        }
        let store = store_with(vec![rt], make_test_db().await);

        assert!(
            store.stop_session("stp0001", 0).await,
            "completed session should still be treated as found"
        );

        let locked = rt_clone.read();
        assert!(matches!(locked.meta.status, SessionStatus::Failed));
        assert_eq!(locked.meta.exit_code, Some(42));
        assert!(
            writer_rx.try_recv().is_err(),
            "completed sessions should not receive a synthetic Ctrl-C"
        );
    }

    #[tokio::test]
    async fn test_kill_session_preserves_completed_failure() {
        let (rt, mut writer_rx) = make_runtime_writable("kil0001", SessionStatus::Failed);
        let rt_clone = rt.clone();
        {
            let mut locked = rt.write();
            locked.meta.exit_code = Some(99);
            locked.meta.ended_at = Some(Utc::now());
            locked.completed_at = Some(Instant::now());
        }
        let store = store_with(vec![rt], make_test_db().await);

        assert!(
            store.kill_session("kil0001").await,
            "completed session should still be treated as found"
        );

        let locked = rt_clone.read();
        assert!(matches!(locked.meta.status, SessionStatus::Failed));
        assert_eq!(locked.meta.exit_code, Some(99));
        assert!(
            writer_rx.try_recv().is_err(),
            "completed sessions should not receive synthetic input during kill"
        );
    }

    #[tokio::test]
    async fn test_kill_session_terminates_running_session() {
        let (rt, _writer_rx) = make_runtime_writable("kilbasic", SessionStatus::Running);
        let store = store_with(vec![rt], make_test_db().await);

        assert!(
            store.kill_session("kilbasic").await,
            "kill should succeed for a running session"
        );

        let sessions = store.sessions.load();
        let handle = sessions
            .get("kilbasic")
            .expect("runtime should remain addressable");
        let rt = handle.read();
        assert!(matches!(
            rt.meta.status,
            SessionStatus::Killed | SessionStatus::Failed
        ));
        assert!(
            rt.is_completed(),
            "killed session should be marked completed"
        );
    }

    #[tokio::test]
    async fn test_stop_session_uses_staged_soft_shutdown_inputs() {
        let (rt, mut writer_rx) = make_runtime_writable("stp0002", SessionStatus::Running);
        let store = store_with(vec![rt], make_test_db().await);

        assert!(
            store.stop_session("stp0002", 1).await,
            "running session should be stoppable"
        );

        let mut writes = Vec::new();
        while let Ok(bytes) = writer_rx.try_recv() {
            writes.push(bytes);
        }

        assert_eq!(writes, expected_soft_stop_inputs());
    }

    #[tokio::test]
    async fn test_stop_all_sessions_runs_in_parallel() {
        let (rt1, _writer_rx1) = make_runtime_writable("stp1001", SessionStatus::Running);
        let (rt2, _writer_rx2) = make_runtime_writable("stp1002", SessionStatus::Running);
        let (rt3, _writer_rx3) = make_runtime_writable("stp1003", SessionStatus::Running);
        let store = store_with(vec![rt1, rt2, rt3], make_test_db().await);

        let started = Instant::now();
        assert!(
            store.stop_all_sessions(1).await,
            "all running sessions should be stoppable"
        );

        assert!(
            started.elapsed() < Duration::from_millis(2_500),
            "stop_all_sessions should stop multiple sessions concurrently"
        );
    }

    // -----------------------------------------------------------------------
    // attach_detach
    // -----------------------------------------------------------------------

    #[test]
    fn instant_to_utc_reconstructs_recent_wall_clock_time() {
        use crate::session::runtime::instant_to_utc;
        let before = Utc::now();
        let instant = Instant::now() - Duration::from_secs(2);
        let converted = instant_to_utc(instant).expect("conversion should succeed");
        let after = Utc::now();

        assert!(converted >= before - chrono::TimeDelta::seconds(3));
        assert!(converted <= after - chrono::TimeDelta::seconds(1));
    }
}
