//! Exact provider counters use decimal text; canonical JSON numbers remain bounded.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const TASK_TOKEN_USAGE_V1: &str = "af/TaskTokenUsage@1";

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTokenUsageV1 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub input_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub output_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_read_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_write_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub reasoning_tokens: Option<DecimalU64>,
    pub chargeable_tokens: DecimalU64,
}

pub const TASK_TOKEN_USAGE_V2: &str = "af/TaskTokenUsage@2";

/// Exact cumulative charge for one Attempt; optional native counters retain their own range.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTokenUsageV2 {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub input_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub output_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_read_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache_write_tokens: Option<DecimalU64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub reasoning_tokens: Option<DecimalU64>,
    pub chargeable_tokens: DecimalU128,
}

impl From<TaskTokenUsageV1> for TaskTokenUsageV2 {
    fn from(value: TaskTokenUsageV1) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            reasoning_tokens: value.reasoning_tokens,
            chargeable_tokens: u128::from(value.chargeable_tokens.get()).into(),
        }
    }
}

pub const TASK_TOKEN_USAGE_V3: &str = "af/TaskTokenUsage@3";

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

impl From<TaskTokenUsageV2> for TaskTokenUsageV3 {
    fn from(value: TaskTokenUsageV2) -> Self {
        let widen = |n: DecimalU64| u128::from(n.get()).into();
        Self {
            input_tokens: value.input_tokens.map(widen),
            output_tokens: value.output_tokens.map(widen),
            cache_read_tokens: value.cache_read_tokens.map(widen),
            cache_write_tokens: value.cache_write_tokens.map(widen),
            reasoning_tokens: value.reasoning_tokens.map(widen),
            chargeable_tokens: value.chargeable_tokens,
        }
    }
}

impl From<TaskTokenUsageV1> for TaskTokenUsageV3 {
    fn from(value: TaskTokenUsageV1) -> Self {
        TaskTokenUsageV2::from(value).into()
    }
}

impl TryFrom<&TaskTokenUsageV3> for TaskTokenUsageV2 {
    type Error = String;
    fn try_from(value: &TaskTokenUsageV3) -> Result<Self, Self::Error> {
        let narrow = |n: DecimalU128| {
            u64::try_from(n.get())
                .map(Into::into)
                .map_err(|_| "Task usage component exceeds the frozen u64 range".to_owned())
        };
        Ok(Self {
            input_tokens: value.input_tokens.map(narrow).transpose()?,
            output_tokens: value.output_tokens.map(narrow).transpose()?,
            cache_read_tokens: value.cache_read_tokens.map(narrow).transpose()?,
            cache_write_tokens: value.cache_write_tokens.map(narrow).transpose()?,
            reasoning_tokens: value.reasoning_tokens.map(narrow).transpose()?,
            chargeable_tokens: value.chargeable_tokens,
        })
    }
}

impl TryFrom<&TaskTokenUsageV3> for TaskTokenUsageV1 {
    type Error = String;
    fn try_from(value: &TaskTokenUsageV3) -> Result<Self, Self::Error> {
        let value = TaskTokenUsageV2::try_from(value)?;
        Ok(Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cache_read_tokens: value.cache_read_tokens,
            cache_write_tokens: value.cache_write_tokens,
            reasoning_tokens: value.reasoning_tokens,
            chargeable_tokens: u64::try_from(value.chargeable_tokens.get())
                .map_err(|_| "Task usage charge exceeds the frozen u64 range".to_owned())?
                .into(),
        })
    }
}
