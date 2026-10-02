//! Kernel-observed measurements and their deterministic comparison (ADR-0132).
//!
//! A measurement is a declared command from the committed code policy, run by the installed
//! `measure` operator a declared number of times against a read-only Snapshot. Its wall time,
//! exit status and output digests are the kernel's own observations; the only values the
//! command contributes are the metrics it prints on its last stdout line, each with the unit the
//! policy declared for it. A comparison is a pure fold over two such artifacts under a declared
//! objective, in exact decimal arithmetic: nothing is saturated, clamped or rounded.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer;
use num_traits::{One, Signed, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::pipeline::ReceiptOutcomeV1;
use super::{is_name, require};
use crate::exec::Command;

pub const MEASUREMENT_V1: &str = "af/Measurement@1";
pub const MEASUREMENT_COMPARISON_V1: &str = "af/MeasurementComparison@1";
/// The schema of the one line a measured command may print last on stdout.
pub const MEASURE_REPORT_V1: &str = "af.measure-report/1";
/// The built-in metric: the kernel's own wall-clock observation of each repetition.
pub const ELAPSED_MS: &str = "elapsed_ms";
pub const MAX_REPETITIONS: u32 = 16;
pub const MAX_REPETITION_WALL_MS: u64 = 3_600_000;
pub const MAX_DECLARED_METRICS: usize = 32;
/// Reported values carry at most this many significant digits.
pub const MAX_SIGNIFICANT_DIGITS: usize = 38;
/// A report line longer than this is malformed; it is never partially read.
pub const MAX_REPORT_LINE_BYTES: usize = 64 * 1024;
/// Derived values stay exact, but no decimal text of any kind exceeds this length.
const MAX_DECIMAL_TEXT: usize = 4096;
pub const DEFAULT_MIN_REPETITIONS: u32 = 3;

/// A finite, non-negative decimal in canonical text: `0`, or digits without a leading zero
/// before an optional point, and at least one digit without a trailing zero after it. No sign,
/// exponent, `+`, leading point or trailing point. Two values are equal exactly when their text
/// is equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MeasureDecimal {
    mantissa: BigUint,
    scale: u32,
}

impl MeasureDecimal {
    pub fn zero() -> Self {
        Self {
            mantissa: BigUint::zero(),
            scale: 0,
        }
    }

    pub fn from_u64(value: u64) -> Self {
        Self {
            mantissa: BigUint::from(value),
            scale: 0,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let malformed = || format!("{text:?} is not a canonical non-negative decimal");
        if text.is_empty() || text.len() > MAX_DECIMAL_TEXT {
            return Err(malformed());
        }
        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (text, None),
        };
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole) || (whole.len() > 1 && whole.starts_with('0')) {
            return Err(malformed());
        }
        if let Some(fraction) = fraction
            && (!digits(fraction) || fraction.ends_with('0'))
        {
            return Err(malformed());
        }
        let joined = format!("{whole}{}", fraction.unwrap_or(""));
        let mantissa = BigUint::parse_bytes(joined.as_bytes(), 10).ok_or_else(malformed)?;
        Ok(Self {
            mantissa,
            scale: u32::try_from(fraction.map_or(0, str::len)).map_err(|_| malformed())?,
        })
    }

    pub fn is_zero(&self) -> bool {
        self.mantissa.is_zero()
    }

    /// Digits from the first non-zero digit to the last digit of the canonical text.
    pub fn significant_digits(&self) -> usize {
        if self.is_zero() {
            1
        } else {
            self.mantissa.to_str_radix(10).len()
        }
    }

    fn exact(&self) -> Exact {
        Exact {
            value: BigInt::from_biguint(Sign::Plus, self.mantissa.clone()),
            scale: self.scale,
        }
    }
}

impl std::fmt::Display for MeasureDecimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&render(&self.mantissa, self.scale))
    }
}

impl Ord for MeasureDecimal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.exact().cmp_exact(&other.exact())
    }
}

impl PartialOrd for MeasureDecimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Serialize for MeasureDecimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MeasureDecimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

fn render(mantissa: &BigUint, scale: u32) -> String {
    let digits = mantissa.to_str_radix(10);
    let scale = scale as usize;
    if scale == 0 {
        return digits;
    }
    let padded = format!("{digits:0>width$}", width = scale + 1);
    let (whole, fraction) = padded.split_at(padded.len() - scale);
    format!("{whole}.{fraction}")
}

/// A signed decimal in canonical text: a [`MeasureDecimal`] with a leading `-` when negative.
/// Zero is always `0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureSignedDecimal {
    negative: bool,
    magnitude: MeasureDecimal,
}

impl MeasureSignedDecimal {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (negative, rest) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let magnitude = MeasureDecimal::parse(rest)?;
        if negative && magnitude.is_zero() {
            return Err("Negative zero is not canonical".into());
        }
        Ok(Self {
            negative,
            magnitude,
        })
    }

    pub fn signum(&self) -> Ordering {
        if self.magnitude.is_zero() {
            Ordering::Equal
        } else if self.negative {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}

impl std::fmt::Display for MeasureSignedDecimal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        self.magnitude.fmt(f)
    }
}

impl Serialize for MeasureSignedDecimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MeasureSignedDecimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// An exact ratio in lowest terms, written `numerator/denominator` with a positive denominator
/// and the sign on the numerator. `improvement / baseline` rarely terminates as a decimal, so it
/// is kept as the fraction itself rather than rounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeasureRatio {
    numerator: BigInt,
    denominator: BigUint,
}

impl MeasureRatio {
    pub fn parse(text: &str) -> Result<Self, String> {
        let malformed = || format!("{text:?} is not a canonical ratio numerator/denominator");
        if text.len() > 2 * MAX_DECIMAL_TEXT {
            return Err(malformed());
        }
        let (numerator, denominator) = text.split_once('/').ok_or_else(malformed)?;
        let integer = |part: &str| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        };
        let (negative, magnitude) = match numerator.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, numerator),
        };
        if !integer(magnitude) || !integer(denominator) || (negative && magnitude == "0") {
            return Err(malformed());
        }
        let magnitude = BigUint::parse_bytes(magnitude.as_bytes(), 10).ok_or_else(malformed)?;
        let denominator = BigUint::parse_bytes(denominator.as_bytes(), 10).ok_or_else(malformed)?;
        if denominator.is_zero() || !magnitude.gcd(&denominator).is_one() {
            return Err(malformed());
        }
        Ok(Self {
            numerator: BigInt::from_biguint(
                if negative { Sign::Minus } else { Sign::Plus },
                magnitude,
            ),
            denominator,
        })
    }

    fn of(numerator: &Exact, denominator: &Exact) -> Option<Self> {
        if denominator.value.is_zero() {
            return None;
        }
        // n / 10^a divided by d / 10^b is (n * 10^b) / (d * 10^a).
        let mut top = &numerator.value * pow10(denominator.scale);
        let mut bottom = &denominator.value * pow10(numerator.scale);
        if bottom.is_negative() {
            top = -top;
            bottom = -bottom;
        }
        let divisor = top.gcd(&bottom);
        if !divisor.is_zero() {
            top /= &divisor;
            bottom /= &divisor;
        }
        Some(Self {
            numerator: top,
            denominator: bottom.to_biguint().expect("positive denominator"),
        })
    }

    /// Whether this ratio is at least the decimal `threshold`, compared exactly.
    pub fn at_least(&self, threshold: &MeasureDecimal) -> bool {
        let threshold = threshold.exact();
        // p/q >= m/10^s  <=>  p * 10^s >= m * q, with q > 0.
        let left = &self.numerator * pow10(threshold.scale);
        let right = threshold.value * BigInt::from_biguint(Sign::Plus, self.denominator.clone());
        left >= right
    }

    /// The exact decimal expansion when the ratio terminates, otherwise `None`.
    pub fn terminating_decimal(&self) -> Option<String> {
        let mut rest = self.denominator.clone();
        let two = BigUint::from(2u32);
        let five = BigUint::from(5u32);
        let (mut twos, mut fives) = (0u32, 0u32);
        while (&rest % &two).is_zero() {
            rest /= &two;
            twos += 1;
        }
        while (&rest % &five).is_zero() {
            rest /= &five;
            fives += 1;
        }
        if !rest.is_one() {
            return None;
        }
        let scale = twos.max(fives);
        let value = &self.numerator * pow10(scale)
            / BigInt::from_biguint(Sign::Plus, self.denominator.clone());
        Some(
            Exact { value, scale }
                .normalized()
                .signed()
                .ok()?
                .to_string(),
        )
    }
}

impl std::fmt::Display for MeasureRatio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

impl Serialize for MeasureRatio {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for MeasureRatio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

fn pow10(exponent: u32) -> BigInt {
    num_traits::pow(BigInt::from(10u32), exponent as usize)
}

/// Exact working form: `value / 10^scale`.
#[derive(Debug, Clone)]
struct Exact {
    value: BigInt,
    scale: u32,
}

impl Exact {
    fn aligned(&self, other: &Self) -> (BigInt, BigInt, u32) {
        let scale = self.scale.max(other.scale);
        (
            &self.value * pow10(scale - self.scale),
            &other.value * pow10(scale - other.scale),
            scale,
        )
    }

    fn cmp_exact(&self, other: &Self) -> Ordering {
        let (left, right, _) = self.aligned(other);
        left.cmp(&right)
    }

    fn add(&self, other: &Self) -> Self {
        let (left, right, scale) = self.aligned(other);
        Self {
            value: left + right,
            scale,
        }
    }

    fn sub(&self, other: &Self) -> Self {
        let (left, right, scale) = self.aligned(other);
        Self {
            value: left - right,
            scale,
        }
    }

    /// Exactly one half: `x / 2 = 5x / 10`.
    fn half(&self) -> Self {
        Self {
            value: &self.value * BigInt::from(5u32),
            scale: self.scale + 1,
        }
    }

    fn normalized(mut self) -> Self {
        if self.value.is_zero() {
            self.scale = 0;
            return self;
        }
        let ten = BigInt::from(10u32);
        while self.scale > 0 && (&self.value % &ten).is_zero() {
            self.value /= &ten;
            self.scale -= 1;
        }
        self
    }

    fn signed(self) -> Result<MeasureSignedDecimal, String> {
        let normalized = self.normalized();
        let negative = normalized.value.is_negative();
        let magnitude = MeasureDecimal {
            mantissa: normalized.value.magnitude().clone(),
            scale: normalized.scale,
        };
        let text = magnitude.to_string();
        if text.len() > MAX_DECIMAL_TEXT {
            return Err("Derived decimal exceeds its text bound".into());
        }
        Ok(MeasureSignedDecimal {
            negative,
            magnitude,
        })
    }

    fn unsigned(self) -> Result<MeasureDecimal, String> {
        let signed = self.signed()?;
        if signed.negative {
            return Err("Derived decimal is negative".into());
        }
        Ok(signed.magnitude)
    }
}

/// The closed vocabulary of units a metric may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnitV1 {
    Ms,
    Bytes,
    Count,
    Ratio,
}

impl MetricUnitV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ms => "ms",
            Self::Bytes => "bytes",
            Self::Count => "count",
            Self::Ratio => "ratio",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ms" => Self::Ms,
            "bytes" => Self::Bytes,
            "count" => Self::Count,
            "ratio" => Self::Ratio,
            _ => return None,
        })
    }
}

/// One metric a measure declares its command reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureMetricV1 {
    pub key: String,
    pub unit: MetricUnitV1,
}

/// `[measures.<name>]` of `af.code-task-policy/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureDefinitionV1 {
    pub command: Command,
    pub repetitions: u32,
    /// `true` binds the Warm Check Cache's `cargo_target` directory as `CARGO_TARGET_DIR`;
    /// `false` binds a fresh directory inside the repetition's runtime directory.
    pub warm: bool,
    /// The wall bound of one repetition.
    pub wall_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<MeasureMetricV1>,
}

impl MeasureDefinitionV1 {
    pub fn validate(&self) -> Result<(), String> {
        require(
            (1..=MAX_REPETITIONS).contains(&self.repetitions),
            "A measure repeats its command 1 to 16 times",
        )?;
        require(
            (1..=MAX_REPETITION_WALL_MS).contains(&self.wall_ms),
            "A measure repetition's wall_ms is 1 to 3600000",
        )?;
        require(
            self.metrics.len() <= MAX_DECLARED_METRICS,
            "A measure declares at most 32 metrics",
        )?;
        let mut keys = BTreeSet::new();
        for metric in &self.metrics {
            require(is_name(&metric.key), "A measure metric key is a name")?;
            if metric.key == ELAPSED_MS {
                return Err(
                    "elapsed_ms is the built-in metric; a declared metric may not reuse its key"
                        .into(),
                );
            }
            require(
                keys.insert(metric.key.as_str()),
                "A measure declares each metric key once",
            )?;
        }
        self.command
            .resolve()
            .map_err(|error| format!("A measure command does not resolve: {error}"))?;
        Ok(())
    }

    /// The declared metrics, without the built-in one.
    pub fn declared(&self) -> BTreeMap<String, MetricUnitV1> {
        self.metrics
            .iter()
            .map(|metric| (metric.key.clone(), metric.unit))
            .collect()
    }

    /// Every metric a measurement of this measure summarizes: the built-in `elapsed_ms` in
    /// `ms`, and each declared one.
    pub fn summarized(&self) -> BTreeMap<String, MetricUnitV1> {
        let mut all = self.declared();
        all.insert(ELAPSED_MS.into(), MetricUnitV1::Ms);
        all
    }

    /// The repetition budget the plan compiler fits into the captured `check_wall_ms`.
    pub fn budget_ms(&self) -> Option<u64> {
        u64::from(self.repetitions).checked_mul(self.wall_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveDirectionV1 {
    Lower,
    Higher,
}

/// The minimum improvement ratio of an objective: canonical decimal text (`"0.1"`) or the
/// integer 0 or 1. A TOML or JSON float is refused, because the parser has rounded it to a
/// binary fraction before the kernel sees it, and a threshold that is compared exactly cannot
/// be captured from an approximation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ImprovementRatio(pub MeasureDecimal);

impl<'de> Deserialize<'de> for ImprovementRatio {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Written {
            Text(String),
            Integer(u64),
            Number(f64),
        }
        let value = match Written::deserialize(deserializer)? {
            Written::Text(text) => MeasureDecimal::parse(&text),
            Written::Integer(value) => Ok(MeasureDecimal::from_u64(value)),
            Written::Number(value) => Err(format!(
                "min_improvement_ratio {value} is a float, which the parser has already rounded; \
                 write it as decimal text, for example \"0.1\""
            )),
        }
        .map_err(serde::de::Error::custom)?;
        Ok(Self(value))
    }
}

fn default_min_repetitions() -> u32 {
    DEFAULT_MIN_REPETITIONS
}

/// `[objectives.<name>]` of `af.code-task-policy/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureObjectiveV1 {
    pub measure: String,
    pub metric: String,
    pub direction: ObjectiveDirectionV1,
    pub min_improvement_ratio: ImprovementRatio,
    #[serde(default = "default_min_repetitions")]
    pub min_repetitions: u32,
}

impl MeasureObjectiveV1 {
    pub fn validate(&self, measures: &BTreeMap<String, MeasureDefinitionV1>) -> Result<(), String> {
        let measure = measures.get(&self.measure).ok_or_else(|| {
            format!(
                "An objective names measure {:?}, which the policy does not declare",
                self.measure
            )
        })?;
        require(
            measure.summarized().contains_key(&self.metric),
            "An objective names a metric its measure does not record",
        )?;
        require(
            self.min_improvement_ratio.0 <= MeasureDecimal::from_u64(1),
            "An objective's min_improvement_ratio is 0 to 1 inclusive",
        )?;
        require(
            (1..=MAX_REPETITIONS).contains(&self.min_repetitions),
            "An objective's min_repetitions is 1 to 16",
        )
    }
}

/// One value a command reported, with the unit it named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricValueV1 {
    pub value: MeasureDecimal,
    pub unit: MetricUnitV1,
}

/// Why a measurement failed. A failed measurement records the repetition that failed and no
/// later one; it never carries a summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementFailureReasonV1 {
    /// The command exited non-zero, was ended by a signal or could not start.
    Exit,
    /// The repetition exceeded its declared `wall_ms`.
    Timeout,
    /// The measure Attempt's deadline cut the repetition, or left no time to start it.
    Deadline,
    /// Declared metrics were not reported on a well-formed `af.measure-report/1` last line.
    MalformedReport,
    /// A reported unit differs from the declared unit.
    UnitMismatch,
    /// The command changed or added an entry of its read-only source.
    SourceMutated,
}

impl MeasurementFailureReasonV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Timeout => "timeout",
            Self::Deadline => "deadline",
            Self::MalformedReport => "malformed_report",
            Self::UnitMismatch => "unit_mismatch",
            Self::SourceMutated => "source_mutated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementFailureV1 {
    /// One-based index of the repetition that failed; it is the last recorded run.
    pub repetition: u32,
    pub reason: MeasurementFailureReasonV1,
    pub detail: String,
}

/// One repetition as the kernel observed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementRunV1 {
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    /// Absent when the command did not exit: it timed out, was cut or could not start.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub exit_code: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub stdout_id: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub stderr_id: Option<String>,
    /// The cargo target this repetition actually ran against when the measure asked for the
    /// Warm Check Cache; absent for `warm = false`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub cache: Option<MeasurementCacheV1>,
    /// The declared metrics parsed from the report line; empty for a failed repetition.
    pub metrics: BTreeMap<String, MetricValueV1>,
}

/// What the Warm Check Cache gave one repetition. `warm` is the condition the command ran
/// under, not the one the policy asked for: a busy key or a discarded directory runs the
/// repetition against a private cold target, and `reason` says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementCacheV1 {
    pub warm: bool,
    /// Bytes the bound directory held before the repetition; 0 for a cold target.
    pub bytes: u64,
    /// Why the repetition ran cold although the measure asked for the cache; absent when warm.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricSummaryV1 {
    pub unit: MetricUnitV1,
    pub median: MeasureDecimal,
    pub min: MeasureDecimal,
    pub max: MeasureDecimal,
    pub n: u32,
}

/// `af/Measurement@1`: every value here was observed or computed by the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementV1 {
    pub plan_id: String,
    pub policy_id: String,
    pub snapshot_id: String,
    pub measure: String,
    /// Content identity of the resolved command: program and resolved arguments.
    pub command_id: String,
    /// The Warm Check Cache toolchain key the repetitions ran under, when one was resolved.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub toolchain_id: Option<String>,
    /// Whether the measure asked for the Warm Check Cache; each run records what it had.
    pub warm: bool,
    pub repetitions: u32,
    pub wall_ms: u64,
    /// The declared metrics and their units, without the built-in `elapsed_ms`.
    pub metrics: BTreeMap<String, MetricUnitV1>,
    /// `passed` or `failed`; a measurement is never inconclusive and never partial.
    pub outcome: ReceiptOutcomeV1,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub failure: Option<MeasurementFailureV1>,
    pub runs: Vec<MeasurementRunV1>,
    /// Per metric, including `elapsed_ms`: empty exactly when the measurement failed.
    pub summary: BTreeMap<String, MetricSummaryV1>,
}

impl MeasurementV1 {
    pub fn validate(&self) -> Result<(), String> {
        for id in [
            &self.plan_id,
            &self.policy_id,
            &self.snapshot_id,
            &self.command_id,
        ]
        .into_iter()
        .chain(self.toolchain_id.as_ref())
        {
            require(crate::is_digest(id), "Measurement identity is not a digest")?;
        }
        require(
            is_name(&self.measure),
            "Measurement names an invalid measure",
        )?;
        require(
            (1..=MAX_REPETITIONS).contains(&self.repetitions)
                && (1..=MAX_REPETITION_WALL_MS).contains(&self.wall_ms),
            "Measurement repetitions or wall are out of bounds",
        )?;
        require(
            self.metrics.len() <= MAX_DECLARED_METRICS
                && self
                    .metrics
                    .keys()
                    .all(|key| is_name(key) && key != ELAPSED_MS),
            "Measurement declares invalid metrics",
        )?;
        for run in &self.runs {
            for id in run.stdout_id.iter().chain(run.stderr_id.iter()) {
                require(crate::is_digest(id), "Measurement output is not a digest")?;
            }
            require(
                super::safe_number(run.started_unix_ms)
                    && super::safe_number(run.elapsed_ms)
                    && run
                        .cache
                        .as_ref()
                        .is_none_or(|cache| super::safe_number(cache.bytes)),
                "Measurement run numbers exceed the canonical JSON bound",
            )?;
            require(
                run.cache.as_ref().is_none_or(|cache| {
                    cache.warm != cache.reason.as_ref().is_some_and(|r| !r.is_empty())
                }),
                "Measurement run cache names a reason exactly when it ran cold",
            )?;
        }
        match (self.outcome, &self.failure) {
            (ReceiptOutcomeV1::Passed, None) => {
                require(
                    self.runs.len() == self.repetitions as usize,
                    "A passed measurement records every declared repetition",
                )?;
                for run in &self.runs {
                    self.complete(run)?;
                }
                require(
                    self.summary == summarize(&self.metrics, &self.runs)?,
                    "Measurement summary differs from its repetitions",
                )
            }
            (ReceiptOutcomeV1::Failed, Some(failure)) => {
                require(
                    failure.repetition >= 1
                        && failure.repetition <= self.repetitions
                        && self.runs.len() == failure.repetition as usize,
                    "A failed measurement ends at the repetition that failed",
                )?;
                let (last, earlier) = self.runs.split_last().expect("at least one run");
                for run in earlier {
                    self.complete(run)?;
                }
                require(
                    last.metrics.is_empty(),
                    "A failed repetition carries no metrics",
                )?;
                require(
                    self.summary.is_empty(),
                    "A failed measurement carries no summary",
                )
            }
            _ => Err("A measurement is passed without a failure or failed with one".into()),
        }
    }

    fn complete(&self, run: &MeasurementRunV1) -> Result<(), String> {
        require(
            run.exit_code == Some(0) && run.stdout_id.is_some() && run.stderr_id.is_some(),
            "A completed repetition exited zero with retained output",
        )?;
        require(
            run.metrics.len() == self.metrics.len()
                && run.metrics.iter().all(|(key, value)| {
                    self.metrics.get(key) == Some(&value.unit)
                        && value.value.significant_digits() <= MAX_SIGNIFICANT_DIGITS
                }),
            "A completed repetition reports exactly the declared metrics and units",
        )
    }

    /// The summary of one metric, when the measurement passed and records it.
    pub fn metric(&self, key: &str) -> Option<&MetricSummaryV1> {
        self.summary.get(key)
    }
}

/// Median, minimum, maximum and count of every metric over completed repetitions. The median of
/// an even sample is the exact mean of its two middle values.
pub fn summarize(
    declared: &BTreeMap<String, MetricUnitV1>,
    runs: &[MeasurementRunV1],
) -> Result<BTreeMap<String, MetricSummaryV1>, String> {
    let mut all = declared.clone();
    all.insert(ELAPSED_MS.into(), MetricUnitV1::Ms);
    let mut summary = BTreeMap::new();
    for (key, unit) in all {
        let mut values = runs
            .iter()
            .map(|run| {
                if key == ELAPSED_MS {
                    Ok(MeasureDecimal::from_u64(run.elapsed_ms))
                } else {
                    run.metrics
                        .get(&key)
                        .map(|value| value.value.clone())
                        .ok_or_else(|| format!("A repetition lacks metric {key}"))
                }
            })
            .collect::<Result<Vec<_>, String>>()?;
        if values.is_empty() {
            return Err("A summary needs at least one repetition".into());
        }
        values.sort();
        summary.insert(
            key,
            MetricSummaryV1 {
                unit,
                median: median(&values)?,
                min: values[0].clone(),
                max: values[values.len() - 1].clone(),
                n: u32::try_from(values.len()).map_err(|_| "Too many repetitions")?,
            },
        );
    }
    Ok(summary)
}

/// The median of an already sorted, non-empty sample.
pub fn median(sorted: &[MeasureDecimal]) -> Result<MeasureDecimal, String> {
    let n = sorted.len();
    if n == 0 {
        return Err("The median of nothing is undefined".into());
    }
    if n % 2 == 1 {
        return Ok(sorted[n / 2].clone());
    }
    sorted[n / 2 - 1]
        .exact()
        .add(&sorted[n / 2].exact())
        .half()
        .unsigned()
}

/// Why a report line could not supply the declared metrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRefusal {
    pub reason: MeasurementFailureReasonV1,
    pub detail: String,
}

fn malformed(detail: impl Into<String>) -> ReportRefusal {
    ReportRefusal {
        reason: MeasurementFailureReasonV1::MalformedReport,
        detail: detail.into(),
    }
}

/// The declared metrics from a command's stdout. Its last line is the report when it is an
/// `af.measure-report/1` JSON object; such a line must report exactly the declared keys, each
/// with its declared unit and a canonical value of at most 38 significant digits. A measure that
/// declares metrics requires the line; one that declares none accepts any other last line.
pub fn parse_report(
    stdout: &[u8],
    declared: &BTreeMap<String, MetricUnitV1>,
) -> Result<BTreeMap<String, MetricValueV1>, ReportRefusal> {
    let trimmed = stdout.strip_suffix(b"\n").unwrap_or(stdout);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    let line = match trimmed.iter().rposition(|byte| *byte == b'\n') {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    };
    let report = (line.len() <= MAX_REPORT_LINE_BYTES)
        .then(|| serde_json::from_slice::<serde_json::Value>(line).ok())
        .flatten()
        .filter(|value| value.get("schema").and_then(|s| s.as_str()) == Some(MEASURE_REPORT_V1));
    let Some(report) = report else {
        if declared.is_empty() {
            return Ok(BTreeMap::new());
        }
        return Err(malformed(if line.len() > MAX_REPORT_LINE_BYTES {
            format!("the last stdout line exceeds {MAX_REPORT_LINE_BYTES} bytes")
        } else {
            format!("the last stdout line is not an {MEASURE_REPORT_V1} object")
        }));
    };
    let object = report
        .as_object()
        .expect("a report with a schema is an object");
    if object.keys().any(|key| key != "schema" && key != "metrics") {
        return Err(malformed(
            "the report has a field other than schema and metrics",
        ));
    }
    let metrics = object
        .get("metrics")
        .and_then(|value| value.as_object())
        .ok_or_else(|| malformed("the report has no metrics object"))?;
    let reported: BTreeSet<&str> = metrics.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = declared.keys().map(String::as_str).collect();
    if reported != expected {
        return Err(malformed(format!(
            "the report names {reported:?}, the measure declares {expected:?}"
        )));
    }
    let mut values = BTreeMap::new();
    let mut mismatch = None;
    for (key, unit) in declared {
        let entry = metrics[key]
            .as_object()
            .filter(|entry| entry.len() == 2)
            .ok_or_else(|| malformed(format!("metric {key} is not an object of value and unit")))?;
        let text = entry
            .get("value")
            .and_then(|value| value.as_str())
            .ok_or_else(|| malformed(format!("metric {key} has no decimal text value")))?;
        let value = MeasureDecimal::parse(text).map_err(malformed)?;
        if value.significant_digits() > MAX_SIGNIFICANT_DIGITS {
            return Err(malformed(format!(
                "metric {key} has more than {MAX_SIGNIFICANT_DIGITS} significant digits"
            )));
        }
        let written = entry
            .get("unit")
            .and_then(|unit| unit.as_str())
            .ok_or_else(|| malformed(format!("metric {key} has no unit")))?;
        let reported = MetricUnitV1::parse(written)
            .ok_or_else(|| malformed(format!("metric {key} has unknown unit {written:?}")))?;
        if reported != *unit && mismatch.is_none() {
            mismatch = Some(ReportRefusal {
                reason: MeasurementFailureReasonV1::UnitMismatch,
                detail: format!(
                    "metric {key} reported unit {}, the measure declares {}",
                    reported.as_str(),
                    unit.as_str()
                ),
            });
        }
        values.insert(
            key.clone(),
            MetricValueV1 {
                value,
                unit: reported,
            },
        );
    }
    match mismatch {
        Some(refusal) => Err(refusal),
        None => Ok(values),
    }
}

/// How one metric compares under the objective's direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonConclusionV1 {
    /// A strictly positive improvement whose ratio is at least `min_improvement_ratio`.
    Improved,
    /// A strictly positive improvement whose ratio is below `min_improvement_ratio`.
    BelowThreshold,
    /// An improvement of exactly zero, including a zero baseline with a zero candidate.
    Unchanged,
    /// A strictly negative improvement.
    Regressed,
    /// Too few repetitions on either side, a failed measurement, or a zero baseline with a
    /// non-zero candidate.
    Inconclusive,
}

impl ComparisonConclusionV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Improved => "improved",
            Self::BelowThreshold => "below_threshold",
            Self::Unchanged => "unchanged",
            Self::Regressed => "regressed",
            Self::Inconclusive => "inconclusive",
        }
    }

    /// The comparison outcome this conclusion gives when it is the objective's metric.
    pub const fn outcome(self) -> ReceiptOutcomeV1 {
        match self {
            Self::Improved => ReceiptOutcomeV1::Passed,
            Self::BelowThreshold | Self::Unchanged | Self::Regressed => ReceiptOutcomeV1::Failed,
            Self::Inconclusive => ReceiptOutcomeV1::Inconclusive,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricComparisonV1 {
    pub unit: MetricUnitV1,
    /// Absent when that side's measurement failed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub baseline_median: Option<MeasureDecimal>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub candidate_median: Option<MeasureDecimal>,
    /// `baseline − candidate` for `lower`, `candidate − baseline` for `higher`; absent when
    /// either median is.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub improvement: Option<MeasureSignedDecimal>,
    /// `improvement / baseline` in lowest terms; absent with a zero or absent baseline.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::present_option"
    )]
    pub ratio: Option<MeasureRatio>,
    pub baseline_n: u32,
    pub candidate_n: u32,
    pub conclusion: ComparisonConclusionV1,
}

/// `af/MeasurementComparison@1`: a deterministic fold over two measurements of one measure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementComparisonV1 {
    pub plan_id: String,
    pub policy_id: String,
    pub baseline_id: String,
    pub candidate_id: String,
    pub objective: String,
    pub measure: String,
    pub metric: String,
    pub direction: ObjectiveDirectionV1,
    pub min_improvement_ratio: MeasureDecimal,
    pub min_repetitions: u32,
    pub metrics: BTreeMap<String, MetricComparisonV1>,
    /// `passed` only for `improved` on the objective's metric; `failed` for `regressed`,
    /// `unchanged` or `below_threshold`; `inconclusive` otherwise.
    pub outcome: ReceiptOutcomeV1,
}

impl MeasurementComparisonV1 {
    pub fn validate(&self) -> Result<(), String> {
        for id in [
            &self.plan_id,
            &self.policy_id,
            &self.baseline_id,
            &self.candidate_id,
        ] {
            require(crate::is_digest(id), "Comparison identity is not a digest")?;
        }
        require(
            is_name(&self.objective) && is_name(&self.measure) && is_name(&self.metric),
            "Comparison names are invalid",
        )?;
        require(
            self.min_improvement_ratio <= MeasureDecimal::from_u64(1)
                && (1..=MAX_REPETITIONS).contains(&self.min_repetitions),
            "Comparison objective bounds are invalid",
        )?;
        let objective = self
            .metrics
            .get(&self.metric)
            .ok_or("Comparison lacks its objective's metric")?;
        require(
            self.metrics.contains_key(ELAPSED_MS)
                && self.metrics.len() <= MAX_DECLARED_METRICS + 1
                && self.metrics.keys().all(|key| is_name(key)),
            "Comparison metrics are invalid",
        )?;
        require(
            self.outcome == objective.conclusion.outcome(),
            "Comparison outcome differs from its objective metric's conclusion",
        )
    }
}

/// Everything the fold needs about the objective.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonObjective<'a> {
    pub name: &'a str,
    pub objective: &'a MeasureObjectiveV1,
}

/// Compare `candidate` with `baseline` under `objective`. Both must measure the objective's
/// measure with the same declared metrics; every other difference is a result, not an error.
pub fn compare_measurements(
    plan_id: &str,
    policy_id: &str,
    (baseline_id, baseline): (&str, &MeasurementV1),
    (candidate_id, candidate): (&str, &MeasurementV1),
    objective: ComparisonObjective<'_>,
) -> Result<MeasurementComparisonV1, String> {
    let ComparisonObjective { name, objective } = objective;
    baseline.validate()?;
    candidate.validate()?;
    if baseline.measure != objective.measure || candidate.measure != objective.measure {
        return Err(format!(
            "Objective {name} compares measure {}, not {} and {}",
            objective.measure, baseline.measure, candidate.measure
        ));
    }
    if baseline.metrics != candidate.metrics {
        return Err("The two measurements declare different metrics".into());
    }
    let mut units = baseline.metrics.clone();
    units.insert(ELAPSED_MS.into(), MetricUnitV1::Ms);
    if !units.contains_key(&objective.metric) {
        return Err(format!(
            "Objective {name} names metric {}, which the measurements do not record",
            objective.metric
        ));
    }
    let threshold = &objective.min_improvement_ratio.0;
    let mut metrics = BTreeMap::new();
    for (key, unit) in units {
        let side = |measurement: &MeasurementV1| {
            measurement
                .metric(&key)
                .map(|summary| (summary.median.clone(), summary.n))
        };
        let (baseline_side, candidate_side) = (side(baseline), side(candidate));
        let baseline_n = baseline_side.as_ref().map_or(0, |(_, n)| *n);
        let candidate_n = candidate_side.as_ref().map_or(0, |(_, n)| *n);
        let improvement = match (&baseline_side, &candidate_side) {
            (Some((base, _)), Some((cand, _))) => Some(match objective.direction {
                ObjectiveDirectionV1::Lower => base.exact().sub(&cand.exact()),
                ObjectiveDirectionV1::Higher => cand.exact().sub(&base.exact()),
            }),
            _ => None,
        };
        let ratio = match (&improvement, &baseline_side) {
            (Some(improvement), Some((base, _))) => MeasureRatio::of(improvement, &base.exact()),
            _ => None,
        };
        let improvement = improvement.map(Exact::signed).transpose()?;
        let conclusion = match (&baseline_side, &candidate_side, &improvement) {
            (Some((base, _)), Some((cand, _)), Some(improvement))
                if baseline_n >= objective.min_repetitions
                    && candidate_n >= objective.min_repetitions =>
            {
                if base.is_zero() && !cand.is_zero() {
                    ComparisonConclusionV1::Inconclusive
                } else {
                    match improvement.signum() {
                        Ordering::Equal => ComparisonConclusionV1::Unchanged,
                        Ordering::Less => ComparisonConclusionV1::Regressed,
                        Ordering::Greater
                            if ratio.as_ref().is_some_and(|r| r.at_least(threshold)) =>
                        {
                            ComparisonConclusionV1::Improved
                        }
                        Ordering::Greater => ComparisonConclusionV1::BelowThreshold,
                    }
                }
            }
            _ => ComparisonConclusionV1::Inconclusive,
        };
        metrics.insert(
            key,
            MetricComparisonV1 {
                unit,
                baseline_median: baseline_side.map(|(median, _)| median),
                candidate_median: candidate_side.map(|(median, _)| median),
                improvement,
                ratio,
                baseline_n,
                candidate_n,
                conclusion,
            },
        );
    }
    let outcome = metrics[&objective.metric].conclusion.outcome();
    let comparison = MeasurementComparisonV1 {
        plan_id: plan_id.into(),
        policy_id: policy_id.into(),
        baseline_id: baseline_id.into(),
        candidate_id: candidate_id.into(),
        objective: name.into(),
        measure: objective.measure.clone(),
        metric: objective.metric.clone(),
        direction: objective.direction,
        min_improvement_ratio: threshold.clone(),
        min_repetitions: objective.min_repetitions,
        metrics,
        outcome,
    };
    comparison.validate()?;
    Ok(comparison)
}

impl MeasurementComparisonV1 {
    /// One Markdown table: a row per metric, the objective's metric marked, and the outcome.
    pub fn render_markdown(&self) -> Result<String, String> {
        self.validate()?;
        let cell = |text: &str| text.replace('|', "\\|");
        let mut out = format!(
            "# Comparison: {}\n\nObjective `{}` on measure `{}`: metric `{}`, {} is better, \
             at least {} improvement over at least {} repetitions per side. Outcome: **{}**.\n\n\
             | Metric | Unit | Baseline median | Candidate median | Improvement | Ratio | n (baseline / candidate) | Conclusion |\n\
             |---|---|---:|---:|---:|---:|---:|---|\n",
            cell(&self.objective),
            cell(&self.objective),
            cell(&self.measure),
            cell(&self.metric),
            match self.direction {
                ObjectiveDirectionV1::Lower => "lower",
                ObjectiveDirectionV1::Higher => "higher",
            },
            self.min_improvement_ratio,
            self.min_repetitions,
            match self.outcome {
                ReceiptOutcomeV1::Passed => "passed",
                ReceiptOutcomeV1::Failed => "failed",
                ReceiptOutcomeV1::Inconclusive => "inconclusive",
            }
        );
        let dash = || "—".to_string();
        for (key, row) in &self.metrics {
            let ratio = row.ratio.as_ref().map_or_else(dash, |ratio| {
                ratio
                    .terminating_decimal()
                    .unwrap_or_else(|| ratio.to_string())
            });
            out.push_str(&format!(
                "| {}{} | {} | {} | {} | {} | {} | {} / {} | {} |\n",
                cell(key),
                if *key == self.metric {
                    " (objective)"
                } else {
                    ""
                },
                row.unit.as_str(),
                row.baseline_median
                    .as_ref()
                    .map_or_else(dash, ToString::to_string),
                row.candidate_median
                    .as_ref()
                    .map_or_else(dash, ToString::to_string),
                row.improvement
                    .as_ref()
                    .map_or_else(dash, ToString::to_string),
                ratio,
                row.baseline_n,
                row.candidate_n,
                row.conclusion.as_str()
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> MeasureDecimal {
        MeasureDecimal::parse(text).unwrap()
    }

    #[test]
    fn decimals_have_one_canonical_spelling() {
        for good in [
            "0",
            "1",
            "10",
            "0.5",
            "12.25",
            "0.001",
            "340282366920938463463374607431768211456",
        ] {
            assert_eq!(d(good).to_string(), good);
        }
        for bad in [
            "", "-1", "+1", "01", "00", "1.", ".5", "1.50", "1e3", "0.0", " 1", "1,5", "NaN", "inf",
        ] {
            assert!(MeasureDecimal::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(d("1000").significant_digits(), 4);
        assert_eq!(d("0.00012").significant_digits(), 2);
        assert!(d("2") > d("1.99999"));
    }

    #[test]
    fn the_median_of_an_even_sample_is_the_exact_mean_of_its_middle_values() {
        let sorted = |values: &[&str]| {
            let mut values: Vec<_> = values.iter().map(|v| d(v)).collect();
            values.sort();
            values
        };
        assert_eq!(median(&sorted(&["3", "1", "2"])).unwrap(), d("2"));
        assert_eq!(median(&sorted(&["1", "2"])).unwrap(), d("1.5"));
        assert_eq!(median(&sorted(&["1", "2", "4", "100"])).unwrap(), d("3"));
        assert_eq!(median(&sorted(&["0.1", "0.2"])).unwrap(), d("0.15"));
        // 38 nines twice plus one: the mean needs a 39th digit, and gets it.
        let big = "9".repeat(38);
        let mean = median(&sorted(&[&big, &format!("{big}.1")])).unwrap();
        assert_eq!(mean.to_string(), format!("{big}.05"));
    }

    #[test]
    fn signed_decimals_and_ratios_are_canonical_and_exact() {
        assert!(MeasureSignedDecimal::parse("-0").is_err());
        assert_eq!(
            MeasureSignedDecimal::parse("-1.5").unwrap().to_string(),
            "-1.5"
        );
        for good in ["1/3", "-2/7", "0/1", "10/1"] {
            assert_eq!(MeasureRatio::parse(good).unwrap().to_string(), good);
        }
        for bad in ["2/6", "1/0", "-0/1", "01/3", "1/-3", "1.5/2", "1", "0/2"] {
            assert!(MeasureRatio::parse(bad).is_err(), "{bad:?}");
        }
        let ratio = MeasureRatio::of(&d("1").exact(), &d("3").exact()).unwrap();
        assert_eq!(ratio.to_string(), "1/3");
        assert!(ratio.at_least(&d("0.333")));
        assert!(!ratio.at_least(&d("0.34")));
        assert_eq!(ratio.terminating_decimal(), None);
        let tenth = MeasureRatio::of(&d("0.5").exact(), &d("5").exact()).unwrap();
        assert_eq!(tenth.to_string(), "1/10");
        assert!(tenth.at_least(&d("0.1")));
        assert_eq!(tenth.terminating_decimal().as_deref(), Some("0.1"));
    }

    fn declared(pairs: &[(&str, MetricUnitV1)]) -> BTreeMap<String, MetricUnitV1> {
        pairs.iter().map(|(k, u)| ((*k).into(), *u)).collect()
    }

    #[test]
    fn a_report_line_supplies_exactly_the_declared_metrics_and_units() {
        let bytes = declared(&[("bytes_written", MetricUnitV1::Bytes)]);
        let line = |metrics: &str| {
            format!("building\n{{\"schema\":\"af.measure-report/1\",\"metrics\":{metrics}}}\n")
                .into_bytes()
        };
        let good = parse_report(
            &line(r#"{"bytes_written":{"value":"4096","unit":"bytes"}}"#),
            &bytes,
        )
        .unwrap();
        assert_eq!(good["bytes_written"].value, d("4096"));
        let refused = |stdout: &[u8]| parse_report(stdout, &bytes).unwrap_err().reason;
        assert_eq!(
            refused(&line(
                r#"{"bytes_written":{"value":"4096","unit":"count"}}"#
            )),
            MeasurementFailureReasonV1::UnitMismatch
        );
        for bad in [
            r#"{"bytes_written":{"value":"4096","unit":"kb"}}"#,
            r#"{"bytes_written":{"value":4096,"unit":"bytes"}}"#,
            r#"{"bytes_written":{"value":"04096","unit":"bytes"}}"#,
            r#"{"bytes_written":{"value":"-1","unit":"bytes"}}"#,
            r#"{}"#,
            r#"{"bytes_written":{"value":"1","unit":"bytes"},"extra":{"value":"1","unit":"bytes"}}"#,
            r#"{"bytes_written":{"value":"1","unit":"bytes","note":"x"}}"#,
        ] {
            assert_eq!(
                refused(&line(bad)),
                MeasurementFailureReasonV1::MalformedReport,
                "{bad}"
            );
        }
        let long = format!(
            r#"{{"bytes_written":{{"value":"{}","unit":"bytes"}}}}"#,
            "1".repeat(39)
        );
        assert_eq!(
            refused(&line(&long)),
            MeasurementFailureReasonV1::MalformedReport
        );
        assert_eq!(
            refused(b"no report\n"),
            MeasurementFailureReasonV1::MalformedReport
        );
        assert_eq!(refused(b""), MeasurementFailureReasonV1::MalformedReport);
        // Only the last line is the report.
        let earlier = [
            line(r#"{"bytes_written":{"value":"1","unit":"bytes"}}"#),
            b"done\n".to_vec(),
        ]
        .concat();
        assert_eq!(
            refused(&earlier),
            MeasurementFailureReasonV1::MalformedReport
        );
        // A measure without declared metrics accepts any last line.
        assert!(
            parse_report(b"anything\n", &BTreeMap::new())
                .unwrap()
                .is_empty()
        );
    }

    fn run(elapsed: u64, metrics: &[(&str, &str)]) -> MeasurementRunV1 {
        let id = format!("sha256:{}", "e".repeat(64));
        MeasurementRunV1 {
            started_unix_ms: 1,
            elapsed_ms: elapsed,
            exit_code: Some(0),
            stdout_id: Some(id.clone()),
            stderr_id: Some(id),
            cache: None,
            metrics: metrics
                .iter()
                .map(|(k, v)| {
                    (
                        (*k).into(),
                        MetricValueV1 {
                            value: d(v),
                            unit: MetricUnitV1::Bytes,
                        },
                    )
                })
                .collect(),
        }
    }

    fn measurement(runs: Vec<MeasurementRunV1>) -> MeasurementV1 {
        let id = format!("sha256:{}", "a".repeat(64));
        let metrics = declared(&[("bytes_written", MetricUnitV1::Bytes)]);
        MeasurementV1 {
            plan_id: id.clone(),
            policy_id: id.clone(),
            snapshot_id: id.clone(),
            measure: "write".into(),
            command_id: id,
            toolchain_id: None,
            warm: false,
            repetitions: runs.len() as u32,
            wall_ms: 1000,
            summary: summarize(&metrics, &runs).unwrap(),
            metrics,
            outcome: ReceiptOutcomeV1::Passed,
            failure: None,
            runs,
        }
    }

    fn objective(
        metric: &str,
        direction: ObjectiveDirectionV1,
        ratio: &str,
        min: u32,
    ) -> MeasureObjectiveV1 {
        MeasureObjectiveV1 {
            measure: "write".into(),
            metric: metric.into(),
            direction,
            min_improvement_ratio: ImprovementRatio(d(ratio)),
            min_repetitions: min,
        }
    }

    fn compare(
        base: &MeasurementV1,
        cand: &MeasurementV1,
        objective: &MeasureObjectiveV1,
    ) -> MeasurementComparisonV1 {
        let id = format!("sha256:{}", "b".repeat(64));
        compare_measurements(
            &id,
            &id,
            (&id, base),
            (&id, cand),
            ComparisonObjective {
                name: "smaller",
                objective,
            },
        )
        .unwrap()
    }

    fn written(values: &[&str]) -> MeasurementV1 {
        measurement(
            values
                .iter()
                .map(|v| run(10, &[("bytes_written", v)]))
                .collect(),
        )
    }

    #[test]
    fn a_comparison_concludes_by_the_stated_rules() {
        let lower = objective("bytes_written", ObjectiveDirectionV1::Lower, "0.1", 3);
        let base = written(&["100", "100", "100"]);
        let conclusion = |cand: &MeasurementV1, objective: &MeasureObjectiveV1| {
            let c = compare(&base, cand, objective);
            (c.metrics["bytes_written"].conclusion, c.outcome)
        };
        assert_eq!(
            conclusion(&written(&["80", "80", "80"]), &lower),
            (ComparisonConclusionV1::Improved, ReceiptOutcomeV1::Passed)
        );
        assert_eq!(
            conclusion(&written(&["95", "95", "95"]), &lower),
            (
                ComparisonConclusionV1::BelowThreshold,
                ReceiptOutcomeV1::Failed
            )
        );
        assert_eq!(
            conclusion(&written(&["90", "90", "90"]), &lower),
            (ComparisonConclusionV1::Improved, ReceiptOutcomeV1::Passed),
            "a ratio exactly at the threshold is enough"
        );
        assert_eq!(
            conclusion(&written(&["120", "120", "120"]), &lower),
            (ComparisonConclusionV1::Regressed, ReceiptOutcomeV1::Failed)
        );
        assert_eq!(
            conclusion(&written(&["100", "100", "100"]), &lower),
            (ComparisonConclusionV1::Unchanged, ReceiptOutcomeV1::Failed)
        );
        // A zero threshold still needs a strictly positive improvement.
        let any = objective("bytes_written", ObjectiveDirectionV1::Lower, "0", 3);
        assert_eq!(
            conclusion(&written(&["100", "100", "100"]), &any).1,
            ReceiptOutcomeV1::Failed
        );
        assert_eq!(
            conclusion(&written(&["99.999", "99.999", "99.999"]), &any).1,
            ReceiptOutcomeV1::Passed
        );
        // Higher is better flips the sign.
        let higher = objective("bytes_written", ObjectiveDirectionV1::Higher, "0.1", 3);
        assert_eq!(
            conclusion(&written(&["120", "120", "120"]), &higher).0,
            ComparisonConclusionV1::Improved
        );
        let c = compare(&base, &written(&["120", "120", "120"]), &higher);
        assert_eq!(
            c.metrics["bytes_written"]
                .improvement
                .as_ref()
                .unwrap()
                .to_string(),
            "20"
        );
        assert_eq!(
            c.metrics["bytes_written"]
                .ratio
                .as_ref()
                .unwrap()
                .to_string(),
            "1/5"
        );
        // Too few repetitions on one side.
        assert_eq!(
            conclusion(&written(&["50"]), &lower),
            (
                ComparisonConclusionV1::Inconclusive,
                ReceiptOutcomeV1::Inconclusive
            )
        );
    }

    #[test]
    fn a_zero_baseline_is_unchanged_only_against_a_zero_candidate() {
        let lower = objective("bytes_written", ObjectiveDirectionV1::Lower, "0.1", 3);
        let zero = written(&["0", "0", "0"]);
        let c = compare(&zero, &zero, &lower);
        assert_eq!(
            c.metrics["bytes_written"].conclusion,
            ComparisonConclusionV1::Unchanged
        );
        assert_eq!(c.metrics["bytes_written"].ratio, None);
        assert_eq!(c.outcome, ReceiptOutcomeV1::Failed);
        let c = compare(&zero, &written(&["5", "5", "5"]), &lower);
        assert_eq!(
            c.metrics["bytes_written"].conclusion,
            ComparisonConclusionV1::Inconclusive
        );
        assert_eq!(
            c.metrics["bytes_written"]
                .improvement
                .as_ref()
                .unwrap()
                .to_string(),
            "-5"
        );
        assert_eq!(c.outcome, ReceiptOutcomeV1::Inconclusive);
    }

    #[test]
    fn a_failed_measurement_makes_every_metric_inconclusive() {
        let lower = objective("elapsed_ms", ObjectiveDirectionV1::Lower, "0.1", 1);
        let base = written(&["100", "100", "100"]);
        let mut failed = written(&["1"]);
        failed.repetitions = 3;
        failed.outcome = ReceiptOutcomeV1::Failed;
        failed.failure = Some(MeasurementFailureV1 {
            repetition: 1,
            reason: MeasurementFailureReasonV1::Exit,
            detail: "exit 3".into(),
        });
        failed.runs[0].exit_code = Some(3);
        failed.runs[0].metrics.clear();
        failed.summary.clear();
        failed.validate().unwrap();
        let c = compare(&base, &failed, &lower);
        assert!(
            c.metrics
                .values()
                .all(|m| m.conclusion == ComparisonConclusionV1::Inconclusive
                    && m.candidate_median.is_none()
                    && m.candidate_n == 0)
        );
        assert_eq!(c.outcome, ReceiptOutcomeV1::Inconclusive);
    }

    #[test]
    fn a_measurement_summary_must_follow_from_its_runs() {
        let mut m = written(&["1", "2"]);
        m.validate().unwrap();
        assert_eq!(m.summary["bytes_written"].median, d("1.5"));
        assert_eq!(m.summary[ELAPSED_MS].median, d("10"));
        m.summary.get_mut("bytes_written").unwrap().median = d("2");
        assert!(m.validate().is_err());
        let mut partial = written(&["1", "2"]);
        partial.repetitions = 3;
        assert!(
            partial.validate().is_err(),
            "a passed measurement is never partial"
        );
    }

    #[test]
    fn the_same_two_measurements_compare_to_the_same_bytes() {
        let lower = objective("elapsed_ms", ObjectiveDirectionV1::Lower, "0.1", 3);
        let base = written(&["1", "2", "3"]);
        let cand = written(&["1", "2", "3"]);
        let first = serde_json::to_vec(&compare(&base, &cand, &lower)).unwrap();
        let second = serde_json::to_vec(&compare(&base, &cand, &lower)).unwrap();
        assert_eq!(first, second);
        let parsed: MeasurementComparisonV1 = serde_json::from_slice(&first).unwrap();
        assert_eq!(serde_json::to_vec(&parsed).unwrap(), first);
        assert!(
            compare(&base, &cand, &lower)
                .render_markdown()
                .unwrap()
                .contains("| elapsed_ms (objective) | ms | 10 | 10 | 0 | 0 | 3 / 3 | unchanged |")
        );
    }

    #[test]
    fn policy_tables_are_bounded() {
        let command = Command {
            program: "python3".into(),
            args: vec![],
        };
        let measure = |repetitions, wall_ms, metrics: Vec<MeasureMetricV1>| MeasureDefinitionV1 {
            command: command.clone(),
            repetitions,
            warm: false,
            wall_ms,
            metrics,
        };
        assert!(measure(1, 1, vec![]).validate().is_ok());
        assert!(measure(16, 3_600_000, vec![]).validate().is_ok());
        assert!(measure(0, 1, vec![]).validate().is_err());
        assert!(measure(17, 1, vec![]).validate().is_err());
        assert!(measure(1, 3_600_001, vec![]).validate().is_err());
        let metric = |key: &str| MeasureMetricV1 {
            key: key.into(),
            unit: MetricUnitV1::Bytes,
        };
        assert!(measure(1, 1, vec![metric(ELAPSED_MS)]).validate().is_err());
        assert!(
            measure(1, 1, vec![metric("a"), metric("a")])
                .validate()
                .is_err()
        );
        let measures = BTreeMap::from([("write".to_string(), measure(3, 1, vec![metric("a")]))]);
        assert!(
            objective("a", ObjectiveDirectionV1::Lower, "1", 16)
                .validate(&measures)
                .is_ok()
        );
        assert!(
            objective("elapsed_ms", ObjectiveDirectionV1::Lower, "0", 1)
                .validate(&measures)
                .is_ok()
        );
        assert!(
            objective("b", ObjectiveDirectionV1::Lower, "0.1", 3)
                .validate(&measures)
                .is_err()
        );
        assert!(
            objective("a", ObjectiveDirectionV1::Lower, "1.1", 3)
                .validate(&measures)
                .is_err()
        );
        assert!(
            objective("a", ObjectiveDirectionV1::Lower, "0.1", 0)
                .validate(&measures)
                .is_err()
        );
        assert!(
            objective("a", ObjectiveDirectionV1::Lower, "0.1", 17)
                .validate(&measures)
                .is_err()
        );
        let parsed: MeasureObjectiveV1 = serde_json::from_value(serde_json::json!({
            "measure": "write", "metric": "a", "direction": "lower", "min_improvement_ratio": "0.1"
        }))
        .unwrap();
        assert_eq!(parsed.min_repetitions, DEFAULT_MIN_REPETITIONS);
        assert_eq!(
            serde_json::to_value(&parsed).unwrap()["min_improvement_ratio"],
            "0.1"
        );
        // A float has been rounded by the parser before the kernel sees it: refused, naming the
        // spelling to use instead. The integers 0 and 1 are exact and accepted.
        let refused = serde_json::from_value::<MeasureObjectiveV1>(serde_json::json!({
            "measure": "write", "metric": "a", "direction": "lower", "min_improvement_ratio": 0.10
        }))
        .unwrap_err()
        .to_string();
        assert!(refused.contains("write it as decimal text"), "{refused}");
        let one: MeasureObjectiveV1 = serde_json::from_value(serde_json::json!({
            "measure": "write", "metric": "a", "direction": "lower", "min_improvement_ratio": 1
        }))
        .unwrap();
        assert_eq!(one.min_improvement_ratio.0, MeasureDecimal::from_u64(1));
    }
}
