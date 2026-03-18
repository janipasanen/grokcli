use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct ContextBudgetManager {
    max_bytes: usize,
    used_bytes: usize,
}

impl ContextBudgetManager {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
        }
    }

    pub fn fit_tool_output(&mut self, output: Value) -> Value {
        let serialized = serde_json::to_string(&output).unwrap_or_default();
        let next = serialized.len();
        if self.used_bytes + next <= self.max_bytes {
            self.used_bytes += next;
            return output;
        }

        let remaining = self.max_bytes.saturating_sub(self.used_bytes);
        let mut summary = serialized;
        if summary.len() > remaining.min(1024) {
            summary.truncate(remaining.min(1024));
            summary.push_str("...[budget truncated]");
        }
        self.used_bytes = self.max_bytes;
        json!({
            "summary": summary,
            "budget_truncated": true
        })
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}
