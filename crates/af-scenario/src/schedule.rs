//! Deterministic fixed-rate intervals and daily schedules with pinned timezone transitions.
use super::contract::*;
use super::state::Run;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const DAY: i64 = 86_400;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    pub name: String,
    pub initial_offset: i32,
    pub transitions: Vec<(i64, i32)>,
    /// TZif's explicit transitions are authoritative only through this instant. Fixed zones
    /// have no horizon. Refuse a future DST rule we have not pinned rather than guessing.
    pub valid_until_unix_seconds: Option<i64>,
}
impl Zone {
    pub fn load(name: &str) -> Result<Self> {
        if name == "UTC" {
            return Ok(Self {
                name: name.into(),
                initial_offset: 0,
                transitions: Vec::new(),
                valid_until_unix_seconds: None,
            });
        }
        if name.starts_with(['+', '-']) {
            let b = name.as_bytes();
            require(
                b.len() == 6
                    && b[3] == b':'
                    && b[1..3].iter().chain(&b[4..]).all(u8::is_ascii_digit),
                "Timezone must be UTC, ±HH:MM, or an IANA zone name",
            )?;
            let h = name[1..3].parse::<i32>().map_err(|e| e.to_string())?;
            let m = name[4..6].parse::<i32>().map_err(|e| e.to_string())?;
            require(h <= 23 && m <= 59, "Invalid timezone offset")?;
            let offset = (h * 3600 + m * 60) * if b[0] == b'-' { -1 } else { 1 };
            return Ok(Self {
                name: name.into(),
                initial_offset: offset,
                transitions: Vec::new(),
                valid_until_unix_seconds: None,
            });
        }
        require(
            name.len() <= 128
                && name.split('/').all(|p| {
                    !p.is_empty()
                        && p != "."
                        && p != ".."
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_+-".contains(&b))
                }),
            "Invalid IANA timezone name",
        )?;
        let root = std::path::Path::new("/usr/share/zoneinfo")
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let path = root
            .join(name)
            .canonicalize()
            .map_err(|e| format!("Timezone {name}: {e}"))?;
        require(path.starts_with(root), "Timezone escaped system zoneinfo")?;
        let bytes =
            af_store::local_fs::read_bounded(&path, 1_048_576).map_err(|e| e.to_string())?;
        Self::tzif(name, &bytes)
    }
    fn tzif(name: &str, bytes: &[u8]) -> Result<Self> {
        fn num(bytes: &[u8], pos: usize) -> Result<usize> {
            let n: [u8; 4] = bytes
                .get(pos..pos + 4)
                .ok_or("Truncated TZif")?
                .try_into()
                .map_err(|_| "Invalid TZif")?;
            Ok(u32::from_be_bytes(n) as usize)
        }
        fn header(bytes: &[u8], base: usize) -> Result<[usize; 6]> {
            require(
                bytes.get(base..base + 4) == Some(b"TZif"),
                "Invalid TZif header",
            )?;
            let mut n = [0; 6];
            for (i, v) in n.iter_mut().enumerate() {
                *v = num(bytes, base + 20 + 4 * i)?;
                require(*v <= 100_000, "Oversized TZif count")?;
            }
            require(n[4] > 0 && n[4] <= 256, "Invalid TZif types")?;
            Ok(n)
        }
        fn length(n: [usize; 6], size: usize) -> usize {
            n[3] * (size + 1) + n[4] * 6 + n[5] + n[2] * (size + 4) + n[1] + n[0]
        }
        let n1 = header(bytes, 0)?;
        let (base, size, n) = if matches!(bytes.get(4), Some(b'2' | b'3' | b'4')) {
            let base = 44 + length(n1, 4);
            (base, 8, header(bytes, base)?)
        } else {
            (0, 4, n1)
        };
        let data = base + 44;
        let end = data + length(n, size);
        require(end <= bytes.len(), "Truncated TZif body")?;
        let type_base = data + n[3] * (size + 1);
        let offsets: Vec<i32> = (0..n[4])
            .map(|i| {
                i32::from_be_bytes(
                    bytes[type_base + i * 6..type_base + i * 6 + 4]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();
        require(
            offsets.iter().all(|n| n.unsigned_abs() <= 86_400),
            "Invalid timezone UTC offset",
        )?;
        let mut transitions = Vec::new();
        for i in 0..n[3] {
            let b = &bytes[data + i * size..data + (i + 1) * size];
            let at = if size == 8 {
                i64::from_be_bytes(b.try_into().unwrap())
            } else {
                i64::from(i32::from_be_bytes(b.try_into().unwrap()))
            };
            let kind = usize::from(bytes[data + n[3] * size + i]);
            let offset = *offsets.get(kind).ok_or("Invalid TZif transition type")?;
            require(
                transitions.last().is_none_or(|(prev, _)| *prev < at),
                "Unordered timezone transitions",
            )?;
            transitions.push((at, offset));
        }
        let tail = std::str::from_utf8(&bytes[end..])
            .map_err(|e| e.to_string())?
            .trim();
        // A comma introduces POSIX recurring daylight rules. Pin the complete explicit TZif
        // interval, never depend on the host's later tzdata or its ambient TZ setting.
        let horizon = if tail.contains(',') {
            transitions.last().map(|(at, _)| *at)
        } else {
            None
        };
        let zone = Self {
            name: name.into(),
            initial_offset: offsets[0],
            transitions,
            valid_until_unix_seconds: horizon,
        };
        zone.validate()?;
        Ok(zone)
    }
    pub fn validate(&self) -> Result<()> {
        text(&self.name)?;
        require(
            self.initial_offset.unsigned_abs() <= 86_400
                && self.transitions.len() <= 100_000
                && self
                    .transitions
                    .iter()
                    .all(|(_, n)| n.unsigned_abs() <= 86_400)
                && self.transitions.windows(2).all(|w| w[0].0 < w[1].0)
                && self
                    .valid_until_unix_seconds
                    .is_none_or(|h| self.transitions.last().is_some_and(|(at, _)| *at == h)),
            "Corrupt pinned timezone",
        )
    }
    pub fn offset(&self, seconds: i64) -> Result<i32> {
        require(
            self.valid_until_unix_seconds.is_none_or(|h| seconds < h),
            "Pinned timezone transition horizon exhausted; define a new Ant revision with current tzdata",
        )?;
        Ok(self
            .transitions
            .iter()
            .rev()
            .find(|(at, _)| *at <= seconds)
            .map_or(self.initial_offset, |(_, o)| *o))
    }
    fn local_day(&self, seconds: i64) -> Result<i64> {
        Ok((seconds + i64::from(self.offset(seconds)?)).div_euclid(DAY))
    }
    fn occurrence(&self, day: i64, hour: u8, minute: u8) -> Result<(i64, bool)> {
        let local = day * DAY + i64::from(hour) * 3600 + i64::from(minute) * 60;
        let offsets: std::collections::BTreeSet<_> = self
            .transitions
            .iter()
            .map(|(_, o)| *o)
            .chain([self.initial_offset])
            .collect();
        let mut matches = Vec::new();
        for offset in offsets {
            let at = local - i64::from(offset);
            if self.offset(at)? == offset {
                matches.push(at);
            }
        }
        if let Some(first) = matches.into_iter().min() {
            return Ok((first, false));
        }
        // Nonexistent spring-forward wall time: retain a skipped occurrence at the transition.
        let mut before = self.initial_offset;
        for &(at, after) in &self.transitions {
            if after > before
                && local >= at.saturating_add(i64::from(before))
                && local < at.saturating_add(i64::from(after))
            {
                return Ok((at, true));
            }
            before = after;
        }
        Err("Cannot resolve local schedule time in pinned timezone".into())
    }
}

/// Explicit schedule generation; interval indexes never use a timezone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schedule {
    Daily {
        timezone: String,
        hour: u8,
        minute: u8,
        start_unix_ms: u64,
    },
    Interval {
        seconds: u64,
        start_unix_ms: u64,
    },
}
impl Schedule {
    pub fn start(&self) -> u64 {
        match self {
            Self::Daily { start_unix_ms, .. } | Self::Interval { start_unix_ms, .. } => {
                *start_unix_ms
            }
        }
    }
    pub fn timezone(&self) -> &str {
        match self {
            Self::Daily { timezone, .. } => timezone,
            Self::Interval { .. } => "UTC",
        }
    }
    fn valid(&self) -> bool {
        self.start() > 0
            && self.start() <= MAX_TIME
            && match self {
                Self::Daily { hour, minute, .. } => *hour < 24 && *minute < 60,
                Self::Interval { seconds, .. } => {
                    *seconds > 0 && *seconds <= af_contracts::json::SAFE_INTEGER_MAX as u64
                }
            }
    }
    fn due(&self, zone: &Zone, index: i64) -> Result<(u64, bool)> {
        match self {
            Self::Daily { hour, minute, .. } => {
                let (seconds, gap) = zone.occurrence(index, *hour, *minute)?;
                Ok((seconds.max(0) as u64 * 1000, gap))
            }
            Self::Interval {
                seconds,
                start_unix_ms,
            } => {
                require(index >= 0, "Invalid interval ordinal")?;
                let at = u128::from(*start_unix_ms) + index as u128 * u128::from(*seconds) * 1000;
                Ok((u64::try_from(at).unwrap_or(u64::MAX), false))
            }
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    Skip,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub overlap: Skip,
    pub missed: Skip,
    pub lateness_ms: u64,
    pub per_run: Limits,
    pub rolling_tokens: u64,
    pub rolling_window_ms: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ant {
    pub schema: String,
    pub id: String,
    pub scenario: Bundle,
    pub inputs: BTreeMap<String, Datum>,
    pub schedule: Schedule,
    pub owner: String,
    pub notification: String,
    pub execution: Execution,
}
impl Ant {
    pub fn validate(&self, host: &dyn crate::Host) -> Result<()> {
        require(self.schema == "af/ant@7", "Unsupported Ant version")?;
        name(&self.id)?;
        name(&self.owner)?;
        text(&self.notification)?;
        self.scenario.validate(host)?;
        self.execution.per_run.validate()?;
        let def = &self.scenario.definitions[&self.scenario.root];
        require(
            self.execution.per_run.within(&def.limits),
            "Ant per-run budget exceeds Scenario",
        )?;
        input_values(&def.inputs, &self.inputs)?;
        require(
            self.inputs.values().all(Datum::scalar),
            "Ant cannot manufacture receipts",
        )?;
        require(
            self.schedule.valid()
                && self.execution.lateness_ms < DAY as u64 * 1000
                && self.execution.rolling_window_ms > 0
                && self.execution.rolling_window_ms <= MAX_TIME
                && self.execution.rolling_tokens <= af_contracts::json::SAFE_INTEGER_MAX as u64
                && self.execution.per_run.max_tokens <= self.execution.rolling_tokens,
            "Invalid Ant schedule or spending limits",
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AntPin {
    pub schema: String,
    pub definition: Ant,
    pub scenario: Pin,
    pub zone: Zone,
    pub revision: String,
}
impl AntPin {
    pub fn new(host: &dyn crate::Host, definition: Ant, policy: Policy) -> Result<Self> {
        definition.validate(host)?;
        let scenario = Pin::new(host, definition.scenario.clone(), policy)?;
        let zone = Zone::load(definition.schedule.timezone())?;
        let revision = hash(&(&definition, &scenario, &zone))?;
        Ok(Self {
            schema: "af/ant-pin@7".into(),
            definition,
            scenario,
            zone,
            revision,
        })
    }
    pub fn validate(&self, host: &dyn crate::Host) -> Result<()> {
        self.definition.validate(host)?;
        self.scenario.validate(host)?;
        self.zone.validate()?;
        require(
            self.schema == "af/ant-pin@7"
                && self.definition.scenario == self.scenario.bundle
                && self.definition.schedule.timezone() == self.zone.name
                && self.revision == hash(&(&self.definition, &self.scenario, &self.zone))?,
            "Corrupt Ant pin",
        )
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntMode {
    Enabled,
    Paused,
    Disabled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OccurrenceOutcome {
    Started {
        run_id: String,
        reserved_tokens: u64,
    },
    SkippedBusy,
    SkippedMissed,
    SkippedPaused,
    SkippedDisabled,
    SkippedBudget,
    SkippedCapacity,
    SkippedMissingTime,
    SkippedRoadmap {
        reason: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Occurrence {
    pub id: String,
    pub local_day: i64,
    /// Inclusive last skipped index; equal to local_day for ordinary receipts.
    pub through_index: i64,
    pub admitted_unix_ms: u64,
    pub due_unix_ms: u64,
    pub outcome: OccurrenceOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AntState {
    pub schema: String,
    pub pin: AntPin,
    pub state: AntMode,
    pub changes: Vec<(u64, AntMode)>,
    pub next_day: i64,
    pub last_tick_unix_ms: u64,
    pub occurrences: Vec<Occurrence>,
    pub retention: crate::retention::Retention,
}
impl AntState {
    pub fn new(pin: AntPin, now: u64) -> Result<Self> {
        let next_day = match &pin.definition.schedule {
            Schedule::Interval { .. } => 0,
            schedule => pin.zone.local_day((schedule.start() / 1000) as i64)?,
        };
        Ok(Self {
            schema: "af/ant-state@8".into(),
            pin,
            state: AntMode::Disabled,
            changes: vec![(now, AntMode::Disabled)],
            next_day,
            last_tick_unix_ms: now,
            occurrences: Vec::new(),
            retention: crate::retention::Retention::new(),
        })
    }
    pub fn validate(&self, host: &dyn crate::Host, runs: &BTreeMap<String, Run>) -> Result<()> {
        self.pin.validate(host)?;
        self.retention.validate(self.next_day)?;
        require(
            self.schema == "af/ant-state@8"
                && self.cursor_valid()
                && self.last_tick_unix_ms > 0
                && self.last_tick_unix_ms <= MAX_TIME
                && self.changes.iter().all(|(t, _)| *t > 0 && *t <= MAX_TIME)
                && !self.changes.is_empty()
                && self.changes.last().is_some_and(|(_, s)| *s == self.state)
                && self.changes.windows(2).all(|w| w[0].0 <= w[1].0)
                && self
                    .occurrences
                    .windows(2)
                    .all(|w| w[0].through_index < w[1].local_day),
            "Corrupt Ant state",
        )?;
        let mut active = 0;
        for o in &self.occurrences {
            require(
                o.id == self.occurrence_id(o.local_day)?
                    && o.local_day <= o.through_index
                    && (o.local_day == o.through_index
                        || (matches!(self.pin.definition.schedule, Schedule::Interval { .. })
                            && matches!(o.outcome, OccurrenceOutcome::SkippedMissed)))
                    && self
                        .pin
                        .definition
                        .schedule
                        .due(&self.pin.zone, o.through_index)?
                        .0
                        <= o.admitted_unix_ms
                    && o.through_index < self.next_day
                    && o.admitted_unix_ms >= o.due_unix_ms
                    && o.admitted_unix_ms <= self.last_tick_unix_ms
                    && o.due_unix_ms
                        == self
                            .pin
                            .definition
                            .schedule
                            .due(&self.pin.zone, o.local_day)?
                            .0
                    && o.due_unix_ms <= self.last_tick_unix_ms,
                "Corrupt occurrence identity or cursor",
            )?;
            if let OccurrenceOutcome::Started {
                run_id,
                reserved_tokens,
            } = &o.outcome
            {
                let run = runs.get(run_id).ok_or("Started occurrence has no Run")?;
                require(
                    *run_id
                        == format!(
                            "ant-{}",
                            o.id.strip_prefix("sha256:")
                                .ok_or("Invalid occurrence ID")?
                        )
                        && run.created_unix_ms == o.admitted_unix_ms
                        && run.ant.as_deref() == Some(&self.pin.definition.id)
                        && run.pin == self.pin.scenario
                        && run.limits == self.pin.definition.execution.per_run
                        && *reserved_tokens == run.limits.max_tokens,
                    "Occurrence Run authority differs",
                )?;
                if run.status.active() {
                    active += 1;
                }
            }
        }
        require(active <= 1, "Overlapping Ant Runs")
    }
    fn cursor_valid(&self) -> bool {
        match self.pin.definition.schedule {
            Schedule::Daily { .. } => (-1..=100_001).contains(&self.next_day),
            Schedule::Interval { .. } => (0..=MAX_TIME as i64 / 1000 + 1).contains(&self.next_day),
        }
    }
    pub fn next_due_unix_ms(&self) -> Result<Option<u64>> {
        let at = self
            .pin
            .definition
            .schedule
            .due(&self.pin.zone, self.next_day)?
            .0;
        Ok((at <= MAX_TIME).then_some(at))
    }
    fn occurrence_id(&self, day: i64) -> Result<String> {
        hash(&(&self.pin.definition.id, &self.pin.revision, day))
    }
    pub fn change(&mut self, state: AntMode, now: u64) -> Result<()> {
        require(
            now >= self.last_tick_unix_ms && self.changes.last().is_none_or(|(t, _)| now >= *t),
            "Ant clock moved backwards",
        )?;
        self.state = state;
        self.changes.push((now, state));
        Ok(())
    }
    #[cfg(test)]
    pub fn tick(
        &mut self,
        host: &dyn crate::Host,
        now: u64,
        runs: &mut BTreeMap<String, Run>,
    ) -> Result<Vec<String>> {
        self.tick_roadmaps(host, now, runs, &mut BTreeMap::new(), None, 0)
    }
    fn tick_roadmaps(
        &mut self,
        host: &dyn crate::Host,
        now: u64,
        runs: &mut BTreeMap<String, Run>,
        roadmaps: &mut BTreeMap<String, super::roadmap::Roadmap>,
        capacity: Option<&BTreeMap<String, u64>>,
        metadata: u64,
    ) -> Result<Vec<String>> {
        require(self.cursor_valid(), "Corrupt Ant calendar cursor")?;
        require(
            now >= self.last_tick_unix_ms
                && now <= MAX_TIME
                && self.changes.last().is_none_or(|(t, _)| *t <= now),
            "Ant tick clock moved backwards or exceeds the calendar bound",
        )?;
        let schedule = &self.pin.definition.schedule;
        require(schedule.valid(), "Invalid interval or daily schedule")?;
        let day_now = match schedule {
            Schedule::Daily { .. } => self.pin.zone.local_day((now / 1000) as i64)?,
            Schedule::Interval {
                seconds,
                start_unix_ms,
            } => {
                if now < *start_unix_ms {
                    self.last_tick_unix_ms = now;
                    return Ok(Vec::new());
                }
                ((now - start_unix_ms) as u128 / (u128::from(*seconds) * 1000)) as i64
            }
        };
        require(
            matches!(schedule, Schedule::Interval { .. }) || day_now - self.next_day < 4096,
            "More than 4096 missed daily occurrences; tick through bounded intermediate times",
        )?;
        let mut started = Vec::new();
        // Compact arbitrarily long interval downtime into one durable skipped range. Never
        // replay a burst, even if lateness policy would admit an older index.
        if matches!(schedule, Schedule::Interval { .. }) && self.next_day < day_now {
            let index = self.next_day;
            self.occurrences.push(Occurrence {
                id: self.occurrence_id(index)?,
                local_day: index,
                through_index: day_now - 1,
                due_unix_ms: schedule.due(&self.pin.zone, index)?.0,
                admitted_unix_ms: now,
                outcome: OccurrenceOutcome::SkippedMissed,
            });
            self.next_day = day_now;
        }
        while self.next_day <= day_now {
            let day = self.next_day;
            let (at, gap) = schedule.due(&self.pin.zone, day)?;
            if at > now {
                break;
            }
            self.next_day += 1;
            if at < schedule.start() {
                continue;
            }
            let id = self.occurrence_id(day)?;
            let state = self
                .changes
                .iter()
                .rev()
                .find(|(t, _)| *t <= at)
                .map_or(AntMode::Disabled, |(_, s)| *s);
            let active = self.occurrences.iter().any(|o| match &o.outcome {
                OccurrenceOutcome::Started { run_id, .. } => {
                    runs.get(run_id).is_some_and(|r| r.status.active())
                }
                _ => false,
            });
            let used = self.occurrences.iter().try_fold(
                self.retention
                    .rolling(now, self.pin.definition.execution.rolling_window_ms)?,
                |used, o| {
                    let charge = match &o.outcome {
                        OccurrenceOutcome::Started {
                            run_id,
                            reserved_tokens,
                        } => match runs.get(run_id) {
                            Some(r) if !r.status.active() => {
                                if r.task_charges
                                    .values()
                                    .any(crate::state::TaskCharge::uncertain)
                                    || r.updated_unix_ms
                                        > now.saturating_sub(
                                            self.pin.definition.execution.rolling_window_ms,
                                        )
                                {
                                    u64::try_from(r.accounted_tokens()?).unwrap_or(u64::MAX)
                                } else {
                                    0
                                }
                            }
                            _ => *reserved_tokens,
                        },
                        _ => 0,
                    };
                    // Only the admission comparison saturates; native Run charges and
                    // retired decimal totals remain exact, including reconciled overruns.
                    Ok::<u64, String>(used.saturating_add(charge))
                },
            )?;
            // Preserve historical due-time and missed dispositions. An otherwise eligible
            // occurrence stays suppressed by any pause/disable before its first tick, even
            // when enabled again before that tick. The clock guard bounds all changes by now.
            let stopped = self
                .changes
                .iter()
                .rev()
                .take_while(|(t, _)| *t > at)
                .find(|(_, mode)| *mode != AntMode::Enabled)
                .map(|(_, mode)| *mode);
            let outcome = if gap {
                OccurrenceOutcome::SkippedMissingTime
            } else if state == AntMode::Paused {
                OccurrenceOutcome::SkippedPaused
            } else if state == AntMode::Disabled {
                OccurrenceOutcome::SkippedDisabled
            } else if now - at > self.pin.definition.execution.lateness_ms {
                OccurrenceOutcome::SkippedMissed
            } else if stopped == Some(AntMode::Paused) {
                OccurrenceOutcome::SkippedPaused
            } else if stopped == Some(AntMode::Disabled) {
                OccurrenceOutcome::SkippedDisabled
            } else if active {
                OccurrenceOutcome::SkippedBusy
            } else if used.saturating_add(self.pin.definition.execution.per_run.max_tokens)
                > self.pin.definition.execution.rolling_tokens
            {
                OccurrenceOutcome::SkippedBudget
            } else {
                let run_id = format!("ant-{}", id.strip_prefix("sha256:").unwrap());
                require(
                    !runs.contains_key(&run_id),
                    "Occurrence Run already exists without its receipt",
                )?;
                let mut run = Run::new(
                    host,
                    run_id.clone(),
                    self.pin.scenario.clone(),
                    self.pin.definition.inputs.clone(),
                    self.pin.definition.execution.per_run.clone(),
                    now,
                    Some(self.pin.definition.id.clone()),
                )?;
                let old_admission = roadmaps.clone();
                let admitted = if let Some(admission) = run.pin.bundle.definitions
                    [&run.pin.bundle.root]
                    .admission
                    .clone()
                {
                    let roadmap = roadmaps
                        .get_mut(&admission.roadmap)
                        .ok_or("Roadmap not found")?;
                    run.goal_claim = roadmap.admit(&run)?;
                    run.goal_claim.is_some()
                } else {
                    true
                };
                if !crate::retention::admission_fits(runs, &run, capacity, metadata)? {
                    *roadmaps = old_admission;
                    OccurrenceOutcome::SkippedCapacity
                } else if admitted {
                    runs.insert(run_id.clone(), run);
                    started.push(run_id.clone());
                    OccurrenceOutcome::Started {
                        run_id,
                        reserved_tokens: self.pin.definition.execution.per_run.max_tokens,
                    }
                } else {
                    OccurrenceOutcome::SkippedRoadmap {reason:"No eligible complete goal fits all roadmap budgets; inspect deferred reasons".into()}
                }
            };
            self.occurrences.push(Occurrence {
                id,
                local_day: day,
                through_index: day,
                admitted_unix_ms: now,
                due_unix_ms: at,
                outcome,
            });
        }
        self.last_tick_unix_ms = now;
        Ok(started)
    }
}

/// Plan each Ant independently under the caller's transaction. A refused timezone horizon or
/// clock on one binding cannot block another binding, and leaves no partial admission behind.
#[cfg(test)]
pub fn tick_all(
    host: &dyn crate::Host,
    ants: &mut BTreeMap<String, AntState>,
    runs: &mut BTreeMap<String, Run>,
    now: u64,
) -> (Vec<String>, BTreeMap<String, String>) {
    tick_all_with_roadmaps(host, ants, runs, &mut BTreeMap::new(), now)
}
pub fn tick_all_with_roadmaps(
    host: &dyn crate::Host,
    ants: &mut BTreeMap<String, AntState>,
    runs: &mut BTreeMap<String, Run>,
    roadmaps: &mut BTreeMap<String, super::roadmap::Roadmap>,
    now: u64,
) -> (Vec<String>, BTreeMap<String, String>) {
    tick_all_with_capacity(host, ants, runs, roadmaps, None, now)
}
pub fn tick_all_with_capacity(
    host: &dyn crate::Host,
    ants: &mut BTreeMap<String, AntState>,
    runs: &mut BTreeMap<String, Run>,
    roadmaps: &mut BTreeMap<String, super::roadmap::Roadmap>,
    capacity: Option<&BTreeMap<String, u64>>,
    now: u64,
) -> (Vec<String>, BTreeMap<String, String>) {
    const ADMISSION_METADATA_FAILURE: u64 = MAX_BYTES;
    let metadata = serde_json::to_vec(&(&ants, &roadmaps, &capacity))
        .map_or(ADMISSION_METADATA_FAILURE, |b| b.len() as u64 + 4096);
    let mut started = Vec::new();
    let mut refused = BTreeMap::new();
    for (id, ant) in ants {
        let before: std::collections::BTreeSet<String> = runs.keys().cloned().collect();
        let mut candidate = ant.clone();
        let old_roadmaps = roadmaps.clone();
        match candidate.tick_roadmaps(host, now, runs, roadmaps, capacity, metadata) {
            Ok(ids) => {
                *ant = candidate;
                started.extend(ids);
            }
            Err(why) => {
                *roadmaps = old_roadmaps;
                runs.retain(|id, _| before.contains(id));
                refused.insert(id.clone(), why);
            }
        }
    }
    (started, refused)
}
