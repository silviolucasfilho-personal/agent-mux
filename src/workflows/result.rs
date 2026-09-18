//! A session's answer: the fenced `workflow-result` block at the end of
//! its final text, validated against the step's schema. Implemented once,
//! above the harness, so a result never depends on which CLI produced it.

use super::document::Schema;
use serde_json::Value;
use std::collections::BTreeMap;

pub const FENCE: &str = "workflow-result";

/// The last fenced ```workflow-result block in `text`, parsed as JSON.
pub fn extract_fenced(text: &str) -> Result<Value, String> {
    let open = format!("```{FENCE}");
    let Some(start) = text.rfind(&open) else {
        return Err(format!("no fenced {FENCE} block in the answer"));
    };
    let body = &text[start + open.len()..];
    let body = body.strip_prefix('\r').unwrap_or(body);
    let body = body.strip_prefix('\n').unwrap_or(body);
    let Some(end) = body.find("```") else {
        return Err(format!("the {FENCE} block is not closed"));
    };
    let json = body[..end].trim();
    serde_json::from_str(json).map_err(|e| format!("the {FENCE} block is not valid JSON: {e}"))
}

/// What a session produced, as the interpreter stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Text(String),
    Object(Value),
    /// No usable answer; the reason is journaled.
    Null(String),
}

impl Outcome {
    pub fn value(&self) -> Value {
        match self {
            Outcome::Text(t) => Value::String(t.clone()),
            Outcome::Object(v) => v.clone(),
            Outcome::Null(_) => Value::Null,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Text(_) => "text",
            Outcome::Object(_) => "object",
            Outcome::Null(_) => "null",
        }
    }
}

/// Interprets a session's final text for a step: free text when no
/// schema is expected, else the fenced block checked against the schema.
pub fn interpret(
    final_text: Option<&str>,
    schema: Option<(&str, &Schema)>,
    all: &BTreeMap<String, Schema>,
) -> Result<Outcome, String> {
    let text = final_text.unwrap_or("").trim();
    match schema {
        None => {
            if text.is_empty() {
                Err("the session produced no answer".into())
            } else {
                Ok(Outcome::Text(text.to_string()))
            }
        }
        Some((name, schema)) => {
            let value = extract_fenced(text)?;
            let problems = schema.check(&value, all);
            if problems.is_empty() {
                Ok(Outcome::Object(value))
            } else {
                Err(format!(
                    "the {FENCE} block does not match schema {name}: {}",
                    problems.join("; ")
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::document::Field;

    #[test]
    fn extracts_the_last_block() {
        let text = "prose\n```workflow-result\n{\"a\":1}\n```\nmore\n```workflow-result\r\n{ \"a\": 2 }\r\n```\n";
        assert_eq!(extract_fenced(text).unwrap()["a"], 2);
        assert!(extract_fenced("nothing").unwrap_err().contains("no fenced"));
        assert!(
            extract_fenced("```workflow-result\n{")
                .unwrap_err()
                .contains("not closed")
        );
        assert!(
            extract_fenced("```workflow-result\nnope\n```")
                .unwrap_err()
                .contains("not valid JSON")
        );
    }

    #[test]
    fn interpret_text_and_objects() {
        let mut all = BTreeMap::new();
        let mut s = Schema::default();
        s.fields.insert(
            "refuted".into(),
            Field {
                ty: "boolean".into(),
                required: true,
                items: None,
                enum_values: None,
                description: None,
            },
        );
        all.insert("verdict".to_string(), s.clone());
        assert_eq!(
            interpret(Some(" hi "), None, &all).unwrap(),
            Outcome::Text("hi".into())
        );
        assert!(interpret(Some(""), None, &all).is_err());
        let ok = interpret(
            Some("x\n```workflow-result\n{\"refuted\": true}\n```"),
            Some(("verdict", &s)),
            &all,
        )
        .unwrap();
        assert_eq!(ok.kind(), "object");
        let bad = interpret(
            Some("```workflow-result\n{}\n```"),
            Some(("verdict", &s)),
            &all,
        )
        .unwrap_err();
        assert!(bad.contains("refuted: missing"), "{bad}");
        assert!(interpret(None, Some(("verdict", &s)), &all).is_err());
    }
}
