//! Exact provider counters use decimal text; canonical JSON numbers remain bounded.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Lifetime totals can exceed one provider's u64 counter without losing JSON identity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecimalU128(u128);

impl DecimalU128 {
    pub const fn get(self) -> u128 {
        self.0
    }
}
impl From<u128> for DecimalU128 {
    fn from(value: u128) -> Self {
        Self(value)
    }
}
impl Serialize for DecimalU128 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for DecimalU128 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || value.len() > 39
            || !value.bytes().all(|b| b.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(serde::de::Error::custom(
                "Expected canonical u128 decimal text",
            ));
        }
        value
            .parse::<u128>()
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DecimalU64(u64);

impl DecimalU64 {
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl From<u64> for DecimalU64 {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for DecimalU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty()
            || value.len() > 20
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(serde::de::Error::custom(
                "Expected canonical u64 decimal text",
            ));
        }
        value
            .parse::<u64>()
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

pub const TASK_TOKEN_USAGE_V3: &str = "af/TaskTokenUsage@3";

pub const TASK_USAGE_OBSERVATION_V1: &str = "af/TaskUsageObservation@1";

/// Native counters and their unambiguous billing floor, independent of the common charged
/// amount. Incomplete reporting never grants another reservation or enlarges any limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUsageObservationV1 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub reported_usage: Option<TaskTokenUsageV3>,
    pub charge_complete: bool,
}

impl TaskUsageObservationV1 {
    pub fn validate(&self) -> Result<(), String> {
        super::require(
            !self.charge_complete || self.reported_usage.is_some(),
            "Complete charge observation requires reported usage, including known zero",
        )
    }

    /// Observations are cumulative, not additive spend. No current contract authorizes a
    /// later observation to erase uncertainty or a previously reported counter/floor.
    pub fn merge_previous(&mut self, previous: &Self) {
        self.charge_complete &= previous.charge_complete;
        if let Some(previous) = &previous.reported_usage {
            match &mut self.reported_usage {
                None => self.reported_usage = Some(previous.clone()),
                Some(current) => {
                    current.input_tokens = current.input_tokens.max(previous.input_tokens);
                    current.output_tokens = current.output_tokens.max(previous.output_tokens);
                    current.cache_read_tokens =
                        current.cache_read_tokens.max(previous.cache_read_tokens);
                    current.cache_write_tokens =
                        current.cache_write_tokens.max(previous.cache_write_tokens);
                    current.reasoning_tokens =
                        current.reasoning_tokens.max(previous.reasoning_tokens);
                    current.chargeable_tokens =
                        current.chargeable_tokens.max(previous.chargeable_tokens);
                }
            }
        }
    }
}

/// One native invocation can contain many turns. Both its components and charge are exact
/// cumulative counters; the captured reservation remains a separate, unchanged u64 limit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTokenUsageV3 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub input_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub output_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_read_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_write_tokens: Option<DecimalU128>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub reasoning_tokens: Option<DecimalU128>,
    pub chargeable_tokens: DecimalU128,
}

impl TaskTokenUsageV3 {
    pub fn charge_only(chargeable_tokens: u128) -> Self {
        Self {
            chargeable_tokens: chargeable_tokens.into(),
            ..Self::default()
        }
    }
}
