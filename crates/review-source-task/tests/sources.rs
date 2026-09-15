use review_source_task::{jira::*, *};
use review_store::Cas;
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

fn issue() -> IssueInput {
    IssueInput {
        schema: "af.issue-input/1".into(),
        id: "10042".into(),
        key: "AF-42".into(),
        revision: "2026-09-12T10:00:00.000+0000".into(),
        summary: "Add offset and limit pagination".into(),
        description: "Preserve input values.".into(),
        acceptance: std::collections::BTreeMap::from([(
            "customfield_10001".into(),
            "Reject invalid bounds.".into(),
        )]),
    }
}
fn selector() -> JiraSelector {
    JiraSelector {
        site: "example.atlassian.net".into(),
        key: "AF-42".into(),
        acceptance_fields: vec!["customfield_10001".into()],
    }
}
fn response() -> Value {
    let i = issue();
    json!({"id":i.id,"key":i.key,"fields":{"summary":i.summary,"updated":i.revision,
    "description":{"type":"doc","version":1,"content":[{"type":"paragraph","content":[{"type":"text","text":i.description}]}]},
    "customfield_10001":"Reject invalid bounds.","private_undeclared":"Never send this field to a Worker"}})
}
struct Recorded {
    status: u16,
    body: Vec<u8>,
}
impl JiraTransport for Recorded {
    fn get_issue(
        &self,
        request: &JiraSelector,
        control: &SourceControl<'_>,
    ) -> Result<HttpResponse, SourceError> {
        control.check()?;
        assert_eq!(
            request.url().unwrap(),
            "https://example.atlassian.net/rest/api/3/issue/AF-42?fields=summary,description,updated,customfield_10001"
        );
        Ok(HttpResponse {
            status: self.status,
            body: self.body.clone(),
        })
    }
}
#[test]
fn replacement_sources_have_equivalent_requirements_and_exact_field_provenance() {
    let cancelled = AtomicBool::new(false);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    let local_json = serde_json::to_vec(&issue()).unwrap();
    let local_toml = toml::to_string(&issue()).unwrap();
    let recorded = Recorded {
        status: 200,
        body: serde_json::to_vec(&response()).unwrap(),
    };
    let select = selector();
    let adapters: Vec<Box<dyn TaskSource + '_>> = vec![
        Box::new(LocalIssueSource {
            bytes: &local_json,
            locator: "issue.json",
            format: LocalFormat::Json,
        }),
        Box::new(LocalIssueSource {
            bytes: local_toml.as_bytes(),
            locator: "issue.toml",
            format: LocalFormat::Toml,
        }),
        Box::new(JiraSource {
            selector: &select,
            transport: &recorded,
        }),
    ];
    let root = tempfile::tempdir().unwrap();
    let cas = Cas::open(root.path()).unwrap();
    let mut captures = vec![];
    for adapter in &adapters {
        let data = adapter.read(&control).unwrap();
        assert_eq!(data.issue.requirements(None), issue().requirements(None));
        let capture = data.capture(&cas).unwrap();
        capture.validate().unwrap();
        assert_eq!(cas.get(&capture.raw_source_id).unwrap(), data.raw);
        assert!(
            !serde_json::to_string(&data.issue.requirements(None))
                .unwrap()
                .contains("private_undeclared")
        );
        captures.push(capture);
        cancelled.store(true, Ordering::Release);
        assert!(matches!(
            adapter.read(&control),
            Err(SourceError::Cancelled)
        ));
        cancelled.store(false, Ordering::Release);
        let expired = SourceControl {
            deadline: Instant::now(),
            cancelled: &cancelled,
        };
        assert!(matches!(adapter.read(&expired), Err(SourceError::TimedOut)));
    }
    assert_eq!(captures[0].fields, captures[1].fields);
    assert_eq!(
        captures[0].fields["description"].text_id,
        captures[2].fields["description"].text_id
    );
    assert_ne!(
        captures[0].fields["description"].value_id,
        captures[2].fields["description"].value_id
    );
    assert_ne!(captures[0].raw_source_id, captures[1].raw_source_id);
    let mut changed = response();
    changed["fields"]["customfield_10001"] = json!("Reject zero limits too.");
    let changed = Recorded {
        status: 200,
        body: serde_json::to_vec(&changed).unwrap(),
    };
    let capture = JiraSource {
        selector: &select,
        transport: &changed,
    }
    .read(&control)
    .unwrap()
    .capture(&cas)
    .unwrap();
    assert_eq!(
        capture.source_revision, captures[2].source_revision,
        "A Jira timestamp is not an immutable response identity"
    );
    assert_ne!(capture.raw_source_id, captures[2].raw_source_id);
    assert_ne!(
        capture.fields["customfield_10001"],
        captures[2].fields["customfield_10001"]
    );
}
#[test]
fn jira_refuses_incomplete_changed_or_unsupported_sources_without_leaking_response_text() {
    let cancelled = AtomicBool::new(false);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    let select = selector();
    for (status, expected) in [
        (401, SourceError::Unauthorized),
        (403, SourceError::Unauthorized),
        (404, SourceError::NotFound),
        (429, SourceError::RateLimited),
        (302, SourceError::Unavailable),
        (500, SourceError::Unavailable),
    ] {
        let transport = Recorded {
            status,
            body: b"private token and diagnostics".to_vec(),
        };
        assert!(
            matches!(JiraSource {selector:&select,transport:&transport}.read(&control),Err(e) if e==expected)
        );
    }
    for pointer in ["/key", "/fields/description", "/fields/customfield_10001"] {
        let mut body = response();
        *body.pointer_mut(pointer).unwrap() = json!(null);
        let transport = Recorded {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        };
        assert!(matches!(
            JiraSource {
                selector: &select,
                transport: &transport
            }
            .read(&control),
            Err(SourceError::Invalid(_))
        ));
    }
    let mut body = response();
    body["fields"]["description"]["content"][0] =
        json!({"type":"inlineCard","attrs":{"url":"https://private.invalid"}});
    let transport = Recorded {
        status: 200,
        body: serde_json::to_vec(&body).unwrap(),
    };
    let error = match (JiraSource {
        selector: &select,
        transport: &transport,
    })
    .read(&control)
    {
        Err(e) => e,
        Ok(_) => panic!("unsupported ADF accepted"),
    };
    assert!(error.to_string().contains("ADF"));
    assert!(!error.to_string().contains("private.invalid"));
    let transport = Recorded {
        status: 200,
        body: vec![b' '; MAX_SOURCE_BYTES + 1],
    };
    assert!(matches!(
        JiraSource {
            selector: &select,
            transport: &transport
        }
        .read(&control),
        Err(SourceError::TooLarge)
    ));
    for site in [
        "example.atlassian.net@evil.invalid",
        "example.atlassian.net:443",
        "EXAMPLE.atlassian.net",
        "a.b.atlassian.net",
        "-bad.atlassian.net",
    ] {
        let mut bad = selector();
        bad.site = site.into();
        assert!(bad.url().is_err());
    }
    for key in ["AF-42?fields=*", "AF-0", "AF-1/other", "AF-1#x"] {
        let mut bad = selector();
        bad.key = key.into();
        assert!(bad.url().is_err());
    }
}
#[test]
fn adf_preserves_order_links_and_code_and_refuses_unknown_semantics() {
    let value = json!({"type":"doc","version":1,"content":[
        {"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Requirements"}]},
        {"type":"orderedList","attrs":{"order":3},"content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"Keep input"}]}]}]},
        {"type":"paragraph","content":[{"type":"text","text":"Spec","marks":[{"type":"link","attrs":{"href":"https://example.invalid/spec"}}]}]},
        {"type":"codeBlock","attrs":{"language":"python"},"content":[{"type":"text","text":"paginate(items, 0, 2)"}]}]});
    let normalized = adf::plain_text(&value).unwrap();
    assert!(normalized.starts_with("## Requirements\n3. Keep input\n"));
    assert!(normalized.contains("[Spec](https://example.invalid/spec)"));
    assert!(normalized.contains("```python\npaginate(items, 0, 2)\n```"));
    let mut deep = json!({"type":"text","text":"x"});
    for _ in 0..30 {
        deep = json!({"type":"blockquote","content":[deep]});
    }
    assert!(adf::plain_text(&json!({"type":"doc","version":1,"content":[deep]})).is_err());
}

#[test]
fn adf_list_continuations_preserve_nesting_and_ordered_marker_width() {
    let paragraph =
        |text: &str| json!({"type":"paragraph","content":[{"type":"text","text":text}]});
    let item = |content: Vec<Value>| json!({"type":"listItem","content":content});
    let bullet = |content: Vec<Value>| json!({"type":"bulletList","content":content});
    let document = |content: Vec<Value>| json!({"type":"doc","version":1,"content":content});
    let flat = document(vec![bullet(vec![
        item(vec![paragraph("a")]),
        item(vec![paragraph("b")]),
    ])]);
    let nested = document(vec![bullet(vec![item(vec![
        paragraph("a"),
        bullet(vec![item(vec![paragraph("b")])]),
    ])])]);
    assert_eq!(adf::plain_text(&flat).unwrap(), "- a\n- b");
    assert_eq!(adf::plain_text(&nested).unwrap(), "- a\n  - b");
    let ordered = document(vec![
        json!({"type":"orderedList","attrs":{"order":9},"content":[
            item(vec![paragraph("outer"), paragraph("continued"), bullet(vec![item(vec![paragraph("nested")])])]),
            item(vec![paragraph("next"), paragraph("still next")])
        ]}),
    ]);
    assert_eq!(
        adf::plain_text(&ordered).unwrap(),
        "9. outer\n   continued\n   - nested\n10. next\n    still next"
    );

    let cancelled = AtomicBool::new(false);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    let select = selector();
    let root = tempfile::tempdir().unwrap();
    let cas = Cas::open(root.path()).unwrap();
    let mut captures = vec![];
    for description in [flat, nested] {
        let mut body = response();
        body["fields"]["description"] = description;
        let recorded = Recorded {
            status: 200,
            body: serde_json::to_vec(&body).unwrap(),
        };
        let data = JiraSource {
            selector: &select,
            transport: &recorded,
        }
        .read(&control)
        .unwrap();
        captures.push(data.capture(&cas).unwrap());
    }
    assert_ne!(
        captures[0].fields["description"].text_id,
        captures[1].fields["description"].text_id
    );
    assert_eq!(
        cas.get(&captures[1].fields["description"].text_id).unwrap(),
        b"- a\n  - b"
    );
}
