use chrono::Local;

mod account;
mod models;
mod paths;
mod provider;
mod state;
mod storage;
mod token;
mod window;

pub use models::{AccountStatus, DailyPoint, RateWindow, Snapshot};
pub use token::scan_sessions;

use crate::state::{RefreshMode, Store};

pub(crate) fn dashboard_snapshot(state: &Store, mode: RefreshMode) -> Snapshot {
    let local_usage = token::read_local_usage(state);
    let tokens = token::select_token_snapshot(
        provider::read_account_usage(state, mode),
        local_usage,
        Local::now().date_naive(),
    );
    let expanded = *state.expanded.lock().unwrap();
    let (windows, status, freshness, rate_updated_at) = provider::read_rate_limits(state, mode);
    Snapshot {
        expanded,
        today: tokens.today,
        last7: tokens.last7,
        last30: tokens.last30,
        daily: tokens.daily,
        windows,
        account: account::query_account_status(state, mode),
        rate_status: status,
        rate_freshness: freshness,
        token_coverage: tokens.token_coverage,
        usage_source: tokens.usage_source,
        today_available: tokens.today_available,
        updated_at: if rate_updated_at.is_empty() {
            Local::now().to_rfc3339()
        } else {
            rate_updated_at
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    window::run();
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        account::{
            auth_plan_type, auth_subscription_active_until, decode_jwt_payload,
            read_account_status_with, subscription_state,
        },
        provider::{
            cached_rate_limits, discard_expired_rate_cache, first_existing_path,
            parse_rate_limits_response, rate_cache_status, rate_retry_delay_secs,
            read_account_usage_with, read_rate_limits_with, recv_response, RATE_CACHE_MAX_AGE_SECS,
        },
        state::{AccountCache, AccountUsageCache, RateCache, RateRetry, RateState},
        storage::{clear_token_data, sums},
        token::{scan_file, select_token_snapshot, token_coverage, LocalTokenUsage},
    };
    use chrono::DateTime;
    use rusqlite::Connection;
    use std::{
        fs,
        io::Write,
        sync::{mpsc, Mutex},
        time::Duration,
    };
    use tempfile::tempdir;

    #[test]
    fn first_existing_path_prefers_the_first_real_candidate() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.cmd");
        let first = dir.path().join("first.cmd");
        let second = dir.path().join("second.exe");
        std::fs::write(&first, "").unwrap();
        std::fs::write(&second, "").unwrap();
        assert_eq!(
            first_existing_path(vec![missing, first.clone(), second]),
            Some(first)
        );
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        storage::initialize(&c).unwrap();
        c
    }
    fn line(total: i64, inc: i64) -> String {
        serde_json::json!({"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }
    fn line_at(total: i64, inc: i64, timestamp: &str) -> String {
        serde_json::json!({"timestamp":timestamp,"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total},"last_token_usage":{"total_tokens":inc}}}}).to_string()
    }

    fn live_window() -> RateWindow {
        RateWindow {
            label: "5 小时".into(),
            used_percent: Some(20.0),
            remaining_percent: Some(80.0),
            resets_at: None,
            window_minutes: Some(300),
        }
    }

    fn point(day: &str, tokens: i64) -> DailyPoint {
        DailyPoint {
            day: day.into(),
            tokens,
        }
    }
    #[test]
    fn aggregation_uses_cumulative_total_not_last_snapshot_field() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        let event_day = Local::now().date_naive().to_string();
        fs::write(
            &p,
            format!(
                "{}\n{}\n",
                line_at(100, 100, &format!("{event_day}T10:00:00Z")),
                line_at(150, 999, &format!("{event_day}T10:01:00Z"))
            ),
        )
        .unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 30).unwrap(), 150);
    }
    #[test]
    fn duplicate_snapshot_is_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n{}\n", line(10, 10), line(10, 0))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 10)
    }
    #[test]
    fn counter_reset_uses_new_total() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::write(&p, format!("{}\n{}\n", line(100, 100), line(3, 3))).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 103)
    }
    #[test]
    fn second_scan_and_append_are_idempotent() {
        let d = tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, format!("{}\n", line(4, 4))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(format!("{}\n", line(9, 5)).as_bytes())
            .unwrap();
        scan_file(&c, &p).unwrap();
        scan_file(&c, &p).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 9)
    }
    #[test]
    fn event_timestamp_controls_the_daily_bucket() {
        let d = tempdir().unwrap();
        let p = d.path().join("private-session.jsonl");
        let timestamp = "2026-08-10T23:30:00Z";
        fs::write(&p, format!("{}\n", line_at(12, 12, timestamp))).unwrap();
        let c = db();
        scan_file(&c, &p).unwrap();
        let expected_day = DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .with_timezone(&Local)
            .date_naive()
            .to_string();
        let (stored_day, tokens): (String, i64) = c
            .query_row("SELECT day,tokens FROM daily", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((stored_day, tokens), (expected_day, 12));
        let stored_file_id: String = c
            .query_row("SELECT file_id FROM cursors", [], |row| row.get(0))
            .unwrap();
        assert!(!stored_file_id.contains("private-session"));
    }

    #[test]
    fn token_coverage_reports_unavailable_sources() {
        assert_eq!(token_coverage(false, true), "unavailable");
        assert_eq!(token_coverage(true, false), "unavailable");
        assert_eq!(token_coverage(true, true), "partial");
    }

    #[test]
    fn recv_response_filters_by_id() {
        let (sender, receiver) = mpsc::channel();
        sender.send(serde_json::json!({"id": 99})).unwrap();
        sender
            .send(serde_json::json!({"id": 2, "result": {}}))
            .unwrap();
        let response = recv_response(&receiver, 2, Duration::from_millis(50)).unwrap();
        assert_eq!(response.get("id").and_then(|value| value.as_i64()), Some(2));
    }

    #[test]
    fn recv_response_times_out_without_matching_id() {
        let (_sender, receiver) = mpsc::channel();
        assert!(recv_response(&receiver, 2, Duration::from_millis(10)).is_none());
    }

    #[test]
    fn cache_is_available_until_fifteen_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS - 1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_some());
    }

    #[test]
    fn cache_expires_after_fifteen_minutes() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS + 1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_none());
    }

    #[test]
    fn rate_cache_status_changes_after_three_and_fifteen_minutes() {
        let now = Local::now();
        let cache_with_age = |seconds| RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(seconds),
        };
        assert_eq!(
            rate_cache_status(Some(&cache_with_age(0)), now),
            Some("live")
        );
        assert_eq!(
            rate_cache_status(Some(&cache_with_age(180)), now),
            Some("stale")
        );
        assert_eq!(rate_cache_status(Some(&cache_with_age(901)), now), None);
    }

    #[test]
    fn rate_limits_parser_rejects_protocol_errors_and_empty_snapshots() {
        assert!(parse_rate_limits_response(
            &serde_json::json!({"id":2,"error":{"code":"unsupported"}})
        )
        .is_err());
        assert!(parse_rate_limits_response(
            &serde_json::json!({"id":2,"result":{"rateLimits":{}}})
        )
        .is_err());
    }

    #[test]
    fn rate_limits_parser_keeps_all_windows_and_sorts_by_duration() {
        let response = serde_json::json!({
            "id": 2,
            "result": {"rateLimits": {
                "primary": {"usedPercent": 50.0, "windowDurationMins": 300},
                "secondary": {"usedPercent": 10.0, "windowDurationMins": 10080},
                "tertiary": {"usedPercent": 80.0, "windowDurationMins": 60}
            }}
        });
        let windows = parse_rate_limits_response(&response).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(
            windows
                .iter()
                .map(|window| window.window_minutes)
                .collect::<Vec<_>>(),
            vec![Some(60), Some(300), Some(10080)]
        );
    }

    #[test]
    fn rate_retry_delay_uses_five_fifteen_then_thirty_seconds() {
        assert_eq!(rate_retry_delay_secs(0), 0);
        assert_eq!(rate_retry_delay_secs(1), 5);
        assert_eq!(rate_retry_delay_secs(2), 15);
        assert_eq!(rate_retry_delay_secs(3), 30);
        assert_eq!(rate_retry_delay_secs(4), 30);
    }

    #[test]
    fn expired_rate_cache_is_removed_from_memory_and_database() {
        let now = Local::now();
        let cache = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now - chrono::Duration::seconds(RATE_CACHE_MAX_AGE_SECS + 1),
        };
        let c = db();
        storage::save_rate_cache(&c, &cache.windows, cache.fetched_at).unwrap();
        let state = Mutex::new(RateState {
            cache: Some(cache),
            retry: RateRetry::default(),
        });
        let db = Mutex::new(c);

        discard_expired_rate_cache(&state, &db, now);

        assert!(state.lock().unwrap().cache.is_none());
        let remaining: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM quota_cache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn concurrent_forced_rate_refreshes_share_one_provider_call() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let state = Arc::new(Mutex::new(RateState {
            cache: None,
            retry: RateRetry::default(),
        }));
        let refresh = Arc::new(Mutex::new(()));
        let db = Arc::new(Mutex::new(db()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let state = Arc::clone(&state);
            let refresh = Arc::clone(&refresh);
            let db = Arc::clone(&db);
            let calls = Arc::clone(&calls);
            workers.push(std::thread::spawn(move || {
                read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    (vec![live_window()], "live".into())
                })
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap().1, "live");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_rate_refresh_ignores_a_fresh_cache() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Mutex::new(RateState {
            cache: Some(RateCache {
                windows: vec![live_window()],
                fetched_at: Local::now(),
            }),
            retry: RateRetry::default(),
        });
        let refresh = Mutex::new(());
        let db = Mutex::new(db());
        let calls = AtomicUsize::new(0);

        let (_, status, _, _) =
            read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                (vec![live_window()], "live".into())
            });

        assert_eq!(status, "live");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_rate_refresh_bypasses_retry_delay() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = Mutex::new(RateState {
            cache: None,
            retry: RateRetry {
                last_attempt: Some(Local::now()),
                consecutive_failures: 3,
            },
        });
        let refresh = Mutex::new(());
        let db = Mutex::new(db());
        let calls = AtomicUsize::new(0);

        let (_, status, _, _) =
            read_rate_limits_with(&state, &refresh, &db, RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                (vec![live_window()], "live".into())
            });

        assert_eq!(status, "live");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn forced_account_status_preserves_the_last_success_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let previous = AccountStatus {
            bound: true,
            plan_type: Some("Plus".into()),
            subscription_active_until: Some("2099-01-01T00:00:00Z".into()),
            subscription_state: "active".into(),
            subscription_error: None,
            checked_at: Some("2098-01-01T00:00:00Z".into()),
        };
        let cache = Mutex::new(Some(AccountCache {
            cache_key: "account".into(),
            status: previous,
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let status =
            read_account_status_with(&cache, &refresh, "account", RefreshMode::Force, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Err("network unavailable".into())
            });

        assert!(status.bound);
        assert_eq!(status.plan_type.as_deref(), Some("Plus"));
        assert_eq!(
            status.subscription_error.as_deref(),
            Some("network unavailable")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_account_status_reuses_a_fresh_value() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountCache {
            cache_key: "account".into(),
            status: AccountStatus {
                bound: true,
                plan_type: Some("Plus".into()),
                subscription_active_until: None,
                subscription_state: "unknown".into(),
                subscription_error: None,
                checked_at: None,
            },
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let status =
            read_account_status_with(&cache, &refresh, "account", RefreshMode::Cached, || {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(AccountStatus::default())
            });

        assert_eq!(status.plan_type.as_deref(), Some("Plus"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn forced_account_usage_ignores_a_fresh_cache() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountUsageCache {
            daily: vec![point("2026-10-09", 10)],
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let daily = read_account_usage_with(&cache, &refresh, RefreshMode::Force, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(vec![point("2026-10-09", 20)])
        })
        .unwrap();

        assert_eq!(daily[0].tokens, 20);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cached_account_usage_reuses_a_fresh_value() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let cache = Mutex::new(Some(AccountUsageCache {
            daily: vec![point("2026-10-09", 10)],
            fetched_at: Local::now(),
        }));
        let refresh = Mutex::new(());
        let calls = AtomicUsize::new(0);

        let daily = read_account_usage_with(&cache, &refresh, RefreshMode::Cached, || {
            calls.fetch_add(1, Ordering::SeqCst);
            Some(vec![point("2026-10-09", 20)])
        })
        .unwrap();

        assert_eq!(daily[0].tokens, 10);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_forced_account_usage_refreshes_share_one_provider_call() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let cache = Arc::new(Mutex::new(None));
        let refresh = Arc::new(Mutex::new(()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let cache = Arc::clone(&cache);
            let refresh = Arc::clone(&refresh);
            let calls = Arc::clone(&calls);
            workers.push(std::thread::spawn(move || {
                read_account_usage_with(&cache, &refresh, RefreshMode::Force, || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(80));
                    Some(vec![point("2026-10-09", 20)])
                })
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap()[0].tokens, 20);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn account_history_with_local_today_prefers_local_today_and_recalculates_totals() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30), point("2026-10-09", 70)]),
            local,
            today,
        );

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_029);
        assert_eq!(selected.last30, 1_029);
        assert_eq!(selected.daily.last().unwrap().tokens, 999);
        assert_eq!(selected.usage_source, "account_history_local_today");
        assert_eq!(selected.token_coverage, "account_history_local_today");
        assert!(selected.today_available);
    }

    #[test]
    fn account_history_without_today_uses_local_today() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(Some(vec![point("2026-10-08", 30)]), local, today);

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_029);
        assert_eq!(selected.last30, 1_029);
        assert_eq!(selected.usage_source, "account_history_local_today");
        assert_eq!(selected.token_coverage, "account_history_local_today");
        assert!(selected.today_available);
    }

    #[test]
    fn unavailable_local_usage_uses_account_today_when_available() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30), point("2026-10-09", 70)]),
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.today, 70);
        assert_eq!(selected.last7, 100);
        assert_eq!(selected.usage_source, "account");
        assert_eq!(selected.token_coverage, "account");
        assert!(selected.today_available);
    }

    #[test]
    fn account_history_without_local_today_marks_today_unavailable() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            Some(vec![point("2026-10-08", 30)]),
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.today, 0);
        assert_eq!(selected.last7, 30);
        assert_eq!(selected.usage_source, "account_history_today_unavailable");
        assert_eq!(selected.token_coverage, "account");
        assert!(!selected.today_available);
    }

    #[test]
    fn unavailable_account_usage_falls_back_to_one_local_group() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let local = LocalTokenUsage {
            today: 999,
            last7: 1_999,
            last30: 2_999,
            daily: vec![point("2026-10-09", 999)],
            coverage: "partial".into(),
        };

        let selected = select_token_snapshot(None, local, today);

        assert_eq!(selected.today, 999);
        assert_eq!(selected.last7, 1_999);
        assert_eq!(selected.last30, 2_999);
        assert_eq!(selected.usage_source, "local");
        assert_eq!(selected.token_coverage, "partial");
        assert!(selected.today_available);
    }

    #[test]
    fn unavailable_local_usage_marks_today_unavailable() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        let selected = select_token_snapshot(
            None,
            LocalTokenUsage {
                today: 0,
                last7: 0,
                last30: 0,
                daily: Vec::new(),
                coverage: "unavailable".into(),
            },
            today,
        );

        assert_eq!(selected.usage_source, "local");
        assert_eq!(selected.token_coverage, "unavailable");
        assert!(!selected.today_available);
    }

    #[test]
    fn future_cache_timestamp_is_not_accepted() {
        let now = Local::now();
        let cached = RateCache {
            windows: vec![RateWindow::default()],
            fetched_at: now + chrono::Duration::seconds(1),
        };
        assert!(cached_rate_limits(Some(&cached), now).is_none());
    }

    #[test]
    fn subscription_state_uses_subscription_date_not_token_expiry() {
        assert_eq!(subscription_state(Some("2099-01-01T00:00:00Z")), "active");
        assert_eq!(subscription_state(Some("2000-01-01T00:00:00Z")), "expired");
        assert_eq!(subscription_state(Some("not-a-date")), "unknown");
        assert_eq!(subscription_state(None), "unknown");
    }

    #[test]
    fn account_status_serialization_never_exposes_email() {
        let value = serde_json::to_value(AccountStatus::default()).unwrap();
        assert!(value.get("email").is_none());
    }

    #[test]
    fn account_claims_keep_plan_and_subscription_date_separate() {
        let payload = serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "2026-10-01T00:00:00Z",
                "chatgpt_subscription_active_until": "2026-10-01T00:00:00Z"
            }
        });
        assert_eq!(auth_plan_type(&payload), None);
        assert_eq!(
            auth_subscription_active_until(&payload),
            Some("2026-10-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn jwt_payload_decoder_rejects_malformed_tokens() {
        assert!(decode_jwt_payload("not-a-jwt").is_none());
        assert!(decode_jwt_payload("a.b.c").is_none());
    }

    #[test]
    fn clear_token_data_removes_events_daily_and_cursors() {
        let c = db();
        c.execute("INSERT INTO daily(day,tokens) VALUES('2026-01-01',10)", [])
            .unwrap();
        c.execute(
            "INSERT INTO cursors(file_id,file_hash,offset,last_total) VALUES('id','hash',1,1)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO events(event_id,file_id,event_offset,total_tokens,delta_tokens,day) VALUES('e','id',0,1,1,'2026-01-01')", []).unwrap();
        clear_token_data(&c).unwrap();
        for table in ["daily", "events"] {
            let count: i64 = c
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0, "table {table} not cleared");
        }
        let cursors: i64 = c
            .query_row("SELECT COUNT(*) FROM cursors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cursors, 1, "scan baselines must survive a statistics clear");
    }

    #[test]
    fn clearing_statistics_only_counts_tokens_added_after_the_clear() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_sessions(&c, dir.path()).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 100);

        clear_token_data(&c).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", line(150, 50)).as_bytes())
            .unwrap();
        scan_sessions(&c, dir.path()).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 50);
    }

    #[test]
    fn session_scan_does_not_retain_data_older_than_thirty_days() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("old.jsonl");
        let old_day = (Local::now().date_naive() - chrono::Days::new(40)).to_string();
        fs::write(
            &path,
            format!("{}\n", line_at(12, 12, &format!("{old_day}T10:00:00Z"))),
        )
        .unwrap();
        let c = db();

        scan_sessions(&c, dir.path()).unwrap();

        let daily_count: i64 = c
            .query_row("SELECT COUNT(*) FROM daily", [], |row| row.get(0))
            .unwrap();
        let event_count: i64 = c
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(daily_count, 0);
        assert_eq!(event_count, 0);
    }

    #[test]
    fn session_scan_retries_an_incomplete_trailing_json_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let complete = line(20, 20);
        let split = 12;
        fs::write(&path, format!("{}\n{}", line(10, 10), &complete[..split])).unwrap();
        let c = db();
        scan_file(&c, &path).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 10);

        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(format!("{}\n", &complete[split..]).as_bytes())
            .unwrap();
        scan_file(&c, &path).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 20);
    }

    #[test]
    fn session_scan_rebuilds_a_truncated_file_without_double_counting() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        fs::write(&path, format!("{}\n", line(100, 100))).unwrap();
        let c = db();
        scan_file(&c, &path).unwrap();
        assert_eq!(sums(&c, 1).unwrap(), 100);

        fs::write(&path, format!("{}\n", line(7, 7))).unwrap();
        scan_file(&c, &path).unwrap();

        assert_eq!(sums(&c, 1).unwrap(), 7);
    }
}
