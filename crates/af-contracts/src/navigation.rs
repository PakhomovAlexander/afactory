//! One bounded navigation wire contract shared by the broker and restricted transport.
use serde::{Deserialize, Serialize};
use std::io;
pub const MAX_FILES: usize = 16_384;
pub const SCHEMA: &str = include_str!("../../../schemas/source-navigation-v3.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "af_list_paths")]
    ListPaths { offset: u32 },
    #[serde(rename = "af_read_range")]
    ReadRange {
        path: String,
        offset: u32,
        length: u32,
    },
    #[serde(rename = "af_search")]
    Search { query: String, offset: u32 },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "tool", deny_unknown_fields)]
pub enum Response {
    #[serde(rename = "af_list_paths")]
    ListPaths {
        paths: Vec<String>,
        #[serde(deserialize_with = "Option::deserialize")]
        next_offset: Option<u32>,
    },
    #[serde(rename = "af_read_range")]
    ReadRange {
        #[serde(deserialize_with = "Option::deserialize")]
        text: Option<String>,
    },
    #[serde(rename = "af_search")]
    Search {
        hits: Vec<Hit>,
        #[serde(deserialize_with = "Option::deserialize")]
        next_offset: Option<u32>,
        non_text_paths: Vec<String>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub text: String,
}
fn denied() -> io::Error {
    io::Error::other("source navigation refused or backend unavailable")
}
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 240
        && path.is_ascii()
        && path.split('/').all(|c| {
            !c.is_empty()
                && c != "."
                && c != ".."
                && ![".git", ".hg", ".svn"]
                    .iter()
                    .any(|v| c.eq_ignore_ascii_case(v))
                && c.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
impl Request {
    /// Strict typed admission, including duplicate/unknown keys and owner-input denial.
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() > 4096 {
            return Err(denied());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|_| denied())?;
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> io::Result<()> {
        let valid = match self {
            Self::ListPaths { offset } => *offset as usize <= MAX_FILES,
            Self::ReadRange { path, length, .. } => {
                valid_path(path) && *length > 0 && *length <= 65536
            }
            Self::Search { query, .. } => {
                !query.is_empty()
                    && query.is_ascii()
                    && query.len() <= 256
                    && !query.contains(['\n', '\r', '\0'])
            }
        };
        if valid { Ok(()) } else { Err(denied()) }
    }
}

/// Parameters are projected from the same versioned schema the broker admits.
pub fn tool_schemas() -> serde_json::Value {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("static navigation schema");
    serde_json::Value::Array(schema["oneOf"].as_array().expect("request alternatives").iter().map(|request| {
        let mut parameters = request.clone();
        let name = parameters["properties"]["tool"]["const"].as_str().expect("tool discriminator").to_owned();
        parameters["properties"].as_object_mut().unwrap().remove("tool");
        parameters["required"].as_array_mut().unwrap().retain(|field| field != "tool");
        serde_json::json!({"type":"function", "name":name,"description":match name.as_str() {"af_read_range"=>"Read a UTF-8 byte range; null text means non-text file", "af_search"=>"Search literal source text with a paginated hit offset", _=>"List source paths with a paginated path offset"},"inputSchema":parameters})
    }).collect())
}
