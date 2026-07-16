use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::FromRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const HOME: &str = env!("CARGO_MANIFEST_DIR");
const SYSTEM: &str = include_str!("../prompt.md");
const MODEL: &str = "grok-4.5";
const URL: &str = "https://api.x.ai/v1/chat/completions";
const MAX_OUTPUT: usize = 40_000;
const MAX_DEPTH: u32 = 4;
const BASH_TIMEOUT: Duration = Duration::from_secs(300);

const OAUTH_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const OAUTH_DEVICE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const OAUTH_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const OAUTH_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const OAUTH_SKEW: u64 = 120;

const BASH: &str = "Run a bash command on the local machine and return its combined stdout and stderr. This is your only tool and it runs with no approval gate. stdin is closed and the command is killed after 300 seconds, so run anything long-lived in the background (nohup … & ).";

const USAGE: &str = "\
spore: a single-tool (bash) terminal coding agent.

usage:
  spore               interactive session
  spore -p \"<task>\"   run one task, print the result, exit
  spore login         provider setup wizard (API key or xAI browser sign-in)

env (also settable in ~/.config/spore/config as KEY=value lines):
  XAI_API_KEY         api key (or SPORE_API_KEY / OPENAI_API_KEY)
  SPORE_API_KEY_CMD   command printing a fresh key each request (OAuth)
  SPORE_MODEL         model id (default grok-4.5)
  SPORE_BASE_URL      chat/completions endpoint (default api.x.ai)
";

struct Provider {
    name: &'static str,
    url: &'static str,
    model: &'static str,
    console: Option<&'static str>, // None => local, no key
}

const PROVIDERS: &[Provider] = &[
    Provider { name: "xai", url: "https://api.x.ai/v1/chat/completions", model: "grok-4.5", console: Some("https://console.x.ai") },
    Provider { name: "openai", url: "https://api.openai.com/v1/chat/completions", model: "gpt-5.2", console: Some("https://platform.openai.com/api-keys") },
    Provider { name: "anthropic", url: "https://api.anthropic.com/v1/chat/completions", model: "claude-sonnet-5", console: Some("https://console.anthropic.com/settings/keys") },
    Provider { name: "gemini", url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", model: "gemini-2.5-pro", console: Some("https://aistudio.google.com/apikey") },
    Provider { name: "groq", url: "https://api.groq.com/openai/v1/chat/completions", model: "moonshotai/kimi-k2-instruct", console: Some("https://console.groq.com/keys") },
    Provider { name: "deepseek", url: "https://api.deepseek.com/v1/chat/completions", model: "deepseek-chat", console: Some("https://platform.deepseek.com/api_keys") },
    Provider { name: "openrouter", url: "https://openrouter.ai/api/v1/chat/completions", model: "openrouter/auto", console: Some("https://openrouter.ai/settings/keys") },
    Provider { name: "ollama", url: "http://localhost:11434/v1/chat/completions", model: "qwen3", console: None },
];

fn env_opt(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

fn die(msg: String) -> ! {
    eprintln!("spore: {msg}");
    std::process::exit(1)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- config files: ~/.config/spore/{config,oauth}, KEY=value lines ----

fn config_dir() -> Option<PathBuf> {
    env_opt("HOME").map(|h| PathBuf::from(h).join(".config").join("spore"))
}

fn load_kv(name: &str) -> HashMap<String, String> {
    let Some(dir) = config_dir() else {
        return HashMap::new();
    };
    let Ok(s) = fs::read_to_string(dir.join(name)) else {
        return HashMap::new();
    };
    s.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn save_kv(name: &str, lines: &[(&str, &str)]) -> Result<(), String> {
    let dir = config_dir().ok_or("HOME not set")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    let body: String = lines.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| e.to_string())?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

// env wins, then the config file, then the compiled default at the call site
fn cfg_opt(file: &HashMap<String, String>, k: &str) -> Option<String> {
    env_opt(k).or_else(|| file.get(k).cloned().filter(|v| !v.is_empty()))
}

// ---- xAI OAuth (RFC 8628 device flow against auth.x.ai) ----

struct OAuth {
    access: String,
    refresh: String,
    expires: u64, // unix epoch, refresh skew already subtracted
}

impl OAuth {
    fn load() -> Option<OAuth> {
        let kv = load_kv("oauth");
        let access = kv.get("SPORE_OAUTH_ACCESS").filter(|v| !v.is_empty())?.clone();
        Some(OAuth {
            access,
            refresh: kv.get("SPORE_OAUTH_REFRESH").cloned().unwrap_or_default(),
            expires: kv
                .get("SPORE_OAUTH_EXPIRES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        })
    }

    fn save(&self) -> Result<(), String> {
        let expires = self.expires.to_string();
        save_kv(
            "oauth",
            &[
                ("SPORE_OAUTH_ACCESS", &self.access),
                ("SPORE_OAUTH_REFRESH", &self.refresh),
                ("SPORE_OAUTH_EXPIRES", &expires),
            ],
        )
    }

    // Build from a token-endpoint grant. Callers decide how to handle a
    // failed save — refresh tokens rotate on every use, so a grant must never
    // be discarded just because persisting it failed.
    fn from_grant(v: &Value, old_refresh: &str) -> Result<OAuth, String> {
        let access = v["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("token response missing access_token")?
            .to_string();
        let refresh = v["refresh_token"].as_str().unwrap_or(old_refresh).to_string();
        let expires_in = v["expires_in"].as_u64().unwrap_or(21_600);
        Ok(OAuth {
            access,
            refresh,
            expires: now() + expires_in.saturating_sub(OAUTH_SKEW),
        })
    }

    fn fresh_access(&mut self) -> Result<String, String> {
        if now() < self.expires {
            return Ok(self.access.clone());
        }
        // Another spore process (e.g. a subagent) may have refreshed and
        // rotated the tokens already — pick up the on-disk state first.
        if let Some(disk) = OAuth::load() {
            if disk.access != self.access {
                *self = disk;
                if now() < self.expires {
                    return Ok(self.access.clone());
                }
            }
        }
        if self.refresh.is_empty() {
            return Err("OAuth token expired with no refresh token; run `spore login`".into());
        }
        let v = post_form(
            OAUTH_TOKEN_URL,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.refresh),
                ("client_id", OAUTH_CLIENT_ID),
            ],
        )?;
        if let Some(e) = v["error"].as_str() {
            return Err(format!("token refresh failed ({e}); run `spore login`"));
        }
        *self = OAuth::from_grant(&v, &self.refresh)?;
        // The old refresh token is already dead; keep running on the new
        // in-memory pair even if persisting fails.
        if let Err(e) = self.save() {
            eprintln!("spore: warning: could not save refreshed oauth tokens: {e}");
        }
        Ok(self.access.clone())
    }
}

fn http_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .build()
}

// POST a form and parse the JSON body, from both 2xx and 4xx responses —
// RFC 8628 reports polling state as HTTP 400 + {"error": …}.
fn post_form(url: &str, form: &[(&str, &str)]) -> Result<Value, String> {
    let resp = match http_agent().post(url).send_form(form) {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e.to_string()),
    };
    let body = resp.into_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|_| format!("bad token endpoint response: {body}"))
}

fn device_flow() -> Result<(), String> {
    let v = post_form(
        OAUTH_DEVICE_URL,
        &[("client_id", OAUTH_CLIENT_ID), ("scope", OAUTH_SCOPE)],
    )?;
    let device_code = v["device_code"].as_str().ok_or("bad device authorization response")?;
    let user_code = v["user_code"].as_str().unwrap_or("");
    let uri = v["verification_uri_complete"]
        .as_str()
        .or_else(|| v["verification_uri"].as_str())
        .ok_or("bad device authorization response")?;
    let mut interval = v["interval"].as_u64().unwrap_or(5);
    let deadline = now() + v["expires_in"].as_u64().unwrap_or(1800);

    println!("\nVisit {uri}\nCode: {user_code}\nWaiting for approval…");
    let _ = io::stdout().flush();
    open_browser(uri);

    loop {
        if now() > deadline {
            return Err("sign-in timed out".into());
        }
        std::thread::sleep(Duration::from_secs(interval));
        // A transient network blip mid-approval shouldn't abort the sign-in;
        // the deadline bounds how long this can retry.
        let Ok(v) = post_form(
            OAUTH_TOKEN_URL,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", OAUTH_CLIENT_ID),
            ],
        ) else {
            continue;
        };
        match v["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => interval += 5,
            Some(e) => return Err(format!("sign-in failed: {e}")),
            None => {
                OAuth::from_grant(&v, "")?.save()?;
                return Ok(());
            }
        }
    }
}

// ---- setup wizard ----

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

// Read one line from fd 0 a byte at a time, so input beyond the newline stays
// unconsumed for child processes (read_secret) and later reads even when stdin
// is a pipe. None on EOF with nothing read.
fn read_line() -> Option<String> {
    let mut f = std::mem::ManuallyDrop::new(unsafe { fs::File::from_raw_fd(0) });
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    let eof = loop {
        match f.read(&mut b) {
            Ok(0) => break true,
            Ok(_) if b[0] == b'\n' => break false,
            Ok(_) => buf.push(b[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
        }
    };
    if eof && buf.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

// Read a line with terminal echo off (delegated to bash's `read -rs`).
fn read_secret() -> Result<String, String> {
    let out = Command::new("bash")
        .arg("-c")
        .arg("IFS= read -rs k && printf %s \"$k\"")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    println!();
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// Merge into the existing config so hand-added keys (SPORE_HOME,
// SPORE_API_KEY_CMD, …) survive re-running the wizard. On an OAuth sign-in
// (key: None) any stored key is removed — a static key would shadow OAuth.
fn save_provider(p: &Provider, key: Option<&str>) -> Result<(), String> {
    let mut kv = load_kv("config");
    kv.insert("SPORE_BASE_URL".into(), p.url.into());
    kv.insert("SPORE_MODEL".into(), p.model.into());
    match key {
        Some(k) => kv.insert("SPORE_API_KEY".into(), k.into()),
        None => kv.remove("SPORE_API_KEY"),
    };
    let mut lines: Vec<(&str, &str)> = kv.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    lines.sort();
    save_kv("config", &lines)
}

fn wizard() -> Result<(), String> {
    println!("spore setup — pick a provider:");
    for (i, p) in PROVIDERS.iter().enumerate() {
        let local = if p.console.is_none() { " (local, no key)" } else { "" };
        println!("  {}) {}{local}", i + 1, p.name);
    }
    let p = loop {
        print!("\nProvider number: ");
        let _ = io::stdout().flush();
        let line = read_line().ok_or("setup aborted")?;
        if let Ok(n) = line.trim().parse::<usize>() {
            if (1..=PROVIDERS.len()).contains(&n) {
                break &PROVIDERS[n - 1];
            }
        }
    };

    if p.name == "xai" {
        println!("\n1) Sign in with browser (Grok / X Premium subscription)\n2) Paste an API key");
        let sign_in = loop {
            print!("Choose 1 or 2: ");
            let _ = io::stdout().flush();
            match read_line().ok_or("setup aborted")?.trim() {
                "1" => break true,
                "2" => break false,
                _ => {}
            }
        };
        if sign_in {
            device_flow()?;
            save_provider(p, None)?;
            println!("Signed in — tokens saved to ~/.config/spore/oauth, using {}.", p.model);
            return Ok(());
        }
    }

    let key = match p.console {
        Some(c) => {
            println!("Opening {c} — create an API key there.");
            open_browser(c);
            print!("Paste API key: ");
            let _ = io::stdout().flush();
            let k = read_secret()?;
            if k.is_empty() {
                return Err("empty API key".into());
            }
            k
        }
        None => "ollama".into(),
    };
    save_provider(p, Some(&key))?;
    println!("Saved to ~/.config/spore/config — using {}.", p.model);
    Ok(())
}

struct Spore {
    key: Option<String>,
    key_cmd: Option<String>,
    oauth: Option<OAuth>,
    model: String,
    url: String,
    home: String,
    depth: u32,
}

impl Spore {
    fn load() -> Spore {
        let file = load_kv("config");
        Spore {
            key_cmd: cfg_opt(&file, "SPORE_API_KEY_CMD"),
            key: env_opt("SPORE_API_KEY")
                .or_else(|| env_opt("XAI_API_KEY"))
                .or_else(|| env_opt("OPENAI_API_KEY"))
                .or_else(|| file.get("SPORE_API_KEY").cloned().filter(|v| !v.is_empty())),
            oauth: OAuth::load(),
            model: cfg_opt(&file, "SPORE_MODEL").unwrap_or_else(|| MODEL.into()),
            url: cfg_opt(&file, "SPORE_BASE_URL").unwrap_or_else(|| URL.into()),
            home: cfg_opt(&file, "SPORE_HOME").unwrap_or_else(|| HOME.into()),
            depth: env_opt("SPORE_DEPTH").and_then(|d| d.parse().ok()).unwrap_or(0),
        }
    }

    fn no_creds(&self) -> bool {
        self.key.is_none() && self.key_cmd.is_none() && self.oauth.is_none()
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }
    if matches!(args.first().map(String::as_str), Some("login") | Some("--login")) {
        if let Err(e) = wizard() {
            die(e);
        }
        println!("spore: setup complete.");
        return;
    }

    let mut spore = Spore::load();
    if spore.depth > MAX_DEPTH {
        die(format!("max subagent depth ({MAX_DEPTH}) reached"));
    }

    let task: Vec<&str> = args
        .iter()
        .filter(|a| *a != "-p" && *a != "--print")
        .map(String::as_str)
        .collect();

    if spore.no_creds() {
        if task.is_empty() && io::stdin().is_terminal() {
            if let Err(e) = wizard() {
                die(e);
            }
            spore = Spore::load();
            if spore.no_creds() {
                die("setup finished without credentials".into());
            }
        } else {
            die("set XAI_API_KEY, OPENAI_API_KEY, SPORE_API_KEY, or SPORE_API_KEY_CMD, or run `spore login`".into());
        }
    }

    if task.is_empty() {
        spore.repl();
    } else {
        let mut msgs = vec![json!({"role": "user", "content": task.join(" ")})];
        println!("{}", spore.run(&mut msgs, true).trim_end());
    }
}

impl Spore {
    fn repl(&mut self) {
        println!("\x1b[1mspore\x1b[0m \x1b[2m· one tool: bash · /login to reconfigure · ctrl-d to exit\x1b[0m");
        let mut msgs = vec![];
        loop {
            print!("\n\x1b[32m❯\x1b[0m ");
            let _ = io::stdout().flush();
            let Some(line) = read_line() else {
                return;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line == "/exit" {
                return;
            }
            if line == "/login" {
                match wizard() {
                    Ok(()) => *self = Spore::load(),
                    Err(e) => eprintln!("spore: {e}"),
                }
                continue;
            }
            msgs.push(json!({"role": "user", "content": line.to_string()}));
            self.run(&mut msgs, false);
        }
    }

    fn run(&mut self, msgs: &mut Vec<Value>, headless: bool) -> String {
        loop {
            let (assistant, calls, text) = match self.turn(msgs, headless) {
                Ok(t) => t,
                Err(e) if headless => die(e),
                Err(e) => {
                    eprintln!("spore: {e}");
                    return String::new();
                }
            };
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

    #[allow(clippy::type_complexity)]
    fn turn(
        &mut self,
        msgs: &[Value],
        headless: bool,
    ) -> Result<(Value, Vec<(String, String)>, String), String> {
        let token = self.token()?;
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

        let resp = http_agent()
            .post(&self.url)
            .set("authorization", &format!("Bearer {token}"))
            .set("content-type", "application/json")
            .send_string(&body.to_string());
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => {
                return Err(format!("api {code}: {}", r.into_string().unwrap_or_default()))
            }
            Err(e) => return Err(e.to_string()),
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
        Ok((assistant, cmds, text))
    }

    fn bash(&self, cmd: &str) -> String {
        let child = Command::new("bash")
            .arg("-c")
            .arg(cmd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("SPORE_DEPTH", (self.depth + 1).to_string())
            .env("SPORE_HOME", &self.home)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return format!("bash failed to start: {e}"),
        };
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());

        let deadline = Instant::now() + BASH_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };

        let mut s = stdout.join().unwrap_or_default();
        s.push_str(&stderr.join().unwrap_or_default());
        match status {
            None => s.push_str(&format!(
                "\n[killed: timed out after {}s]",
                BASH_TIMEOUT.as_secs()
            )),
            Some(st) => {
                let code = st.code().unwrap_or(-1);
                if s.trim().is_empty() {
                    s = format!("(no output; exit {code})");
                } else if code != 0 {
                    s.push_str(&format!("\n[exit {code}]"));
                }
            }
        }
        truncate(s)
    }

    // keycmd > static key (env, then config file) > OAuth
    fn token(&mut self) -> Result<String, String> {
        if let Some(cmd) = &self.key_cmd {
            return match Command::new("bash").arg("-c").arg(cmd).output() {
                Ok(o) if o.status.success() => {
                    let t = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if t.is_empty() {
                        return Err("SPORE_API_KEY_CMD produced no token".into());
                    }
                    Ok(t)
                }
                _ => Err("SPORE_API_KEY_CMD failed".into()),
            };
        }
        if let Some(k) = &self.key {
            return Ok(k.clone());
        }
        match self.oauth.as_mut() {
            Some(o) => o.fresh_access(),
            None => Err("no credentials; run `spore login`".into()),
        }
    }
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
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
