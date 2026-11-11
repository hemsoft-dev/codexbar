use std::sync::{atomic::AtomicBool, mpsc::channel};
use std::time::Duration;

use codexbar_providers::codex_app_server::{AppServer, AppServerError, Transport};
use serde_json::{Value, json};

const SECRET: &str = "SYNTHETIC_CREDENTIAL_7DA8";

fn messages() -> Vec<String> {
    vec![
        format!("Bearer {SECRET}"),
        format!("access_token={SECRET} refresh_token={SECRET}"),
        format!("code={SECRET}\nCookie: session={SECRET}"),
        format!("{SECRET}{}", "\u{754c}".repeat(500)),
    ]
}

struct Sink;
impl Transport for Sink {
    fn send(&mut self, _: &Value) -> Result<(), AppServerError> {
        Ok(())
    }
}

fn assert_private(error: &AppServerError) {
    assert!(
        !error.to_string().contains(SECRET),
        "CLI error leaked synthetic credential"
    );
    assert!(
        !format!("{error:?}").contains(SECRET),
        "debug error retained synthetic credential"
    );
}

#[test]
fn cli_request_errors_discard_credential_bearing_messages() {
    for message in messages() {
        let (tx, rx) = channel();
        tx.send(json!({"id":1,"error":{"code":-32000,"message":message}}))
            .unwrap();
        let error = AppServer::over(Box::new(Sink), rx).renew().unwrap_err();
        assert_private(&error);
    }
}

#[test]
fn cli_login_failures_discard_credential_bearing_messages() {
    for message in messages() {
        let (tx, rx) = channel();
        tx.send(json!({"id":1,"result":{"loginId":"synthetic-login","authUrl":"https://example.invalid"}}))
            .unwrap();
        tx.send(json!({"method":"account/login/completed","params":{"loginId":"synthetic-login","success":false,"error":message}})).unwrap();
        let error = AppServer::over(Box::new(Sink), rx)
            .begin_sign_in()
            .unwrap()
            .finish(Duration::from_secs(1), &AtomicBool::new(false))
            .unwrap_err();
        assert_private(&error);
    }
}
