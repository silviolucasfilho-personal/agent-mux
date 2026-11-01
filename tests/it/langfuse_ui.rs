use agent_mux::app::langfuse_view::{LangfusePane, LangfuseView, Outcome};
use agent_mux::config::ResolvedLangfuse;
use agent_mux::tracing::langfuse::api::{
    LangfuseTraceSummary, ListTracesParams, get_trace, list_traces,
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

#[test]
fn http_mock_list_and_get_traces() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_thread = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);

            if req.contains("GET /api/public/traces?") {
                let body = r#"{
                    "data": [{
                        "id": "mock-trace-1",
                        "timestamp": "2026-11-01T10:00:00Z",
                        "name": "unit-test",
                        "sessionId": "mock-sess-1",
                        "projectId": "mock-proj",
                        "latency": 2.1,
                        "totalCost": 0.05
                    }],
                    "meta": { "page": 1, "limit": 50, "totalItems": 1, "totalPages": 1 }
                }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            } else if req.contains("GET /api/public/traces/mock-trace-1") {
                let body = r#"{
                    "id": "mock-trace-1",
                    "timestamp": "2026-11-01T10:00:00Z",
                    "name": "unit-test",
                    "sessionId": "mock-sess-1",
                    "projectId": "mock-proj",
                    "observations": [{
                        "id": "obs-1",
                        "type": "GENERATION",
                        "name": "mock-model",
                        "startTime": "2026-11-01T10:00:01Z"
                    }]
                }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        }
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

    let params = ListTracesParams::default();
    let resp = list_traces(&lf, &params).unwrap();
    assert_eq!(resp.data.len(), 1);
    assert_eq!(resp.data[0].id, "mock-trace-1");

    let detail = get_trace(&lf, "mock-trace-1").unwrap();
    assert_eq!(detail.id, "mock-trace-1");
    assert_eq!(detail.observations.len(), 1);
    assert_eq!(detail.observations[0].id, "obs-1");

    server_thread.join().unwrap();
}

#[test]
fn cli_help_command_succeeds() {
    assert!(agent_mux::tracing::langfuse::cli::run(&["--help".to_string()]).is_ok());
    assert!(agent_mux::tracing::langfuse::cli::run(&["-h".to_string()]).is_ok());
}
