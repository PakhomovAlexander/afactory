//! Bounded ADF normalization. Unsupported semantic nodes fail explicitly instead of disappearing.
use crate::SourceError;
use serde_json::Value;

pub fn plain_text(value: &Value) -> Result<String, SourceError> {
    if value.get("type").and_then(Value::as_str) != Some("doc")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(invalid());
    }
    let mut state = Renderer { nodes: 0 };
    let text = state.node(value, 0)?;
    if text.len() > 131072 || text.trim().is_empty() {
        return Err(invalid());
    }
    Ok(text.trim_end_matches('\n').into())
}
fn invalid() -> SourceError {
    SourceError::Invalid(
        "Unsupported or malformed ADF; normalize the selected field explicitly".into(),
    )
}
struct Renderer {
    nodes: usize,
}
impl Renderer {
    fn node(&mut self, value: &Value, depth: usize) -> Result<String, SourceError> {
        self.nodes += 1;
        if self.nodes > 4096 || depth > 24 {
            return Err(invalid());
        }
        let object = value.as_object().ok_or_else(invalid)?;
        if object.keys().any(|k| {
            !matches!(
                k.as_str(),
                "type" | "version" | "content" | "text" | "attrs" | "marks"
            )
        }) {
            return Err(invalid());
        }
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if kind != "doc" && value.get("version").is_some() {
            return Err(invalid());
        }
        if kind == "text" {
            if value.get("content").is_some() || value.get("attrs").is_some() {
                return Err(invalid());
            }
            let mut text = value
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?
                .to_owned();
            if let Some(marks) = value.get("marks") {
                for mark in marks.as_array().ok_or_else(invalid)? {
                    let shape = mark.as_object().ok_or_else(invalid)?;
                    let is_link = mark.get("type").and_then(Value::as_str) == Some("link");
                    if shape
                        .keys()
                        .any(|k| k != "type" && !(is_link && k == "attrs"))
                    {
                        return Err(invalid());
                    }
                    if is_link
                        && mark
                            .get("attrs")
                            .and_then(Value::as_object)
                            .is_none_or(|attrs| {
                                attrs
                                    .keys()
                                    .any(|k| !matches!(k.as_str(), "href" | "title"))
                            })
                    {
                        return Err(invalid());
                    }
                    text = match mark.get("type").and_then(Value::as_str) {
                        Some("strong") => format!("**{text}**"),
                        Some("em") => format!("_{text}_"),
                        Some("strike") => format!("~~{text}~~"),
                        Some("code") => format!("`{text}`"),
                        Some("underline") => format!("<u>{text}</u>"),
                        Some("link") => {
                            let href = mark
                                .pointer("/attrs/href")
                                .and_then(Value::as_str)
                                .ok_or_else(invalid)?;
                            let title = match mark.pointer("/attrs/title") {
                                None => String::new(),
                                Some(Value::String(s)) => format!(" {s:?}"),
                                _ => return Err(invalid()),
                            };
                            format!("[{text}]({href}{title})")
                        }
                        _ => return Err(invalid()),
                    };
                    if text.len() > 131072 {
                        return Err(invalid());
                    }
                }
            }
            return Ok(text);
        }
        if value.get("text").is_some() || value.get("marks").is_some() {
            return Err(invalid());
        }
        let children = match value.get("content") {
            Some(Value::Array(items)) => items.as_slice(),
            None => &[],
            _ => return Err(invalid()),
        };
        let mut content = String::new();
        let attrs = value.get("attrs");
        let allowed_attrs: &[&str] = match kind {
            "heading" => &["level"],
            "orderedList" => &["order"],
            "codeBlock" => &["language"],
            _ => &[],
        };
        if let Some(attrs) = attrs {
            if attrs
                .as_object()
                .ok_or_else(invalid)?
                .keys()
                .any(|k| !allowed_attrs.contains(&k.as_str()))
            {
                return Err(invalid());
            }
        }
        for (i, child) in children.iter().enumerate() {
            let mut part = self.node(child, depth + 1)?;
            if matches!(kind, "bulletList" | "orderedList") {
                if child.get("type").and_then(Value::as_str) != Some("listItem") {
                    return Err(invalid());
                }
                let marker = if kind == "bulletList" {
                    "- ".into()
                } else {
                    let start = match value.pointer("/attrs/order") {
                        None => 1,
                        Some(n) => n.as_u64().filter(|n| *n <= 1000000).ok_or_else(invalid)?,
                    };
                    format!("{}. ", start + i as u64)
                };
                let mut lines = part.trim_end_matches('\n').split('\n');
                let mut item = format!("{marker}{}\n", lines.next().unwrap_or_default());
                let indent = " ".repeat(marker.len());
                for line in lines {
                    item.push_str(&indent);
                    item.push_str(line);
                    item.push('\n');
                }
                part = item;
            }
            content.push_str(&part);
            if content.len() > 131072 {
                return Err(invalid());
            }
        }
        match kind {
            "doc" if depth == 0 => Ok(content),
            "paragraph" | "listItem" => Ok(format!("{content}\n")),
            "heading" => {
                let level = value
                    .pointer("/attrs/level")
                    .and_then(Value::as_u64)
                    .filter(|n| (1..=6).contains(n))
                    .ok_or_else(invalid)?;
                Ok(format!("{} {content}\n", "#".repeat(level as usize)))
            }
            "bulletList" | "orderedList" => Ok(content),
            "blockquote" => Ok(content.lines().map(|s| format!("> {s}\n")).collect()),
            "codeBlock" => {
                let language = match value.pointer("/attrs/language") {
                    None => "",
                    Some(s) => s.as_str().ok_or_else(invalid)?,
                };
                Ok(format!("```{language}\n{content}\n```\n"))
            }
            "hardBreak" if children.is_empty() => Ok("\n".into()),
            "rule" if children.is_empty() => Ok("\n---\n".into()),
            _ => Err(invalid()),
        }
    }
}
