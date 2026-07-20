use crate::adaptor::ChannelConfig;
use crate::db::models::Channel;

pub struct Dispatcher;

impl Dispatcher {
    /// Build an ordered failover queue based on priority, weight, and model support
    pub fn select_channels(channels: &[Channel], requested_model: &str) -> Vec<Channel> {
        let mut candidates: Vec<Channel> = channels
            .iter()
            .filter(|c| {
                if c.status != 1 {
                    return false;
                }
                let models: Vec<String> = serde_json::from_str(&c.models).unwrap_or_default();
                if models.is_empty() || models.iter().any(|m| m == requested_model) {
                    return true;
                }
                // Also check model_mapping keys — mapped model names are also accepted
                let mapping: serde_json::Value =
                    serde_json::from_str(&c.model_mapping).unwrap_or(serde_json::Value::Object(Default::default()));
                if let Some(obj) = mapping.as_object() {
                    return obj.contains_key(requested_model);
                }
                false
            })
            .cloned()
            .collect();

        if candidates.is_empty() {
            return Vec::new();
        }

        candidates.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.weight.cmp(&a.weight)));

        let mut ordered = Vec::with_capacity(candidates.len());
        let mut start = 0;

        while start < candidates.len() {
            let priority = candidates[start].priority;
            let mut end = start;
            while end < candidates.len() && candidates[end].priority == priority {
                end += 1;
            }

            let mut group = candidates[start..end].to_vec();
            let mut rng = rand::rng();

            while !group.is_empty() {
                let total_weight: i64 = group.iter().map(|c| c.weight.max(0)).sum();
                let index = if total_weight > 0 {
                    let mut point = rand::Rng::random_range(&mut rng, 0..total_weight);
                    let mut selected = 0;
                    for (idx, channel) in group.iter().enumerate() {
                        point -= channel.weight.max(0);
                        if point < 0 {
                            selected = idx;
                            break;
                        }
                    }
                    selected
                } else {
                    0
                };

                ordered.push(group.remove(index));
            }

            start = end;
        }

        ordered
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
