use crate::config::config::AppConfig;
use crate::persistence::session_store::SessionStore;
use crate::provider::models::build_simple_request;
use crate::provider::xai_client::XaiClient;
use anyhow::Result;

pub struct AgentRuntime {
    cfg: AppConfig,
    max_steps: u32,
}

impl AgentRuntime {
    pub fn new(cfg: AppConfig, max_steps: u32) -> Self {
        Self { cfg, max_steps }
    }

    pub async fn run_ask(&self, client: &XaiClient, prompt: String) -> Result<()> {
        let session = SessionStore::for_new_session()?;
        session.append("user_message", SessionStore::prompt_payload(&prompt))?;

        // Deterministic loop scaffold: request -> inspect -> continue/stop.
        // Tool dispatch gets introduced in the next implementation tasks.
        for _step in 0..self.max_steps {
            let request = build_simple_request(
                self.cfg.model.clone(),
                prompt.clone(),
                self.cfg.stream,
                self.cfg.parallel_tool_calls,
                self.cfg.store,
                self.cfg.max_output_tokens,
                self.cfg.temperature,
            );

            if self.cfg.stream {
                let text = client.stream_response_to_stdout(&request).await?;
                session.append("assistant_message", SessionStore::assistant_payload(&text))?;
                println!();
            } else {
                let text = client.create_response(&request).await?;
                session.append("assistant_message", SessionStore::assistant_payload(&text))?;
                println!("{text}");
            }
            break;
        }
        eprintln!("session log: {}", session.path().display());
        Ok(())
    }
}
