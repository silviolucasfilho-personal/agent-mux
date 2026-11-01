use agent_mux::app::langfuse_view::{LangfusePane, LangfuseView, Outcome};
use agent_mux::config::ResolvedLangfuse;
use agent_mux::tracing::langfuse::api::{
    LangfuseTraceSummary, ListTracesParams, check_health, get_trace, list_traces,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn make_key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn fake_langfuse() -> ResolvedLangfuse {
    ResolvedLangfuse {
        host: "https://cloud.langfuse.com".into(),
        public_key: "pk-lf-test".into(),
        secret_key: "sk-lf-test".into(),
        project_id: Some("proj_test123".into()),
        flush_interval_ms: 100,
        secret_from_file: false,
        legacy_keys: false,
    }
}

#[test]
fn unconfigured_view_handles_keys_safely() {
    let mut view = LangfuseView::new(None, None);
    assert!(view.langfuse.is_none());
    assert_eq!(view.pane, LangfusePane::Traces);

    // Opening web when unconfigured is a no-op
    assert_eq!(
        view.handle_key(&make_key(KeyCode::Char('w'))),
        Outcome::None
    );

    // Tab cycles pane
    assert_eq!(view.handle_key(&make_key(KeyCode::Tab)), Outcome::None);
    assert_eq!(view.pane, LangfusePane::Tree);

    // Esc closes
    assert_eq!(view.handle_key(&make_key(KeyCode::Esc)), Outcome::Close);
}

#[test]
fn pane_navigation_and_zoom() {
    let mut view = LangfuseView::new(Some(fake_langfuse()), None);
    assert_eq!(view.pane, LangfusePane::Traces);

    view.handle_key(&make_key(KeyCode::Tab));
    assert_eq!(view.pane, LangfusePane::Tree);

    view.handle_key(&make_key(KeyCode::Tab));
    assert_eq!(view.pane, LangfusePane::Detail);

    view.handle_key(&make_key(KeyCode::Tab));
    assert_eq!(view.pane, LangfusePane::Traces);

    view.handle_key(&make_key(KeyCode::BackTab));
    assert_eq!(view.pane, LangfusePane::Detail);

    // Zoom toggle
    assert!(!view.zoomed);
    view.handle_key(&make_key(KeyCode::Char('z')));
    assert!(view.zoomed);
    view.handle_key(&make_key(KeyCode::Char('z')));
    assert!(!view.zoomed);
}

#[test]
fn filter_input_flow() {
    let mut view = LangfuseView::new(Some(fake_langfuse()), None);
    assert!(!view.text_entry_active());

    // Press '/' to start filtering
    view.handle_key(&make_key(KeyCode::Char('/')));
    assert!(view.text_entry_active());

    // Type "test"
    view.handle_key(&make_key(KeyCode::Char('t')));
    view.handle_key(&make_key(KeyCode::Char('e')));
    view.handle_key(&make_key(KeyCode::Char('s')));
    view.handle_key(&make_key(KeyCode::Char('t')));
    assert_eq!(view.filter_input.as_deref(), Some("test"));

    // Enter commits filter
    view.handle_key(&make_key(KeyCode::Enter));
    assert!(!view.text_entry_active());
    assert_eq!(view.active_filter.as_deref(), Some("test"));
}

#[test]
fn web_launch_url_resolution() {
    let mut view = LangfuseView::new(Some(fake_langfuse()), Some("claude:s1".into()));
    view.traces = vec![LangfuseTraceSummary {
        id: "tr-456".into(),
        timestamp: "2026-11-01T10:00:00Z".into(),
        name: Some("test trace".into()),
        session_id: Some("claude:s1".into()),
        user_id: None,
        project_id: Some("proj_test123".into()),
        release: None,
        version: None,
        tags: vec![],
        public: None,
        bookmarked: None,
        latency: Some(1.5),
        total_cost: Some(0.01),
        observations: vec![],
        scores: vec![],
    }];
    view.selected_trace = 0;

    let outcome = view.handle_key(&make_key(KeyCode::Char('w')));
    match outcome {
        Outcome::OpenUrl(url) => {
            assert_eq!(
                url,
                "https://cloud.langfuse.com/project/proj_test123/traces/tr-456"
            );
        }
        other => panic!("expected Outcome::OpenUrl, got {other:?}"),
    }

    let o_outcome = view.handle_key(&make_key(KeyCode::Char('o')));
    match o_outcome {
        Outcome::OpenUrl(url) => {
            assert_eq!(
                url,
                "https://cloud.langfuse.com/project/proj_test123/traces/tr-456"
            );
        }
        other => panic!("expected Outcome::OpenUrl, got {other:?}"),
    }
}

/// Serves one canned answer per request and returns the request lines seen.
fn mock_langfuse(
    answers: Vec<(&'static str, String)>,
) -> (ResolvedLangfuse, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let mut seen = Vec::new();
        for (status, body) in answers {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            seen.push(req.lines().next().unwrap_or_default().to_string());
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
        seen
    });
    let lf = ResolvedLangfuse {
        host: format!("http://127.0.0.1:{port}"),
        public_key: "pk-test".into(),
        secret_key: "sk-test".into(),
        project_id: Some("mock-proj".into()),
        flush_interval_ms: 100,
        secret_from_file: false,
        legacy_keys: false,
    };
    (lf, server)
}

#[test]
fn http_mock_list_and_get_traces_over_v2_observations() {
    let list = r#"{
        "data": [{
            "id": "root-1", "traceId": "mock-trace-1", "startTime": "2026-11-01T10:00:00Z",
            "projectId": "mock-proj", "type": "SPAN", "isRootObservation": true,
            "traceName": "unit-test", "sessionId": "mock-sess-1", "latency": 2.1
        }],
        "meta": { "cursor": "next==" }
    }"#;
    let detail_page_1 = r#"{
        "data": [{
            "id": "root-1", "traceId": "mock-trace-1", "startTime": "2026-11-01T10:00:00Z",
            "projectId": "mock-proj", "type": "SPAN", "isRootObservation": true,
            "traceName": "unit-test", "sessionId": "mock-sess-1"
        }],
        "meta": { "cursor": "more==" }
    }"#;
    let detail_page_2 = r#"{
        "data": [{
            "id": "obs-1", "traceId": "mock-trace-1", "startTime": "2026-11-01T10:00:01Z",
            "projectId": "mock-proj", "type": "GENERATION", "parentObservationId": "root-1",
            "name": "mock-model"
        }],
        "meta": {}
    }"#;
    let (lf, server) = mock_langfuse(vec![
        ("200 OK", list.into()),
        ("200 OK", detail_page_1.into()),
        ("200 OK", detail_page_2.into()),
    ]);

    let params = ListTracesParams {
        session_id: Some("mock-sess-1".into()),
        ..Default::default()
    };
    let page = list_traces(&lf, &params).unwrap();
    assert_eq!(page.data.len(), 1);
    assert_eq!(page.data[0].id, "mock-trace-1");
    assert_eq!(page.data[0].name.as_deref(), Some("unit-test"));
    assert_eq!(page.next_cursor.as_deref(), Some("next=="));

    let detail = get_trace(&lf, "mock-trace-1").unwrap();
    assert_eq!(detail.id, "mock-trace-1");
    let ids: Vec<&str> = detail.observations.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["root-1", "obs-1"], "both cursor pages are read");

    let seen = server.join().unwrap();
    assert!(
        seen[0].starts_with("GET /api/public/v2/observations?"),
        "{}",
        seen[0]
    );
    assert!(seen[0].contains("isRootObservation=true"), "{}", seen[0]);
    assert!(
        seen[0].contains("fromStartTime="),
        "every read is bounded: {}",
        seen[0]
    );
    assert!(seen[0].contains("sessionId=mock-sess-1"), "{}", seen[0]);
    assert!(seen[1].contains("traceId=mock-trace-1"), "{}", seen[1]);
    assert!(seen[2].contains("cursor=more%3D%3D"), "{}", seen[2]);
    assert!(
        seen.iter().all(|l| !l.contains("/api/public/traces")),
        "the v1 trace endpoints are never called: {seen:?}"
    );
}

#[test]
fn health_check_rejects_bad_keys_and_reports_the_project() {
    let (lf, server) = mock_langfuse(vec![
        (
            "401 Unauthorized",
            r#"{"message":"Invalid credentials. Confirm that you've configured the correct host.","error":"UnauthorizedError"}"#.into(),
        ),
        (
            "200 OK",
            r#"{"data":[{"id":"cm-proj","name":"agent-mux","organization":{},"metadata":{}}]}"#
                .into(),
        ),
    ]);
    let err = check_health(&lf).unwrap_err();
    assert!(err.starts_with("authentication failed (HTTP 401)"), "{err}");
    let project = check_health(&lf).unwrap();
    assert_eq!(project.id, "cm-proj");
    assert_eq!(project.name.as_deref(), Some("agent-mux"));
    let seen = server.join().unwrap();
    assert!(
        seen.iter()
            .all(|l| l.starts_with("GET /api/public/projects ")),
        "health needs an authenticated endpoint, not /health: {seen:?}"
    );
}

#[test]
fn cli_help_command_succeeds() {
    assert!(agent_mux::tracing::langfuse::cli::run(&["--help".to_string()]).is_ok());
    assert!(agent_mux::tracing::langfuse::cli::run(&["-h".to_string()]).is_ok());
}
