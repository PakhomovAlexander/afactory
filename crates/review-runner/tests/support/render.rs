//! The prompt section a model adapter appends for one `ReviewerInputs`.
//!
//! Shared by the render suites through `#[path]`; `ReviewerInputs::render_into` writes into a
//! caller-owned buffer, and every assertion here wants the section on its own.

use review_runner::ReviewerInputs;

pub fn render(inputs: &ReviewerInputs) -> Result<String, String> {
    let mut prompt = String::new();
    inputs.render_into(&mut prompt)?;
    Ok(prompt)
}
