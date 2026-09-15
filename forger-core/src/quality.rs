//! Multi-agent quality mode.
//!
//! N candidate [`crate::agent::Agent`] loops (default 2, max 3) run the same
//! task **in parallel on real `tokio::spawn` tasks**. That works because
//! [`crate::plugin::SharedProvider`] is `Arc<dyn Provider + Send + Sync>` —
//! the backend is shareable across tasks, not locked to one sequential
//! `run_turn().await` after another.
//!
//! A separate reviewer scores each complete result 0–10 and the highest
//! score wins (ties keep the earlier candidate). Merge is "pick the best
//! complete candidate", not a diff merge — that is future work.
//!
//! Each extra candidate multiplies token spend. Keep the default at 2.

use crate::agent::{Agent, AgentEvent, TurnOutcome, UserTurn};
use crate::agent_loop::AgentLoop;
use crate::error::AgentError;
use crate::message::Message;
use crate::plugin::SharedProvider;
use crate::session::Session;
use crate::tool::ToolSpec;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
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

/// An [`AgentEvent`] from one spawned quality candidate, tagged with its index.
#[derive(Debug, Clone)]
pub struct QualityEvent {
    pub candidate: usize,
    pub event: AgentEvent,
}

pub struct QualityRunner {
    pub config: QualityConfig,
    pub reviewer: SharedProvider,
}

impl QualityRunner {
    pub fn new(reviewer: SharedProvider, config: QualityConfig) -> Self {
        Self {
            config: config.clamped(),
            reviewer,
        }
    }

    /// Run `n` independent copies of `make_agent` on the same task, score, pick best.
    ///
    /// Candidates are `tokio::spawn`'d immediately so their provider calls overlap.
    /// Events from those tasks are fanned in on this task via a channel (the
    /// `Agent` sink itself is not `'static`). Abort outstanding tasks if `cancel`
    /// fires while joining.
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
        self.run_with_events(make_agent, task, cancel, &mut |_| {})
            .await
    }

    pub async fn run_with_events<F, A>(
        &self,
        make_agent: F,
        task: &str,
        cancel: CancellationToken,
        on_event: &mut (dyn FnMut(QualityEvent) + Send),
    ) -> Result<(Session, QualityReport), AgentError>
    where
        F: Fn() -> A + Send + Sync,
        A: Agent + Send + 'static,
    {
        let n = self.config.candidates as usize;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<QualityEvent>();
        let mut set = tokio::task::JoinSet::new();
        for i in 0..n {
            if cancel.is_cancelled() {
                set.abort_all();
                return Err(AgentError::Cancelled);
            }
            let agent = make_agent();
            let task = task.to_string();
            let task_cancel = cancel.clone();
            let tx = tx.clone();
            set.spawn(async move {
                let mut sink = move |event| {
                    let _ = tx.send(QualityEvent {
                        candidate: i,
                        event,
                    });
                };
                let mut session = Session::new();
                let outcome = agent
                    .run_turn(
                        &mut session,
                        UserTurn { text: task },
                        task_cancel,
                        &mut sink,
                    )
                    .await;
                (i, session, outcome)
            });
        }
        drop(tx);

        let mut raw = Vec::with_capacity(n);
        let mut joining = true;
        let mut events_open = true;
        loop {
            if !joining && !events_open {
                break;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    set.abort_all();
                    return Err(AgentError::Cancelled);
                }
                ev = rx.recv(), if events_open => {
                    match ev {
                        Some(ev) => on_event(ev),
                        None => events_open = false,
                    }
                }
                next = set.join_next(), if joining => {
                    match next {
                        None => joining = false,
                        Some(Ok(v)) => {
                            raw.push(v);
                            if raw.len() >= n {
                                joining = false;
                            }
                        }
                        Some(Err(e)) if e.is_cancelled() => {
                            set.abort_all();
                            return Err(AgentError::Cancelled);
                        }
                        Some(Err(e)) => {
                            set.abort_all();
                            return Err(AgentError::Other(format!(
                                "candidate task join error: {e}"
                            )));
                        }
                    }
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
                Ok(TurnOutcome::Cancelled) => String::new(),
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

        let winner_index = pick_winner(&scored);
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

/// Run N clones of `agent` in parallel, then score with `reviewer`.
///
/// `AgentLoop` is `Clone` because it holds a [`SharedProvider`], so each clone
/// can move into `tokio::spawn`.
pub async fn run_quality_turn(
    agent: AgentLoop,
    reviewer: SharedProvider,
    task: &str,
    n_candidates: u8,
    cancel: CancellationToken,
) -> Result<(Session, QualityReport), AgentError> {
    let runner = QualityRunner::new(
        reviewer,
        QualityConfig {
            candidates: n_candidates,
        },
    );
    runner.run(move || agent.clone(), task, cancel).await
}

/// Like [`run_quality_turn`], but forwards each spawned candidate's events so a
/// UI can show overlapping work instead of a silent join.
pub async fn run_quality_turn_with_events(
    agent: AgentLoop,
    reviewer: SharedProvider,
    task: &str,
    n_candidates: u8,
    cancel: CancellationToken,
    on_event: &mut (dyn FnMut(QualityEvent) + Send),
) -> Result<(Session, QualityReport), AgentError> {
    let runner = QualityRunner::new(
        reviewer,
        QualityConfig {
            candidates: n_candidates,
        },
    );
    runner
        .run_with_events(move || agent.clone(), task, cancel, on_event)
        .await
}

fn pick_winner(scored: &[ScoredCandidate]) -> usize {
    scored
        .iter()
        .max_by(|a, b| a.score.cmp(&b.score).then(b.index.cmp(&a.index)))
        .map(|c| c.index)
        .unwrap_or(0)
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
    use super::{parse_score, pick_winner, ScoredCandidate};

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

    #[test]
    fn pick_winner_keeps_first_on_tie() {
        let c = |index, score| ScoredCandidate {
            index,
            score,
            rationale: String::new(),
            text: String::new(),
        };
        assert_eq!(pick_winner(&[c(0, 8), c(1, 8)]), 0);
        assert_eq!(pick_winner(&[c(0, 7), c(1, 9), c(2, 9)]), 1);
    }
}
