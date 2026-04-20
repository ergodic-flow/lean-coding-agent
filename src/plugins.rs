use std::fs;
use std::path::Path;

use mlua::{Lua, LuaSerdeExt};

use crate::api::{ToolDef, ToolFunction};

pub struct PluginManager {
    lua: Lua,
    tools: Vec<ToolDef>,
}

impl PluginManager {
    pub fn new() -> Self {
        let lua = Lua::new();

        let handlers = lua.create_table().expect("lua table alloc");
        lua.set_named_registry_value("_handlers", handlers)
            .expect("lua registry");

        lua.globals()
            .set(
                "tool",
                lua.create_function(|lua, def: mlua::Value| {
                    let tools: mlua::Table = lua.globals().get("_plugins_acc")?;
                    tools.push(def)?;
                    Ok(())
                })
                .expect("lua create_function"),
            )
            .expect("lua globals");

        lua.globals()
            .set(
                "http_get",
                lua.create_function(|_, url: String| {
                    let agent = ureq::AgentBuilder::new()
                        .user_agent("coding-agent/0.1")
                        .timeout_read(std::time::Duration::from_secs(30))
                        .build();
                    let response = agent.get(&url).call().map_err(|e| {
                        mlua::Error::external(format!("HTTP request failed: {}", e))
                    })?;
                    response.into_string().map_err(|e| {
                        mlua::Error::external(format!("Failed to read response: {}", e))
                    })
                })
                .expect("lua create_function http_get"),
            )
            .expect("lua globals set http_get");

        lua.globals()
            .set(
                "json_decode",
                lua.create_function(|lua, json_str: String| {
                    let val: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| {
                        mlua::Error::external(format!("JSON parse error: {}", e))
                    })?;
                    lua.to_value(&val)
                })
                .expect("lua create_function json_decode"),
            )
            .expect("lua globals set json_decode");

        Self {
            lua,
            tools: Vec::new(),
        }
    }

    pub fn load_dir(&mut self, dir: &Path) -> Result<(), String> {
        if !dir.exists() {
            return Ok(());
        }

        let entries: Vec<_> = fs::read_dir(dir)
            .map_err(|e| format!("Cannot read '{}': {}", dir.display(), e))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "lua"))
            .collect();

        for entry in entries {
            let path = entry.path();
            match self.load_file(&path) {
                Ok(count) => eprintln!("[plugins] loaded {} tool(s) from {}", count, path.display()),
                Err(e) => eprintln!("[plugins] error in {}: {}", path.display(), e),
            }
        }

        Ok(())
    }

    fn load_file(&mut self, path: &Path) -> Result<usize, String> {
        let acc = self.lua.create_table().map_err(|e| e.to_string())?;
        self.lua
            .globals()
            .set("_plugins_acc", acc)
            .map_err(|e| e.to_string())?;

        let code =
            fs::read_to_string(path).map_err(|e| format!("Read error: {}", e))?;

        self.lua
            .load(&code)
            .set_name(path.to_str().unwrap_or("plugin"))
            .exec()
            .map_err(|e| format!("Error in {}: {}", path.display(), e))?;

        let acc: mlua::Table = self
            .lua
            .globals()
            .get("_plugins_acc")
            .map_err(|e| e.to_string())?;
        let handlers: mlua::Table = self
            .lua
            .named_registry_value("_handlers")
            .map_err(|e| e.to_string())?;

        let mut count = 0;
        for def in acc.sequence_values::<mlua::Table>() {
            let def = def.map_err(|e| format!("Bad tool definition: {}", e))?;

            let name: String = def
                .get("name")
                .map_err(|e| format!("Missing 'name': {}", e))?;
            let description: String = def
                .get("description")
                .map_err(|e| format!("Missing 'description': {}", e))?;
            let params_val: mlua::Value = def
                .get("parameters")
                .map_err(|e| format!("Missing 'parameters': {}", e))?;
            let handler: mlua::Function = def
                .get("handler")
                .map_err(|e| format!("Missing 'handler': {}", e))?;

            let parameters: serde_json::Value = self
                .lua
                .from_value(params_val)
                .map_err(|e| format!("Invalid parameters for '{}': {}", name, e))?;

            handlers
                .set(name.clone(), handler)
                .map_err(|e| e.to_string())?;

            self.tools.push(ToolDef {
                tool_type: "function".into(),
                function: ToolFunction {
                    name,
                    description,
                    parameters,
                },
            });

            count += 1;
        }

        Ok(count)
    }

    pub fn definitions(&self) -> &[ToolDef] {
        &self.tools
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.function.name == name)
    }

    pub fn execute(&self, name: &str, args: serde_json::Value) -> String {
        let handlers: mlua::Table = match self.lua.named_registry_value("_handlers") {
            Ok(h) => h,
            Err(e) => return format!("Registry error: {}", e),
        };

        let handler: mlua::Function = match handlers.get(name) {
            Ok(h) => h,
            Err(e) => return format!("No handler for '{}': {}", name, e),
        };

        let args_val: mlua::Value = match self.lua.to_value(&args) {
            Ok(v) => v,
            Err(e) => return format!("Args conversion error: {}", e),
        };

        match handler.call::<String>(args_val) {
            Ok(s) => s,
            Err(e) => format!("Plugin '{}' error: {}", name, e),
        }
    }
}
