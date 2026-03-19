use crate::agent::context_budget::ContextBudgetManager;
use crate::agent::instructions::{build_multi_step_instructions, build_single_step_instructions};
use crate::config::config::AppConfig;
use crate::output::{OutputSink, StdOutputSink};
use crate::persistence::patch_store::PatchStore;
use crate::persistence::session_store::SessionStore;
use crate::provider::LanguageModelProvider;
use crate::provider::agent_tools::{
    effective_built_in_tools_json, effective_model_for_tools, requested_built_in_tools_json,
};
use crate::provider::models::build_simple_request;
use crate::tools::registry::{ToolCallRequest, ToolCallResult, ToolRegistry};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::env;
use std::fmt::Write as FmtWrite;
use std::io::{self, Write};
use std::sync::Arc;

pub struct AgentRuntime {
    cfg: AppConfig,
    max_steps: u32,
    output: Arc<dyn OutputSink>,
    approval_handler: Arc<dyn ApprovalHandler>,
}

pub trait ApprovalHandler: Send + Sync {
    fn request_approval(&self, action: &str, reason: Option<&str>) -> Result<bool>;
}

pub struct StdioApprovalHandler;

impl ApprovalHandler for StdioApprovalHandler {
    fn request_approval(&self, action: &str, reason: Option<&str>) -> Result<bool> {
        prompt_user_approval(action, reason)
    }
}

#[derive(Debug, Clone, Default)]
struct VerificationState {
    last_failure: Option<VerificationFailure>,
}

#[derive(Debug, Clone)]
struct VerificationFailure {
    tool_name: String,
    exit_code: Option<i32>,
    timed_out: bool,
    blocked: bool,
}

impl AgentRuntime {
    pub fn new(cfg: AppConfig, max_steps: u32) -> Self {
        Self {
            cfg,
            max_steps,
            output: Arc::new(StdOutputSink),
            approval_handler: Arc::new(StdioApprovalHandler),
        }
    }

    pub fn with_output(mut self, output: Arc<dyn OutputSink>) -> Self {
        self.output = output;
        self
    }

    pub fn with_approval_handler(mut self, approval_handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = approval_handler;
        self
    }

    pub async fn run_ask(
        &self,
        client: &dyn LanguageModelProvider,
        prompt: String,
        resume_session: Option<SessionStore>,
    ) -> Result<()> {
        validate_model_configuration(&self.cfg)?;
        let session = match resume_session {
            Some(existing) => existing,
            None => SessionStore::for_new_session()?,
        };
        let resumed_previous_response_id = session.last_provider_response_id()?;
        if resumed_previous_response_id.is_some() {
            session.append(
                "resume",
                json!({
                    "previous_response_id": resumed_previous_response_id
                }),
            )?;
        }
        session.append("user_message", SessionStore::prompt_payload(&prompt))?;
        let repo_root = env::current_dir()?;
        let requested_model = normalize_model_name(&self.cfg.model);
        let normalized_model =
            effective_model_for_tools(&self.cfg, &requested_model, &self.cfg.api_mode);
        let requested_built_in_tools = requested_built_in_tools_json(&self.cfg);
        let built_in_tools = effective_built_in_tools_json(&self.cfg, &normalized_model);
        if !requested_built_in_tools.is_empty() && normalized_model != requested_model {
            self.output.stderr_line(&format!(
                "switching model from {} to {} because xAI server-side tools require a Grok-4 family model",
                requested_model, normalized_model
            ));
        }

        if self.max_steps <= 1 {
            let single_step_instructions = build_single_step_instructions(&repo_root);
            let text = if self.cfg.api_mode.eq_ignore_ascii_case("chat_completions") {
                let body = json!({
                    "model": normalized_model.clone(),
                    "messages": [
                        { "role": "system", "content": single_step_instructions.clone() },
                        { "role": "user", "content": prompt.clone() }
                    ],
                    "stream": false,
                    "temperature": self.cfg.temperature
                });
                let resp = client.create_chat_completion_json(&body).await?;
                resp.get("choices")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.first())
                    .and_then(|c| c.get("message"))
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            } else {
                let response_tools = responses_tools(&built_in_tools, None);
                let mut request = build_simple_request(
                    normalized_model.clone(),
                    prompt.clone(),
                    self.cfg.stream,
                    self.cfg.parallel_tool_calls,
                    self.cfg.store,
                    self.cfg.max_output_tokens,
                    self.cfg.temperature,
                    if response_tools.is_empty() {
                        None
                    } else {
                        Some(response_tools)
                    },
                );
                request.instructions = Some(single_step_instructions.clone());
                let resp = if self.cfg.stream {
                    let t = client.stream_response_text(&request).await;
                    self.output.stdout_line("");
                    t
                } else {
                    client.create_response_text(&request).await
                };
                match resp {
                    Ok(t) => t,
                    Err(err) if is_not_found_error(&err) => {
                        self.output.stderr_line(
                            "responses endpoint unavailable, falling back to /v1/chat/completions",
                        );
                        let body = json!({
                            "model": normalized_model.clone(),
                            "messages": [
                                { "role": "system", "content": single_step_instructions.clone() },
                                { "role": "user", "content": prompt.clone() }
                            ],
                            "stream": false,
                            "temperature": self.cfg.temperature
                        });
                        let fallback = client.create_chat_completion_json(&body).await?;
                        fallback
                            .get("choices")
                            .and_then(Value::as_array)
                            .and_then(|arr| arr.first())
                            .and_then(|c| c.get("message"))
                            .and_then(|m| m.get("content"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    }
                    Err(err) => return Err(err),
                }
            };
            if !text.trim().is_empty() {
                self.output.stdout_line(&text);
            }
            session.append("assistant_message", SessionStore::assistant_payload(&text))?;
            self.output
                .stderr_line(&format!("session log: {}", session.path().display()));
            self.output.flush();
            return Ok(());
        }

        let registry = ToolRegistry::new(&repo_root);
        let tool_definitions = Value::Array(responses_tools(
            &built_in_tools,
            Some(registry.definitions_json()),
        ));
        let mut approval_cache = HashSet::new();
        if self.cfg.api_mode.eq_ignore_ascii_case("chat_completions") {
            self.run_chat_completions_loop(
                client,
                &session,
                &registry,
                &repo_root,
                &mut approval_cache,
                prompt,
            )
            .await?;
        } else {
            match self
                .run_responses_loop(
                    client,
                    &session,
                    &registry,
                    &tool_definitions,
                    &normalized_model,
                    &repo_root,
                    &mut approval_cache,
                    resumed_previous_response_id.clone(),
                    prompt.clone(),
                )
                .await
            {
                Ok(()) => {}
                Err(err) => {
                    if is_not_found_error(&err) {
                        self.output.stderr_line(
                            "responses endpoint unavailable, falling back to /v1/chat/completions",
                        );
                        self.run_chat_completions_loop(
                            client,
                            &session,
                            &registry,
                            &repo_root,
                            &mut approval_cache,
                            prompt,
                        )
                        .await?;
                    } else {
                        return Err(err);
                    }
                }
            }
        }
        self.output
            .stderr_line(&format!("session log: {}", session.path().display()));
        self.output.flush();
        Ok(())
    }
}

fn is_not_found_error(err: &anyhow::Error) -> bool {
    err.chain().any(|e| {
        let s = e.to_string();
        s.contains("404") || s.contains("Not Found")
    })
}

fn normalize_model_name(model: &str) -> String {
    if model == "grok-4.1-fast-reasoning" {
        "grok-4-1-fast-reasoning".to_string()
    } else if model == "grok-4.1-fast-non-reasoning" {
        "grok-4-1-fast-non-reasoning".to_string()
    } else {
        model.to_string()
    }
}

fn responses_tools(built_in_tools: &[Value], local_tools: Option<Value>) -> Vec<Value> {
    let mut tools = built_in_tools.to_vec();
    if let Some(local) = local_tools.and_then(|value| value.as_array().cloned()) {
        tools.extend(local);
    }
    tools
}

fn validate_model_configuration(cfg: &AppConfig) -> Result<()> {
    let model = normalize_model_name(&cfg.model);
    if model.contains("multi-agent") {
        bail!(
            "{} is not compatible with this CLI's client-side tool architecture; use grok-code-fast-1, grok-4-1-fast-reasoning, grok-4-1-fast-non-reasoning, grok-4.20-beta-0309-reasoning, or grok-4.20-beta-0309-non-reasoning",
            model
        );
    }
    Ok(())
}

impl AgentRuntime {
    async fn run_responses_loop(
        &self,
        client: &dyn LanguageModelProvider,
        session: &SessionStore,
        registry: &ToolRegistry,
        tool_definitions: &Value,
        model: &str,
        repo_root: &std::path::Path,
        approval_cache: &mut HashSet<String>,
        resumed_previous_response_id: Option<String>,
        prompt: String,
    ) -> Result<()> {
        let mut previous_response_id = resumed_previous_response_id;
        let mut budget = ContextBudgetManager::new(self.cfg.context_budget_bytes);
        let mut verification = VerificationState::default();
        let instructions = build_multi_step_instructions(repo_root);
        let store = true;
        if !self.cfg.store {
            self.output.stderr_line(
                "forcing store=true for multi-step Responses mode so previous_response_id works",
            );
        }
        let mut pending_input = vec![json!({
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt }]
        })];
        let mut completed = false;

        for step in 0..self.max_steps {
            let mut body = json!({
                "model": model,
                "input": pending_input,
                "tools": tool_definitions.clone(),
                "stream": self.cfg.stream,
                "parallel_tool_calls": self.cfg.parallel_tool_calls,
                "store": store,
                "max_output_tokens": self.cfg.max_output_tokens,
                "temperature": self.cfg.temperature
            });
            if let Some(obj) = body.as_object_mut() {
                if previous_response_id.is_none() {
                    obj.insert("instructions".to_string(), json!(instructions.clone()));
                }
                if let Some(prev) = previous_response_id.clone() {
                    obj.insert("previous_response_id".to_string(), json!(prev));
                }
            }

            let mut streamed = false;
            let response = if self.cfg.stream {
                match client.create_response_stream_json(&body).await {
                    Ok(value) => {
                        streamed = true;
                        value
                    }
                    Err(err) => {
                        self.output.stderr_line(&format!(
                            "streaming failed, falling back to non-streaming: {}",
                            err
                        ));
                        let mut fallback_body = body.clone();
                        if let Some(obj) = fallback_body.as_object_mut() {
                            obj.insert("stream".to_string(), json!(false));
                        }
                        client.create_response_json(&fallback_body).await?
                    }
                }
            } else {
                client.create_response_json(&body).await?
            };
            session.append("provider_response", response.clone())?;
            previous_response_id = response
                .get("id")
                .and_then(Value::as_str)
                .map(ToString::to_string);

            let text = extract_text(&response);
            if !text.trim().is_empty() {
                if step > 0 && !streamed {
                    self.output.stdout_line("");
                }
                if !streamed {
                    self.output.stdout(&text);
                }
                session.append("assistant_message", SessionStore::assistant_payload(&text))?;
            }

            let tool_calls = extract_tool_calls(&response);
            if !tool_calls.is_empty() {
                self.output.flush();
            }
            if tool_calls.is_empty() {
                if let Some(reminder) = verification_followup_message(&verification) {
                    self.output.stderr_line(&reminder);
                    session.append(
                        "runtime_notice",
                        json!({
                            "kind": "verification_failed_continue",
                            "message": reminder
                        }),
                    )?;
                    pending_input = vec![json!({
                        "role": "user",
                        "content": [{ "type": "input_text", "text": reminder }]
                    })];
                    continue;
                }
                self.output.stdout_line("");
                completed = true;
                break;
            }

            let mut outputs = Vec::new();
            for call in tool_calls {
                let action = describe_tool_call(&call.name, &call.arguments);
                session.append(
                    "tool_call",
                    json!({
                      "name": call.name,
                      "arguments": call.arguments,
                      "call_id": call.call_id,
                      "action": action
                    }),
                )?;
                self.output.stderr_line(&format!("tool> {action}"));
                let Some(result) = self.execute_with_approval(
                    registry,
                    session,
                    approval_cache,
                    &call.name,
                    &call.arguments,
                )?
                else {
                    self.output
                        .stderr_line(&format!("tool '{}' was not approved; stopping.", call.name));
                    return Ok(());
                };
                session.append(
                    "tool_result",
                    json!({
                      "name": result.name,
                      "result": result.result,
                      "call_id": call.call_id
                    }),
                )?;
                emit_tool_notice(session, &call.name, &result.result, self.cfg.verbose_tools)?;
                update_verification_state(
                    &mut verification,
                    &call.name,
                    &call.arguments,
                    &result.result,
                );
                emit_tool_result_preview(
                    &*self.output,
                    &call.name,
                    &result.result,
                    self.cfg.verbose_tools,
                );
                maybe_record_applied_patch(
                    &call.name,
                    &call.arguments,
                    &result.result,
                    repo_root,
                    session.path(),
                )?;
                let budgeted = budget.fit_tool_output(result.result.clone());
                outputs.push(json!({
                    "type": "function_call_output",
                    "call_id": call.call_id,
                    "output": serde_json::to_string(&budgeted)?
                }));
            }

            if outputs.is_empty() {
                self.output.stdout_line("");
                break;
            } else {
                pending_input = outputs;
            }
        }
        if !completed {
            emit_max_steps_notice(session, self.max_steps, &*self.output)?;
        }
        Ok(())
    }

    async fn run_chat_completions_loop(
        &self,
        client: &dyn LanguageModelProvider,
        session: &SessionStore,
        registry: &ToolRegistry,
        repo_root: &std::path::Path,
        approval_cache: &mut HashSet<String>,
        prompt: String,
    ) -> Result<()> {
        let instructions = build_multi_step_instructions(repo_root);
        let mut messages = vec![
            json!({
                "role": "system",
                "content": instructions
            }),
            json!({
                "role": "user",
                "content": prompt
            }),
        ];
        let mut budget = ContextBudgetManager::new(self.cfg.context_budget_bytes);
        let mut verification = VerificationState::default();
        let mut completed = false;

        for step in 0..self.max_steps {
            let body = json!({
                "model": normalize_model_name(&self.cfg.model),
                "messages": messages,
                "tools": registry.definitions_chat_json(),
                "tool_choice": "auto",
                "stream": false,
                "parallel_tool_calls": self.cfg.parallel_tool_calls,
                "temperature": self.cfg.temperature
            });
            let response = client.create_chat_completion_json(&body).await?;
            session.append("provider_response", response.clone())?;

            let Some(choice) = response
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|arr| arr.first())
            else {
                self.output.stdout_line("");
                break;
            };
            let Some(message) = choice.get("message") else {
                self.output.stdout_line("");
                break;
            };

            let text = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !text.trim().is_empty() {
                if step > 0 {
                    self.output.stdout_line("");
                }
                self.output.stdout(&text);
                session.append("assistant_message", SessionStore::assistant_payload(&text))?;
            }

            messages.push(json!({
                "role": "assistant",
                "content": message.get("content").cloned().unwrap_or_else(|| json!("")),
                "tool_calls": message.get("tool_calls").cloned().unwrap_or_else(|| json!([]))
            }));

            let tool_calls = extract_chat_tool_calls(message);
            if !tool_calls.is_empty() {
                self.output.flush();
            }
            if tool_calls.is_empty() {
                if let Some(reminder) = verification_followup_message(&verification) {
                    self.output.stderr_line(&reminder);
                    session.append(
                        "runtime_notice",
                        json!({
                            "kind": "verification_failed_continue",
                            "message": reminder
                        }),
                    )?;
                    messages.push(json!({
                        "role": "user",
                        "content": reminder
                    }));
                    continue;
                }
                self.output.stdout_line("");
                completed = true;
                break;
            }

            for call in tool_calls {
                let action = describe_tool_call(&call.name, &call.arguments);
                session.append(
                    "tool_call",
                    json!({
                      "name": call.name,
                      "arguments": call.arguments,
                      "call_id": call.call_id,
                      "action": action
                    }),
                )?;
                self.output.stderr_line(&format!("tool> {action}"));
                let Some(result) = self.execute_with_approval(
                    registry,
                    session,
                    approval_cache,
                    &call.name,
                    &call.arguments,
                )?
                else {
                    self.output
                        .stderr_line(&format!("tool '{}' was not approved; stopping.", call.name));
                    return Ok(());
                };
                session.append(
                    "tool_result",
                    json!({
                      "name": result.name,
                      "result": result.result,
                      "call_id": call.call_id
                    }),
                )?;
                emit_tool_notice(session, &call.name, &result.result, self.cfg.verbose_tools)?;
                update_verification_state(
                    &mut verification,
                    &call.name,
                    &call.arguments,
                    &result.result,
                );
                emit_tool_result_preview(
                    &*self.output,
                    &call.name,
                    &result.result,
                    self.cfg.verbose_tools,
                );
                maybe_record_applied_patch(
                    &call.name,
                    &call.arguments,
                    &result.result,
                    repo_root,
                    session.path(),
                )?;
                let budgeted = budget.fit_tool_output(result.result.clone());
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call.call_id,
                    "content": serde_json::to_string(&budgeted)?
                }));
            }
        }

        if !completed {
            emit_max_steps_notice(session, self.max_steps, &*self.output)?;
        }

        Ok(())
    }

    fn execute_with_approval(
        &self,
        registry: &ToolRegistry,
        session: &SessionStore,
        approval_cache: &mut HashSet<String>,
        name: &str,
        args: &Value,
    ) -> Result<Option<ToolCallResult>> {
        let action = describe_tool_call(name, args);
        let first = registry.execute(ToolCallRequest {
            name: name.to_string(),
            arguments: args.clone(),
        })?;
        if !result_is_approval_required(&first.result) {
            return Ok(Some(first));
        }

        let reason = approval_reason(&first.result);
        if name == "apply_patch" {
            if let Some(patch) = args.get("patch").and_then(Value::as_str) {
                render_patch_preview(&*self.output, patch);
            }
        }
        let cache_key = approval_cache_key(name, args, reason.as_deref());
        if let Some(key) = cache_key.as_ref() {
            if approval_cache.contains(key) {
                session.append(
                    "approval_decision",
                    json!({
                        "tool": name,
                        "approved": true,
                        "source": "session_cache"
                    }),
                )?;
                let approved_result = rerun_with_approved(registry, name, args)?;
                return Ok(Some(approved_result));
            }
        }
        session.append(
            "approval_request",
            json!({
                "tool": name,
                "action": action,
                "reason": reason,
                "mode": if self.cfg.auto_approve { "auto" } else { "interactive" }
            }),
        )?;

        let approved = if self.cfg.auto_approve {
            true
        } else {
            self.approval_handler
                .request_approval(&action, reason.as_deref())?
        };

        session.append(
            "approval_decision",
            json!({
                "tool": name,
                "approved": approved
            }),
        )?;
        if !approved {
            return Ok(None);
        }
        if let Some(key) = cache_key {
            approval_cache.insert(key);
        }

        let second = rerun_with_approved(registry, name, args)?;
        Ok(Some(second))
    }
}

fn rerun_with_approved(
    registry: &ToolRegistry,
    name: &str,
    args: &Value,
) -> Result<ToolCallResult> {
    let mut approved_args = args.clone();
    if let Some(map) = approved_args.as_object_mut() {
        map.insert("approved".to_string(), json!(true));
    } else {
        approved_args = json!({ "approved": true });
    }
    let second = registry.execute(ToolCallRequest {
        name: name.to_string(),
        arguments: approved_args,
    })?;
    Ok(second)
}

#[derive(Debug, Clone)]
struct PendingToolCall {
    call_id: String,
    name: String,
    arguments: Value,
}

fn extract_text(response: &Value) -> String {
    let mut pieces = Vec::new();
    if let Some(output) = response.get("output").and_then(Value::as_array) {
        for item in output {
            if let Some(content) = item.get("content").and_then(Value::as_array) {
                for chunk in content {
                    if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                        pieces.push(text.to_string());
                    }
                    if let Some(text) = chunk.get("output_text").and_then(Value::as_str) {
                        pieces.push(text.to_string());
                    }
                }
            }
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                pieces.push(text.to_string());
            }
        }
    }
    pieces.join("")
}

fn extract_tool_calls(response: &Value) -> Vec<PendingToolCall> {
    let mut calls = Vec::new();
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return calls;
    };

    for item in output {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let is_function_call = item_type == "function_call"
            || item_type == "tool_call"
            || item_type.ends_with(".function_call");

        if !is_function_call {
            continue;
        }

        let name = item
            .get("name")
            .or_else(|| item.get("function").and_then(|f| f.get("name")))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let Some(name) = name else {
            continue;
        };

        let call_id = item
            .get("call_id")
            .or_else(|| item.get("id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("call_{}", calls.len() + 1));

        let raw_args = item
            .get("arguments")
            .or_else(|| item.get("function").and_then(|f| f.get("arguments")))
            .cloned()
            .unwrap_or_else(|| json!({}));

        let arguments = match raw_args {
            Value::String(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| json!({})),
            Value::Object(_) => raw_args,
            _ => json!({}),
        };

        calls.push(PendingToolCall {
            call_id,
            name,
            arguments,
        });
    }

    calls
}

fn extract_chat_tool_calls(message: &Value) -> Vec<PendingToolCall> {
    let mut calls = Vec::new();
    let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
        return calls;
    };
    for (idx, call) in tool_calls.iter().enumerate() {
        let name = call
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        let Some(name) = name else {
            continue;
        };
        let call_id = call
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| format!("call_{}", idx + 1));
        let args_raw = call
            .get("function")
            .and_then(|f| f.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!("{}"));
        let arguments = match args_raw {
            Value::String(s) => serde_json::from_str::<Value>(&s).unwrap_or_else(|_| json!({})),
            Value::Object(_) => args_raw,
            _ => json!({}),
        };
        calls.push(PendingToolCall {
            call_id,
            name,
            arguments,
        });
    }
    calls
}

fn result_is_approval_required(result: &Value) -> bool {
    result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn approval_reason(result: &Value) -> Option<String> {
    result
        .get("decision_reason")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn prompt_user_approval(action: &str, reason: Option<&str>) -> Result<bool> {
    match reason {
        Some(r) if !r.trim().is_empty() => {
            eprintln!("approval required for {action}: {r}");
        }
        _ => {
            eprintln!("approval required for {action}");
        }
    }
    eprint!("Approve? [y/N]: ");
    io::stderr().flush()?;

    let mut line = String::new();
    let bytes = io::stdin().read_line(&mut line)?;
    if bytes == 0 {
        return Ok(false);
    }
    let normalized = line.trim().to_ascii_lowercase();
    Ok(normalized == "y" || normalized == "yes")
}

fn render_patch_preview(output: &dyn OutputSink, patch: &str) {
    const MAX_LINES: usize = 200;
    output.stderr_line("patch preview:");
    let mut count = 0usize;
    for line in patch.lines() {
        if count >= MAX_LINES {
            output.stderr_line("...[patch preview truncated]...");
            break;
        }
        output.stderr_line(line);
        count += 1;
    }
}

fn describe_tool_call(tool_name: &str, args: &Value) -> String {
    match tool_name {
        "read_file" => {
            let path = string_arg(args, "path").unwrap_or("?");
            format!("read_file path={path:?}")
        }
        "list_directory" => {
            let path = string_arg(args, "path").unwrap_or(".");
            format!("list_directory path={path:?}")
        }
        "search_text" => {
            let pattern = string_arg(args, "pattern").unwrap_or("?");
            match string_arg(args, "path") {
                Some(path) => format!("search_text pattern={pattern:?} path={path:?}"),
                None => format!("search_text pattern={pattern:?}"),
            }
        }
        "run_shell_command" => describe_shell_tool(args),
        "run_tests" => describe_wrapper_tool("run_tests", args, wrapper_command("run_tests", args)),
        "build_project" => describe_wrapper_tool(
            "build_project",
            args,
            wrapper_command("build_project", args),
        ),
        "run_linter" => {
            describe_wrapper_tool("run_linter", args, wrapper_command("run_linter", args))
        }
        "run_formatter" => describe_wrapper_tool(
            "run_formatter",
            args,
            wrapper_command("run_formatter", args),
        ),
        "apply_patch" => describe_apply_patch(args),
        "write_file" => {
            let path = string_arg(args, "path").unwrap_or("?");
            let bytes = string_arg(args, "content").map(|s| s.len()).unwrap_or(0);
            format!("write_file path={path:?} bytes={bytes}")
        }
        "git_status" => "git_status".to_string(),
        "git_diff" => {
            let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
            format!("git_diff staged={staged}")
        }
        "checkpoint_repo" => "checkpoint_repo create repo diff checkpoint".to_string(),
        "undo_last_patch" => "undo_last_patch reverse latest recorded patch".to_string(),
        _ => tool_name.to_string(),
    }
}

fn describe_shell_tool(args: &Value) -> String {
    let command = string_arg(args, "command").unwrap_or("?");
    let cwd = string_arg(args, "working_directory").unwrap_or(".");
    match args.get("timeout_seconds").and_then(Value::as_u64) {
        Some(timeout) => {
            format!(
                "run_shell_command command={command:?} cwd={cwd:?} timeout={}s",
                timeout
            )
        }
        None => format!("run_shell_command command={command:?} cwd={cwd:?}"),
    }
}

fn describe_wrapper_tool(tool_name: &str, args: &Value, command: String) -> String {
    let language = string_arg(args, "language").unwrap_or(match tool_name {
        "run_tests" | "build_project" | "run_linter" | "run_formatter" => "rust",
        _ => "?",
    });
    format!("{tool_name} language={language:?} command={command:?}")
}

fn wrapper_command(tool_name: &str, args: &Value) -> String {
    let language = string_arg(args, "language").unwrap_or("rust");
    let is_swift = language.eq_ignore_ascii_case("swift");
    match tool_name {
        "run_tests" => {
            if is_swift {
                "swift test".to_string()
            } else {
                "cargo test".to_string()
            }
        }
        "build_project" => {
            if is_swift {
                "swift build".to_string()
            } else {
                "cargo build".to_string()
            }
        }
        "run_linter" => {
            if is_swift {
                "swift format lint".to_string()
            } else {
                "cargo clippy".to_string()
            }
        }
        "run_formatter" => {
            if is_swift {
                "swift format .".to_string()
            } else {
                "cargo fmt".to_string()
            }
        }
        _ => "?".to_string(),
    }
}

fn describe_apply_patch(args: &Value) -> String {
    let patch = string_arg(args, "patch").unwrap_or_default();
    let files = patch_files_from_unified_diff(patch);
    let line_count = patch.lines().count();
    if files.is_empty() {
        format!("apply_patch lines={line_count}")
    } else {
        format!("apply_patch files={files:?} lines={line_count}")
    }
}

fn patch_files_from_unified_diff(patch: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in patch.lines() {
        let candidate = if let Some(rest) = line.strip_prefix("+++ b/") {
            Some(rest)
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if rest == "/dev/null" {
                None
            } else {
                Some(rest)
            }
        } else {
            None
        };
        if let Some(path) = candidate {
            if !files.iter().any(|existing| existing == path) {
                files.push(path.to_string());
            }
        }
    }
    files
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn update_verification_state(
    state: &mut VerificationState,
    tool_name: &str,
    args: &Value,
    result: &Value,
) {
    if !is_verification_tool(tool_name, args) {
        return;
    }

    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let timed_out = result
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_code = result
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|v| v as i32);

    if !blocked && !timed_out && exit_code == Some(0) {
        state.last_failure = None;
        return;
    }

    state.last_failure = Some(VerificationFailure {
        tool_name: verification_tool_name(tool_name, args),
        exit_code,
        timed_out,
        blocked,
    });
}

fn verification_followup_message(state: &VerificationState) -> Option<String> {
    let failure = state.last_failure.as_ref()?;
    let status = if failure.blocked {
        "was blocked".to_string()
    } else if failure.timed_out {
        "timed out".to_string()
    } else if let Some(code) = failure.exit_code {
        format!("exited with code {code}")
    } else {
        "did not succeed".to_string()
    };

    Some(format!(
        "The last verification step `{}` {}. Do not claim success yet. Continue using tools until build/tests pass, or explain the concrete blocker.",
        failure.tool_name, status
    ))
}

fn is_verification_tool(tool_name: &str, args: &Value) -> bool {
    match tool_name {
        "run_tests" | "build_project" => true,
        "run_shell_command" => {
            let command = string_arg(args, "command")
                .unwrap_or_default()
                .to_ascii_lowercase();
            command.contains("cargo test")
                || command.contains("swift test")
                || command.contains("cargo build")
                || command.contains("swift build")
        }
        _ => false,
    }
}

fn verification_tool_name(tool_name: &str, args: &Value) -> String {
    match tool_name {
        "run_shell_command" => string_arg(args, "command")
            .map(ToString::to_string)
            .unwrap_or_else(|| tool_name.to_string()),
        _ => tool_name.to_string(),
    }
}

fn emit_tool_result_preview(
    output: &dyn OutputSink,
    tool_name: &str,
    result: &Value,
    verbose_tools: bool,
) {
    if let Some(preview) = render_tool_result_preview(tool_name, result) {
        let mut lines = preview.lines();
        if let Some(summary) = lines.next() {
            output.stderr_line(&format!("tool< {summary}"));
        }
        if verbose_tools {
            let detail = lines.collect::<Vec<_>>().join("\n");
            if !detail.is_empty() {
                output.stderr_line(&detail);
            }
        }
    }
}

fn render_tool_result_preview(tool_name: &str, result: &Value) -> Option<String> {
    match tool_name {
        "read_file" => render_read_file_result(result),
        "list_directory" => render_list_directory_result(result),
        "apply_patch" => Some(render_apply_patch_result(result)),
        "write_file" => Some(render_write_file_result(result)),
        "checkpoint_repo" => Some(render_checkpoint_result(result)),
        "undo_last_patch" => Some(render_undo_result(result)),
        _ if is_command_like_result(result) => Some(render_command_like_result(tool_name, result)),
        _ => serde_json::to_string_pretty(result)
            .ok()
            .map(|body| format!("{tool_name}\n{body}")),
    }
}

fn render_read_file_result(result: &Value) -> Option<String> {
    let path = result.get("path").and_then(Value::as_str)?;
    let content = result
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let was_truncated = result
        .get("was_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let preview = truncate_preview_text(content);
    let mut out = format!("read_file path={path:?}");
    if was_truncated {
        out.push_str(" truncated");
    }
    if preview.is_empty() {
        return Some(out);
    }
    out.push('\n');
    out.push_str(&preview);
    Some(out)
}

fn render_list_directory_result(result: &Value) -> Option<String> {
    let path = result.get("path").and_then(Value::as_str).unwrap_or(".");
    let entries = result.get("entries").and_then(Value::as_array)?;
    let was_truncated = result
        .get("was_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut body = String::new();
    for entry in entries {
        if let Some(name) = entry.as_str() {
            let _ = writeln!(&mut body, "{name}");
        }
    }
    let preview = truncate_preview_text(&body);
    let mut out = format!("list_directory path={path:?} entries={}", entries.len());
    if was_truncated {
        out.push_str(" truncated");
    }
    if !preview.is_empty() {
        out.push('\n');
        out.push_str(&preview);
    }
    Some(out)
}

fn render_command_like_result(tool_name: &str, result: &Value) -> String {
    let command = result.get("command").and_then(Value::as_str);
    let cwd = result.get("working_directory").and_then(Value::as_str);
    let exit_code = result.get("exit_code").and_then(Value::as_i64);
    let duration_ms = result.get("duration_ms").and_then(Value::as_u64);
    let timed_out = result
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let blocked = result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let was_truncated = result
        .get("was_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let decision_reason = result
        .get("decision_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let status = if approval_required {
        "approval-required".to_string()
    } else if blocked {
        "blocked".to_string()
    } else if timed_out {
        "timed-out".to_string()
    } else if let Some(code) = exit_code {
        if code == 0 {
            "ok".to_string()
        } else {
            format!("failed exit={code}")
        }
    } else {
        "done".to_string()
    };

    let mut out = format!("{tool_name} {status}");
    if let Some(command) = command {
        let _ = write!(&mut out, " command={command:?}");
    }
    if let Some(cwd) = cwd {
        let _ = write!(&mut out, " cwd={cwd:?}");
    }
    if let Some(duration_ms) = duration_ms {
        let _ = write!(&mut out, " duration={}ms", duration_ms);
    }
    if was_truncated {
        out.push_str(" truncated");
    }
    if let Some(reason) = decision_reason {
        let _ = write!(&mut out, " reason={reason:?}");
    }

    let stdout = result
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !stdout.is_empty() {
        out.push_str("\nstdout\n");
        out.push_str(&truncate_preview_text(stdout));
    }
    if !stderr.is_empty() {
        out.push_str("\nstderr\n");
        out.push_str(&truncate_preview_text(stderr));
    }
    out
}

fn render_apply_patch_result(result: &Value) -> String {
    let valid = result
        .get("valid")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let applied = result
        .get("applied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let status = if approval_required {
        "approval-required"
    } else if !valid {
        "invalid"
    } else if applied {
        "applied"
    } else {
        "checked"
    };
    let mut out = format!("apply_patch {status}");
    let check_stderr = result
        .get("check_stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !check_stderr.is_empty() {
        out.push_str("\ncheck\n");
        out.push_str(&truncate_preview_text(check_stderr));
    }
    let apply_stderr = result
        .get("apply_stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !apply_stderr.is_empty() {
        out.push_str("\napply\n");
        out.push_str(&truncate_preview_text(apply_stderr));
    }
    out
}

fn render_write_file_result(result: &Value) -> String {
    let path = result.get("path").and_then(Value::as_str).unwrap_or("?");
    let written = result
        .get("written")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bytes_written = result
        .get("bytes_written")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let status = if approval_required {
        "approval-required"
    } else if written {
        "written"
    } else {
        "failed"
    };
    format!("write_file {status} path={path:?} bytes={bytes_written}")
}

fn render_checkpoint_result(result: &Value) -> String {
    let created = result
        .get("created")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let path = result.get("path").and_then(Value::as_str).unwrap_or("");
    let stderr = result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let status = if approval_required {
        "approval-required"
    } else if created {
        "created"
    } else {
        "skipped"
    };
    let mut out = format!("checkpoint_repo {status}");
    if !path.is_empty() {
        let _ = write!(&mut out, " path={path:?}");
    }
    if !stderr.is_empty() {
        out.push_str("\nstderr\n");
        out.push_str(&truncate_preview_text(stderr));
    }
    out
}

fn render_undo_result(result: &Value) -> String {
    let undone = result
        .get("undone")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let approval_required = result
        .get("approval_required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let message = result
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let status = if approval_required {
        "approval-required"
    } else if undone {
        "undone"
    } else {
        "skipped"
    };
    if message.is_empty() {
        format!("undo_last_patch {status}")
    } else {
        format!(
            "undo_last_patch {status}\n{}",
            truncate_preview_text(message)
        )
    }
}

fn is_command_like_result(result: &Value) -> bool {
    result.get("stdout").is_some() || result.get("stderr").is_some()
}

fn truncate_preview_text(text: &str) -> String {
    const MAX_LINES: usize = 120;
    const MAX_BYTES: usize = 12 * 1024;

    if text.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let mut truncated = false;

    for (idx, line) in text.lines().enumerate() {
        let extra = if idx == 0 { line.len() } else { line.len() + 1 };
        if idx >= MAX_LINES || out.len() + extra > MAX_BYTES {
            truncated = true;
            break;
        }
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }

    if out.is_empty() && !text.is_empty() {
        out = text.chars().take(512).collect();
        if out.len() < text.len() {
            truncated = true;
        }
    }

    if truncated {
        out.push_str("\n...[tool output truncated]...");
    }
    out
}

fn emit_tool_notice(
    session: &SessionStore,
    tool_name: &str,
    result: &Value,
    verbose_tools: bool,
) -> Result<()> {
    let _ = verbose_tools;
    match tool_name {
        "checkpoint_repo" => {
            let created = result
                .get("created")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let path = result.get("path").and_then(Value::as_str).unwrap_or("");
            session.append(
                "tool_notice",
                json!({
                    "tool": tool_name,
                    "created": created,
                    "path": path
                }),
            )?;
        }
        "undo_last_patch" => {
            let undone = result
                .get("undone")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let message = result.get("message").and_then(Value::as_str).unwrap_or("");
            session.append(
                "tool_notice",
                json!({
                    "tool": tool_name,
                    "undone": undone,
                    "message": message
                }),
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn approval_cache_key(tool_name: &str, args: &Value, reason: Option<&str>) -> Option<String> {
    let tier1_tools = ["run_tests", "run_linter", "run_formatter", "build_project"];
    if tier1_tools.iter().any(|t| *t == tool_name) {
        let reason_key = reason.unwrap_or("tier1");
        return Some(format!("{}::{}", tool_name, reason_key));
    }
    if tool_name != "run_shell_command" {
        return None;
    }
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    let tier1_patterns = [
        "cargo test",
        "cargo fmt",
        "cargo clippy",
        "cargo build",
        "swift build",
        "swift test",
        "swift format",
    ];
    if !tier1_patterns.iter().any(|p| lower.contains(p)) {
        return None;
    }
    let reason_key = reason.unwrap_or("tier1");
    Some(format!("{}::{}", tool_name, reason_key))
}

fn maybe_record_applied_patch(
    tool_name: &str,
    args: &Value,
    result: &Value,
    repo_root: &std::path::Path,
    session_path: &std::path::Path,
) -> Result<()> {
    if tool_name != "apply_patch" {
        return Ok(());
    }
    let applied = result
        .get("applied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !applied {
        return Ok(());
    }
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if patch.trim().is_empty() {
        return Ok(());
    }
    let store = PatchStore::default()?;
    store.append(repo_root, patch, Some(session_path))?;
    Ok(())
}

fn emit_max_steps_notice(
    session: &SessionStore,
    max_steps: u32,
    output: &dyn OutputSink,
) -> Result<()> {
    let message = format!(
        "max_steps ({max_steps}) reached before the task completed; increase the step limit or use a workflow preset."
    );
    output.stderr_line(&message);
    session.append(
        "runtime_notice",
        json!({
            "kind": "max_steps_reached",
            "max_steps": max_steps,
            "message": message
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn describe_run_shell_command_includes_command_and_cwd() {
        let description = describe_tool_call(
            "run_shell_command",
            &json!({
                "command": "swift test",
                "working_directory": "Tests",
                "timeout_seconds": 600
            }),
        );
        assert_eq!(
            description,
            "run_shell_command command=\"swift test\" cwd=\"Tests\" timeout=600s"
        );
    }

    #[test]
    fn describe_run_tests_infers_underlying_command() {
        let description = describe_tool_call("run_tests", &json!({ "language": "swift" }));
        assert_eq!(
            description,
            "run_tests language=\"swift\" command=\"swift test\""
        );
    }

    #[test]
    fn describe_apply_patch_lists_files() {
        let patch = "\
diff --git a/Tests/AppTests/File.swift b/Tests/AppTests/File.swift
--- a/Tests/AppTests/File.swift
+++ b/Tests/AppTests/File.swift
@@ -1 +1 @@
-old
+new
";
        let description = describe_tool_call("apply_patch", &json!({ "patch": patch }));
        assert!(
            description.contains("Tests/AppTests/File.swift"),
            "description should include patched file"
        );
        assert!(
            description.contains("lines="),
            "description should include line count"
        );
    }

    #[test]
    fn render_command_like_result_uses_compact_status() {
        let rendered = render_command_like_result(
            "build_project",
            &json!({
                "command": "swift build",
                "working_directory": ".",
                "exit_code": 1,
                "duration_ms": 312,
                "timed_out": false,
                "blocked": false,
                "approval_required": false,
                "was_truncated": false,
                "stdout": "",
                "stderr": "compile failed"
            }),
        );

        assert!(rendered.starts_with("build_project failed exit=1"));
        assert!(!rendered.contains("timed_out=false"));
        assert!(!rendered.contains("approval_required=false"));
        assert!(rendered.contains("\nstderr\ncompile failed"));
    }

    #[test]
    fn render_apply_patch_result_uses_status_words() {
        let rendered = render_apply_patch_result(&json!({
            "valid": false,
            "applied": false,
            "approval_required": false,
            "check_stderr": "corrupt patch",
            "apply_stderr": ""
        }));

        assert!(rendered.starts_with("apply_patch invalid"));
        assert!(!rendered.contains("valid=false"));
        assert!(rendered.contains("\ncheck\ncorrupt patch"));
    }

    #[test]
    fn compact_result_preview_keeps_summary_on_first_line() {
        let rendered = render_command_like_result(
            "build_project",
            &json!({
                "command": "swift build",
                "working_directory": ".",
                "exit_code": 1,
                "duration_ms": 312,
                "timed_out": false,
                "blocked": false,
                "approval_required": false,
                "was_truncated": false,
                "stdout": "a",
                "stderr": "b"
            }),
        );

        let mut lines = rendered.lines();
        let summary = lines.next().expect("summary line");
        let detail = lines.collect::<Vec<_>>().join("\n");

        assert_eq!(
            summary,
            "build_project failed exit=1 command=\"swift build\" cwd=\".\" duration=312ms"
        );
        assert!(detail.contains("stdout"));
        assert!(detail.contains("stderr"));
    }

    #[test]
    fn verification_followup_message_reports_failed_run_tests() {
        let mut state = VerificationState::default();
        update_verification_state(
            &mut state,
            "run_tests",
            &json!({ "language": "swift" }),
            &json!({ "exit_code": 1, "timed_out": false, "blocked": false }),
        );
        let message = verification_followup_message(&state).expect("message");
        assert!(message.contains("run_tests"));
        assert!(message.contains("exited with code 1"));
        assert!(message.contains("Do not claim success yet"));
    }

    #[test]
    fn verification_state_clears_after_success() {
        let mut state = VerificationState::default();
        update_verification_state(
            &mut state,
            "run_tests",
            &json!({ "language": "swift" }),
            &json!({ "exit_code": 1, "timed_out": false, "blocked": false }),
        );
        update_verification_state(
            &mut state,
            "run_tests",
            &json!({ "language": "swift" }),
            &json!({ "exit_code": 0, "timed_out": false, "blocked": false }),
        );
        assert!(verification_followup_message(&state).is_none());
    }
}
