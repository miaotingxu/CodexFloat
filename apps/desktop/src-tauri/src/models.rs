use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RateWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub window_minutes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub expanded: bool,
    pub today: i64,
    pub last7: i64,
    pub last30: i64,
    pub daily: Vec<DailyPoint>,
    pub windows: Vec<RateWindow>,
    pub account: AccountStatus,
    pub rate_status: String,
    pub rate_freshness: String,
    pub token_coverage: String,
    pub usage_source: String,
    pub today_available: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyPoint {
    pub day: String,
    pub tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub bound: bool,
    pub plan_type: Option<String>,
    pub subscription_active_until: Option<String>,
    pub subscription_state: String,
    pub subscription_error: Option<String>,
    pub checked_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{AccountStatus, Snapshot};

    #[test]
    fn snapshot_keeps_camel_case_renderer_fields() {
        let encoded = serde_json::to_value(Snapshot {
            expanded: false,
            today: 0,
            last7: 0,
            last30: 0,
            daily: vec![],
            windows: vec![],
            account: AccountStatus::default(),
            rate_status: "unavailable".into(),
            rate_freshness: "unavailable".into(),
            token_coverage: "unavailable".into(),
            usage_source: "local".into(),
            today_available: false,
            updated_at: String::new(),
        })
        .unwrap();
        assert!(encoded.get("rateStatus").is_some());
        assert!(encoded.get("todayAvailable").is_some());
        assert!(encoded.get("usage_source").is_none());
    }
}
