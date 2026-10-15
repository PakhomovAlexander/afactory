//! The two recorded catalogs of 2026-10-15, as `tests/fixtures/provider-models/` keeps them:
//! Claude Code 2.1.300's `list_models` answer and codex-cli 0.162.1's `model/list` result. The
//! fields the recording elided (descriptions, the effort levels of the alias entries, the efforts
//! of the Codex models other than `gpt-6.1-sol` and `gpt-6-luna`) are filled to match what was
//! recorded; every exact ID, alias, display name and the effort facts the requirements cite are
//! verbatim.
use super::*;

const CLAUDE: &str =
    include_str!("../../../tests/fixtures/provider-models/claude-list-models.json");
const CODEX: &str = include_str!("../../../tests/fixtures/provider-models/codex-model-list.json");
const CACHE: &str = include_str!("../../../tests/fixtures/provider-models/codex-models-cache.json");

fn claude() -> CatalogReport {
    let (catalog, skipped) =
        parse_claude_list_models(&serde_json::from_str(CLAUDE).unwrap()).unwrap();
    CatalogReport {
        source: CLAUDE_SOURCE,
        version: Some("2.1.300".into()),
        fetched_at: None,
        catalog,
        skipped,
    }
}

fn codex() -> CatalogReport {
    let (catalog, skipped) = parse_codex_model_list(&serde_json::from_str(CODEX).unwrap()).unwrap();
    CatalogReport {
        source: CODEX_SOURCE,
        version: Some("0.162.1".into()),
        fetched_at: Some("2026-10-15T09:58:12Z".into()),
        catalog,
        skipped,
    }
}

fn listed(id: &str, kind: Kind, report: CatalogReport) -> ProviderCatalog {
    ProviderCatalog {
        id: id.into(),
        kind,
        outcome: CatalogOutcome::Listed(report),
    }
}

fn resolve(catalog: &ProviderCatalog, spelling: &str) -> Vec<String> {
    let request = parse_resolve(spelling).unwrap();
    table(std::slice::from_ref(catalog), Some(&request))
        .into_iter()
        .skip_while(|line| !line.starts_with("  resolve"))
        .collect()
}

#[test]
fn the_claude_catalog_has_one_row_per_exact_id_with_its_aliases() {
    let report = claude();
    let rows: Vec<(&str, &str, Vec<&str>)> = report
        .catalog
        .models
        .iter()
        .map(|model| {
            (
                model.id.as_str(),
                model.display_name.as_str(),
                model.aliases.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("claude-opus-5-5", "Opus 5.5", vec!["default", "opus"]),
            ("claude-fable-5-1", "Fable 5.1", vec!["fable"]),
            ("claude-sonnet-5-5", "Sonnet 5.5", vec!["sonnet"]),
            ("claude-haiku-5-5", "Haiku 5.5", vec!["haiku"]),
        ]
    );
    let fable = report.catalog.model("claude-fable-5-1").unwrap();
    assert_eq!(fable.efforts, rules::CLAUDE_EFFORTS);
    assert_eq!(fable.default_effort, None);
    assert!(fable.other_names.contains(&"Fable".to_owned()));
    // The picker never lists the exact IDs the API still accepts.
    assert!(report.catalog.model("claude-opus-4-8").is_none());
    assert_eq!(report.skipped, 0);
}

#[test]
fn the_codex_catalog_keeps_each_models_levels_and_default() {
    let report = codex();
    let ids: Vec<&str> = report
        .catalog
        .models
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol"
        ]
    );
    let sol = report.catalog.model("gpt-6.1-sol").unwrap();
    assert_eq!(sol.display_name, "GPT-6.1-Sol");
    assert_eq!(sol.default_effort.as_deref(), Some("low"));
    assert_eq!(
        effort_levels(sol),
        "low* medium high xhigh max ultra",
        "the default is marked"
    );
    let luna = report.catalog.model("gpt-6-luna").unwrap();
    assert!(!luna.efforts.contains(&"ultra".to_owned()));
}

#[test]
fn an_entry_outside_g_is_left_out_and_counted() {
    let mut list: serde_json::Value = serde_json::from_str(CODEX).unwrap();
    list["data"][0]["id"] = "gpt 6/sol".into();
    let (catalog, skipped) = parse_codex_model_list(&list).unwrap();
    assert_eq!(skipped, 1);
    assert_eq!(catalog.models.len(), 4);
    assert!(parse_codex_model_list(&serde_json::json!({})).is_err());
    assert!(parse_claude_list_models(&serde_json::json!({"models": 1})).is_err());
}

#[test]
fn the_table_prints_the_header_the_rows_and_what_the_list_proves() {
    let lines = table(
        &[
            listed("claude-work", Kind::Claude, claude()),
            listed("codex-main", Kind::Codex, codex()),
        ],
        None,
    );
    let text = lines.join("\n");
    for part in [
        "claude-work (claude)  source claude list_models  version 2.1.300  freshness live",
        "codex-main (codex)  source codex app-server model/list  version 0.162.1  freshness models_cache.json fetched_at 2026-10-15T09:58:12Z",
        "EXACT ID",
        "af recognises these spellings for this Provider",
        "proven only by the charged `af provider doctor`",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let fable = lines
        .iter()
        .find(|line| line.trim_start().starts_with("claude-fable-5-1"))
        .unwrap();
    for part in ["Fable 5.1", "fable", "low medium high xhigh max"] {
        assert!(fable.contains(part), "{part}: {fable}");
    }
}

#[test]
fn an_unavailable_catalog_prints_the_static_rules_and_invents_nothing() {
    let catalogs = [ProviderCatalog {
        id: "claude-work".into(),
        kind: Kind::Claude,
        outcome: CatalogOutcome::Unavailable("claude exited 1".into()),
    }];
    assert_eq!(exit_code(&catalogs), EXIT_USAGE_UNAVAILABLE);
    let text = table(&catalogs, None).join("\n");
    assert!(text.contains("catalog unavailable"), "{text}");
    assert!(text.contains("low, medium, high, xhigh, max"), "{text}");
    assert!(text.contains("fable → claude-fable-5-1"), "{text}");
    assert!(!text.contains("EXACT ID"), "{text}");
    let refused = [ProviderCatalog {
        outcome: CatalogOutcome::CliRefused("incompatible".into()),
        ..catalogs[0].clone()
    }];
    assert_eq!(exit_code(&refused), EXIT_PROVIDER_CLI_MISSING);
    let document = document(&catalogs, None, 7);
    assert_eq!(document["providers"][0]["state"], "unavailable");
    assert_eq!(document["providers"][0]["models"], serde_json::json!([]));
}

#[test]
fn resolve_names_the_exact_id_and_prints_both_configurations() {
    let claude = listed("claude-work", Kind::Claude, claude());
    let lines = resolve(&claude, "Fable 5.1/xhigh").join("\n");
    for part in [
        "resolve  `Fable 5.1/xhigh` on claude-work (claude)",
        "`Fable 5.1` is the display name of the exact ID claude-fable-5-1",
        "xhigh: confirmed, claude-fable-5-1 lists low, medium, high, xhigh, max",
        "reviewer.toml  [runner] program = \"claude\"  args = [{ value = \"--model\" }, { value = \"claude-fable-5-1\" }, { value = \"--effort\" }, { value = \"xhigh\" }]",
        "worker.toml    [runner]",
        "kind = \"model\"",
        "provider_kind = \"claude\"",
        "model = \"claude-fable-5-1\"",
        "effort = \"xhigh\"",
        "bindings       [providers] \"PACKAGE\" = \"claude-work\"",
        "--provider NODE=claude-work",
    ] {
        assert!(lines.contains(part), "{part}:\n{lines}");
    }
    let lines = resolve(&claude, "fable").join("\n");
    assert!(
        lines.contains("`fable` is an alias of the exact ID claude-fable-5-1; af binds exact IDs"),
        "{lines}"
    );
    let lines = resolve(&claude, "claude-fable-5.1").join("\n");
    assert!(
        lines.contains("nearest catalog IDs: claude-fable-5-1"),
        "{lines}"
    );
    assert!(!lines.contains("reviewer.toml"), "{lines}");
    let lines = resolve(&claude, "claude-opus-4-8").join("\n");
    assert!(
        lines.contains(
            "`claude-opus-4-8` is not in this harness's catalog; passes af's spelling rule; usability only by af provider doctor"
        ),
        "{lines}"
    );
    let codex = listed("codex-main", Kind::Codex, codex());
    let lines = resolve(&codex, "GPT-6 Sol/ultra").join("\n");
    assert!(lines.contains("names the exact ID gpt-6-sol"), "{lines}");
    assert!(lines.contains("ultra: confirmed"), "{lines}");
    assert!(
        lines.contains("{ value = \"model_reasoning_effort=\\\"ultra\\\"\" }"),
        "{lines}"
    );
    let lines = resolve(&codex, "gpt-6-luna/ultra").join("\n");
    assert!(
        lines.contains(
            "ultra: effort not listed for gpt-6-luna, which lists low, medium, high, xhigh, max"
        ),
        "{lines}"
    );
    assert!(
        !lines.contains("worker.toml"),
        "af never substitutes: {lines}"
    );
}

/// The printed configurations are af's own schemas: the worker block is a `TaskWorkerRunner`
/// and the reviewer args read back as the resolved pair.
#[test]
fn the_configurations_parse_as_afs_own_schemas() {
    for (kind, model, effort) in [
        (Kind::Claude, "claude-fable-5-1", "xhigh"),
        (Kind::Codex, "gpt-6-sol", "ultra"),
    ] {
        let configuration = configuration("p-1", kind, model, effort);
        let runner: toml::Value = toml::from_str(&configuration.worker.join("\n")).unwrap();
        let runner: review_config::task::catalog::TaskWorkerRunner =
            runner["runner"].clone().try_into().unwrap();
        assert_eq!(
            runner,
            review_config::task::catalog::TaskWorkerRunner::Model {
                provider_kind: kind.name().into(),
                model: model.into(),
                effort: effort.into(),
            }
        );
        let reviewer = configuration
            .reviewer
            .replace("[runner] ", "[runner]\n")
            .replace("  args", "\nargs");
        let manifest =
            format!("name = \"x\"\nversion = \"1.0.0\"\nsubjects = [\"diff\"]\n{reviewer}\n");
        let manifest: review_config::lock::PackageManifest = toml::from_str(&manifest).unwrap();
        let settings =
            review_config::lock::reviewer_runner_settings_from_manifest(&manifest).unwrap();
        assert_eq!(
            (settings.model.as_str(), settings.effort.as_str()),
            (model, effort)
        );
    }
}

#[test]
fn the_cache_freshness_is_read_only_from_a_bounded_regular_file() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(cache_fetched_at(home.path()), None);
    std::fs::write(home.path().join("models_cache.json"), CACHE).unwrap();
    assert_eq!(
        cache_fetched_at(home.path()).as_deref(),
        Some("2026-10-15T09:58:12Z")
    );
    std::fs::write(
        home.path().join("models_cache.json"),
        r#"{"fetched_at":"$(rm -rf /)"}"#,
    )
    .unwrap();
    assert_eq!(cache_fetched_at(home.path()), None);
    assert_eq!(
        version_word("codex-cli 0.162.1").as_deref(),
        Some("0.162.1")
    );
}

#[test]
fn a_resolve_spelling_is_bounded_and_its_effort_is_in_g() {
    assert_eq!(
        parse_resolve("Fable 5.1/xhigh").unwrap(),
        ResolveRequest {
            spelling: "Fable 5.1".into(),
            effort: Some("xhigh".into())
        }
    );
    assert_eq!(parse_resolve("fable").unwrap().effort, None);
    for bad in ["", "a;b", "fable/x y", "x\ny"] {
        assert!(parse_resolve(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn a_control_response_is_found_by_its_request_id() {
    let captured = format!(
        "{}\n{}\n",
        serde_json::json!({"type":"system","subtype":"init"}),
        serde_json::json!({"type":"control_response","response":{"subtype":"error","request_id":MODELS_REQUEST,"error":"Unsupported control request subtype"}}),
    );
    assert_eq!(
        control_response(captured.as_bytes(), MODELS_REQUEST),
        Some(Err(
            "claude answered `list_models` with an error: Unsupported control request subtype"
                .into()
        ))
    );
    assert_eq!(control_response(captured.as_bytes(), VERSION_REQUEST), None);
}
