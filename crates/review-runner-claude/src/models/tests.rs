use super::*;

fn row(id: &str, name: &str, aliases: &[&str], efforts: &[&str]) -> CatalogModel {
    CatalogModel {
        id: id.into(),
        display_name: name.into(),
        other_names: vec![],
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        efforts: efforts.iter().map(|level| (*level).to_owned()).collect(),
        default_effort: None,
        hidden: false,
    }
}

/// The rows of the Claude Code 2.1.300 `list_models` catalog of 2026-10-15, with `fable` added
/// from Claude Code's documentation.
fn claude() -> Catalog {
    Catalog {
        models: vec![
            row(
                "claude-opus-5-5",
                "Opus 5.5",
                &["default", "opus"],
                &CLAUDE_EFFORTS,
            ),
            row("claude-fable-5-1", "Fable 5.1", &[], &CLAUDE_EFFORTS),
            row(
                "claude-sonnet-5-5",
                "Sonnet 5.5",
                &["sonnet"],
                &CLAUDE_EFFORTS,
            ),
            row("claude-haiku-5-5", "Haiku 5.5", &["haiku"], &CLAUDE_EFFORTS),
        ],
    }
    .with_documented_aliases(Kind::Claude)
}

/// The rows of the codex-cli 0.162.1 `model/list` catalog of 2026-10-15.
fn codex() -> Catalog {
    let all = ["low", "medium", "high", "xhigh", "max", "ultra"];
    Catalog {
        models: vec![
            row("gpt-6.1-sol", "GPT-6.1-Sol", &[], &all),
            row("gpt-6-astra", "GPT-6-Astra", &[], &all),
            row("gpt-6-sol", "GPT-6-Sol", &[], &all),
            row("gpt-6-luna", "GPT-6-Luna", &[], &all[..5]),
            row("gpt-5.6-sol", "GPT-5.6-Sol", &[], &all[..4]),
        ],
    }
}

#[test]
fn the_documented_alias_is_added_beside_the_catalogs_own() {
    let catalog = claude();
    assert_eq!(catalog.alias("fable").unwrap().id, "claude-fable-5-1");
    assert_eq!(catalog.alias("default").unwrap().id, "claude-opus-5-5");
    // An alias the catalog already lists is not added twice.
    assert_eq!(
        catalog.model("claude-opus-5-5").unwrap().aliases,
        ["default", "opus"]
    );
}

#[test]
fn static_rules_refuse_aliases_misspellings_unversioned_ids_and_unknown_efforts() {
    let refused = |kind, model, effort| check_static(kind, model, Some(effort)).unwrap_err();
    let alias = refused(Kind::Claude, "fable", "xhigh");
    assert_eq!(
        alias.rule,
        Rule::Alias {
            exact: "claude-fable-5-1".into(),
            documented: true
        }
    );
    assert_eq!(alias.nearest, ["claude-fable-5-1"]);
    assert_eq!(
        refused(Kind::Claude, "Opus", "high").rule,
        Rule::Alias {
            exact: "claude-opus-5-5".into(),
            documented: true
        }
    );
    let misspelled = refused(Kind::Claude, "claude-fable-5.1", "xhigh");
    assert_eq!(misspelled.rule, Rule::Misspelled);
    assert_eq!(misspelled.nearest, ["claude-fable-5-1"]);
    for model in ["claude-opus", "claude-opus-latest", "gpt"] {
        let kind = if model == "gpt" {
            Kind::Codex
        } else {
            Kind::Claude
        };
        assert_eq!(
            refused(kind, model, "high").rule,
            Rule::Unversioned,
            "{model}"
        );
    }
    assert_eq!(
        refused(Kind::Claude, "claude-opus", "high").nearest,
        ["claude-opus-5-5"]
    );
    let effort = refused(Kind::Claude, "claude-fable-5-1", "ultra");
    assert_eq!(effort.rule, Rule::EffortVocabulary);
    assert!(
        effort
            .fact(Kind::Claude, "claude-fable-5-1", Some("ultra"))
            .ends_with("low, medium, high, xhigh, max"),
    );
    assert_eq!(
        refused(Kind::Claude, "claude-fable-5-1", "xhihg").nearest,
        ["xhigh"]
    );
    // G first: a value outside it is refused with G's words whatever else it fails.
    assert_eq!(
        refused(Kind::Claude, "fable@home", "ultra").rule,
        Rule::NotPrintable { option: "--model" }
    );
    assert_eq!(
        refused(Kind::Claude, "claude-fable-5-1", "a b").rule,
        Rule::NotPrintable { option: "--effort" }
    );
    for (kind, model, effort) in [
        (Kind::Claude, "claude-fable-5-1", "xhigh"),
        (Kind::Claude, "claude-opus-4-8", "max"),
        (Kind::Claude, "us.anthropic.claude-opus-5-5-v1:0", "high"),
        (Kind::Codex, "gpt-6-sol", "ultra"),
        (Kind::Codex, "gpt-6-sol", "high"),
    ] {
        assert_eq!(check_static(kind, model, Some(effort)), Ok(()), "{model}");
    }
    assert_eq!(check_static(Kind::Claude, "claude-fable-5-1", None), Ok(()));
}

#[test]
fn catalog_rules_confirm_listed_pairs_and_accept_custom_ids_as_unverified() {
    let (claude, codex) = (claude(), codex());
    let verdict = |kind, catalog, model, effort| check(kind, catalog, model, Some(effort));
    assert_eq!(
        verdict(Kind::Claude, Some(&claude), "claude-fable-5-1", "xhigh"),
        Verdict::Ok
    );
    assert_eq!(
        verdict(Kind::Codex, Some(&codex), "gpt-6-sol", "ultra"),
        Verdict::Ok
    );
    assert_eq!(
        verdict(Kind::Claude, Some(&claude), "claude-opus-4-8", "high"),
        Verdict::Unverified
    );
    assert_eq!(
        verdict(Kind::Claude, None, "claude-fable-5-1", "xhigh"),
        Verdict::Unverified
    );
    let Verdict::Refused(luna) = verdict(Kind::Codex, Some(&codex), "gpt-6-luna", "ultra") else {
        panic!("gpt-6-luna lists no ultra");
    };
    assert_eq!(
        luna.label(Kind::Codex, "gpt-6-luna", Some("ultra")),
        "effort not listed: low, medium, high, xhigh, max"
    );
    let Verdict::Refused(default) = verdict(Kind::Claude, Some(&claude), "default", "high") else {
        panic!("default is an alias");
    };
    assert_eq!(
        default.label(Kind::Claude, "default", Some("high")),
        "alias -> claude-opus-5-5"
    );
    let Verdict::Refused(near) = verdict(Kind::Claude, Some(&claude), "claude-fabel-5-1", "high")
    else {
        panic!("a near miss of a listed ID");
    };
    assert_eq!(near.rule, Rule::Misspelled);
    assert_eq!(near.nearest[0], "claude-fable-5-1");
    assert_eq!(
        Verdict::Unverified.label(Kind::Claude, "x-1", None),
        "unverified against the catalog"
    );
}

#[test]
fn resolve_reads_every_human_spelling_and_never_substitutes() {
    let (claude, codex) = (claude(), codex());
    assert_eq!(
        resolve(Kind::Claude, &claude, "Fable 5.1"),
        Resolution::DisplayName {
            id: "claude-fable-5-1".into()
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "fable"),
        Resolution::Alias {
            id: "claude-fable-5-1".into()
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "claude-fable-5-1"),
        Resolution::Exact {
            id: "claude-fable-5-1".into()
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "claude-fable-5.1"),
        Resolution::Misspelled {
            nearest: vec!["claude-fable-5-1".into()]
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "claude-opus-4-8"),
        Resolution::NotListed { rule: None }
    );
    assert!(matches!(
        resolve(Kind::Claude, &claude, "claude-opus"),
        Resolution::NotListed { rule: Some(_) }
    ));
    assert_eq!(
        resolve(Kind::Codex, &codex, "GPT-6 Sol"),
        Resolution::FamilyVersion {
            id: "gpt-6-sol".into()
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "Sonnet 5.5"),
        Resolution::DisplayName {
            id: "claude-sonnet-5-5".into()
        }
    );
    assert_eq!(
        resolve(Kind::Claude, &claude, "Haiku 5 5"),
        Resolution::FamilyVersion {
            id: "claude-haiku-5-5".into()
        }
    );
}

#[test]
fn every_refusal_names_the_provider_the_fact_the_nearest_spellings_and_the_command() {
    let provider = ProviderRef {
        id: Some("claude-work"),
        kind: Kind::Claude,
    };
    let alias = check_static(Kind::Claude, "fable", Some("xhigh")).unwrap_err();
    let hint = alias.hint(provider, "fable", Some("xhigh"));
    for part in [
        "af refused the binding of model fable (effort xhigh) for Provider claude-work (claude) \
         (model_not_accepted)",
        "the exact ID claude-fable-5-1",
        "af binds exact IDs",
        "nearest accepted spellings: claude-fable-5-1",
        "`af provider models claude-work` (token-free)",
    ] {
        assert!(hint.contains(part), "{part}: {hint}");
    }
    let diagnostic = format!("{}{hint}; Worker text", hint::DIAGNOSTIC_PREFIX);
    assert_eq!(hint::find(&diagnostic), Some(hint.as_str()));
    let effort = check_static(Kind::Claude, "claude-fable-5-1", Some("ultra")).unwrap_err();
    let hint = effort.hint(provider, "claude-fable-5-1", Some("ultra"));
    assert!(
        hint.contains(
            "the effort level `ultra` is not one Claude Code accepts: low, medium, high, xhigh, max"
        ),
        "{hint}"
    );
    // A package parser knows only the kind.
    let unknown = ProviderRef {
        id: None,
        kind: Kind::Codex,
    };
    let hint = check_static(Kind::Codex, "gpt", Some("high"))
        .unwrap_err()
        .hint(unknown, "gpt", Some("high"));
    assert!(hint.contains("for a codex Provider"), "{hint}");
    assert!(hint.contains(VERSIONED_RULE), "{hint}");
    assert!(hint.contains("`af provider models` (token-free)"), "{hint}");
    // A value outside G is never echoed, only its length.
    let outside = check_static(Kind::Claude, "/Users/alice/keys-1", Some("high")).unwrap_err();
    let hint = outside.hint(provider, "/Users/alice/keys-1", Some("high"));
    assert!(hint.contains("its --model value (19 bytes)"), "{hint}");
    assert!(
        !hint.contains("alice") && !hint.contains("versioned"),
        "{hint}"
    );
}

#[test]
fn nearest_spellings_share_a_family_or_are_two_edits_away() {
    let ids = ["gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-sol"];
    assert_eq!(
        nearest_of(ids.into_iter(), "gpt-6.2-sol"),
        ["gpt-6.1-sol", "gpt-5.6-sol", "gpt-6-sol"]
    );
    assert_eq!(nearest_of(ids.into_iter(), "gpt-6-lunar"), ["gpt-6-luna"]);
    assert!(nearest_of(ids.into_iter(), "claude-opus-5-5").is_empty());
    assert_eq!(distance("kitten", "sitting"), 3);
    assert_eq!(distance("", "abc"), 3);
}
