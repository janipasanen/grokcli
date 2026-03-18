use clap::ValueEnum;

#[derive(Debug, Clone, ValueEnum)]
pub enum WorkflowPreset {
    RustTests,
    SwiftBuild,
    ReviewChanged,
}

pub fn apply_preset_prompt(preset: &WorkflowPreset, prompt: &str) -> String {
    match preset {
        WorkflowPreset::RustTests => {
            format!(
                "Rust test-fix workflow.\n1) Run cargo test.\n2) Inspect failing modules only.\n3) Propose minimal patch.\n4) Re-run cargo test.\n\nUser task: {prompt}"
            )
        }
        WorkflowPreset::SwiftBuild => {
            format!(
                "Swift build-diagnosis workflow.\n1) Run swift build.\n2) Read files from diagnostics.\n3) Explain root cause.\n4) Propose minimal fix.\n\nUser task: {prompt}"
            )
        }
        WorkflowPreset::ReviewChanged => {
            format!(
                "Code review workflow.\n1) Run git diff --unified=3.\n2) Identify risks and regressions.\n3) Return findings by severity.\n\nUser task: {prompt}"
            )
        }
    }
}
