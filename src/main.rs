use std::env;
use std::io::{self, BufRead, BufReader, Write};
use std::process::Command;

use serde_json::{json, Value};

const HOME: &str = env!("CARGO_MANIFEST_DIR");
const SYSTEM: &str = include_str!("../prompt.md");
const MODEL: &str = "grok-4.5";
const URL: &str = "https://api.x.ai/v1/chat/completions";
const MAX_OUTPUT: usize = 40_000;
const MAX_DEPTH: u32 = 4;

const BASH: &str = "Run a bash command on the local machine and return its combined stdout and stderr. This is your only tool and it runs with no approval gate.";

const USAGE: &str = "\
spore: a single-tool (bash) terminal coding agent.

usage:
  spore               interactive session
  spore -p \"<task>\"   run one task, print the result, exit

env:
  XAI_API_KEY         api key (or SPORE_API_KEY / OPENAI_API_KEY)
  SPORE_API_KEY_CMD   command printing a fresh key each request (OAuth)
  SPORE_MODEL         model id (default grok-4.5)
  SPORE_BASE_URL      chat/completions endpoint (default api.x.ai)
";

fn env_opt(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

fn die(msg: String) -> ! {
    eprintln!("spore: {msg}");
    std::process::exit(1)
}

struct Spore {
    key: Option<String>,
    key_cmd: Option<String>,
    model: String,
    url: String,
    depth: u32,
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }

    let key_cmd = env_opt("SPORE_API_KEY_CMD");
    let key = env_opt("SPORE_API_KEY")
        .or_else(|| env_opt("XAI_API_KEY"))
        .or_else(|| env_opt("OPENAI_API_KEY"));
    if key.is_none() && key_cmd.is_none() {
        die("set XAI_API_KEY, OPENAI_API_KEY, SPORE_API_KEY, or SPORE_API_KEY_CMD".into());
    }

    let depth: u32 = env_opt("SPORE_DEPTH")
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    if depth > MAX_DEPTH {
        die(format!("max subagent depth ({MAX_DEPTH}) reached"));
    }

    let spore = Spore {
        key,
        key_cmd,
        model: env_opt("SPORE_MODEL").unwrap_or_else(|| MODEL.into()),
        url: env_opt("SPORE_BASE_URL").unwrap_or_else(|| URL.into()),
        depth,
    };

    let task: Vec<&str> = args
        .iter()
        .filter(|a| *a != "-p" && *a != "--print")
        .map(String::as_str)
        .collect();
    if task.is_empty() {
        spore.repl();
    } else {
        let mut msgs = vec![json!({"role": "user", "content": task.join(" ")})];
        println!("{}", spore.run(&mut msgs, true).trim_end());
    }
}

impl Spore {
    fn repl(&self) {
        println!("\x1b[1mspore\x1b[0m \x1b[2m· one tool: bash · ctrl-d to exit\x1b[0m");
        let stdin = io::stdin();
        let mut msgs = vec![];
        loop {
            print!("\n\x1b[32m❯\x1b[0m ");
            let _ = io::stdout().flush();
            let mut line = String::new();
            if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "/exit" {
                return;
            }
            msgs.push(json!({"role": "user", "content": line}));
            self.run(&mut msgs, false);
        }
    }

    fn run(&self, msgs: &mut Vec<Value>, headless: bool) -> String {
        loop {
            let (assistant, calls, text) = self.turn(msgs, headless);
            msgs.push(assistant);
            if calls.is_empty() {
                return text;
            }
            for (id, cmd) in calls {
                eprintln!("\x1b[2m❯ {cmd}\x1b[0m");
                let out = self.bash(&cmd);
                for l in out.lines() {
                    eprintln!("\x1b[2m  {l}\x1b[0m");
                }
                msgs.push(json!({"role": "tool", "tool_call_id": id, "content": out}));
            }
        }
    }

    fn turn(&self, msgs: &[Value], headless: bool) -> (Value, Vec<(String, String)>, String) {
        let mut all = vec![json!({"role": "system", "content": SYSTEM})];
        all.extend(msgs.iter().cloned());
        let body = json!({
            "model": self.model,
            "messages": all,
            "stream": true,
            "tools": [{"type": "function", "function": {
                "name": "bash",
                "description": BASH,
                "parameters": {
                    "type": "object",
                    "properties": {"cmd": {"type": "string", "description": "The bash command to run."}},
                    "required": ["cmd"],
                },
            }}],
        });

        let resp = ureq::post(&self.url)
            .set("authorization", &format!("Bearer {}", self.token()))
            .set("content-type", "application/json")
            .send_string(&body.to_string());
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                die(format!("api {code}: {}", r.into_string().unwrap_or_default()))
            }
            Err(e) => die(e.to_string()),
        };

        let mut text = String::new();
        let mut calls: Vec<(String, String)> = vec![];
        for line in BufReader::new(resp.into_reader()).lines().map_while(Result::ok) {
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            if data.trim() == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = &v["choices"][0]["delta"];
            if let Some(t) = delta["content"].as_str() {
                text.push_str(t);
                emit(t, headless);
            }
            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    let i = tc["index"].as_u64().unwrap_or(0) as usize;
                    while calls.len() <= i {
                        calls.push((String::new(), String::new()));
                    }
                    if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                        calls[i].0 = id.into();
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        calls[i].1.push_str(a);
                    }
                }
            }
        }
        if !text.is_empty() {
            emit("\n", headless);
        }

        let mut assistant = json!({"role": "assistant", "content": text});
        if !calls.is_empty() {
            if text.is_empty() {
                assistant["content"] = Value::Null;
            }
            assistant["tool_calls"] = calls
                .iter()
                .map(|(id, args)| {
                    json!({"id": id, "type": "function",
                           "function": {"name": "bash", "arguments": args}})
                })
                .collect();
        }

        let cmds = calls
            .iter()
            .map(|(id, args)| {
                let cmd = serde_json::from_str::<Value>(args)
                    .ok()
                    .and_then(|v| v["cmd"].as_str().map(str::to_string))
                    .unwrap_or_default();
                (id.clone(), cmd)
            })
            .collect();
        (assistant, cmds, text)
    }

    fn bash(&self, cmd: &str) -> String {
        let out = Command::new("bash")
            .arg("-c")
            .arg(cmd)
            .env("SPORE_DEPTH", (self.depth + 1).to_string())
            .env("SPORE_HOME", HOME)
            .output();
        let o = match out {
            Ok(o) => o,
            Err(e) => return format!("bash failed to start: {e}"),
        };
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&o.stderr));
        let code = o.status.code().unwrap_or(-1);
        if s.trim().is_empty() {
            s = format!("(no output; exit {code})");
        } else if code != 0 {
            s.push_str(&format!("\n[exit {code}]"));
        }
        truncate(s)
    }

    fn token(&self) -> String {
        let Some(cmd) = &self.key_cmd else {
            return self.key.clone().unwrap_or_default();
        };
        match Command::new("bash").arg("-c").arg(cmd).output() {
            Ok(o) if o.status.success() => {
                let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if t.is_empty() {
                    die("SPORE_API_KEY_CMD produced no token".into());
                }
                t
            }
            _ => die("SPORE_API_KEY_CMD failed".into()),
        }
    }
}

fn emit(s: &str, headless: bool) {
    if headless {
        eprint!("{s}");
        let _ = io::stderr().flush();
    } else {
        print!("{s}");
        let _ = io::stdout().flush();
    }
}

fn truncate(s: String) -> String {
    let c: Vec<char> = s.chars().collect();
    if c.len() <= MAX_OUTPUT {
        return s;
    }
    let h = MAX_OUTPUT / 2;
    let head: String = c[..h].iter().collect();
    let tail: String = c[c.len() - h..].iter().collect();
    format!(
        "{head}\n… [{} chars truncated] …\n{tail}",
        c.len() - MAX_OUTPUT
    )
}
