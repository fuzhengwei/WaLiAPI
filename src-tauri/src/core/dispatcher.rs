use crate::adaptor::ChannelConfig;
use crate::db::models::Channel;

pub struct Dispatcher;

impl Dispatcher {
    /// Select a channel for the request based on priority, weight, and model support
    pub fn select_channel(channels: &[Channel], requested_model: &str) -> Option<Channel> {
        // Filter channels that support the requested model
        let mut candidates: Vec<&Channel> = channels
            .iter()
            .filter(|c| {
                if c.status != 1 {
                    return false;
                }
                let models: Vec<String> = serde_json::from_str(&c.models).unwrap_or_default();
                if models.iter().any(|m| m == requested_model) {
                    return true;
                }
                // Also check model mapping aliases
                let mapping: serde_json::Value = serde_json::from_str(&c.model_mapping).unwrap_or(serde_json::Value::Null);
                if let Some(obj) = mapping.as_object() {
                    return obj.contains_key(requested_model);
                }
                false
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Sort by priority (desc), then by weight (desc)
        candidates.sort_by(|a, b| {
            b.priority.cmp(&a.priority).then(b.weight.cmp(&a.weight))
        });

        // Simple weighted random selection among top priority channels
        let max_priority = candidates[0].priority;
        let top_candidates: Vec<&Channel> = candidates
            .iter()
            .filter(|c| c.priority == max_priority)
            .copied()
            .collect();

        let total_weight: i64 = top_candidates.iter().map(|c| c.weight).sum();
        if total_weight <= 0 {
            return Some(top_candidates[0].clone());
        }

        let mut rng = rand::rng();
        let mut point = rand::Rng::random_range(&mut rng, 0..total_weight);
        for c in &top_candidates {
            point -= c.weight;
            if point < 0 {
                return Some((*c).clone());
            }
        }

        Some(top_candidates[0].clone())
    }

    pub fn channel_to_config(channel: &Channel) -> ChannelConfig {
        let models: Vec<String> = serde_json::from_str(&channel.models).unwrap_or_default();
        let model_mapping: serde_json::Value = serde_json::from_str(&channel.model_mapping).unwrap_or(serde_json::Value::Object(Default::default()));
        let extra: serde_json::Value = serde_json::from_str(&channel.config).unwrap_or(serde_json::Value::Object(Default::default()));

        ChannelConfig {
            base_url: channel.base_url.clone(),
            api_key: channel.api_key.clone(),
            models,
            model_mapping,
            extra,
        }
    }
}
