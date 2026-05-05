use std::io::Read;

use serde::{Deserialize, Serialize, Serializer};

fn serialize_provider<S: Serializer>(val: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
    match val {
        Some(name) => {
            #[derive(Serialize)]
            struct ProviderPayload {
                order: Vec<String>,
            }
            s.serialize_some(&ProviderPayload { order: vec![name.clone()] })
        }
        None => s.serialize_none(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_provider"
    )]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interleaved: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamOptions {
    pub include_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Multimodal(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlData },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrlData {
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "system")]
    System { content: String },
    #[serde(rename = "user")]
    User { content: UserContent },
    #[serde(rename = "assistant")]
    Assistant {
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct ResponseMessage {
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, alias = "reasoning")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StreamChunk {
    pub choices: Vec<StreamChoice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct StreamChoice {
    pub delta: StreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamDelta {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<StreamToolCall>>,
    #[serde(default, alias = "reasoning")]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct StreamToolCall {
    pub index: u32,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    #[serde(default)]
    pub function: Option<StreamFunctionCall>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamFunctionCall {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum StreamEvent {
    ContentDelta(String),
    ThinkingDelta(String),
    ToolCallBegin { index: usize, id: String, name: String },
    ToolCallDelta { index: usize, arguments: String },
    Done { message: ResponseMessage, usage: Option<Usage> },
}

pub struct ApiClient {
    base_url: String,
    api_key: Option<String>,
    agent: ureq::Agent,
}

impl ApiClient {
    pub fn new(base_url: String, api_key: Option<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_read(std::time::Duration::from_secs(300))
            .timeout_write(std::time::Duration::from_secs(30))
            .build();
        Self {
            base_url,
            api_key,
            agent,
        }
    }

    pub fn chat_stream<F>(&self, request: &ChatRequest, mut on_event: F) -> Result<(), String>
    where
        F: FnMut(StreamEvent) -> Result<(), String>,
    {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut req = self.agent.post(&url);

        if let Some(ref key) = self.api_key {
            req = req.set("Authorization", &format!("Bearer {}", key));
        }

        let body = serde_json::to_value(request).map_err(|e| format!("serialize: {}", e))?;
        let response = match req.send_json(body) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let status = response.status_text().to_string();
                let body = response
                    .into_string()
                    .unwrap_or_else(|e| format!("<failed to read error body: {}>", e));
                return Err(format!("API request failed: {} {}: {}", code, status, body));
            }
            Err(e) => return Err(format!("API request failed: {}", e)),
        };

        let mut reader = response.into_reader();
        let mut buffer = String::new();
        let mut byte_buf = [0u8; 4096];

        let mut tool_calls: Vec<(String, String, String)> = Vec::new();
        let mut content_acc = String::new();
        let mut reasoning_acc = String::new();

        loop {
            let n = reader
                .read(&mut byte_buf)
                .map_err(|e| format!("read stream: {}", e))?;
            if n == 0 {
                break;
            }
            let text = std::str::from_utf8(&byte_buf[..n])
                .map_err(|e| format!("utf8 decode: {}", e))?;
            buffer.push_str(text);

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();

                if !line.starts_with("data: ") {
                    continue;
                }
                let data = &line[6..];
                if data == "[DONE]" {
                    let msg = ResponseMessage {
                        role: "assistant".to_string(),
                        content: if content_acc.is_empty() {
                            None
                        } else {
                            Some(content_acc.clone())
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(
                                tool_calls
                                    .into_iter()
                                    .map(|(id, name, arguments)| ToolCall {
                                        id,
                                        call_type: "function".to_string(),
                                        function: FunctionCall { name, arguments },
                                    })
                                    .collect(),
                            )
                        },
                        reasoning_content: if reasoning_acc.is_empty() {
                            None
                        } else {
                            Some(reasoning_acc.clone())
                        },
                    };
                    // usage comes from the last chunk, not accumulated here
                    on_event(StreamEvent::Done {
                        message: msg,
                        usage: None,
                    })?;
                    return Ok(());
                }

                if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(error) = value.get("error") {
                        return Err(format!("API stream error: {}", error));
                    }
                }

                let chunk: StreamChunk =
                    serde_json::from_str(data).map_err(|e| format!("parse chunk: {} - {}", e, data))?;

                if let Some(usage) = &chunk.usage {
                    on_event(StreamEvent::Done {
                        message: ResponseMessage {
                            role: "assistant".to_string(),
                            content: if content_acc.is_empty() {
                                None
                            } else {
                                Some(content_acc.clone())
                            },
                            tool_calls: if tool_calls.is_empty() {
                                None
                            } else {
                                Some(
                                    tool_calls
                                        .iter()
                                        .map(|(id, name, arguments)| ToolCall {
                                            id: id.clone(),
                                            call_type: "function".to_string(),
                                            function: FunctionCall {
                                                name: name.clone(),
                                                arguments: arguments.clone(),
                                            },
                                        })
                                        .collect(),
                                )
                            },
                            reasoning_content: if reasoning_acc.is_empty() {
                                None
                            } else {
                                Some(reasoning_acc.clone())
                            },
                        },
                        usage: Some(usage.clone()),
                    })?;
                    return Ok(());
                }

                let choice = match chunk.choices.first() {
                    Some(c) => c,
                    None => continue,
                };

                if let Some(ref reasoning) = choice.delta.reasoning_content {
                    reasoning_acc.push_str(reasoning);
                    on_event(StreamEvent::ThinkingDelta(reasoning.clone()))?;
                }

                if let Some(text) = choice.delta.content.as_ref().filter(|text| !text.is_empty()) {
                    content_acc.push_str(text);
                    on_event(StreamEvent::ContentDelta(text.clone()))?;
                }

                if let Some(ref calls) = choice.delta.tool_calls {
                    for tc in calls {
                        let idx = tc.index as usize;
                        while tool_calls.len() <= idx {
                            tool_calls.push((String::new(), String::new(), String::new()));
                        }
                        if let Some(ref id) = tc.id {
                            tool_calls[idx].0 = id.clone();
                        }
                        if let Some(ref func) = tc.function {
                            if let Some(ref name) = func.name {
                                tool_calls[idx].1 = name.clone();
                                on_event(StreamEvent::ToolCallBegin {
                                    index: idx,
                                    id: tool_calls[idx].0.clone(),
                                    name: name.clone(),
                                })?;
                            }
                            if let Some(ref args) = func.arguments {
                                tool_calls[idx].2.push_str(args);
                                on_event(StreamEvent::ToolCallDelta {
                                    index: idx,
                                    arguments: args.clone(),
                                })?;
                            }
                        }
                    }
                }


            }
        }

        Ok(())
    }
}

