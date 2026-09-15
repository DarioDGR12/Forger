//! Multi-agent quality mode.
//!
//! N candidate [`crate::agent::Agent`] loops (default 2, max 3) run the same
//! task in parallel. A separate reviewer loop scores each complete result
//! 0–10 and the highest score wins. Merge is "pick the best complete
//! candidate", not a diff merge — that is future work.
//!
//! Each extra candidate multiplies token spend. Keep the default at 2.

use crate::agent::{Agent, AgentEvent, TurnOutcome, UserTurn};
use crate::error::AgentError;
use crate::message::Message;
use crate::plugin::Provider;
use crate::session::Session;
use crate::tool::ToolSpec;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_CANDIDATES: u8 = 2;
pub const MAX_CANDIDATES: u8 = 3;

const REVIEWER_PROMPT: &str = r#"You are a QA reviewer for a coding agent. Score the candidate solution from 0 to 10.
Reply with a single JSON object, no markdown: {"score": <0-10 integer>, "rationale": "<one sentence>"}.
Score 0 if the candidate is empty, crashed, or clearly incomplete."#;

#[derive(Debug, Clone)]
pub struct QualityConfig {
    pub candidates: u8,
}

impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            candidates: DEFAULT_CANDIDATES,
        }
    }
}

impl QualityConfig {
    pub fn clamped(self) -> Self {
        Self {
            candidates: self.candidates.clamp(1, MAX_CANDIDATES).max(1),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub index: usize,
    pub score: u8,
    pub rationale: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityReport {
    pub winner_index: usize,
    pub candidates: Vec<ScoredCandidate>,
}

pub struct QualityRunner {
    pub config: QualityConfig,
    pub reviewer: Arc<dyn Provider>,
}

impl QualityRunner {
    pub fn new(reviewer: Arc<dyn Provider>, config: QualityConfig) -> Self {
        Self {
            config: config.clamped(),
            reviewer,
        }
    }

    /// Run `n` independent copies of `make_agent` on the same task, score, pick best.
    pub async fn run<F, A>(
        &self,
        make_agent: F,
        task: &str,
        cancel: CancellationToken,
    ) -> Result<(Session, QualityReport), AgentError>
    where
        F: Fn() -> A + Send + Sync,
        A: Agent + Send + 'static,
    {
        let n = self.config.candidates as usize;
        let mut joins = Vec::with_capacity(n);
        for i in 0..n {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let agent = make_agent();
            let task = task.to_string();
            let cancel = cancel.clone();
            joins.push(tokio::spawn(async move {
                let mut session = Session::new();
                let mut sink = |_ev: AgentEvent| {};
                let outcome = agent
                    .run_turn(
                        &mut session,
                        UserTurn { text: task },
                        cancel,
                        &mut sink,
                    )
                    .await;
                (i, session, outcome)
            }));
        }

        let mut raw = Vec::with_capacity(n);
        for j in joins {
            match j.await {
                Ok(v) => raw.push(v),
                Err(e) => {
                    return Err(AgentError::Other(format!("candidate task join error: {e}")));
                }
            }
        }
        raw.sort_by_key(|(i, _, _)| *i);

        let mut scored = Vec::new();
        for (i, session, outcome) in &raw {
            let text = match outcome {
                Ok(TurnOutcome::Completed) => session
                    .last_assistant_text()
                    .unwrap_or("")
                    .to_string(),
                Ok(TurnOutcome::Cancelled) | Ok(TurnOutcome::StepLimit) => String::new(),
                Ok(TurnOutcome::Failed) | Err(_) => session
                    .last_assistant_text()
                    .unwrap_or("")
                    .to_string(),
            };
            let (score, rationale) = self.score_candidate(*i, task, &text, &cancel).await?;
            scored.push(ScoredCandidate {
                index: *i,
                score,
                rationale,
                text,
            });
        }

        let winner_index = scored
            .iter()
            .max_by_key(|c| c.score)
            .map(|c| c.index)
            .unwrap_or(0);
        let winner_session = raw
            .into_iter()
            .find(|(i, _, _)| *i == winner_index)
            .map(|(_, s, _)| s)
            .unwrap_or_else(Session::new);

        Ok((
            winner_session,
            QualityReport {
                winner_index,
                candidates: scored,
            },
        ))
    }

    async fn score_candidate(
        &self,
        index: usize,
        task: &str,
        text: &str,
        cancel: &CancellationToken,
    ) -> Result<(u8, String), AgentError> {
        if text.is_empty() {
            return Ok((0, "empty or incomplete candidate".into()));
        }
        let messages = vec![
            Message::system(REVIEWER_PROMPT),
            Message::user(format!(
                "Task:\n{task}\n\nCandidate {index}:\n{text}\n\nScore this candidate."
            )),
        ];
        let mut stream = self
            .reviewer
            .stream(&messages, &[] as &[ToolSpec], cancel.clone())
            .await?;
        let mut buf = String::new();
        while let Some(ev) = stream.next().await {
            let ev = ev?;
            if let crate::message::StreamEvent::TextDelta { text } = ev {
                buf.push_str(&text);
            }
        }
        Ok(parse_score(&buf))
    }
}

fn parse_score(raw: &str) -> (u8, String) {
    let trimmed = raw.trim();
    let json_slice = if let (Some(s), Some(e)) = (trimmed.find('{'), trimmed.rfind('}')) {
        &trimmed[s..=e]
    } else {
        trimmed
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_slice) {
        let score = v
            .get("score")
            .and_then(|s| s.as_u64())
            .unwrap_or(0)
            .min(10) as u8;
        let rationale = v
            .get("rationale")
            .and_then(|s| s.as_str())
            .unwrap_or("no rationale")
            .to_string();
        return (score, rationale);
    }
    (0, format!("reviewer output was not valid JSON: {raw}"))
}

#[cfg(test)]
mod tests {
    use super::parse_score;

    #[test]
    fn parse_score_reads_json_even_with_prose_wrapper() {
        let (s, r) = parse_score("Here you go: {\"score\": 8, \"rationale\": \"solid\"}");
        assert_eq!(s, 8);
        assert_eq!(r, "solid");
    }

    #[test]
    fn parse_score_garbage_is_zero() {
        let (s, _) = parse_score("I like it a lot");
        assert_eq!(s, 0);
    }
}
