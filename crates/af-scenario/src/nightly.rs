//! Deterministic nightly identity and separately installed machine release authority.
use super::contract::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn validate_tag(tag: &str) -> Result<()> {
    let b = tag.as_bytes();
    require(
        b.len() == 18
            && b.starts_with(b"nightly-")
            && b[12] == b'.'
            && b[15] == b'.'
            && b[8..12]
                .iter()
                .chain(&b[13..15])
                .chain(&b[16..18])
                .all(u8::is_ascii_digit),
        "Expected nightly-YYYY.MM.DD",
    )?;
    let year: u32 = tag[8..12].parse().map_err(|_| "Invalid year")?;
    let month: usize = tag[13..15].parse().map_err(|_| "Invalid month")?;
    let day: u32 = tag[16..18].parse().map_err(|_| "Invalid day")?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    require(
        year > 0 && (1..=12).contains(&month) && day > 0 && day <= days[month - 1],
        "Invalid nightly calendar date",
    )
}

pub fn tag_for_day(day: i64) -> Result<String> {
    require(
        (-719162..=2932896).contains(&day),
        "Nightly day is outside the calendar",
    )?;
    // Civil date from Unix day, with Euclidean eras for pre-epoch dates.
    let z = day + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(m <= 2);
    let tag = format!("nightly-{year:04}.{m:02}.{d:02}");
    validate_tag(&tag)?;
    Ok(tag)
}

pub fn predecessor(op: Operation) -> Option<&'static str> {
    match op {
        Operation::NightlyCompareV1 => Some("nightly_built"),
        Operation::NightlyReportV1 => Some("nightly_compared"),
        Operation::PublishNightlyV1 => Some("nightly_reported"),
        _ => None,
    }
}
pub fn is_nightly(op: Operation) -> bool {
    matches!(
        op,
        Operation::NightlyBuildV1
            | Operation::NightlyCompareV1
            | Operation::NightlyReportV1
            | Operation::PublishNightlyV1
    )
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub repository: String,
    pub configuration_digest: String,
}
pub fn validate_policy(policy: &Policy) -> Result<()> {
    require(
        policy.nightly_releases.len() <= 32,
        "Too many nightly scopes",
    )?;
    for (id, scope) in &policy.nightly_releases {
        let parts: Vec<_> = scope.repository.split('/').collect();
        require(
            parts.len() == 2
                && parts.iter().all(|s| {
                    !s.is_empty()
                        && s.len() <= 100
                        && *s != "."
                        && *s != ".."
                        && s.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
                })
                && af_contracts::is_digest(&scope.configuration_digest),
            "Invalid nightly release scope",
        )?;
        let adapter = policy
            .adapters
            .get(id)
            .ok_or("Nightly scope lacks adapter")?;
        require(
            adapter
                .destinations
                .contains(&format!("github:{}", scope.repository))
                && adapter.operations.contains(&Operation::PublishNightlyV1),
            "Nightly scope differs from adapter",
        )?;
    }
    for (id, adapter) in &policy.adapters {
        require(
            !adapter.operations.iter().any(|op| is_nightly(*op))
                || policy.nightly_releases.contains_key(id),
            "Nightly operations require explicit standing machine scope",
        )?;
    }
    Ok(())
}
pub fn validate_dispatch(
    policy: &Policy,
    grant: &Grant,
    inputs: &BTreeMap<String, Datum>,
) -> Result<()> {
    if !is_nightly(grant.operation) {
        return Ok(());
    }
    let scope = policy
        .nightly_releases
        .get(&grant.adapter)
        .ok_or("Missing nightly machine scope")?;
    let Some(Datum::NightlyTag(tag)) = inputs.get("nightly_tag") else {
        return Err("Missing nightly tag".into());
    };
    validate_tag(tag)?;
    require(
        grant.destination == format!("github:{}", scope.repository)
            && inputs.get("subject") == Some(&Datum::Text(tag.clone()))
            && inputs.get("revision") == Some(&Datum::Text(scope.configuration_digest.clone())),
        "Nightly identity differs from machine scope",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_dates_and_epoch() {
        for s in [
            "nightly-2024.02.29",
            "nightly-2000.02.29",
            "nightly-2026.11.16",
        ] {
            validate_tag(s).unwrap();
        }
        for s in [
            "nightly-2026.02.29",
            "nightly-1900.02.29",
            "nightly-0000.01.01",
            "nightly-2026.13.01",
            "nightly-2026.01.00",
            "nightly-２０２６.01.01",
        ] {
            assert!(validate_tag(s).is_err(), "{s}");
        }
        assert_eq!(tag_for_day(0).unwrap(), "nightly-1970.01.01");
        assert_eq!(tag_for_day(-1).unwrap(), "nightly-1969.12.31");
        assert_eq!(tag_for_day(19782).unwrap(), "nightly-2024.02.29");
    }
}

#[cfg(test)]
mod occurrence_tests {
    use super::*;
    use crate::{schedule::*, tests::*};
    #[test]
    fn scheduled_local_date_is_pinned_across_restart_rollover_and_overlap() {
        let host = TestHost::default();
        let mut def = ant("Europe/Moscow", 0, 30).pin.definition;
        def.scenario
            .definitions
            .get_mut("main")
            .unwrap()
            .inputs
            .insert("night".into(), Port::NightlyTag);
        def.occurrence_inputs
            .insert("night".into(), OccurrenceInput::ScheduledNightlyTag);
        let (_, policy) = simple();
        let pin = AntPin::new(&host, def.clone(), policy.clone()).unwrap();
        valid("ant-pin-v8.json", &pin);
        let mut ant = AntState::new(pin, START).unwrap();
        ant.change(AntMode::Enabled, START).unwrap();
        let mut runs = BTreeMap::new();
        // The next local day begins at 21:00 UTC. 00:30 Moscow remains the prior UTC day.
        let due = START + 21 * 3_600_000 + 30 * 60_000;
        let ids = ant.tick(&host, due, &mut runs).unwrap();
        assert_eq!(ids.len(), 1);
        let local_day = (START / DAY + 1) as i64;
        let expected = Datum::NightlyTag(tag_for_day(local_day).unwrap());
        assert_eq!(runs[&ids[0]].frames["root"].inputs["night"], expected);
        let bytes = serde_json::to_vec(&ant).unwrap();
        let mut resumed: AntState = serde_json::from_slice(&bytes).unwrap();
        resumed.validate(&host, &runs).unwrap();
        assert!(
            resumed
                .tick(&host, due + DAY, &mut runs)
                .unwrap()
                .is_empty()
        );
        assert_eq!(runs[&ids[0]].frames["root"].inputs["night"], expected);
        assert_eq!(
            resumed.occurrences.last().unwrap().outcome,
            OccurrenceOutcome::SkippedBusy
        );
        let mut bad = runs.clone();
        bad.get_mut(&ids[0])
            .unwrap()
            .frames
            .get_mut("root")
            .unwrap()
            .inputs
            .insert(
                "night".into(),
                Datum::NightlyTag(tag_for_day(local_day + 1).unwrap()),
            );
        assert!(resumed.validate(&host, &bad).is_err());
        def.schedule = Schedule::Interval {
            seconds: 60,
            start_unix_ms: START,
        };
        assert!(AntPin::new(&host, def, policy).is_err());
    }
    #[test]
    fn occurrence_conflicts_and_wrong_ports_are_refused() {
        let host = TestHost::default();
        let mut def = ant("UTC", 3, 0).pin.definition;
        def.occurrence_inputs
            .insert("night".into(), OccurrenceInput::ScheduledNightlyTag);
        let (_, policy) = simple();
        assert!(AntPin::new(&host, def.clone(), policy.clone()).is_err());
        def.scenario
            .definitions
            .get_mut("main")
            .unwrap()
            .inputs
            .insert("night".into(), Port::Text);
        assert!(AntPin::new(&host, def.clone(), policy.clone()).is_err());
        def.scenario
            .definitions
            .get_mut("main")
            .unwrap()
            .inputs
            .insert("night".into(), Port::NightlyTag);
        def.inputs.insert(
            "night".into(),
            Datum::NightlyTag("nightly-2026.11.16".into()),
        );
        assert!(AntPin::new(&host, def, policy).is_err());
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    use crate::{schedule::Ant, tests::*};
    use std::path::PathBuf;

    #[test]
    fn native_release_bundle_has_typed_predecessors_zero_tokens_and_exact_machine_scope() {
        let root = std::env::var_os("AF_WORKSPACE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let definition: Ant = serde_json::from_slice(
            &std::fs::read(root.join("fixtures/scenarios/nightly-ant.json")).unwrap(),
        )
        .unwrap();
        let bundle = definition.scenario.clone();
        let host = TestHost::default();
        definition.validate(&host).unwrap();
        valid("ant-v8.json", &definition);
        valid("scenario-bundle-v7.json", &bundle);
        let (_, mut policy) = simple();
        let operations = [
            Operation::NightlyBuildV1,
            Operation::NightlyCompareV1,
            Operation::NightlyReportV1,
            Operation::PublishNightlyV1,
            Operation::Observe,
        ];
        policy.adapters.insert(
            "nightly".into(),
            Adapter {
                executable: "/installed/nightly/adapter".into(),
                digest: digest(b"adapter"),
                operations: operations.into(),
                destinations: ["github:owner/af".into()].into(),
                max_wall_ms: 30000,
                max_output_bytes: 65536,
            },
        );
        assert!(Pin::new(&host, bundle.clone(), policy.clone()).is_err());
        let revision = format!("sha256:{}", "1".repeat(64));
        policy.nightly_releases.insert(
            "nightly".into(),
            Scope {
                repository: "owner/af".into(),
                configuration_digest: revision.clone(),
            },
        );
        let pin = Pin::new(&host, bundle.clone(), policy.clone()).unwrap();
        valid("scenario-pin-v7.json", &pin);
        valid("scenario-policy-v6.json", &policy);
        let def = &bundle.definitions[&bundle.root];
        assert_eq!(def.limits.max_tokens, 0);
        assert_eq!(def.steps.len(), 8);
        let mut changed = bundle.clone();
        let Kind::Action {
            required_condition, ..
        } = &mut changed.definitions.get_mut(&bundle.root).unwrap().steps[2].operation
        else {
            panic!()
        };
        *required_condition = Some("unrelated".into());
        assert!(Pin::new(&host, changed, policy.clone()).is_err());
        let tag = "nightly-2026.11.16".to_owned();
        let mut inputs = BTreeMap::from([
            ("subject".into(), Datum::Text(tag.clone())),
            ("nightly_tag".into(), Datum::NightlyTag(tag)),
            ("revision".into(), Datum::Text(revision)),
        ]);
        let grant = Grant {
            adapter: "nightly".into(),
            operation: Operation::NightlyBuildV1,
            destination: "github:owner/af".into(),
        };
        validate_dispatch(&policy, &grant, &inputs).unwrap();
        inputs.insert("subject".into(), Datum::Text("nightly-2026.11.17".into()));
        assert!(validate_dispatch(&policy, &grant, &inputs).is_err());
        inputs.insert("subject".into(), inputs["nightly_tag"].clone());
        assert!(validate_dispatch(&policy, &grant, &inputs).is_err());
        let mut wrong = policy;
        wrong
            .nightly_releases
            .get_mut("nightly")
            .unwrap()
            .configuration_digest = "not a digest".into();
        assert!(wrong.validate().is_err());
    }
}
