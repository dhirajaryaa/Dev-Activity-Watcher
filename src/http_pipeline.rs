//! Turso `/v2/pipeline` HTTP client.
//!
//! Production `CloudBackend`: speaks the Hrana-over-HTTP protocol that both
//! libSQL and Turso-engine databases serve. One small hand-rolled client on
//! `ureq` (blocking, rustls) instead of the much larger `libsql` crate tree —
//! same philosophy as the hand-rolled Hyprland client.
//!
//! Transactions use the baton: request 1 sends `BEGIN` plus every statement;
//! if any step fails, request 2 sends `ROLLBACK`, otherwise `COMMIT`. The
//! baton keeps the server-side connection (and its open transaction) alive
//! between the two round trips, so a failed statement can never partially
//! apply. A connection that dies mid-transaction is rolled back by the
//! server when it times out.

use crate::cloud::{CloudBackend, CloudValue, Stmt};
use crate::{AtwError, Result};

pub struct HttpBackend {
    url: String,
    token: String,
    agent: ureq::Agent,
}

impl HttpBackend {
    pub fn new(url: &str, token: &str) -> Result<Self> {
        if url.trim().is_empty() {
            return Err(AtwError::Config("sync.url is empty".into()));
        }
        if token.trim().is_empty() {
            return Err(AtwError::Config("sync token is empty".into()));
        }
        // Accept both libsql:// and https:// — normalize to https.
        let url = url
            .trim()
            .trim_end_matches('/')
            .replacen("libsql://", "https://", 1);
        if !url.starts_with("https://") {
            return Err(AtwError::Config(format!(
                "sync.url must be an https URL, got {url}"
            )));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent("atw/0.1")
            .build();
        Ok(HttpBackend {
            url,
            token: token.to_string(),
            agent,
        })
    }

    /// Send one pipeline request; returns (baton, raw result objects).
    fn pipeline(
        &self,
        baton: Option<&str>,
        steps: Vec<serde_json::Value>,
    ) -> Result<(Option<String>, Vec<serde_json::Value>)> {
        let mut body = serde_json::json!({ "requests": steps });
        if let Some(b) = baton {
            body["baton"] = serde_json::Value::String(b.to_string());
        }
        let response = self
            .agent
            .post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .send_json(body)
            .map_err(map_ureq_error)?;
        let value: serde_json::Value = response
            .into_json()
            .map_err(|e| AtwError::Http(format!("pipeline response was not JSON: {e}")))?;
        let baton = value
            .get("baton")
            .and_then(|b| b.as_str())
            .map(|s| s.to_string());
        let results = value
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| AtwError::Http("pipeline response missing results".into()))?
            .clone();
        Ok((baton, results))
    }

    fn execute_step(sql: &str, params: &[CloudValue]) -> serde_json::Value {
        serde_json::json!({
            "type": "execute",
            "stmt": { "sql": sql, "args": args_json(params) }
        })
    }
}

impl CloudBackend for HttpBackend {
    fn transaction(&mut self, stmts: Vec<Stmt>) -> Result<()> {
        if stmts.is_empty() {
            return Ok(());
        }
        // Round trip 1: BEGIN + every statement on one connection.
        let mut steps = Vec::with_capacity(stmts.len() + 1);
        steps.push(serde_json::json!({
            "type": "execute",
            "stmt": { "sql": "BEGIN", "args": [] }
        }));
        for stmt in &stmts {
            steps.push(Self::execute_step(&stmt.sql, &stmt.params));
        }
        let (baton, results) = self.pipeline(None, steps)?;

        let mut failure: Option<String> = None;
        for (i, result) in results.iter().enumerate() {
            if result.get("type").and_then(|t| t.as_str()) == Some("error") {
                let message = result
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error")
                    .to_string();
                // Step 0 is BEGIN; the first statement is step 1.
                if i == 0 {
                    failure = Some(format!("BEGIN failed: {message}"));
                } else {
                    failure = Some(message);
                }
                break;
            }
        }

        let baton = match baton {
            Some(b) => b,
            None => {
                return Err(AtwError::Http(
                    "server closed the connection before COMMIT/ROLLBACK".into(),
                ))
            }
        };

        // Round trip 2: settle the transaction and free the connection.
        let settle = if failure.is_some() { "ROLLBACK" } else { "COMMIT" };
        let (_, results) = self.pipeline(
            Some(&baton),
            vec![
                serde_json::json!({
                    "type": "execute",
                    "stmt": { "sql": settle, "args": [] }
                }),
                serde_json::json!({ "type": "close" }),
            ],
        )?;

        if let Some(message) = failure {
            return Err(constraint_or_http(&message));
        }
        if let Some(err) = results.iter().find(|r| {
            r.get("type").and_then(|t| t.as_str()) == Some("error")
        }) {
            let message = err
                .pointer("/error/message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(constraint_or_http(&format!("{settle} failed: {message}")));
        }
        Ok(())
    }

    fn query(&mut self, sql: &str, params: Vec<CloudValue>) -> Result<Vec<Vec<CloudValue>>> {
        let (_, results) = self.pipeline(None, vec![Self::execute_step(sql, &params)])?;
        let first = results
            .into_iter()
            .next()
            .ok_or_else(|| AtwError::Http("pipeline returned no results".into()))?;
        match first.get("type").and_then(|t| t.as_str()) {
            Some("ok") => {
                let rows = first
                    .pointer("/response/result/rows")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .unwrap_or_default();
                Ok(rows
                    .iter()
                    .map(|row| {
                        row.as_array()
                            .map(|cells| cells.iter().map(decode_value).collect())
                            .unwrap_or_default()
                    })
                    .collect())
            }
            Some("error") => {
                let message = first
                    .pointer("/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown error");
                Err(constraint_or_http(message))
            }
            _ => Err(AtwError::Http("unexpected pipeline result".into())),
        }
    }
}

fn args_json(params: &[CloudValue]) -> serde_json::Value {
    serde_json::Value::Array(
        params
            .iter()
            .map(|v| match v {
                CloudValue::Null => serde_json::json!({ "type": "null" }),
                CloudValue::Text(s) => serde_json::json!({ "type": "text", "value": s }),
                // Hrana encodes integers as strings to avoid precision loss.
                CloudValue::Int(i) => {
                    serde_json::json!({ "type": "integer", "value": i.to_string() })
                }
            })
            .collect(),
    )
}

fn decode_value(cell: &serde_json::Value) -> CloudValue {
    match cell.get("type").and_then(|t| t.as_str()) {
        Some("text") => CloudValue::Text(
            cell.get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        ),
        Some("integer") => CloudValue::Int(
            cell.get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| cell.get("value").and_then(|v| v.as_i64()))
                .unwrap_or(0),
        ),
        _ => CloudValue::Null,
    }
}

/// SQLite reports duplicate keys as "UNIQUE constraint failed: …"; surface
/// those distinctly so duplicate batches can be acked instead of retried.
fn constraint_or_http(message: &str) -> AtwError {
    if message.contains("UNIQUE constraint failed") {
        AtwError::UniqueConstraint(message.to_string())
    } else {
        AtwError::Http(message.to_string())
    }
}

fn map_ureq_error(err: ureq::Error) -> AtwError {
    match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            AtwError::Http(format!("HTTP {code}: {}", truncate(&body, 300)))
        }
        ureq::Error::Transport(t) => AtwError::Http(format!("transport: {t}")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_encode_integers_as_strings() {
        let json = args_json(&[
            CloudValue::Text("abc".into()),
            CloudValue::Int(42),
            CloudValue::Null,
        ]);
        assert_eq!(json[1]["type"], "integer");
        assert_eq!(json[1]["value"], "42");
        assert_eq!(json[0]["value"], "abc");
        assert_eq!(json[2]["type"], "null");
    }

    #[test]
    fn decode_handles_text_integer_null() {
        assert_eq!(
            decode_value(&serde_json::json!({"type":"text","value":"x"})),
            CloudValue::Text("x".into())
        );
        assert_eq!(
            decode_value(&serde_json::json!({"type":"integer","value":"123"})),
            CloudValue::Int(123)
        );
        assert_eq!(
            decode_value(&serde_json::json!({"type":"null"})),
            CloudValue::Null
        );
    }

    #[test]
    fn url_normalization_rules() {
        let b = HttpBackend::new("libsql://db-org.turso.io/", "tok").unwrap();
        assert_eq!(b.url, "https://db-org.turso.io");
        assert!(HttpBackend::new("http://insecure.example", "tok").is_err());
        assert!(HttpBackend::new("", "tok").is_err());
        assert!(HttpBackend::new("https://x.turso.io", "").is_err());
    }

    #[test]
    fn unique_constraint_is_classified() {
        assert!(matches!(
            constraint_or_http("UNIQUE constraint failed: batches.batch_id"),
            AtwError::UniqueConstraint(_)
        ));
        assert!(matches!(constraint_or_http("boom"), AtwError::Http(_)));
    }
}
